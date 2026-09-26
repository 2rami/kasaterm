"""다기기 빠른 패치 릴리스 — 범위를 계획 하나에 고정하고, 나쵸 승인 한 번으로 단계를 끝까지 추적한다.

정본 흐름은 그대로다: 버전 커밋·태그 push → `.github/workflows/release.yml`(msi·dmg 빌드, 릴리스 첨부, appcast
서명·커밋). 기기 쪽은 이미 있는 업데이터가 받는다 — macOS Sparkle, Windows WinSparkle, iOS 는 TestFlight.

- plan    커밋·다음 패치 버전·포함 변경·플랫폼/채널·기기 범위·기준 피드 해시·서명 관문을 한 파일에 고정한다.
          `--json` 의 `approval_scope` 를 나쵸가 그대로 승인 요청으로 만든다.
- dry-run 모든 단계의 명령과 원격 사실(태그·CI·피드)을 읽기만으로 보인다.
- run     검사·굽기(격리 워크트리)를 실제로 하고, 게시 단계는 명령만 보인다.
          `--live --approval ap_…` 일 때만 나쵸에서 승인을 한 번 소비하고 게시한다. 끝난 단계는 건너뛴다.
- status  원격 사실과 기기마다 지금 판(버전+SHA)·목표·마지막 확인·오프라인.

docs/fast-patch-release.md.
"""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time

from tools.release import deps, devices, nacho
from tools.release.backend import PUBLISH_STAGES, TESTS, RealBackend, identity_block, identity_of
from tools.release.common import Pending, Refused, fetch_feed, feed_item, sha256_bytes, version_text, version_tuple
from tools.release.proc import Http, Runner

SCHEMA = "kasa-release-plan/2"
# 나쵸가 plan_id 를 다시 재는 키 — 계약이다(docs/fast-patch-release.md 「나쵸와의 계약」). 바꾸면 SCHEMA 를 올린다.
CORE_KEYS = ("schema", "commit", "branch", "remote", "version", "tag", "channel", "platforms", "ios_build", "devices",
             "device_ids", "controller", "feed_base", "stages")
# 창 안(학생·에이전트)에서 도는 표식. 나쵸 도구는 이것들을 걷은 env 로 부르고 KASATERM_RELEASE_INVOKER 를 단다.
PANE_MARKERS = ("KASATERM_PANE_ID", "CLAUDECODE", "CLAUDE_CODE_SESSION_ID", "CODEX_SANDBOX", "KASATERM_ORIGIN")
STAGES = ["verify", "build", "tag", "release", "feed", "devices"]
STATE_DIR = Path(os.environ.get("KASATERM_RELEASE_DIR", Path.home() / ".config/kasaterm/releases"))
MAC_FEED = "https://2rami.github.io/kasaterm/appcast.xml"
WIN_FEED = "https://2rami.github.io/kasaterm/appcast-win.xml"
RELEASES = "https://github.com/2rami/kasaterm/releases/latest"
INSTALLED_APP = Path.home() / "Applications/kasaterm.app"


def now_ms():
    return int(time.time() * 1000)


def canonical(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode()


def git(repo, *args, check=True, env=None):
    out = subprocess.run(["git", "--no-pager", "-C", str(repo), *args], check=check, env=env,
                         stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=60)
    return out.stdout.decode().strip()


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
    edkey = re.search(r"<key>SUPublicEDKey</key>\s*<string>([^<]+)</string>", bake)
    sign = re.search(r"KASATERM_SIGN_ID:\s*(.+)", ci)
    sign_id = sign.group(1).strip().strip("'\"") if sign else None
    team = re.search(r"\(([A-Z0-9]{10})\)\s*$", sign_id or "")
    return {
        "macos": {
            "updater": "sparkle" if "Sparkle.framework" in bake else None,
            "feed": feed.group(1) if feed else None,
            "ed_public_key": edkey.group(1).strip() if edkey else None,
            "channels": ["stable", "preview"] if "allowedChannels" in mac_src else ["stable"],
            # CI 가 쓸 서명 신원 — release.yml 에서 읽은 예상. 실제 신원은 release 단계가 dmg 를 열어 다시 잰다.
            "ci_identity": {"authority": sign_id, "notarized": "notarytool" in ci, "predicted": True,
                            "team": team.group(1) if team and (sign_id or "").startswith("Developer ID Application") else None},
            "apply": "업데이터가 받고 사람이 「설치 후 재실행」 — 강제 종료 없음",
        },
        "windows": {
            "updater": "winsparkle" if text("app/kasaterm/src/win_sparkle.rs") else None,
            "feed": WIN_FEED,
            "channels": ["stable"],
            "apply": "MSI 설치본만 — 토스트 [설치]·판 번호 줄을 눌렀을 때 MSI 실행",
        },
        "ios": {
            "path": "testflight" if text("mobile/tool/testflight.sh") else None,
            "channels": ["testflight-internal", "testflight-external(베타 심사)", "app-store(심사)"],
            "hotpatch": False,
            "apply": "TestFlight 앱에서 사람이 업데이트 — 바이너리 핫패치·심사 우회 없음. 이 도구의 단계 밖",
        },
    }


def fetch_version(http, base):
    status, raw = http.get(base.rstrip("/") + "/version", timeout=3)
    if status != 200:
        return {"reachable": False, "error": f"http {status}" if status else "unreachable"}
    try:
        v = json.loads(raw.decode())
    except ValueError:
        return {"reachable": False, "error": "bad json"}
    return {"reachable": True, "version": v.get("version"), "build": v.get("build"), "machine_id": v.get("machine_id"),
            "os": v.get("os")}


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


def how_to_apply(device):
    """기기에서 새 판이 들어가는 길. 원격 설치 창구는 아직 없다 — 사람이 그 기기의 업데이터로 받는다."""
    osname = device.get("os")
    if osname == "macos":
        return "Sparkle — 판 번호 줄·앱 메뉴 「업데이트 확인…」, 설치·재실행은 사람이"
    if osname == "windows":
        return "WinSparkle — 시작 토스트 [설치]·판 번호 줄(MSI 설치본만)"
    return f"원격 설치 창구 없음 — 그 기기에서 판 번호 줄을 누르거나 {RELEASES} 에서 받기"


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


def feed_base_of(http, mac, win):
    a, b = fetch_feed(http, mac), fetch_feed(http, win)
    if a is None or b is None:
        return None, None, None
    base = sha256_bytes(json.dumps({"macos": sha256_bytes(a), "windows": sha256_bytes(b)}, sort_keys=True).encode())
    return base, feed_item(a)["version"], feed_item(b)["version"]


def make_plan(repo, remote="origin", branch="main", channel="stable", feed=MAC_FEED, feed_win=WIN_FEED, devices=None,
              version=None, ios_build=None, http=None, runner=None, installed_app=INSTALLED_APP, controller=None,
              tools=None):
    repo = Path(repo)
    http, runner = http or Http(), runner or Runner("dry")
    errors, blocks = [], []
    tools = tools if tools is not None else deps.check(runner)
    blocks.extend(deps.blocks(tools))
    genv = deps.git_env(tools)
    if deps.uses_lfs(repo) and not (tools.get("git-lfs") or {}).get("path"):
        errors.append("저장소가 git LFS 를 쓰는데 git-lfs 가 없다 — 격리 워크트리·검사·굽기가 깨진다(설치는 사람이, 또는 KASATERM_GIT_LFS)")
    commit = git(repo, "rev-parse", "HEAD")
    status = subprocess.run(["git", "--no-pager", "-C", str(repo), "status", "--porcelain", "--untracked-files=no"],
                            env=genv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=120)
    if status.returncode != 0:
        errors.append(f"git status 가 실패했다 — {status.stderr.decode(errors='replace').strip()[:160]}")
    elif status.stdout.strip():
        errors.append("워킹트리에 커밋 안 된 변경이 있다 — 계획은 커밋만 싣는다")
    git(repo, "fetch", "-q", remote, check=False)
    remote_head = git(repo, "rev-parse", f"{remote}/{branch}", check=False)
    if subprocess.run(["git", "-C", str(repo), "merge-base", "--is-ancestor", commit, f"{remote}/{branch}"]).returncode != 0:
        errors.append(f"커밋 {commit[:8]} 이 {remote}/{branch} 에 없다 — push 된 커밋만 릴리스한다")
    tags = remote_versions(repo, remote)
    feed_base, fv, fv_win = feed_base_of(http, feed, feed_win)
    if not feed_base:
        blocks.append("피드를 읽지 못해 기준 해시를 못 쟀다")
    known = [t for t in tags] + [p for p in (version_tuple(fv), version_tuple(fv_win), version_tuple(cargo_version(repo))) if p]
    floor = max(known or [(0, 0, 0)])
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
        git(repo, "fetch", "-q", remote, f"refs/tags/{base_tag}", check=False)
        base_commit = git(repo, "rev-parse", "FETCH_HEAD^{commit}", check=False)
    rng = f"{base_commit}..{commit}" if re.fullmatch(r"[0-9a-f]{40}", base_commit or "") else commit
    log = [line.split(" ", 1) for line in git(repo, "log", "--format=%H %s", rng).splitlines() if line]
    paths = git(repo, "diff", "--name-only", base_commit, commit).splitlines() if rng != commit else []
    kinds = classify(paths)
    if not log:
        errors.append("지난 릴리스 뒤로 포함할 커밋이 없다")
    caps = capabilities(repo)
    if channel not in caps["macos"]["channels"]:
        errors.append(f"채널 {channel} 은 이 앱의 업데이터에 없다 — 있는 것: {', '.join(caps['macos']['channels'])}")
    platforms = (["macos", "windows"] if kinds["native"] else []) + (["ios"] if kinds["mobile"] else [])
    desktop = [p for p in platforms if p != "ios"]
    if not desktop:
        blocks.append("데스크톱에 게시할 변경이 없다 — iOS 는 TestFlight 경로로 따로 간다")

    installed = identity_of(runner, installed_app, tools) if Path(installed_app).exists() else None
    signing = {"release": caps["macos"]["ci_identity"], "installed": installed, "installed_app": str(installed_app),
               "remote": "원격 기기의 설치본 신원은 /version 이 알려 주지 않아 모른다"}
    if "macos" in desktop:
        why = identity_block(signing["release"], installed)
        if why:
            blocks.append(f"mac 서명: {why} — 태그를 올리면 CI 가 mac appcast 까지 게시하므로 태그부터 막는다")

    scope_rows = []
    for d in devices or []:
        seen = fetch_version(http, d["base"])
        state, why = compare(repo, seen, version_text(want), [commit])
        if state in ("update", "current") and not seen.get("machine_id"):
            state, why = "unscoped", "기기가 machine_id 를 안 알려 승인 범위에 못 넣는다"
        scope_rows.append({**d, **seen, "state": state, "why": why, "how": how_to_apply(seen),
                           "checked_at_ms": now_ms() if seen["reachable"] else None})
    in_scope = [d for d in scope_rows if d["state"] in ("update", "current")]
    controller = controller if controller is not None else nacho.local_machine_id()
    if not controller:
        blocks.append("이 기기의 machine_id 가 없다 — 승인 소비 기기를 댈 수 없다")
    core = {
        "schema": SCHEMA, "commit": commit, "branch": branch, "remote": remote,
        "version": version_text(want), "tag": f"v{version_text(want)}", "channel": channel,
        "platforms": desktop,
        # TestFlight 는 같은 빌드 번호를 두 번 받지 않는다 — testflight.sh 와 같은 yymmddHHMM 이라 단조롭다.
        "ios_build": (ios_build or time.strftime("%y%m%d%H%M")) if "ios" in platforms else None,
        "devices": sorted(d["label"] for d in in_scope),
        "device_ids": sorted(d["machine_id"] for d in in_scope),
        "controller": controller, "feed_base": feed_base, "stages": STAGES,
    }
    plan = {
        **core, "plan_id": sha256_bytes(canonical(core))[7:23], "created_at_ms": now_ms(), "errors": errors,
        "live_blocks": blocks,
        "base": {"tag": base_tag, "commit": base_commit or None},
        "remote_head": remote_head,
        "feed": {"source": feed, "windows": feed_win, "version": fv, "windows_version": fv_win},
        "changes": {"commits": [{"sha": s, "subject": t} for s, t in log], "files": kinds,
                    "needs": {"desktop_update": bool(kinds["native"]), "ios_build": bool(kinds["mobile"])}},
        "capabilities": caps, "signing": signing, "baseline": scope_rows, "ready_builds": ready_builds(repo, commit),
        "tools": tools,
        "tests": [" ".join(t) for t in TESTS],
    }
    if feed_base and controller and desktop:
        scope = nacho.release_scope(plan, controller, feed_base)
        problem = nacho.scope_problem(scope)
        if problem:
            blocks.append(f"승인 범위: {problem}")
        else:
            plan["approval_scope"] = scope
            plan["approval_scope_hash"] = nacho.scope_hash(scope)
    return plan


def core_of(plan):
    return {k: plan[k] for k in CORE_KEYS}


def plan_hash_ok(plan):
    """계획 파일이 손대지 않은 것인가 — 나쵸도 같은 식으로 다시 잰다."""
    return plan.get("schema") == SCHEMA and sha256_bytes(canonical(core_of(plan)))[7:23] == plan.get("plan_id")


def invoker_problem(env):
    """live 는 나쵸 도구만 — 학생·에이전트 창에서 임의로 치지 못하게 한다. 사고 방지 표식이고, 경계는 주인 승인이다."""
    inside = [k for k in PANE_MARKERS if env.get(k)]
    if inside:
        return f"창 안에서는 live 게시를 못 한다({', '.join(inside)}) — 주인 승인 뒤 나쵸 도구가 친다"
    if env.get("KASATERM_RELEASE_INVOKER") != "nacho-tool":
        return "live 게시는 나쵸 도구만 친다(KASATERM_RELEASE_INVOKER=nacho-tool 이 없다)"
    return None


def live_ready(plan, state):
    return (not plan["errors"] and not plan["live_blocks"] and plan_hash_ok(plan)
            and all((state.get("stages") or {}).get(s, {}).get("status") == "done" for s in ("verify", "build")))


def status_doc(plan, state):
    """나쵸가 읽는 상태 — 단추는 verify·build 가 끝나고 live_ready 일 때만 띄운다(승인 10분이 굽기에 안 먹히게)."""
    appr = state.get("approval")
    return {"plan_id": plan["plan_id"], "plan_hash_ok": plan_hash_ok(plan), "tag": plan["tag"], "commit": plan["commit"],
            "stages": {s: {"status": (state.get("stages") or {}).get(s, {}).get("status", "pending")} for s in plan["stages"]},
            "approval": {"id": appr["id"], "consumed_at_ms": appr["consumed_at_ms"]} if appr else None,
            "remote": state.get("remote"), "errors": plan["errors"], "live_blocks": plan["live_blocks"],
            "live_ready": live_ready(plan, state),
            "devices": {k: {x: v.get(x) for x in ("state", "version", "build", "checked_at_ms")}
                        for k, v in (state.get("devices") or {}).items()}}


def plan_dir(plan_id, state_dir=None):
    return Path(state_dir or STATE_DIR) / plan_id


def save_plan(plan, state_dir=None):
    d = plan_dir(plan["plan_id"], state_dir)
    d.mkdir(parents=True, exist_ok=True)
    (d / "plan.json").write_text(json.dumps(plan, ensure_ascii=False, indent=2))
    return d


def rollout_path(plan_id, rollout_id, state_dir=None, suffix=".json"):
    if not re.fullmatch(r"[0-9a-f]{16}", rollout_id or ""):
        raise Refused("rollout id 는 16자리 소문자 hex 다")
    return plan_dir(plan_id, state_dir) / "rollouts" / f"{rollout_id}{suffix}"


def save_rollout(plan_id, rollout, state_dir=None):
    path = rollout_path(plan_id, rollout["id"], state_dir)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(rollout, ensure_ascii=False, indent=2))
    return path


def device_apply(plan_id, plan, state, rollout_id, approval_id, live, state_dir=None, runner=None, env=None):
    """기기 업데이트 러너를 띄운다 — 나쵸 도구만(live 가드는 게시와 같다). 소비·차례·기다림·중단은 러너
    (`kasaterm-cli app-update run`, `kasa_socket::app_update::run`)가 하고, 여기는 계획·게시 상태·rollout 을 맞춰 본 뒤 넘긴다."""
    try:
        path = rollout_path(plan_id, rollout_id, state_dir)
    except Refused as e:
        print(f"멈춤: {e}", file=sys.stderr)
        return 2
    if not path.exists():
        print(f"멈춤: rollout {rollout_id} 가 없다 — device-plan 을 먼저", file=sys.stderr)
        return 2
    rollout = json.loads(path.read_text())
    stages = state.get("stages") or {}
    problems = [p for p in (
        None if rollout.get("release_plan") == plan["plan_id"] else "rollout 이 이 계획의 것이 아니다",
        None if plan_hash_ok(plan) else "계획 파일이 바뀌었다",
        None if all(stages.get(s, {}).get("status") == "done" for s in ("release", "feed")) else "릴리스·피드 단계가 끝나지 않았다",
        None if nacho.valid_approval_id(approval_id or "") else "--approval ap_… 가 없다",
    ) if p]
    if problems:
        print("멈춤: " + " · ".join(problems), file=sys.stderr)
        return 2
    cli = devices.cli_path()
    grant = rollout_path(plan_id, rollout_id, state_dir, ".grant.json")
    argv = [cli or "kasaterm-cli", "app-update", "run", "--approval", approval_id, "--rollout", str(path), "--record", str(grant)]
    if not live:
        print("미리보기(아무것도 안 보냄): " + " ".join(argv))
        print(f"  차례: {', '.join(j['machine_id'] for j in rollout['jobs'])} · 대상당 최대 {(30 * 60 + 210) // 60}분")
        return 0
    why = invoker_problem(os.environ if env is None else env)
    if why:
        print(f"멈춤: {why}", file=sys.stderr)
        return 2
    if not cli:
        print("멈춤: kasaterm-cli 를 찾지 못했다", file=sys.stderr)
        return 2
    runner = runner or Runner("live")
    r = runner.run(argv, timeout=len(rollout["jobs"]) * 2100 + 120, kind="publish")
    print(r.out, end="")
    if r.err:
        print(r.err, end="", file=sys.stderr)
    return 0 if r.ok else 1


def load(plan_id, state_dir=None):
    d = plan_dir(plan_id, state_dir)
    plan = json.loads((d / "plan.json").read_text())
    state_path = d / "state.json"
    state = json.loads(state_path.read_text()) if state_path.exists() else {"stages": {}, "devices": {}}
    return plan, state


def save_state(plan_id, state, state_dir=None):
    (plan_dir(plan_id, state_dir) / "state.json").write_text(json.dumps(state, ensure_ascii=False, indent=2))


def track_devices(repo, plan, state, devices, http):
    """기기마다 지금 판·목표·마지막 확인. 적용은 각 기기 업데이터가 한다 — 여기서 설치·재시작하지 않는다."""
    rows = {}
    targets = targets_of(plan, state)
    mac_block = next((b for b in plan.get("live_blocks", []) if b.startswith("mac 서명")), None)
    for d in devices:
        seen = fetch_version(http, d["base"])
        prev = (state.get("devices") or {}).get(d["label"], {})
        if not seen["reachable"] and prev:
            seen = {**{k: prev.get(k) for k in ("version", "build", "machine_id", "os")}, "reachable": False,
                    "error": seen.get("error")}
        st, why = compare(repo, seen, plan["version"], targets)
        if st == "update" and seen.get("os") == "macos" and mac_block:
            st, why = "blocked", mac_block
        elif st == "update" and (state.get("stages") or {}).get("feed", {}).get("status") == "done":
            why += " · 피드는 나갔다 — 기기 업데이터가 받고 사람이 껐다 켜면 적용"
        rows[d["label"]] = {**seen, "state": st, "why": why, "how": how_to_apply(seen), "base": d["base"],
                            "checked_at_ms": now_ms() if seen["reachable"] else prev.get("checked_at_ms")}
    state["devices"] = rows
    return rows


def backend_for(repo, plan, mode, state_dir=None, devices=None, http=None, runner=None):
    http = http or Http()
    runner = runner or Runner(mode)
    fleet = devices if devices is not None else [{"label": d["label"], "base": d["base"]} for d in plan["baseline"]]
    return RealBackend(repo, runner, http, plan_dir(plan["plan_id"], state_dir) / "work",
                       lambda p, s: track_devices(repo, p, s, fleet, http), plan["tools"])


def run(plan_id, backend, state_dir=None, approval_id=None, authority=None, at_ms=None):
    """검사·굽기는 실제로, 게시 단계는 live 일 때만. live 는 나쵸 승인을 게시 첫 단계 직전에 한 번 소비한다."""
    plan, state = load(plan_id, state_dir)
    if plan["errors"]:
        raise Refused("계획에 막힘이 있다 — " + "; ".join(plan["errors"]))
    mode = backend.runner.mode
    live = mode == "live"
    if live and plan["live_blocks"]:
        raise Refused("live 게시가 막혀 있다 — " + "; ".join(plan["live_blocks"]))
    if live and not plan_hash_ok(plan):
        raise Refused("계획 파일이 계획 id 와 맞지 않는다 — 손댄 계획으로는 게시하지 않는다")
    if live and not live_ready(plan, state):
        raise Refused(f"먼저 run {plan_id} 로 검사·굽기를 끝내라 — 승인 10분이 굽기 중에 지나지 않게, 단추는 그 뒤에 띄운다")
    persist = mode != "dry"

    def save():
        if persist:
            save_state(plan_id, state, state_dir)

    approved = False
    for stage in plan["stages"]:
        if state["stages"].get(stage, {}).get("status") == "done" and stage != "devices":
            continue
        if stage in PUBLISH_STAGES and not live:
            state["stages"][stage] = {"status": "dry", "at_ms": now_ms(), "detail": backend.preview(stage, plan)}
            continue
        if stage in PUBLISH_STAGES and not approved:
            state["approval"] = take_approval(plan, state, backend, authority, approval_id, at_ms or now_ms(), stage)
            approved = True
            save()
        state["stages"][stage] = {"status": "running", "at_ms": now_ms()}
        save()
        try:
            detail = getattr(backend, stage)(plan, state)
        except Pending as p:
            state["stages"][stage] = {"status": "waiting", "at_ms": now_ms(), "detail": str(p)}
            save()
            return state
        except Refused as e:
            state["stages"][stage] = {"status": "failed", "at_ms": now_ms(), "detail": str(e)}
            save()
            raise
        dry = isinstance(detail, dict) and detail.get("dry")
        state["stages"][stage] = {"status": "dry" if dry else "done", "at_ms": now_ms(), "detail": detail}
        save()
    return state


def take_approval(plan, state, backend, authority, approval_id, at_ms, stage):
    """처음이면 지금 피드로 범위를 다시 재서 소비하고, 재개면 작업 기록의 범위를 나쵸 기록과 대조한다."""
    if not authority or not approval_id:
        raise Refused("게시 단계는 나쵸 승인(--approval ap_…)이 있어야 한다 — 로컬 파일은 승인으로 치지 않는다")
    record = state.get("approval")
    if record:
        scope = record["scope"]
        if nacho.release_scope(plan, plan["controller"], scope["feed_base"]) != scope:
            raise Refused("작업 기록의 승인 범위가 계획과 다르다")
    else:
        now_base = backend.feed_base(plan)
        if now_base != plan["feed_base"]:
            raise Refused("계획 뒤에 피드가 바뀌었다(다른 게시가 있었다) — 새 계획이 필요하다")
        scope = nacho.release_scope(plan, plan["controller"], now_base)
    try:
        got = nacho.acquire(authority, approval_id, scope, plan["controller"], record, at_ms,
                            stage=stage, remote=backend.resume_facts(plan) if record else None)
    except nacho.Denied as e:
        raise Refused(f"승인: {e}")
    return {**got, "scope": scope}


def cleanup(backend, plan):
    wt = backend.workdir / "wt"
    if wt.exists() and backend.runner.mode != "dry":
        backend.git("worktree", "remove", "--force", str(wt), kind="local")


def describe(plan, state=None):
    lines = [f"계획 {plan['plan_id']} · {plan['tag']} · 커밋 {plan['commit'][:8]} · 채널 {plan['channel']}",
             f"  기준 {plan['base']['tag'] or '없음'} · 피드 mac {plan['feed']['version'] or '미확인'} / win {plan['feed']['windows_version'] or '미확인'}"
             f" · 기준 해시 {(plan['feed_base'] or '없음')[:19]} · 커밋 {len(plan['changes']['commits'])}개",
             "  게시: " + (", ".join(plan["platforms"]) or "데스크톱 변경 없음")
             + (f" · iOS 는 TestFlight 판({plan['ios_build']})이 따로 필요" if plan["ios_build"] else "")]
    for e in plan["errors"]:
        lines.append(f"  막힘: {e}")
    for b in plan["live_blocks"]:
        lines.append(f"  게시 막힘: {b}")
    rel, ins = plan["signing"]["release"], plan["signing"]["installed"]
    lines.append(f"  서명: CI {rel.get('authority') or '미확인'}(팀 {rel.get('team') or '없음'}, 공증 {'함' if rel.get('notarized') else '안 함'})"
                 f" · 설치본 {(ins or {}).get('authority') or '미확인'}(팀 {(ins or {}).get('team') or '없음'})")
    if plan.get("approval_scope"):
        lines.append(f"  승인 범위 해시 {plan['approval_scope_hash'][:19]} · 기기 {len(plan['device_ids'])}대 · 조종 기기 {plan['controller'][:12]}")
    appr = (state or {}).get("approval")
    if appr:
        lines.append(f"  승인 {appr['id']} 소비됨 · {appr['scope_hash'][:19]}")
    for stage in plan["stages"]:
        st = ((state or {}).get("stages") or {}).get(stage, {})
        detail = st.get("detail")
        text = detail if isinstance(detail, str) else ""
        if isinstance(detail, dict) and detail.get("would"):
            w = detail["would"]
            text = "; ".join(w) if isinstance(w, list) else w
        lines.append(f"  [{st.get('status', 'pending'):>7}] {stage}" + (f" — {text}" if text else ""))
    rows = (state or {}).get("devices") or {d["label"]: d for d in plan["baseline"]}
    for label, d in rows.items():
        have = f"{d.get('version') or '?'} · {d.get('build') or '?'}"
        seen = d.get("checked_at_ms")
        ago = f"{max(0, (now_ms() - seen) // 1000)}초 전" if seen else "닿은 기록 없음"
        lines.append(f"  기기 {label}: {have} → {plan['version']} · {d.get('why', d.get('state'))} · 마지막 확인 {ago}")
        if d.get("state") in ("update", "blocked", "offline", "unscoped"):
            lines.append(f"      받는 길: {d.get('how') or how_to_apply(d)}")
    return "\n".join(lines)


def main(argv=None):
    ap = argparse.ArgumentParser(prog="patch-release", description=__doc__.splitlines()[0])
    ap.add_argument("--state-dir", default=None)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("plan")
    p.add_argument("--repo", default=".")
    p.add_argument("--channel", default="stable")
    p.add_argument("--feed", default=MAC_FEED)
    p.add_argument("--feed-win", default=WIN_FEED)
    p.add_argument("--devices", default=None, help="[{label, base}] JSON. 없으면 이 기기와 명부")
    p.add_argument("--version", default=None)
    p.add_argument("--ios-build", default=None)
    p.add_argument("--json", action="store_true")
    for name in ("dry-run", "status", "run", "device-plan", "device-apply"):
        s = sub.add_parser(name)
        s.add_argument("plan_id")
        s.add_argument("--repo", default=".")
        if name in ("status", "device-plan"):
            s.add_argument("--json", action="store_true")
        if name in ("run", "device-apply"):
            s.add_argument("--live", action="store_true", help="게시 단계까지 — 나쵸 승인을 한 번 소비한다")
            s.add_argument("--approval", default=None)
        if name == "device-apply":
            s.add_argument("--rollout", required=True)
    a = ap.parse_args(argv)

    if a.cmd == "plan":
        plan = make_plan(a.repo, channel=a.channel, feed=a.feed, feed_win=a.feed_win, devices=load_devices(a.devices),
                         version=a.version, ios_build=a.ios_build)
        d = save_plan(plan, a.state_dir)
        if a.json:
            print(json.dumps(plan, ensure_ascii=False, indent=2))
        else:
            print(describe(plan))
            print(f"  계획 파일: {d / 'plan.json'}")
        return 1 if plan["errors"] else 0
    plan, state = load(a.plan_id, a.state_dir)
    if a.cmd == "device-plan":
        targets, problem = devices.gather_facts(Runner("dry"), plan["device_ids"])
        rows = devices.plan_devices(plan, state, targets, problem)
        now = int(time.time() * 1000)
        rollout, why_none = devices.update_rollout(plan, state, targets, rows, now)
        if rollout:
            save_rollout(a.plan_id, rollout, a.state_dir)
        if a.json:
            out = []
            for r in rows:
                job, why = devices.update_job(plan, state, targets.get(r["machine_id"]), now)
                out.append({**r, "job": job, "job_problem": why})
            print(json.dumps({"plan_id": plan["plan_id"], "dry_run": True, "devices": out, "rollout": rollout,
                              "rollout_problem": why_none}, ensure_ascii=False, indent=2))
            return 0
        print(f"기기 받기·설치 예약 계획(dry-run) · {plan['tag']} · 기기 {len(rows)}대" + (f" · {problem}" if problem else ""))
        for r in rows:
            print(f"  {r['label']} ({r['os'] or 'OS 모름'}) — {r['status']}")
            for why in r["reasons"]:
                print(f"      막힘 {why['code']}: {why['why']}")
            for i, step in enumerate(r["steps"], 1):
                print(f"      {i}. {step}")
            print(f"      지금 받는 길: {r['fallback']}")
        if rollout:
            names = ", ".join(j["machine_id"][:8] for j in rollout["jobs"])
            print(f"  rollout {rollout['id']} — 차례: {names} · 승인 범위 {rollout['approval_scope_hash'][:19]}")
        else:
            print(f"  rollout 없음 — {why_none}")
        return 0
    if a.cmd == "device-apply":
        return device_apply(a.plan_id, plan, state, a.rollout, a.approval, a.live, a.state_dir)
    if a.cmd == "status":
        backend = backend_for(a.repo, plan, "dry", a.state_dir)
        state["remote"] = backend.observe(plan)
        backend.tracker(plan, state)
        save_state(a.plan_id, state, a.state_dir)
        if a.json:
            print(json.dumps(status_doc(plan, state), ensure_ascii=False, indent=2))
            return 0
        print(describe(plan, state))
        r = state["remote"]
        print(f"  원격: 태그 {(r.get('tag') or '없음')[:8]} · main {(r.get('main') or '?')[:8]} · CI {r.get('ci')} · 피드 mac {r.get('feed_macos')} / win {r.get('feed_windows')}")
        return 0
    mode = "dry" if a.cmd == "dry-run" else ("live" if a.live else "local")
    backend = backend_for(a.repo, plan, mode, a.state_dir)
    authority = None
    if mode == "live":
        why = invoker_problem(os.environ)
        if why:
            print(f"멈춤: {why}", file=sys.stderr)
            return 2
        try:
            authority = nacho.NachoAuthority.from_env(backend.http)
        except nacho.Denied as e:
            print(f"멈춤: 승인 — {e}", file=sys.stderr)
            return 2
    try:
        state = run(a.plan_id, backend, a.state_dir, approval_id=getattr(a, "approval", None), authority=authority)
    except Refused as e:
        print(f"멈춤: {e}", file=sys.stderr)
        return 2
    finally:
        cleanup(backend, plan)
    print(describe(plan, state))
    if mode == "dry":
        for c in backend.runner.calls:
            if not c["ran"]:
                print(f"  would ({c['kind']}): {' '.join(c['argv'])}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
