"""실제 단계 — 정본 파이프라인(버전 커밋·태그 → release.yml → appcast)을 그대로 쓰고, 앞뒤를 읽어 맞춘다.

`tag-release.sh` 를 그대로 부르지 않는 까닭: 그 스크립트는 로컬 태그만 보고, 지금 체크아웃의 `main` 가지를 push 한다.
공유 워킹트리에서는 그 `main` 이 계획 커밋이라는 보장이 없다. 그래서 같은 일(같은 치환·같은 커밋 메시지·같은 태그)을
계획 커밋의 격리 워크트리에서 하고, `HEAD:main` 과 태그를 `--atomic` 으로 한 번에 올린다 — 둘 중 하나만 올라가는
반쪽 상태를 만들지 않는다. CI 는 태그 push 로 도는 release.yml 그대로다.

release.yml 이 `MAC_ARTIFACT: local` 이면 mac dmg 는 이 기기가 만든다(tools/release/macsign.py): 굽기 단계가 버전 커밋으로
Developer ID·hardened runtime 서명해 굽고, 태그 단계가 승인 뒤 공증·staple·재검증을 마친 다음에만 push 하며, 릴리스
단계가 그 dmg 를 덮지 않고 올린다. CI 는 그 dmg 를 검증만 하고, 여기서는 받은 파일이 공증한 해시와 같은지 다시 본다.
열쇠고리는 부르는 쪽이 `KASATERM_RELEASE_UNLOCK=1` 로 맡겼을 때만 기존 풀기 도우미로 서명·공증 바로 앞에서 푼다.
"""

import base64
import json
import os
from pathlib import Path
import re
import shutil

from tools.release import deps, macsign
from tools.release.common import (CHANNEL_MANIFEST, Pending, Refused, channel_manifest, feed_fingerprint,
                                  feed_item, fetch_feed, sha256_bytes, sha256_file, validate_channel_manifest, version_tuple)

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
    who = "로컬 mac 판" if (release or {}).get("source") == "local" else "CI mac 판"
    if not release or not release.get("team"):
        return f"{who}에 팀 서명 신원이 없다(자체 서명) — Developer ID 설치본에 자동 배포하지 않는다"
    if not release.get("notarized"):
        return f"{who}이 공증되지 않았다 — Developer ID 설치본에 자동 배포하지 않는다"
    if not installed or not installed.get("verified") or not installed.get("team"):
        return "설치본 서명 신원을 확인하지 못했다"
    if release["team"] != installed["team"]:
        return f"서명 팀이 다르다({who} {release['team']} ≠ 설치본 {installed['team']})"
    return None


def mac_local(plan):
    """이 계획이 mac dmg 를 이 기기에서 서명·공증하는가 — 계획 때 준비를 다 확인한 경우에만 설정을 돌려준다."""
    cfg = plan.get("mac_artifact") or {}
    return cfg if cfg.get("mode") == "local" and cfg.get("identity") and not cfg.get("problems") else None


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
    def __init__(self, repo, runner, http, workdir, tracker, tools, slug=REPO_SLUG, remote="origin", unlock=False):
        self.repo, self.runner, self.http, self.workdir = Path(repo), runner, http, Path(workdir)
        self.tracker, self.tools, self.slug, self.remote = tracker, tools, slug, remote
        self.unlock = unlock

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

    def git(self, *args, kind="read", timeout=120, cwd=None, extra=None):
        return self.runner.run(["git", "-C", str(cwd or self.repo), *args], timeout=timeout, kind=kind,
                               env={**deps.git_env(self.tools), **(extra or {})})

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
        fingerprint, _, _ = feed_fingerprint(self.http, plan["feed"]["source"],
                                           plan["feed"].get("windows") if plan["channel"] == "stable" else None)
        if fingerprint is None:
            raise Refused("피드를 읽지 못해 기준 해시를 못 쟀다")
        return fingerprint

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
        for key, src in (("feed_macos", plan["feed"]["source"]), ("feed_windows", plan["feed"].get("windows"))):
            if src is None:
                continue
            raw = fetch_feed(self.http, src)
            out[key] = feed_item(raw)["version"] if raw is not None else None
        return out

    def ci_run(self, tag):
        """그 태그의 가장 최근 release.yml 실행 — 태그 push 로 돈 것, 또는 main 의 워크플로로 그 태그를 마무리한 수동 실행
        (`release vX.Y.Z (both|macos)`, release.yml run-name). 태그의 워크플로는 커밋에 박혀 고칠 수 없어, 태그 실행이
        멈추면 마무리 실행이 뒤를 잇는다. mac 만 마무리한 실행이면 뒤의 산출물 검사가 msi 없음으로 멈춘다(성공으로 안 친다)."""
        r = self.runner.run([self.gh, "run", "list", "--repo", self.slug, "--workflow", "release.yml", "--json",
                             "databaseId,status,conclusion,headBranch,headSha,event,displayTitle", "--limit", "30"], timeout=60)
        if not r.ok:
            raise Refused(f"CI 상태를 읽지 못했다 — {r.tail(2) or '시간 초과'}")
        finish = re.compile(rf"release {re.escape(tag)} \((both|macos)\)")
        runs = [x for x in json.loads(r.out or "[]") if x.get("headBranch") == tag
                or (x.get("event") == "workflow_dispatch" and finish.fullmatch(x.get("displayTitle") or ""))]
        return runs[0] if runs else None

    def appcast_retry_hint(self, run, plan):
        # 검사·공증 실패를 재실행으로 가리지 않도록 게시 job의 CAS 소진만 복구 안내한다.
        run_id = run.get("databaseId")
        if run.get("conclusion") != "failure" or type(run_id) is not int or run_id <= 0:
            return ""
        view = self.runner.run([self.gh, "run", "view", str(run_id), "--repo", self.slug, "--json", "jobs"], timeout=60)
        if not view.ok:
            return ""
        try:
            rows = json.loads(view.out).get("jobs", [])
            jobs = {job["name"]: job for job in rows}
            expected = {"resolve": "success", "build-dmg": "success", "appcast": "failure",
                        "build-msi": "success" if "windows" in plan["platforms"] else "skipped"}
            if len(rows) != 4 or set(jobs) != set(expected):
                return ""
            if any(jobs[name].get("status") != "completed" or jobs[name].get("conclusion") != conclusion
                   for name, conclusion in expected.items()):
                return ""
            appcast = jobs["appcast"]
            job_id = appcast.get("databaseId")
            steps = appcast.get("steps", [])
            failed = [step.get("name") for step in steps if step.get("conclusion") not in ("success", "skipped")]
            completed = {step.get("name") for step in steps if step.get("conclusion") == "success"}
            if (type(job_id) is not int or job_id <= 0 or failed != ["Publish verified appcasts"]
                    or not {"Checkout main", "Sparkle signing tools", "Generate signed appcasts"} <= completed):
                return ""
        except (ValueError, KeyError, TypeError, AttributeError):
            return ""
        logs = self.runner.run([self.gh, "run", "view", str(run_id), "--repo", self.slug,
                                "--job", str(job_id), "--log-failed"], timeout=60)
        if not logs.ok or not any(line.split()[-1:] == ["KASATERM_APPCAST_CAS_EXHAUSTED"] for line in logs.out.splitlines()):
            return ""
        return (f" — main 경합 재시도 소진. 명시적 게시 job 재실행: gh run rerun {run_id} --job {job_id} --repo {self.slug}"
                " (검사·굽기·공증 job은 다시 실행하지 않음)")

    # ── 격리 워크트리 ──────────────────────────────────────────────────────
    def worktree(self, plan, name="wt"):
        wt = self.workdir / name
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

    def ensure_bump(self, wt, plan):
        """버전 커밋 — tag-release.sh 와 같은 치환·메시지. 날짜를 계획 시각에 못 박아 몇 번을 다시 만들어도 같은 커밋이다.
        굽기와 태그가 다른 실행이어도(그 사이 워크트리를 치운다) dmg 가 태그 커밋에서 구운 것임을 해시로 잇는다."""
        if self.git("rev-parse", "HEAD", cwd=wt).out.strip() == plan["commit"]:
            cargo = wt / "Cargo.toml"
            text = cargo.read_text()
            bumped = re.sub(r'^version = "[^"]*"', f'version = "{plan["version"]}"', text, count=1, flags=re.M)
            if bumped == text:
                raise Refused("Cargo.toml 의 워크스페이스 버전 줄을 못 찾았다")
            cargo.write_text(bumped)
            manifest_path = wt / CHANNEL_MANIFEST
            manifest_path.parent.mkdir(parents=True, exist_ok=True)
            manifest_path.write_text(json.dumps(channel_manifest(plan["channel"], plan["tag"], plan["commit"], plan["platforms"]),
                                               sort_keys=True, indent=2) + "\n")
            added = self.git("add", "--", "Cargo.toml", CHANNEL_MANIFEST, kind="local", cwd=wt)
            if not added.ok:
                raise Refused(f"버전·채널 manifest를 준비하지 못했다 — {added.tail(2)}")
            stamp = f"@{plan['created_at_ms'] // 1000} +0000"
            c = self.git("commit", "-qm", f"chore(release): {plan['tag']}", kind="local", cwd=wt,
                         extra={"GIT_AUTHOR_DATE": stamp, "GIT_COMMITTER_DATE": stamp})
            if not c.ok:
                raise Refused(f"버전 커밋 실패 — {c.tail(3)}")
        bump = self.git("rev-parse", "HEAD", cwd=wt).out.strip()
        if self.bump_parent_ok(wt, plan) != bump:
            raise Refused(f"격리 워크트리가 이 계획의 버전 커밋이 아니다({bump[:8] or '?'})")
        manifest_path = wt / CHANNEL_MANIFEST
        try:
            manifest = json.loads(manifest_path.read_text()) if manifest_path.exists() else None
        except (OSError, ValueError) as error:
            raise Refused("버전 커밋의 release channel manifest를 읽지 못했다") from error
        channel, platforms = validate_channel_manifest(manifest, plan["tag"], plan["commit"])
        if channel != plan["channel"] or platforms != plan["platforms"]:
            raise Refused("버전 커밋의 채널·플랫폼이 계획과 다르다")
        return bump

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
        local = mac_local(plan)
        if local:
            return self.build_signed(plan, local)
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
        if self.bake(wt, self.env(), "build").skipped:
            return {"would": "격리 워크트리에서 scripts/build-app.sh", "dry": True}
        binary = wt / "dist/kasaterm.app/Contents/MacOS/kasaterm"
        if not binary.exists():
            raise Refused("굽기는 끝났는데 번들이 없다")
        return {"built": str(wt / "dist/kasaterm.app"), "sha256": sha256_file(binary),
                "identity": identity_of(self.runner, wt / "dist/kasaterm.app", self.tools)}

    def build_signed(self, plan, local):
        """공증에 낼 판 — 버전 커밋으로, Developer ID·hardened runtime·보안 타임스탬프로 굽고 dmg 까지 서명한다.

        ready 판은 계획 커밋(버전을 안 올린 판)을 평소 서명으로 구운 것이라 쓰지 않는다. 열쇠고리는 풀지 않는다 —
        잠겨 있으면 build-app.sh 의 서명이 실패하고, 그 까닭을 그대로 보인다.
        """
        wt = self.worktree(plan)
        if self.dry:
            return {"would": [f"격리 워크트리에 버전 커밋({plan['tag']}) → KASATERM_SIGN_HARDENED=1 KASATERM_SIGN_ID={local['identity']['name']}"
                              f" KASATERM_SIGN_KEYCHAIN={local['keychain']} bash scripts/build-app.sh",
                              f"dmg → {self.workdir / 'out' / asset_names(plan['tag'])['macos']} (공유 dist 안 씀) · dmg 서명",
                              "번들 안 Mach-O 전부 Developer ID·팀·hardened runtime·타임스탬프 확인"], "dry": True}
        bump = self.ensure_bump(wt, plan)
        env = {**self.env(), "KASATERM_SIGN_ID": local["identity"]["sha1"], "KASATERM_SIGN_KEYCHAIN": local["keychain"],
               "KASATERM_SIGN_HARDENED": "1"}
        helper = self.unlock_helper(local)
        if helper:
            env["KASATERM_SIGN_UNLOCK"] = helper
        else:
            self.probe_key(local)
        self.bake(wt, env, "build-signed", f" (열쇠고리가 잠겼으면 사람이 {local.get('unlock_helper') or '열쇠고리 풀기'} 를 먼저 — 이 도구는 풀지 않는다)")
        app = wt / "dist/kasaterm.app"
        facts = self.bundle_facts(wt, app, plan, bump)
        dmg = self.make_dmg(app, plan, local)
        rows, debuggable = macsign.scan(self.runner, self.tool("codesign"), app)
        problems = macsign.readiness_problems(rows, local["identity"]["team"], debuggable)
        if problems:
            raise Refused("공증에 낼 수 없는 번들 — " + "; ".join(problems[:6]) + (f" 외 {len(problems) - 6}건" if len(problems) > 6 else ""))
        return {"commit": bump, "built": str(app), "dmg": str(dmg), "dmg_sha256": sha256_file(dmg), **facts,
                "machos": len(rows), "identity": identity_of(self.runner, app, self.tools), "signed_with": local["identity"]["name"]}

    def bake(self, wt, env, label, hint=""):
        """build-app.sh — 실패하면 전체 출력을 작업 폴더에 남긴다. 끝 몇 줄만으로는 npm 출력이 컴파일 오류를 가린다."""
        # verify가 만든 무시 파일 Cargo.lock은 버전 커밋 뒤에도 예전 버전이다.
        # 존재해도 먼저 동기화해야 굽기 도중 입력이 바뀌어 증명서가 불확실해지지 않는다.
        if not self.dry:
            m = self.runner.run([self.tool("cargo"), "metadata", "--format-version", "1"], cwd=wt, timeout=900,
                                env=env, kind="local")
            if not m.ok:
                raise Refused(f"Cargo.lock 을 동기화하지 못했다 — {'시간 초과' if m.timed_out else m.tail(2)}")
        r = self.runner.run(["bash", "scripts/build-app.sh"], cwd=wt, timeout=3600, env=env, kind="local")
        if r.ok or r.skipped:
            return r
        log = self.workdir / f"{label}.log"
        log.parent.mkdir(parents=True, exist_ok=True)
        log.write_text(f"--- stderr ---\n{r.err or ''}\n--- stdout ---\n{r.out or ''}")
        why = "시간 초과" if r.timed_out else "\n".join((r.err or r.out or "").strip().splitlines()[-4:])
        raise Refused(f"굽기 실패 — {why} · 전체 기록 {log}{hint}")

    def unlock_helper(self, local):
        """맡겼을 때만 쓰는 풀기 도우미 — 계획이 본 자리 그대로, 지금도 실행 파일일 때."""
        helper = local.get("unlock_helper")
        if not self.unlock:
            return None
        if not helper or not os.access(helper, os.X_OK):
            raise Refused("KASATERM_RELEASE_UNLOCK=1 인데 계획이 본 풀기 도우미가 없다")
        return helper

    def probe_key(self, local):
        target = self.workdir / "probe" / "true"
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile("/usr/bin/true", target)
        r = self.runner.run(macsign.probe_command(self.tool("codesign"), target, local), timeout=30, kind="local")
        if not r.ok:
            raise Refused(f"서명 열쇠를 지금 못 쓴다({'시간 초과 — 화면 암호창' if r.timed_out else r.tail(1)}) — 열쇠고리가 잠겼다면 사람이"
                          f" {local.get('unlock_helper') or '열쇠고리 풀기'} 를 먼저 하거나, 나쵸 도구가 KASATERM_RELEASE_UNLOCK=1 로 맡긴다")

    def bundle_facts(self, wt, app, plan, bump):
        """굽힌 번들이 이 계획의 판인가 — 판 번호와, 굽기 증명서의 원본 커밋(깨끗한 버전 커밋)."""
        binary = app / "Contents/MacOS/kasaterm"
        if not binary.exists():
            raise Refused("굽기는 끝났는데 번들이 없다")
        version = macsign.bundle_version(app)
        if version != plan["version"]:
            raise Refused(f"번들 판 번호({version or '없음'})가 계획({plan['version']})과 다르다")
        try:
            raw = (wt / "dist/kasaterm.build.json").read_text()
            # 워크트리는 끝나면 치운다 — 판정 근거인 증명서는 작업 폴더에 남긴다.
            (self.workdir / "last-build.json").write_text(raw)
            source = json.loads(raw).get("source") or {}
        except (OSError, ValueError):
            raise Refused("굽기 증명서(dist/kasaterm.build.json)가 없다 — 어느 커밋을 구웠는지 모르는 판은 내지 않는다")
        if source.get("source_commit") != bump or source.get("dirty") is not False:
            raise Refused(f"굽기 증명서의 커밋({(source.get('source_commit') or '불확실')[:8]})이 버전 커밋({bump[:8]})의 깨끗한 판이 아니다")
        return {"version": version, "sha256": sha256_file(binary)}

    def make_dmg(self, app, plan, local=None, out=None):
        """release.yml(ci)과 같은 모양의 dmg — 앱과 /Applications 바로가기, UDZO. 계획 작업 폴더에만 만든다."""
        out = Path(out or self.workdir / "out")
        stage = out / "stage"
        shutil.rmtree(stage, ignore_errors=True)
        stage.mkdir(parents=True)
        c = self.runner.run([self.tool("ditto"), str(app), str(stage / "kasaterm.app")], timeout=600, kind="local")
        if not c.ok:
            raise Refused(f"dmg 스테이징 실패 — {c.tail(2)}")
        os.symlink("/Applications", stage / "Applications")
        dmg = out / asset_names(plan["tag"])["macos"]
        (out / "notary.json").unlink(missing_ok=True)
        for argv in macsign.dmg_commands(self.tools, stage, dmg, local):
            r = self.runner.run(argv, timeout=900, kind="local")
            if not r.ok:
                raise Refused(f"dmg 만들기 실패({os.path.basename(argv[0])}) — {r.tail(2)}")
        shutil.rmtree(stage, ignore_errors=True)
        return dmg

    def notarize(self, plan, local, built):
        """애플 공증 → staple → 공증 표·서명·판 번호 재확인. 이미 staple 한 같은 판이면 다시 내지 않는다(재개)."""
        dmg = Path(built.get("dmg") or "")
        if not built.get("dmg_sha256") or not dmg.is_file():
            raise Refused("구운 dmg 가 없다 — run 으로 다시 굽는다")
        record_path = dmg.parent / "notary.json"
        try:
            record = json.loads(record_path.read_text())
        except (OSError, ValueError):
            record = {}
        now = sha256_file(dmg)
        xcrun = self.tool("xcrun")
        if now == built["dmg_sha256"]:
            helper = self.unlock_helper(local)
            if helper:
                u = self.runner.run([helper], timeout=60, kind="publish")
                if not u.ok:
                    raise Refused(f"열쇠고리 풀기 실패 — {u.tail(2)}")
            r = self.runner.run(macsign.notarize_command(xcrun, dmg, local), timeout=3600, kind="publish")
            got = macsign.parse_notary(r.out)
            if not r.ok or got["status"] != "Accepted":
                log = f" · 기록: xcrun notarytool log {got['id']} --keychain-profile {local['notary_profile']}" if got["id"] else ""
                raise Refused(f"공증 실패({got['status'] or ('시간 초과' if r.timed_out else r.tail(2))}){log}"
                              f" — 열쇠고리가 잠겼으면 사람이 {local.get('unlock_helper') or '열쇠고리 풀기'} 를 먼저")
            s = self.runner.run([xcrun, "stapler", "staple", str(dmg)], timeout=300, kind="local")
            if not s.ok:
                raise Refused(f"staple 실패 — {s.tail(2)}")
            record = {"before": built["dmg_sha256"], "after": sha256_file(dmg), "id": got["id"]}
            record_path.write_text(json.dumps(record))
            now = record["after"]
        elif not (record.get("before") == built["dmg_sha256"] and record.get("after") == now):
            raise Refused("dmg 가 굽고 나서 바뀌었다(공증 기록과도 다르다) — run 으로 다시 굽는다")
        return {"dmg": str(dmg), "dmg_sha256": now, "id": record.get("id"), **self.verify_notarized(plan, local, dmg)}

    def verify_notarized(self, plan, local, dmg):
        """올리기 전 마지막 확인 — CI 가 할 검증을 여기서 먼저 한다. 하나라도 어긋나면 태그를 올리지 않는다."""
        seal = macsign.dmg_notarized(self.runner, self.tools, dmg)
        if not (seal["notarized"] and seal["stapled"]):
            raise Refused(f"dmg 공증 표가 확인되지 않는다(공증 {'됨' if seal['notarized'] else '안 됨'}, staple {'됨' if seal['stapled'] else '안 됨'})")

        def inspect(app):
            rows, debuggable = macsign.scan(self.runner, self.tool("codesign"), app)
            return identity_of(self.runner, app, self.tools), macsign.bundle_version(app), \
                macsign.readiness_problems(rows, local["identity"]["team"], debuggable)

        ident, version, problems = self.mounted(dmg, inspect)
        why = [w for w in (
            None if ident and ident.get("verified") else "앱 서명이 깨졌다",
            None if (ident or {}).get("team") == local["identity"]["team"] else f"앱 팀이 {(ident or {}).get('team') or '없음'}",
            None if (ident or {}).get("notarized") else "앱이 공증되지 않았다",
            None if version == plan["version"] else f"앱 판 번호가 {version or '없음'}",
        ) if w] + problems
        if why:
            raise Refused("공증한 dmg 확인 실패 — " + "; ".join(why[:6]))
        return {"notarized": True, "stapled": True, "team": ident["team"], "version": version}

    def mounted(self, dmg, fn):
        mnt = self.workdir / "mnt"
        shutil.rmtree(mnt, ignore_errors=True)
        mnt.mkdir(parents=True)
        hdiutil = self.tool("hdiutil")
        a = self.runner.run([hdiutil, "attach", "-readonly", "-nobrowse", "-mountpoint", str(mnt), str(dmg)],
                            timeout=180, kind="local")
        if not a.ok:
            raise Refused(f"dmg 를 열지 못했다 — {a.tail(2)}")
        try:
            return fn(mnt / "kasaterm.app")
        finally:
            self.runner.run([hdiutil, "detach", str(mnt)], timeout=120, kind="local")

    def upload_dmg(self, plan, notarized):
        """공증한 dmg 를 릴리스에 올린다 — 같은 해시가 이미 있으면 두고, 다른 것이 있으면 덮지 않고 멈춘다."""
        tag, name = plan["tag"], asset_names(plan["tag"])["macos"]
        dmg = Path(notarized.get("dmg") or "")
        if dmg.name != name or not dmg.is_file() or sha256_file(dmg) != notarized.get("dmg_sha256"):
            raise Refused("공증한 dmg 가 그 자리에 없거나 바뀌었다 — 올리지 않는다")
        last = None
        for _ in range(2):
            view = self.runner.run([self.gh, "release", "view", tag, "--repo", self.slug, "--json", "assets,isPrerelease,isDraft"], timeout=60)
            if view.ok:
                release = json.loads(view.out or "{}")
                if bool(release.get("isPrerelease")) != (plan["channel"] == "preview") or release.get("isDraft"):
                    raise Refused("기존 GitHub Release의 채널·공개 상태가 계획과 다르다 — 변경하지 않는다")
                have = {a["name"]: a for a in release.get("assets", [])}.get(name)
                if have:
                    if have.get("digest") == notarized["dmg_sha256"]:
                        return "already"
                    raise Refused(f"릴리스에 다른 {name} 이 이미 있다({(have.get('digest') or '해시 없음')[:19]}) — 덮지 않는다")
                up = self.runner.run([self.gh, "release", "upload", tag, str(dmg), "--repo", self.slug],
                                     timeout=1800, kind="publish")
                if not up.ok:
                    raise Refused(f"dmg 올리기 실패 — {'시간 초과' if up.timed_out else up.tail(2)}")
                return "uploaded"
            flags = ["--prerelease", "--latest=false"] if plan["channel"] == "preview" else []
            last = self.runner.run([self.gh, "release", "create", tag, str(dmg), "--repo", self.slug, "--verify-tag",
                                    "--title", f"kasaterm {tag}", "--notes", f"kasaterm {tag}", *flags], timeout=1800, kind="publish")
            if last.ok:
                return "created"
            # Windows job 이 그 사이 릴리스를 먼저 만들었을 수 있다 — 다시 읽고 올린다.
        raise Refused(f"릴리스를 만들지도 읽지도 못했다 — {last.tail(2) if last else '알 수 없음'}")

    def tag_commands(self, plan):
        return [["git", "worktree", "add", "--detach", str(self.workdir / "wt"), plan["commit"]],
                ["(Cargo.toml", "버전", "→", plan["version"], "채널", "→", plan["channel"], CHANNEL_MANIFEST + ")"],
                ["git", "commit", "-qm", f"chore(release): {plan['tag']}"],
                ["git", "push", "--atomic", self.remote, f"HEAD:refs/heads/{plan['branch']}", f"HEAD:refs/tags/{plan['tag']}"]]

    def preview(self, stage, plan):
        """live 가 아닐 때 게시 단계가 보이는 것 — 명령과 원격 사실. 로컬 저장소에도 흔적을 안 남긴다."""
        seen = self.observe(plan)
        local = mac_local(plan)
        dmg = self.workdir / "out" / asset_names(plan["tag"])["macos"]
        if stage == "tag":
            first = [" ".join(macsign.notarize_command("xcrun", dmg, local)) + " (애플 공증 — 굽은 dmg 그대로)",
                     f"xcrun stapler staple {dmg}",
                     "spctl·stapler validate·codesign 으로 공증·팀·hardened runtime·판 번호 재확인 — 어긋나면 push 안 함"] if local else []
            return {"would": first + [" ".join(c) for c in self.tag_commands(plan)], "remote": seen}
        if stage == "release":
            if local:
                return {"would": [f"gh release create {plan['tag']} {dmg} --verify-tag (있으면 upload, 다른 dmg 가 있으면 덮지 않고 멈춤)",
                                  f"release.yml 이 그 dmg 를 검증(팀 {local['identity']['team']}·공증·staple·판 번호)하고 그 해시에만 EdDSA",
                                  "완료 확인 → 산출물 받기 · 크기·해시 · dmg 가 공증한 그 파일인지"], "remote": seen}
            return {"would": f"release.yml(태그 push 로 돈다) 완료 확인 → {', '.join(asset_names(plan['tag']).values())} 받기 · 크기·해시 · dmg 서명 신원",
                    "remote": seen}
        return {"would": f"{plan['channel']} 피드가 목표 판·산출물 이름·크기를 가리키는지, EdDSA 서명이 산출물과 맞는지(저장소 공개키)",
                "remote": seen}

    def tag(self, plan, state):
        """버전 커밋 + 태그를 격리 워크트리에서 만들고 한 번에 올린다. 로컬 태그는 만들지 않는다(공유 저장소 refs 불변).
        로컬 mac 판이면 push 전에 공증·staple·재검증을 끝낸다 — 공증이 안 되면 태그도 안 선다."""
        tag, main = plan["tag"], "refs/heads/" + plan["branch"]
        local = mac_local(plan)
        built = (state["stages"].get("build", {}).get("detail") or {}) if local else {}
        tagged = self.remote_ref("refs/tags/" + tag)
        if tagged:
            done = self.reconcile_tag(plan, tagged)
            if local:
                if tagged != built.get("commit"):
                    raise Refused(f"원격 태그({tagged[:8]})가 이 기기에서 구운 판의 커밋({(built.get('commit') or '?')[:8]})이 아니다 — 올릴 dmg 가 태그와 안 맞는다")
                done["notarized"] = self.notarize(plan, local, built)
            return done
        head = self.remote_ref(main)
        if head != plan["commit"]:
            raise Refused(f"원격 {plan['branch']} 가 계획 커밋이 아니다({(head or '?')[:8]}) — 계획에 없는 변경이 섞이므로 새 계획이 필요하다")
        wt = self.worktree(plan)
        bump = self.ensure_bump(wt, plan)
        extra = {}
        if local:
            if bump != built.get("commit"):
                raise Refused(f"구운 판의 커밋({(built.get('commit') or '?')[:8]})과 지금 버전 커밋({bump[:8]})이 다르다 — run 으로 다시 굽는다")
            extra["notarized"] = self.notarize(plan, local, built)
        push = self.git("push", "--atomic", self.remote, f"HEAD:{main}", f"HEAD:refs/tags/{tag}",
                        kind="publish", timeout=180, cwd=wt)
        if push.ok:
            return {"tag": tag, "commit": bump, "pushed": True, **extra}
        # 응답이 실패·시간 초과여도 원격에 반영됐을 수 있다 — 원격을 다시 읽어 맞춘다.
        now_tag, now_main = self.remote_ref("refs/tags/" + tag), self.remote_ref(main)
        if now_tag == bump and now_main == bump:
            return {"tag": tag, "commit": bump, "pushed": True, "note": "push 응답은 실패였지만 원격에 반영돼 있었다", **extra}
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
        raw = self.git("show", f"{tagged}:{CHANNEL_MANIFEST}")
        try:
            manifest = json.loads(raw.out) if raw.ok else None
        except ValueError as error:
            raise Refused("원격 태그의 release channel manifest를 읽지 못했다") from error
        channel, platforms = validate_channel_manifest(manifest, plan["tag"], plan["commit"])
        if channel != plan["channel"] or platforms != plan["platforms"]:
            raise Refused("원격 태그의 채널·플랫폼이 계획과 다르다")
        return {"tag": plan["tag"], "commit": tagged, "result": "이미 선 태그 — 다시 세우지 않음"}

    def release(self, plan, state):
        tag = plan["tag"]
        expected_commit = (state["stages"].get("tag", {}).get("detail") or {}).get("commit")
        if expected_commit and self.remote_ref("refs/tags/" + tag) != expected_commit:
            raise Refused("게시할 태그 커밋이 검증한 버전 커밋에서 바뀌었다")
        local = mac_local(plan)
        notarized = (state["stages"].get("tag", {}).get("detail") or {}).get("notarized") if local else None
        uploaded = None
        if local:
            if not notarized:
                raise Refused("이 계획의 공증 기록이 없다 — 로컬 mac 판은 태그 단계가 공증한 dmg 만 올린다")
            uploaded = self.upload_dmg(plan, notarized)
        run = self.ci_run(tag)
        if not run:
            raise Pending("CI 가 아직 안 떴다")
        if run.get("status") != "completed":
            raise Pending(f"CI 진행 중({run.get('status')}) · run {run.get('databaseId')}")
        if run.get("event") == "push" and expected_commit and run.get("headSha") != expected_commit:
            raise Refused("CI가 검증한 커밋이 이 계획의 태그 커밋과 다르다")
        if run.get("conclusion") != "success":
            raise Refused(f"CI 실패({run.get('conclusion')}) · run {run.get('databaseId')} — 산출물·appcast 를 확인하지 않는다"
                          + self.appcast_retry_hint(run, plan))
        view = self.runner.run([self.gh, "release", "view", tag, "--repo", self.slug, "--json", "assets,isDraft,isPrerelease"], timeout=60)
        if not view.ok:
            raise Refused(f"릴리스를 읽지 못했다 — {view.tail(2)}")
        release = json.loads(view.out)
        if release.get("isDraft") or bool(release.get("isPrerelease")) != (plan["channel"] == "preview"):
            raise Refused("릴리스의 공개 상태·채널이 계획과 다르다")
        assets = {a["name"]: a for a in release.get("assets", [])}
        want = {platform: name for platform, name in asset_names(tag).items() if platform in plan["platforms"]}
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
        if local:
            detail["dmg_upload"] = uploaded
            if (got.get("macos") or {}).get("sha256") != notarized["dmg_sha256"]:
                raise Refused("릴리스의 dmg 가 이 기기가 공증한 그 파일이 아니다 — appcast 를 확인하지 않는다")
        if "macos" in got:
            detail["mac_identity"] = {**self.dmg_identity(Path(got["macos"]["path"])), **({"source": "local"} if local else {})}
            why = identity_block(detail["mac_identity"], plan["signing"].get("installed"))
            if why:
                raise Refused(f"mac 판 서명 확인 — {why}")
        return detail

    def dmg_identity(self, dmg):
        return self.mounted(dmg, lambda app: identity_of(self.runner, app, self.tools))

    def preflight(self, plan):
        """열쇠 없이 해 보는 로컬 판 굽기 — 같은 버전 커밋·같은 dmg 모양, 서명만 ad-hoc. 공유 dist·설치본·열쇠고리를 안 건드린다.
        공증 전에 서명이 바꿔야 할 조각과, hardened 서명 목록 밖의 Mach-O 를 미리 센다."""
        wt = self.worktree(plan, "wt-preflight")
        try:
            bump = self.ensure_bump(wt, plan)
            env = {**self.env(), "KASATERM_SIGN_KEYCHAIN": str(self.workdir / "no-keychain"),
                   "KASATERM_SIGN_ID": "kasaterm-preflight-unsigned"}
            self.bake(wt, env, "preflight")
            app = wt / "dist/kasaterm.app"
            facts = self.bundle_facts(wt, app, plan, bump)
            dmg = self.make_dmg(app, plan, None, out=self.workdir / "preflight")
            rows, debuggable = macsign.scan(self.runner, self.tool("codesign"), app)
            team = ((plan.get("mac_artifact") or {}).get("identity") or {}).get("team")
            return {"commit": bump, "dmg": str(dmg), "dmg_sha256": sha256_file(dmg), **facts,
                    "machos": [r["path"] for r in rows], "uncovered": [r["path"] for r in rows if not r["covered"]],
                    "signing_would_fix": macsign.readiness_problems(rows, team, debuggable)}
        finally:
            self.git("worktree", "remove", "--force", str(wt), kind="local")

    def feed(self, plan, state):
        assets = (state["stages"].get("release", {}).get("detail") or {}).get("assets") or {}
        key = plan["capabilities"]["macos"].get("ed_public_key")
        openssl = self.tool("openssl")
        ok, version, why = deps.probe_openssl(self.runner, openssl)
        if not ok:
            raise Refused(f"계획에 고정한 openssl({openssl}, {version or '판 모름'})이 지금은 {why} — 서명을 확인하지 않고 멈춘다")
        seen, signatures = {}, {}
        for platform, src in (("macos", plan["feed"]["source"]), ("windows", plan["feed"]["windows"])):
            if platform not in plan["platforms"]:
                continue
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
            expected_url = f"https://github.com/{self.slug}/releases/download/{plan['tag']}/{asset['name']}"
            if item["url"] != expected_url or item["length"] != asset["size"]:
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
