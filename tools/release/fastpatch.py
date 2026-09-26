"""다기기 빠른 패치 릴리스 — 범위를 계획 하나에 고정하고, 그 계획에 묶인 승인 한 번으로 단계를 끝까지 추적한다.

정본 흐름은 그대로다: `scripts/tag-release.sh`(버전 커밋·태그 push) → `.github/workflows/release.yml`
(msi·dmg 빌드, 릴리스 첨부, appcast 서명·커밋). 기기 쪽은 이미 있는 업데이터가 받는다 — macOS Sparkle,
Windows WinSparkle, iOS 는 TestFlight. 이 도구는 그 앞뒤를 묶는다:

- plan    정확한 커밋·다음 패치 버전·포함 변경·플랫폼/채널·기기 범위를 한 파일에 고정하고 계획 id 를 낸다.
- dry-run 단계별로 무엇을 할지(명령·확인)를 보여 주고 막힘을 다시 잰다. 아무것도 바꾸지 않는다.
- run     계획 id 에 묶인 승인 파일로 단계를 이어 간다. 끝난 단계는 건너뛰므로 다시 돌려도 태그가 두 번 서지 않는다.
- status  단계 상태와 기기마다 지금 판(버전+SHA)·목표·마지막 확인·오프라인을 보인다.

이 판에서 실제로 움직이는 백엔드는 mock(임시 저장소·로컬 피드·가짜 기기)뿐이다. 실제 태그 push·릴리스·
appcast 게시·설치·재시작은 막혀 있다 — docs/fast-patch-release.md.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import urllib.request

SCHEMA = "kasa-release-plan/1"
STAGES = ["verify", "build", "tag", "release", "feed", "devices"]
STATE_DIR = Path(os.environ.get("KASATERM_RELEASE_DIR", Path.home() / ".config/kasaterm/releases"))


class Refused(Exception):
    """단계를 움직이지 않은 까닭 — 사람에게 그대로 보인다."""


def now_ms():
    return int(time.time() * 1000)


def canonical(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode()


def git(repo, *args, check=True):
    out = subprocess.run(["git", "--no-pager", "-C", str(repo), *args], check=check,
                         stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60)
    return out.stdout.decode().strip()


def version_tuple(text):
    m = re.fullmatch(r"v?(\d+)\.(\d+)\.(\d+)", (text or "").strip())
    return tuple(int(x) for x in m.groups()) if m else None


def version_text(parts):
    return ".".join(str(x) for x in parts)


def cargo_version(repo):
    for line in (Path(repo) / "Cargo.toml").read_text().splitlines():
        m = re.match(r'^version = "([^"]+)"', line)
        if m:
            return m.group(1)
    return None


def remote_versions(repo, remote="origin"):
    """원격 태그의 버전들. 로컬 태그는 늦을 수 있다(이 기기엔 v0.2.0 이 없던 적이 있다)."""
    out = git(repo, "ls-remote", "--tags", remote, check=False)
    found = []
    for line in out.splitlines():
        ref = line.split("\t")[-1]
        if ref.endswith("^{}"):
            continue
        parts = version_tuple(ref.rsplit("/", 1)[-1])
        if parts:
            found.append(parts)
    return sorted(set(found))


def read_feed(feed):
    if not feed:
        return None
    if re.match(r"https?://", feed):
        with urllib.request.urlopen(feed, timeout=10) as r:
            return r.read().decode()
    return Path(feed).read_text()


def feed_version(xml):
    """appcast 의 최신 항목 버전. CI 는 최신 한 건만 싣는다(release.yml)."""
    m = re.search(r"<sparkle:version>([^<]+)</sparkle:version>", xml or "")
    return m.group(1).strip() if m else None


def classify(paths):
    """바뀐 파일 → 어느 배포가 필요한가. 네이티브는 앱 업데이트, 폰은 TestFlight 판, 문서·인프라는 기기에 안 간다."""
    kinds = {"native": [], "mobile": [], "feed": [], "docs": [], "infra": []}
    for p in paths:
        if re.match(r"docs/appcast[^/]*\.xml$", p):
            kinds["feed"].append(p)
        elif p.startswith("mobile/"):
            kinds["mobile"].append(p)
        elif p.startswith("docs/") or (p.endswith(".md") and "/" not in p):
            kinds["docs"].append(p)
        elif p.startswith((".github/", "scripts/", "tools/")):
            kinds["infra"].append(p)
        else:
            kinds["native"].append(p)
    return kinds


def capabilities(repo):
    """업데이터·채널·서명을 코드에서 읽는다. 채널은 앱이 Sparkle 에 넘길 때만 있다고 말한다."""
    repo = Path(repo)

    def text(rel):
        p = repo / rel
        return p.read_text(errors="ignore") if p.exists() else ""

    mac_src, bake, ci = text("app/kasaterm/src/macos_sparkle.rs"), text("scripts/build-app.sh"), text(".github/workflows/release.yml")
    feed = re.search(r"<key>SUFeedURL</key>\s*<string>([^<]+)</string>", bake)
    return {
        "macos": {
            "updater": "sparkle" if "Sparkle.framework" in bake else None,
            "feed": feed.group(1) if feed else None,
            "channels": ["stable", "preview"] if "allowedChannels" in mac_src else ["stable"],
            "ci_signing": "kasaterm-ci(자체 서명)" if "KASATERM_SIGN_ID: kasaterm-ci" in ci else "미확인",
            "notarized": "notarytool" in ci,
            "apply": "업데이터가 받고 사람이 「설치 후 재실행」 — 강제 종료 없음",
        },
        "windows": {
            "updater": "winsparkle" if text("app/kasaterm/src/win_sparkle.rs") else None,
            "feed": "https://2rami.github.io/kasaterm/appcast-win.xml",
            "channels": ["stable"],
            "apply": "MSI 설치본만 — 토스트 [설치] 를 눌렀을 때 MSI 실행",
        },
        "ios": {
            "path": "testflight" if text("mobile/tool/testflight.sh") else None,
            "channels": ["testflight-internal", "testflight-external(베타 심사)", "app-store(심사)"],
            "hotpatch": False,
            "apply": "TestFlight 앱에서 사람이 업데이트 — 바이너리 핫패치·심사 우회 없음",
        },
        "notes": [
            "로컬 굽기 판은 Developer ID, CI 릴리스는 kasaterm-ci 자체 서명이다 — 로컬 판에서 CI 판으로 Sparkle 이 받아 주는지는 실기 1회 확인 전까지 모른다",
        ] if "kasaterm-ci" in ci else [],
    }


def fetch_version(base, timeout=3.0):
    try:
        with urllib.request.urlopen(base.rstrip("/") + "/version", timeout=timeout) as r:
            v = json.loads(r.read().decode())
        return {"reachable": True, "version": v.get("version"), "build": v.get("build"), "machine_id": v.get("machine_id")}
    except Exception as e:  # noqa: BLE001 — 꺼진 기기는 여러 모양으로 실패한다
        return {"reachable": False, "error": type(e).__name__}


def compare(repo, device, target_version, targets):
    """기기 판과 목표. 버전이 같아도 SHA 가 다르면 다른 판이다 — 「0.2.0」만으로는 못 가른다.

    [targets] 는 목표로 인정하는 커밋들 — 계획 커밋과, 태그가 선 뒤엔 그 버전 커밋(CI 가 굽는 자리).
    """
    if not device.get("reachable"):
        return "offline", "오프라인 — 마지막으로 본 판 기준"
    have = version_tuple(device.get("version"))
    want = version_tuple(target_version)
    build = (device.get("build") or "").strip()
    sha = build.rstrip("+")
    if have and want and have > want:
        return "newer", "기기가 더 새 판 — 적용 안 함(다운그레이드 방지)"
    if have == want and sha and not build.endswith("+") and any(t.startswith(sha) for t in targets):
        return "current", "목표 판"
    if sha and re.fullmatch(r"[0-9a-f]{7,40}", sha):
        known = git(repo, "cat-file", "-t", sha, check=False) == "commit"
        inside = known and subprocess.run(
            ["git", "-C", str(repo), "merge-base", "--is-ancestor", sha, targets[0]]).returncode == 0
        if not inside:
            return "hold", "기기 판에 권위 main 에 없는 커밋이 있다 — 올리면 그 커밋이 빠진다(보류)"
    elif not sha or sha == "unknown":
        return "update", "업데이트 대상 · 기기가 SHA 를 안 알려 버전으로만 견줌"
    dirty = " · 미커밋 포함 판" if build.endswith("+") else ""
    return "update", f"업데이트 대상{dirty}"


def targets_of(plan, state):
    tagged = ((state or {}).get("stages", {}).get("tag", {}).get("detail") or {})
    return [plan["commit"]] + ([tagged["commit"]] if isinstance(tagged, dict) and tagged.get("commit") else [])


def ready_builds(repo, commit):
    """자동설치 dist 를 안 덮고 구워 둔 ready 판들 — 같은 커밋이면 빌드 단계가 새로 굽지 않아도 된다."""
    out = []
    for m in sorted((Path(repo) / "dist").glob("kasaterm.build.ready-*.json")):
        try:
            src = json.loads(m.read_text()).get("source") or {}
        except (OSError, ValueError):
            continue
        out.append({"manifest": m.name, "commit": src.get("source_commit"), "dirty": src.get("dirty"),
                    "matches": src.get("source_commit") == commit and src.get("dirty") is False})
    return out


def tunnel_port(label):
    """machines.rs `tunnel_port` 와 같은 식 — 앱이 ssh 기계마다 드는 8765 터널의 로컬 포트."""
    h = 0x811C9DC5
    for b in label.encode():
        h = ((h ^ b) * 0x01000193) & 0xFFFFFFFF
    return 18900 + h % 90


def roster_devices():
    """이 기기와 명부 기기. 명부의 ssh 항목은 앱이 든 터널 포트로 닿는다."""
    out = [{"label": "이 기기", "base": "http://127.0.0.1:8765"}]
    try:
        for m in json.loads((Path.home() / ".config/kasaterm/machines.json").read_text()):
            base = m.get("base") or (f"http://127.0.0.1:{tunnel_port(m['label'])}" if m.get("ssh") else None)
            if m.get("label") and base:
                out.append({"label": m["label"], "base": base})
    except (OSError, ValueError, KeyError):
        pass
    return out


def load_devices(path):
    return json.loads(Path(path).read_text()) if path else roster_devices()


def make_plan(repo, remote="origin", branch="main", channel="stable", feed=None, devices=None,
              version=None, ios_build=None):
    repo = Path(repo)
    errors = []
    commit = git(repo, "rev-parse", "HEAD")
    if git(repo, "status", "--porcelain", "--untracked-files=no"):
        errors.append("워킹트리에 커밋 안 된 변경이 있다 — 계획은 커밋만 싣는다")
    git(repo, "fetch", "-q", remote, check=False)
    remote_head = git(repo, "rev-parse", f"{remote}/{branch}", check=False)
    if subprocess.run(["git", "-C", str(repo), "merge-base", "--is-ancestor", commit, f"{remote}/{branch}"]).returncode != 0:
        errors.append(f"커밋 {commit[:8]} 이 {remote}/{branch} 에 없다 — push 된 커밋만 릴리스한다")
    tags = remote_versions(repo, remote)
    fv = feed_version(read_feed(feed)) if feed else None
    floor = max([t for t in tags] + [p for p in (version_tuple(fv), version_tuple(cargo_version(repo))) if p] or [(0, 0, 0)])
    if version:
        want = version_tuple(version)
        if not want:
            errors.append(f"버전 모양이 아니다: {version}")
            want = floor
        elif want in tags:
            errors.append(f"태그 v{version_text(want)} 가 원격에 이미 있다")
        elif want <= floor:
            errors.append(f"v{version_text(want)} 는 이미 나간 판(v{version_text(floor)}) 보다 낮거나 같다 — 다운그레이드")
    else:
        want = (floor[0], floor[1], floor[2] + 1)
    base_tag = f"v{version_text(max(tags))}" if tags else None
    base_commit = git(repo, "rev-parse", f"{base_tag}^{{commit}}", check=False) if base_tag else ""
    if base_tag and not re.fullmatch(r"[0-9a-f]{40}", base_commit or ""):
        git(repo, "fetch", "-q", remote, f"refs/tags/{base_tag}:refs/tags/{base_tag}", check=False)
        base_commit = git(repo, "rev-parse", f"{base_tag}^{{commit}}", check=False)
    rng = f"{base_commit}..{commit}" if re.fullmatch(r"[0-9a-f]{40}", base_commit or "") else commit
    log = [line.split(" ", 1) for line in git(repo, "log", "--format=%H %s", rng).splitlines() if line]
    paths = [p for p in git(repo, "diff", "--name-only", f"{base_commit}", commit).splitlines()] if rng != commit else []
    kinds = classify(paths)
    if not log:
        errors.append("지난 릴리스 뒤로 포함할 커밋이 없다")
    caps = capabilities(repo)
    if channel not in caps["macos"]["channels"]:
        errors.append(f"채널 {channel} 은 이 앱의 업데이터에 없다 — 있는 것: {', '.join(caps['macos']['channels'])}")
    platforms = []
    if kinds["native"]:
        platforms += ["macos", "windows"]
    if kinds["mobile"]:
        platforms.append("ios")
    scope = []
    for d in devices or []:
        seen = fetch_version(d["base"])
        state, why = compare(repo, seen, version_text(want), [commit])
        scope.append({**d, **seen, "state": state, "why": why,
                      "checked_at_ms": now_ms() if seen["reachable"] else None})
    core = {
        "schema": SCHEMA, "commit": commit, "branch": branch, "remote": remote,
        "version": version_text(want), "tag": f"v{version_text(want)}", "channel": channel,
        "platforms": platforms,
        # TestFlight 는 같은 빌드 번호를 두 번 받지 않는다 — testflight.sh 와 같은 yymmddHHMM 이라 단조롭다.
        "ios_build": (ios_build or time.strftime("%y%m%d%H%M")) if "ios" in platforms else None,
        "devices": sorted(d["label"] for d in scope if d["state"] in ("update", "current", "offline")),
        "stages": STAGES,
    }
    plan_id = hashlib.sha256(canonical(core)).hexdigest()[:16]
    return {
        **core, "plan_id": plan_id, "created_at_ms": now_ms(), "errors": errors,
        "base": {"tag": base_tag, "commit": base_commit or None},
        "remote_head": remote_head, "feed": {"source": feed, "version": fv},
        "changes": {"commits": [{"sha": s, "subject": t} for s, t in log], "files": kinds,
                    "needs": {"desktop_update": bool(kinds["native"]), "ios_build": bool(kinds["mobile"])}},
        "capabilities": caps, "baseline": scope, "ready_builds": ready_builds(repo, commit),
        "tests": ["cargo test -p kasaterm", "cargo test -p kasa-mcp", "cargo test -p kasa-socket"]
        + (["flutter test --no-pub (mobile)"] if kinds["mobile"] else []),
    }


def plan_dir(plan_id, state_dir=None):
    return Path(state_dir or STATE_DIR) / plan_id


def save_plan(plan, state_dir=None):
    d = plan_dir(plan["plan_id"], state_dir)
    d.mkdir(parents=True, exist_ok=True)
    (d / "plan.json").write_text(json.dumps(plan, ensure_ascii=False, indent=2))
    return d


def load(plan_id, state_dir=None):
    d = plan_dir(plan_id, state_dir)
    plan = json.loads((d / "plan.json").read_text())
    state_path = d / "state.json"
    state = json.loads(state_path.read_text()) if state_path.exists() else {"stages": {}, "devices": {}}
    return plan, state


def save_state(plan_id, state, state_dir=None):
    (plan_dir(plan_id, state_dir) / "state.json").write_text(json.dumps(state, ensure_ascii=False, indent=2))


def approval_template(plan):
    """나쵸가 주인 확인을 받은 뒤 채워 `approval.json` 으로 둔다. 도구는 스스로 승인하지 않는다."""
    return {"plan_id": plan["plan_id"], "tag": plan["tag"], "commit": plan["commit"],
            "stages": plan["stages"], "devices": plan["devices"],
            "approved_by": "", "approved_at_ms": 0, "expires_at_ms": 0, "source": "nacho"}


def check_approval(plan, approval, at_ms=None):
    """이 계획 그대로에만, 만료 전에만. 범위를 넓힌 승인·다른 계획의 승인은 받지 않는다."""
    if not approval:
        return "승인 파일이 없다 — 나쵸가 주인 확인을 받아 approval.json 을 둔 뒤에만 움직인다"
    at_ms = at_ms or now_ms()
    for key in ("plan_id", "tag", "commit"):
        if approval.get(key) != plan[key]:
            return f"승인의 {key} 가 계획과 다르다 — 이 계획의 승인이 아니다"
    if approval.get("stages") != plan["stages"] or sorted(approval.get("devices") or []) != plan["devices"]:
        return "승인 범위(단계·기기)가 계획과 다르다"
    if not approval.get("approved_by") or approval.get("source") != "nacho":
        return "승인한 사람·출처가 비었다"
    if not approval.get("expires_at_ms") or approval["expires_at_ms"] <= at_ms:
        return "승인이 만료됐다 — 새로 받아야 한다"
    return None


class RealBackend:
    """실제 게시 단계는 이 판에서 막아 둔다. 승인 연결·mock 검사까지만 — 게시는 다음 판에서 따로 연결한다."""

    def __getattr__(self, stage):
        def refuse(*_a, **_k):
            raise Refused(f"{stage}: 실제 백엔드는 이 판에서 막혀 있다(게시·설치·재시작 안 함) — --mock 으로 검사만")
        return refuse


def run(plan_id, backend, state_dir=None, at_ms=None):
    plan, state = load(plan_id, state_dir)
    if plan["errors"]:
        raise Refused("계획에 막힘이 있다 — " + "; ".join(plan["errors"]))
    approval_path = plan_dir(plan_id, state_dir) / "approval.json"
    approval = json.loads(approval_path.read_text()) if approval_path.exists() else None
    why = check_approval(plan, approval, at_ms)
    if why:
        raise Refused(why)
    for stage in plan["stages"]:
        if state["stages"].get(stage, {}).get("status") == "done" and stage != "devices":
            continue
        state["stages"][stage] = {"status": "running", "at_ms": now_ms()}
        save_state(plan_id, state, state_dir)
        try:
            detail = getattr(backend, stage)(plan, state)
        except Refused as e:
            state["stages"][stage] = {"status": "failed", "at_ms": now_ms(), "detail": str(e)}
            save_state(plan_id, state, state_dir)
            raise
        state["stages"][stage] = {"status": "done", "at_ms": now_ms(), "detail": detail}
        save_state(plan_id, state, state_dir)
    return state


def track_devices(repo, plan, state, devices):
    """기기마다 지금 판·목표·마지막 확인. 적용은 각 기기 업데이터가 한다 — 여기서 재시작하지 않는다."""
    rows = {}
    targets = targets_of(plan, state)
    for d in devices:
        seen = fetch_version(d["base"])
        prev = (state.get("devices") or {}).get(d["label"], {})
        if not seen["reachable"] and prev:
            seen = {**{k: prev.get(k) for k in ("version", "build", "machine_id")}, "reachable": False,
                    "error": seen.get("error")}
        st, why = compare(repo, seen, plan["version"], targets)
        if st == "update" and (state.get("stages") or {}).get("feed", {}).get("status") == "done":
            why += " · 피드는 나갔다 — 기기 업데이터가 받고 사람이 껐다 켜면 적용"
        rows[d["label"]] = {**seen, "state": st, "why": why, "base": d["base"],
                            "checked_at_ms": now_ms() if seen["reachable"] else prev.get("checked_at_ms")}
    state["devices"] = rows
    return rows


class MockBackend:
    """임시 저장소(원격은 bare repo)·로컬 피드·가짜 기기로 단계 전체를 돈다. 실제 기기·원격은 안 건드린다."""

    def __init__(self, repo, feed_path, artifacts_dir, devices, drop=()):
        self.repo, self.feed_path, self.artifacts, self.fleet, self.drop = Path(repo), Path(feed_path), Path(artifacts_dir), devices, set(drop)

    def verify(self, plan, state):
        return {"tests": plan["tests"], "result": "mock: 검사 목록 기록"}

    def build(self, plan, state):
        return {"isolated": True, "commit": plan["commit"], "result": "mock: 격리 워크트리 서명 빌드 자리"}

    def tag(self, plan, state):
        git(self.repo, "fetch", "-q", plan["remote"])
        tag = plan["tag"]
        remote_tag = git(self.repo, "ls-remote", "--tags", plan["remote"], f"refs/tags/{tag}", check=False)
        if remote_tag:
            git(self.repo, "fetch", "-q", plan["remote"], f"refs/tags/{tag}:refs/tags/{tag}", check=False)
            tagged = git(self.repo, "rev-parse", f"{tag}^{{commit}}")
            if git(self.repo, "rev-parse", f"{tagged}^") != plan["commit"]:
                raise Refused(f"태그 {tag} 가 계획 커밋 위의 버전 커밋이 아니다 — 손대지 않는다")
            return {"tag": tag, "commit": tagged, "result": "이미 선 태그 — 다시 세우지 않음"}
        if git(self.repo, "rev-parse", f"{plan['remote']}/{plan['branch']}") != plan["commit"]:
            raise Refused("main 이 계획 뒤로 움직였다 — 계획에 없는 변경이 섞이므로 새 계획이 필요하다")
        git(self.repo, "checkout", "-q", plan["commit"])
        cargo = self.repo / "Cargo.toml"
        cargo.write_text(re.sub(r'^version = "[^"]*"', f'version = "{plan["version"]}"', cargo.read_text(), count=1, flags=re.M))
        git(self.repo, "commit", "-qam", f"chore(release): {tag}")
        bump = git(self.repo, "rev-parse", "HEAD")
        git(self.repo, "tag", tag)
        git(self.repo, "push", "-q", plan["remote"], f"HEAD:refs/heads/{plan['branch']}", tag)
        return {"tag": tag, "commit": bump}

    def release(self, plan, state):
        out = self.artifacts / plan["tag"]
        out.mkdir(parents=True, exist_ok=True)
        made = {}
        for name in (f"kasaterm-{plan['tag']}.dmg", f"kasaterm-{plan['version']}-windows-x86_64.msi"):
            if any(name.endswith(d) for d in self.drop):
                continue
            body = f"{plan['tag']} {name}".encode()
            (out / name).write_bytes(body)
            made[name] = hashlib.sha256(body).hexdigest()
        missing = [k for k in ("dmg", "msi") if not any(n.endswith("." + k) for n in made)]
        if missing:
            raise Refused(f"릴리스 산출물이 모자라다({', '.join(missing)}) — appcast 는 쓰지 않는다")
        return {"artifacts": made}

    def feed(self, plan, state):
        artifacts = state["stages"].get("release", {}).get("detail", {}).get("artifacts") or {}
        if len(artifacts) < 2:
            raise Refused("검증된 릴리스 산출물 없이 appcast 를 쓰지 않는다")
        current = feed_version(self.feed_path.read_text()) if self.feed_path.exists() else None
        if current == plan["version"]:
            return {"feed": current, "result": "이미 목표 판 — 다시 쓰지 않음"}
        if current and version_tuple(current) > version_tuple(plan["version"]):
            raise Refused(f"피드가 이미 더 새 판({current}) — 다운그레이드 안 함")
        dmg = next(n for n in artifacts if n.endswith(".dmg"))
        self.feed_path.write_text(
            f'<?xml version="1.0" standalone="yes"?>\n<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" version="2.0">'
            f'<channel><item><title>{plan["version"]}</title><sparkle:version>{plan["version"]}</sparkle:version>'
            f'<sparkle:shortVersionString>{plan["version"]}</sparkle:shortVersionString>'
            f'<enclosure url="file://{self.artifacts / plan["tag"] / dmg}" sparkle:edSignature="mock" length="1" type="application/octet-stream"/>'
            f'</item></channel></rss>\n')
        return {"feed": plan["version"]}

    def devices(self, plan, state):
        rows = track_devices(self.repo, plan, state, self.fleet)
        return {label: r["state"] for label, r in rows.items()}


def describe(plan, state=None):
    lines = [f"계획 {plan['plan_id']} · {plan['tag']} · 커밋 {plan['commit'][:8]} · 채널 {plan['channel']}",
             f"  기준 {plan['base']['tag'] or '없음'} · 피드 {plan['feed']['version'] or '미확인'} · 커밋 {len(plan['changes']['commits'])}개",
             "  배포: " + (", ".join(plan["platforms"]) or "기기에 가는 변경 없음")
             + (" · iOS 는 TestFlight 판이 따로 필요" if plan["changes"]["needs"]["ios_build"] else "")]
    for e in plan["errors"]:
        lines.append(f"  막힘: {e}")
    for n in plan["capabilities"]["notes"]:
        lines.append(f"  주의: {n}")
    for stage in plan["stages"]:
        st = ((state or {}).get("stages") or {}).get(stage, {})
        lines.append(f"  [{st.get('status', 'pending'):>7}] {stage}" + (f" — {st['detail']}" if isinstance(st.get("detail"), str) else ""))
    rows = (state or {}).get("devices") or {d["label"]: d for d in plan["baseline"]}
    for label, d in rows.items():
        have = f"{d.get('version') or '?'} · {d.get('build') or '?'}"
        seen = d.get("checked_at_ms")
        ago = f"{max(0, (now_ms() - seen) // 1000)}초 전" if seen else "닿은 기록 없음"
        lines.append(f"  기기 {label}: {have} → {plan['version']} · {d.get('why', d.get('state'))} · 마지막 확인 {ago}")
    return "\n".join(lines)


def main(argv=None):
    ap = argparse.ArgumentParser(prog="patch-release", description=__doc__.splitlines()[0])
    ap.add_argument("--state-dir", default=None)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("plan")
    p.add_argument("--repo", default=".")
    p.add_argument("--channel", default="stable")
    p.add_argument("--feed", default="https://2rami.github.io/kasaterm/appcast.xml")
    p.add_argument("--devices", default=None, help="[{label, base}] JSON. 없으면 이 기기와 명부")
    p.add_argument("--version", default=None)
    p.add_argument("--ios-build", default=None)
    for name in ("dry-run", "status", "approval-template"):
        s = sub.add_parser(name)
        s.add_argument("plan_id")
        s.add_argument("--devices", default=None)
        s.add_argument("--repo", default=".")
    r = sub.add_parser("run")
    r.add_argument("plan_id")
    r.add_argument("--mock", default=None, help="mock 백엔드 설정 JSON {repo, feed, artifacts, devices}")
    a = ap.parse_args(argv)

    if a.cmd == "plan":
        plan = make_plan(a.repo, channel=a.channel, feed=a.feed, devices=load_devices(a.devices),
                         version=a.version, ios_build=a.ios_build)
        d = save_plan(plan, a.state_dir)
        print(describe(plan))
        print(f"  계획 파일: {d / 'plan.json'}")
        return 1 if plan["errors"] else 0
    plan, state = load(a.plan_id, a.state_dir)
    if a.cmd == "approval-template":
        print(json.dumps(approval_template(plan), ensure_ascii=False, indent=2))
        return 0
    if a.cmd == "status":
        track_devices(a.repo, plan, state, load_devices(a.devices))
        save_state(a.plan_id, state, a.state_dir)
        print(describe(plan, state))
        return 0
    if a.cmd == "dry-run":
        print(describe(plan, state))
        steps = {
            "verify": " · ".join(plan["tests"]),
            "build": "태그 전 굽기 확인 — 격리 워크트리(git worktree --detach)에서 scripts/build-app.sh, 자동설치 dist 안 덮음"
                     + "".join(f" · {b['manifest']} 가 같은 커밋이라 갈음 가능" for b in plan.get("ready_builds", []) if b["matches"]),
            "tag": f"scripts/tag-release.sh {plan['tag']} (main 이 {plan['commit'][:8]} 그대로일 때만)",
            "release": ".github/workflows/release.yml 완료 대기 — dmg·msi 와 sha256 확인",
            "feed": f"appcast 가 {plan['version']} 을 가리키는지 확인(산출물 확인 전엔 안 봄)",
            "devices": "기기별 /version 으로 지금 판·목표 추적 — 적용은 각 업데이터, 재시작 안 함",
        }
        for stage in plan["stages"]:
            print(f"  would {stage}: {steps[stage]}")
        why = check_approval(plan, None)
        print(f"  승인: {why}")
        return 1 if plan["errors"] else 0
    backend = RealBackend()
    if a.mock:
        cfg = json.loads(Path(a.mock).read_text())
        backend = MockBackend(cfg["repo"], cfg["feed"], cfg["artifacts"], cfg["devices"], cfg.get("drop", ()))
    try:
        state = run(a.plan_id, backend, a.state_dir)
    except Refused as e:
        print(f"멈춤: {e}", file=sys.stderr)
        return 2
    print(describe(plan, state))
    return 0


if __name__ == "__main__":
    sys.exit(main())
