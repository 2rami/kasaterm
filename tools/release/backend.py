"""실제 단계 — 정본 파이프라인(버전 커밋·태그 → release.yml → appcast)을 그대로 쓰고, 앞뒤를 읽어 맞춘다.

`tag-release.sh` 를 그대로 부르지 않는 까닭: 그 스크립트는 로컬 태그만 보고, 지금 체크아웃의 `main` 가지를 push 한다.
공유 워킹트리에서는 그 `main` 이 계획 커밋이라는 보장이 없다. 그래서 같은 일(같은 치환·같은 커밋 메시지·같은 태그)을
계획 커밋의 격리 워크트리에서 하고, `HEAD:main` 과 태그를 `--atomic` 으로 한 번에 올린다 — 둘 중 하나만 올라가는
반쪽 상태를 만들지 않는다. CI 는 태그 push 로 도는 release.yml 그대로다.
"""

import base64
import json
import os
from pathlib import Path
import re
import shutil

from tools.release import deps
from tools.release.common import Pending, Refused, feed_item, fetch_feed, sha256_bytes, sha256_file, version_tuple

REPO_SLUG = "2rami/kasaterm"
TESTS = (["cargo", "test", "-p", "kasaterm", "--release"],
         ["cargo", "test", "-p", "kasa-mcp", "--release"],
         ["cargo", "test", "-p", "kasa-socket", "--release"])
PUBLISH_STAGES = ("tag", "release", "feed")
# Ed25519 SubjectPublicKeyInfo 머리 — Sparkle 공개키(32바이트)를 openssl 이 읽는 PEM 으로 싼다.
_ED25519_SPKI = bytes.fromhex("302a300506032b6570032100")


def asset_names(tag):
    return {"macos": f"kasaterm-{tag}.dmg", "windows": f"kasaterm-{tag}-windows-x86_64.msi"}


def parse_identity(text):
    auth = re.search(r"^Authority=(.+)$", text, re.M)
    team = re.search(r"^TeamIdentifier=(.+)$", text, re.M)
    t = team.group(1).strip() if team else None
    return {"authority": auth.group(1).strip() if auth else None, "team": None if t in (None, "not set") else t}


def identity_of(runner, app, tools):
    """번들의 서명 신원 — 읽기만. 서명이 깨졌으면 신원을 말하지 않는다."""
    codesign, spctl = tools["codesign"]["path"], tools["spctl"]["path"]
    if not codesign or not spctl:
        return {"verified": False, "authority": None, "team": None, "notarized": False, "why": "codesign·spctl 이 없다"}
    verify = runner.run([codesign, "--verify", "--deep", "--strict", str(app)], timeout=180)
    if verify.skipped:
        return None
    if not verify.ok:
        return {"verified": False, "authority": None, "team": None, "notarized": False, "why": verify.tail(2)}
    shown = runner.run([codesign, "-dvv", str(app)], timeout=60)
    ident = parse_identity((shown.err or "") + "\n" + (shown.out or ""))
    gate = runner.run([spctl, "--assess", "--type", "execute", "-vv", str(app)], timeout=60)
    return {"verified": True, **ident, "notarized": gate.ok and "Notarized" in (gate.err + gate.out)}


def identity_block(release, installed):
    """자동 배포해도 되는 같은 신원인가 — 아니면 정확한 까닭. 보안 설정을 끄는 길은 없다."""
    if not release or not release.get("team"):
        return "CI mac 판에 팀 서명 신원이 없다(자체 서명) — Developer ID 설치본에 자동 배포하지 않는다"
    if not release.get("notarized"):
        return "CI mac 판이 공증되지 않았다 — Developer ID 설치본에 자동 배포하지 않는다"
    if not installed or not installed.get("verified") or not installed.get("team"):
        return "설치본 서명 신원을 확인하지 못했다"
    if release["team"] != installed["team"]:
        return f"서명 팀이 다르다(CI {release['team']} ≠ 설치본 {installed['team']})"
    return None


def ed25519_ok(runner, openssl, public_b64, signature_b64, path, scratch):
    """Sparkle EdDSA 는 파일 원문에 대한 Ed25519 서명이다. 저장소에 박힌 공개키로 openssl 이 확인한다."""
    try:
        raw = base64.b64decode(public_b64, validate=True)
        sig = base64.b64decode(signature_b64, validate=True)
    except (ValueError, TypeError):
        return False
    if len(raw) != 32 or len(sig) != 64:
        return False
    scratch.mkdir(parents=True, exist_ok=True)
    pem = scratch / "sparkle-ed25519.pem"
    pem.write_text("-----BEGIN PUBLIC KEY-----\n" + base64.b64encode(_ED25519_SPKI + raw).decode() + "\n-----END PUBLIC KEY-----\n")
    sig_path = scratch / (Path(path).name + ".sig")
    sig_path.write_bytes(sig)
    r = runner.run([openssl, "pkeyutl", "-verify", "-pubin", "-inkey", str(pem), "-rawin",
                    "-in", str(path), "-sigfile", str(sig_path)], timeout=120, kind="local")
    return r.skipped or (r.ok and "Signature Verified Successfully" in r.out)


class RealBackend:
    def __init__(self, repo, runner, http, workdir, tracker, tools, slug=REPO_SLUG, remote="origin"):
        self.repo, self.runner, self.http, self.workdir = Path(repo), runner, http, Path(workdir)
        self.tracker, self.tools, self.slug, self.remote = tracker, tools, slug, remote

    def tool(self, name):
        """계획에 못 박힌 절대경로. 없으면 그 도구가 왜 없는지 그대로 말하고 멈춘다."""
        t = self.tools.get(name) or {}
        if not t.get("path"):
            raise Refused(t.get("why") or f"{name} 이 계획의 도구 표에 없다")
        return t["path"]

    @property
    def gh(self):
        return self.tool("gh")

    @property
    def dry(self):
        return self.runner.mode == "dry"

    def git(self, *args, kind="read", timeout=120, cwd=None):
        return self.runner.run(["git", "-C", str(cwd or self.repo), *args], timeout=timeout, kind=kind,
                               env=deps.git_env(self.tools))

    def env(self):
        return {**deps.tool_env(self.tools, ("cargo", "git-lfs", "gh")), "CARGO_TARGET_DIR": str(self.workdir / "target")}

    # ── 원격 상태 ─────────────────────────────────────────────────────────
    def remote_ref(self, ref):
        r = self.git("ls-remote", self.remote, ref, timeout=60)
        if not r.ok:
            raise Refused(f"원격을 읽지 못했다({ref}) — {r.tail(2) or '시간 초과'}")
        # 주석 태그는 ^{} 줄이 가리키는 커밋이 진짜 대상이다
        rows = dict(line.split("\t")[::-1] for line in r.out.splitlines() if "\t" in line)
        return rows.get(ref + "^{}") or rows.get(ref)

    def feed_base(self, plan):
        mac = fetch_feed(self.http, plan["feed"]["source"])
        win = fetch_feed(self.http, plan["feed"]["windows"])
        if mac is None or win is None:
            raise Refused("피드를 읽지 못해 기준 해시를 못 쟀다")
        return sha256_bytes(json.dumps({"macos": sha256_bytes(mac), "windows": sha256_bytes(win)}, sort_keys=True).encode())

    def resume_facts(self, plan):
        """나쵸 `resume` 에 싣는 원격 사실 — 지금 main, 태그가 섰으면 그 커밋의 부모(= 계획 커밋이어야 한다)."""
        out = {"main": self.remote_ref("refs/heads/" + plan["branch"])}
        tagged = self.remote_ref("refs/tags/" + plan["tag"])
        if tagged:
            self.git("fetch", "-q", self.remote, f"refs/tags/{plan['tag']}", timeout=120)
            parent = self.git("rev-parse", f"{tagged}^")
            out["tag_parent"] = parent.out.strip() if parent.ok else None
        return out

    def observe(self, plan):
        """dry-run·status 가 보이는 원격 사실 — 전부 읽기."""
        out = {}
        try:
            out["main"] = self.remote_ref("refs/heads/" + plan["branch"])
            out["tag"] = self.remote_ref("refs/tags/" + plan["tag"])
        except Refused as e:
            out["error"] = str(e)
        run = self.ci_run(plan["tag"]) if out.get("tag") else None
        out["ci"] = {k: run.get(k) for k in ("databaseId", "status", "conclusion")} if run else None
        for key, src in (("feed_macos", plan["feed"]["source"]), ("feed_windows", plan["feed"]["windows"])):
            raw = fetch_feed(self.http, src)
            out[key] = feed_item(raw)["version"] if raw is not None else None
        return out

    def ci_run(self, tag):
        r = self.runner.run([self.gh, "run", "list", "--repo", self.slug, "--workflow", "release.yml", "--json",
                             "databaseId,status,conclusion,headBranch,headSha,event", "--limit", "30"], timeout=60)
        if not r.ok:
            raise Refused(f"CI 상태를 읽지 못했다 — {r.tail(2) or '시간 초과'}")
        runs = [x for x in json.loads(r.out or "[]") if x.get("headBranch") == tag]
        return runs[0] if runs else None

    # ── 격리 워크트리 ──────────────────────────────────────────────────────
    def worktree(self, plan):
        wt = self.workdir / "wt"
        if (wt / ".git").exists():
            head = self.git("rev-parse", "HEAD", cwd=wt)
            if head.ok and head.out.strip() in (plan["commit"], self.bump_parent_ok(wt, plan)):
                return wt
            self.git("worktree", "remove", "--force", str(wt), kind="local")
        if not self.dry:
            self.workdir.mkdir(parents=True, exist_ok=True)
        r = self.git("worktree", "add", "--detach", str(wt), plan["commit"], kind="local")
        if not r.ok:
            raise Refused(f"격리 워크트리를 못 만들었다 — {r.tail(3)}")
        return wt

    def bump_parent_ok(self, wt, plan):
        parent = self.git("rev-parse", "HEAD^", cwd=wt)
        return self.git("rev-parse", "HEAD", cwd=wt).out.strip() if parent.ok and parent.out.strip() == plan["commit"] else None

    # ── 단계 ──────────────────────────────────────────────────────────────
    def verify(self, plan, state):
        wt = self.worktree(plan)
        ran = []
        cargo = self.tool("cargo")
        for argv in TESTS:
            argv = [cargo, *argv[1:]]
            r = self.runner.run(argv, cwd=wt, timeout=3600, env=self.env(), kind="local")
            ran.append(" ".join(argv))
            if r.timed_out:
                raise Refused(f"검사 시간 초과: {' '.join(argv)}")
            if not r.ok:
                raise Refused(f"검사 실패: {' '.join(argv)} — {r.tail(4)}")
        return {"worktree": str(wt), "ran": ran, "dry": self.dry}

    def build(self, plan, state):
        """태그 전 굽기 확인. 같은 커밋의 ready 판이 있으면 그 판의 서명·바이너리 해시를 다시 재고 갈음한다."""
        for b in plan.get("ready_builds", []):
            if not b["matches"]:
                continue
            app = self.repo / "dist" / b["manifest"].replace("kasaterm.build.ready-", "kasaterm.app.ready-").replace(".json", "")
            manifest = json.loads((self.repo / "dist" / b["manifest"]).read_text())
            want = ((manifest.get("components") or {}).get("app") or {}).get("sha256")
            binary = app / "Contents/MacOS/kasaterm"
            if not binary.exists() or not want or sha256_file(binary) != "sha256:" + want:
                continue
            ident = identity_of(self.runner, app, self.tools)
            if ident and not ident["verified"]:
                continue
            return {"reused": str(app), "sha256": "sha256:" + want, "identity": ident}
        wt = self.worktree(plan)
        r = self.runner.run(["bash", "scripts/build-app.sh"], cwd=wt, timeout=3600, env=self.env(), kind="local")
        if r.skipped:
            return {"would": "격리 워크트리에서 scripts/build-app.sh", "dry": True}
        if not r.ok:
            raise Refused(f"굽기 실패 — {'시간 초과' if r.timed_out else r.tail(4)}")
        binary = wt / "dist/kasaterm.app/Contents/MacOS/kasaterm"
        if not binary.exists():
            raise Refused("굽기는 끝났는데 번들이 없다")
        return {"built": str(wt / "dist/kasaterm.app"), "sha256": sha256_file(binary),
                "identity": identity_of(self.runner, wt / "dist/kasaterm.app", self.tools)}

    def tag_commands(self, plan):
        return [["git", "worktree", "add", "--detach", str(self.workdir / "wt"), plan["commit"]],
                ["(Cargo.toml", "워크스페이스", "버전", "→", plan["version"], "—", "tag-release.sh", "와", "같은", "치환)"],
                ["git", "commit", "-qam", f"chore(release): {plan['tag']}"],
                ["git", "push", "--atomic", self.remote, f"HEAD:refs/heads/{plan['branch']}", f"HEAD:refs/tags/{plan['tag']}"]]

    def preview(self, stage, plan):
        """live 가 아닐 때 게시 단계가 보이는 것 — 명령과 원격 사실. 로컬 저장소에도 흔적을 안 남긴다."""
        seen = self.observe(plan)
        if stage == "tag":
            return {"would": [" ".join(c) for c in self.tag_commands(plan)], "remote": seen}
        if stage == "release":
            return {"would": f"release.yml(태그 push 로 돈다) 완료 확인 → {', '.join(asset_names(plan['tag']).values())} 받기 · 크기·해시 · dmg 서명 신원",
                    "remote": seen}
        return {"would": "두 피드가 목표 판·산출물 이름·크기를 가리키는지, EdDSA 서명이 산출물과 맞는지(저장소 공개키)",
                "remote": seen}

    def tag(self, plan, state):
        """버전 커밋 + 태그를 격리 워크트리에서 만들고 한 번에 올린다. 로컬 태그는 만들지 않는다(공유 저장소 refs 불변)."""
        tag, main = plan["tag"], "refs/heads/" + plan["branch"]
        tagged = self.remote_ref("refs/tags/" + tag)
        if tagged:
            return self.reconcile_tag(plan, tagged)
        head = self.remote_ref(main)
        if head != plan["commit"]:
            raise Refused(f"원격 {plan['branch']} 가 계획 커밋이 아니다({(head or '?')[:8]}) — 계획에 없는 변경이 섞이므로 새 계획이 필요하다")
        wt = self.worktree(plan)
        if self.git("rev-parse", "HEAD", cwd=wt).out.strip() == plan["commit"]:
            cargo = wt / "Cargo.toml"
            text = cargo.read_text()
            bumped = re.sub(r'^version = "[^"]*"', f'version = "{plan["version"]}"', text, count=1, flags=re.M)
            if bumped == text:
                raise Refused("Cargo.toml 의 워크스페이스 버전 줄을 못 찾았다")
            cargo.write_text(bumped)
            c = self.git("commit", "-qam", f"chore(release): {tag}", kind="local", cwd=wt)
            if not c.ok:
                raise Refused(f"버전 커밋 실패 — {c.tail(3)}")
        bump = self.git("rev-parse", "HEAD", cwd=wt).out.strip()
        push = self.git("push", "--atomic", self.remote, f"HEAD:{main}", f"HEAD:refs/tags/{tag}",
                        kind="publish", timeout=180, cwd=wt)
        if push.ok:
            return {"tag": tag, "commit": bump, "pushed": True}
        # 응답이 실패·시간 초과여도 원격에 반영됐을 수 있다 — 원격을 다시 읽어 맞춘다.
        now_tag, now_main = self.remote_ref("refs/tags/" + tag), self.remote_ref(main)
        if now_tag == bump and now_main == bump:
            return {"tag": tag, "commit": bump, "pushed": True, "note": "push 응답은 실패였지만 원격에 반영돼 있었다"}
        if not now_tag and now_main == plan["commit"]:
            raise Refused(f"push 실패 — 원격은 그대로다(다시 돌리면 이어서) · {'시간 초과' if push.timed_out else push.tail(2)}")
        raise Refused(f"push 뒤 원격이 반쪽이다(태그 {(now_tag or '없음')[:8]}, {plan['branch']} {(now_main or '?')[:8]}) — 손대지 않고 멈춘다")

    def reconcile_tag(self, plan, tagged):
        # FETCH_HEAD 로만 받는다 — 로컬 태그 ref 를 만들지 않는다.
        self.git("fetch", "-q", self.remote, f"refs/tags/{plan['tag']}", timeout=120)
        parent = self.git("rev-parse", f"{tagged}^")
        cargo = self.git("show", f"{tagged}:Cargo.toml")
        m = re.search(r'^version = "([^"]*)"', cargo.out or "", re.M)
        if not (parent.ok and parent.out.strip() == plan["commit"] and m and m.group(1) == plan["version"]):
            raise Refused(f"태그 {plan['tag']} 가 이미 있는데 이 계획의 버전 커밋이 아니다 — 손대지 않는다")
        return {"tag": plan["tag"], "commit": tagged, "result": "이미 선 태그 — 다시 세우지 않음"}

    def release(self, plan, state):
        tag = plan["tag"]
        run = self.ci_run(tag)
        if not run:
            raise Pending("CI 가 아직 안 떴다")
        if run.get("status") != "completed":
            raise Pending(f"CI 진행 중({run.get('status')}) · run {run.get('databaseId')}")
        if run.get("conclusion") != "success":
            raise Refused(f"CI 실패({run.get('conclusion')}) · run {run.get('databaseId')} — 산출물·appcast 를 확인하지 않는다")
        view = self.runner.run([self.gh, "release", "view", tag, "--repo", self.slug, "--json", "assets,isDraft"], timeout=60)
        if not view.ok:
            raise Refused(f"릴리스를 읽지 못했다 — {view.tail(2)}")
        assets = {a["name"]: a for a in json.loads(view.out).get("assets", [])}
        want = asset_names(tag)
        missing = [p for p, name in want.items() if name not in assets]
        if missing:
            raise Refused(f"릴리스 산출물이 모자라다({', '.join(missing)}) — appcast 로 넘어가지 않는다")
        dl = self.workdir / "assets" / tag
        dl.mkdir(parents=True, exist_ok=True)
        got = {}
        for platform, name in want.items():
            r = self.runner.run([self.gh, "release", "download", tag, "--repo", self.slug, "--pattern", name,
                                 "--dir", str(dl), "--clobber"], timeout=900, kind="local")
            if r.skipped:
                continue
            path = dl / name
            if not r.ok or not path.exists():
                raise Refused(f"{name} 를 받지 못했다 — {'시간 초과' if r.timed_out else r.tail(2)}")
            digest = sha256_file(path)
            if path.stat().st_size != assets[name].get("size"):
                raise Refused(f"{name} 크기가 릴리스 기록과 다르다")
            if assets[name].get("digest") and assets[name]["digest"] != digest:
                raise Refused(f"{name} 해시가 릴리스 기록과 다르다")
            got[platform] = {"name": name, "size": path.stat().st_size, "sha256": digest, "path": str(path)}
        detail = {"run": run.get("databaseId"), "assets": got}
        if "macos" in got:
            detail["mac_identity"] = self.dmg_identity(Path(got["macos"]["path"]))
            why = identity_block(detail["mac_identity"], plan["signing"].get("installed"))
            if why:
                raise Refused(f"mac 판 서명 확인 — {why}")
        return detail

    def dmg_identity(self, dmg):
        mnt = self.workdir / "mnt"
        shutil.rmtree(mnt, ignore_errors=True)
        mnt.mkdir(parents=True)
        hdiutil = self.tool("hdiutil")
        a = self.runner.run([hdiutil, "attach", "-readonly", "-nobrowse", "-mountpoint", str(mnt), str(dmg)],
                            timeout=180, kind="local")
        if not a.ok:
            raise Refused(f"dmg 를 열지 못했다 — {a.tail(2)}")
        try:
            return identity_of(self.runner, mnt / "kasaterm.app", self.tools)
        finally:
            self.runner.run([hdiutil, "detach", str(mnt)], timeout=120, kind="local")

    def feed(self, plan, state):
        assets = (state["stages"].get("release", {}).get("detail") or {}).get("assets") or {}
        key = plan["capabilities"]["macos"].get("ed_public_key")
        openssl = self.tool("openssl")
        ok, version, why = deps.probe_openssl(self.runner, openssl)
        if not ok:
            raise Refused(f"계획에 고정한 openssl({openssl}, {version or '판 모름'})이 지금은 {why} — 서명을 확인하지 않고 멈춘다")
        seen, signatures = {}, {}
        for platform, src in (("macos", plan["feed"]["source"]), ("windows", plan["feed"]["windows"])):
            raw = fetch_feed(self.http, src)
            if raw is None:
                raise Pending(f"{platform} 피드를 읽지 못했다")
            item = feed_item(raw)
            have, want = version_tuple(item["version"]), version_tuple(plan["version"])
            if have and have > want:
                raise Refused(f"{platform} 피드가 이미 더 새 판({item['version']}) — 되돌리지 않는다")
            if have != want:
                raise Pending(f"{platform} 피드가 아직 {item['version']} — CI 가 appcast 를 올리기를 기다린다")
            asset = assets.get(platform)
            if not asset:
                raise Refused(f"검증된 {platform} 산출물 없이 피드를 확인하지 않는다")
            if (item["url"] or "").rsplit("/", 1)[-1] != asset["name"] or item["length"] != asset["size"]:
                raise Refused(f"{platform} 피드가 가리키는 파일이 릴리스 산출물과 다르다")
            if not key or not item["signature"]:
                raise Refused(f"{platform} 피드 EdDSA 서명이나 저장소 공개키가 없다")
            if not ed25519_ok(self.runner, openssl, key, item["signature"], asset["path"], self.workdir / "sig"):
                raise Refused(f"{platform} 피드 EdDSA 서명이 산출물과 맞지 않는다")
            seen[platform] = sha256_bytes(raw)
            signatures[platform] = item["signature"]
        before = (state["stages"].get("feed", {}).get("detail") or {}).get("hashes")
        if before and before != seen:
            raise Refused("한 번 확인한 피드가 그 뒤 바뀌었다 — 누가 다시 게시했는지 먼저 본다")
        # 기기 업데이트 작업이 이 서명을 싣는다 — 기기는 피드가 같은 서명을 말하는지 다시 대조하고, 받은 원문으로 다시 검증한다.
        return {"hashes": seen, "signatures": signatures, "version": plan["version"]}

    def devices(self, plan, state):
        rows = self.tracker(plan, state)
        return {label: r["state"] for label, r in rows.items()}
