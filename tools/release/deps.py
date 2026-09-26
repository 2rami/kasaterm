"""게시에 쓰는 도구를 PATH 에 맡기지 않는다 — 기능을 재서 고르고, 계획에 절대경로로 못 박는다.

macOS 기본 `/usr/bin/openssl` 은 LibreSSL 이라 Ed25519 원문 검증(`pkeyutl -rawin`)이 안 된다. 사람 셸은 homebrew 가 PATH
앞에 있어 멀쩡해 보이고, 나쵸가 부르는 셸은 `/usr/bin` 이 앞이라 **같은 명령이 다른 openssl 을 잡았다**(2026-09-27 나쵸
직접 검증: 기본 셸에서 검사 31건 중 27건 오류). 그래서 openssl 은 RFC 8032 시험 벡터를 실제로 검증해 보고(맞는 서명은
통과, 틀린 서명은 거부) 둘 다 맞는 것만 쓴다. 없으면 까닭을 말하고 멈춘다 — 설치는 하지 않는다.
"""

import base64
import os
from pathlib import Path
import shutil
import tempfile

OPENSSL_CANDIDATES = ("/opt/homebrew/opt/openssl@3/bin/openssl", "/usr/local/opt/openssl@3/bin/openssl",
                      "/opt/homebrew/bin/openssl", "/usr/local/bin/openssl")
GH_CANDIDATES = (str(Path.home() / ".local/bin/gh"), "/opt/homebrew/bin/gh", "/usr/local/bin/gh")
CARGO_CANDIDATES = (str(Path.home() / ".cargo/bin/cargo"), "/opt/homebrew/bin/cargo", "/usr/local/bin/cargo")
# git 은 LFS 필터를 `git-lfs` 이름으로 부른다 — 나쵸 기본 PATH 엔 없어서 격리 워크트리가 「git-lfs: command not found」로
# 깨졌다(2026-09-27 재현). 찾은 것의 폴더만 git 을 부를 때 PATH 앞에 붙인다.
GIT_LFS_CANDIDATES = (str(Path.home() / ".local/bin/git-lfs"), "/opt/homebrew/bin/git-lfs", "/usr/local/bin/git-lfs")
# 시스템 도구는 자리가 정해져 있다 — 이름으로 부르면 PATH 앞의 다른 것이 끼어들 수 있다.
SYSTEM = {"codesign": "/usr/bin/codesign", "spctl": "/usr/sbin/spctl", "hdiutil": "/usr/bin/hdiutil"}

# RFC 8032 7.1 TEST 2 — 한 바이트 메시지.
_PUB = bytes.fromhex("3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c")
_MSG = bytes.fromhex("72")
_SIG = bytes.fromhex("92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da"
                     "085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00")
_SPKI = bytes.fromhex("302a300506032b6570032100")


def probe_openssl(runner, path):
    """(쓸 수 있나, 판 문자열, 안 되는 까닭)."""
    ver = runner.run([path, "version"], timeout=15)
    version = (ver.out or ver.err or "").strip().splitlines()[0] if (ver.out or ver.err) else ""
    if not ver.ok:
        return False, version, "실행되지 않는다"
    with tempfile.TemporaryDirectory(prefix="ossl-probe-") as d:
        d = Path(d)
        (d / "pub.pem").write_text("-----BEGIN PUBLIC KEY-----\n" + base64.b64encode(_SPKI + _PUB).decode()
                                   + "\n-----END PUBLIC KEY-----\n")
        (d / "msg").write_bytes(_MSG)
        (d / "bad").write_bytes(b"\x73")
        (d / "sig").write_bytes(_SIG)
        base = [path, "pkeyutl", "-verify", "-pubin", "-inkey", str(d / "pub.pem"), "-rawin", "-sigfile", str(d / "sig")]
        good = runner.run(base + ["-in", str(d / "msg")], timeout=15)
        bad = runner.run(base + ["-in", str(d / "bad")], timeout=15)
    if not (good.ok and "Signature Verified Successfully" in good.out):
        return False, version, "Ed25519 원문 검증을 못 한다" + (" (LibreSSL)" if "LibreSSL" in version else "")
    if bad.ok:
        return False, version, "틀린 서명도 통과시킨다"
    return True, version, ""


def find_openssl(runner, env=None, which=shutil.which, candidates=OPENSSL_CANDIDATES):
    """`KASATERM_OPENSSL` 이 있으면 그것만 본다(사람이 고른 것을 몰래 바꾸지 않는다). 없으면 PATH 의 것, 알려진 openssl@3 순."""
    env = os.environ if env is None else env
    chosen = (env.get("KASATERM_OPENSSL") or "").strip()
    order = [chosen] if chosen else [which("openssl") or ""] + list(candidates)
    seen, rejected = set(), []
    for path in order:
        if not path or not os.path.exists(path):
            continue
        real = os.path.realpath(path)
        if real in seen:
            continue
        seen.add(real)
        ok, version, why = probe_openssl(runner, path)
        if ok:
            return {"path": path, "version": version, "rejected": rejected}
        rejected.append({"path": path, "version": version, "why": why})
    tried = "; ".join(f"{r['path']} ({r['version'] or '판 모름'}: {r['why']})" for r in rejected) or "찾은 openssl 없음"
    hint = "KASATERM_OPENSSL 이 가리킨 것만 봤다" if chosen else "openssl@3 을 설치하거나 KASATERM_OPENSSL 로 가리켜라"
    return {"path": None, "rejected": rejected,
            "why": f"Ed25519 를 검증할 openssl 이 없다 — {tried}. {hint}(자동 설치하지 않는다)"}


def find_tool(runner, name, candidates, version_args=("--version",), env=None, which=shutil.which):
    env = os.environ if env is None else env
    var = "KASATERM_" + name.upper().replace("-", "_")
    chosen = (env.get(var) or "").strip()
    order = [chosen] if chosen else [which(name) or ""] + list(candidates)
    for path in order:
        if path and os.path.exists(path):
            r = runner.run([path, *version_args], timeout=30)
            if r.ok:
                return {"path": path, "version": (r.out or r.err).strip().splitlines()[0] if (r.out or r.err) else ""}
    return {"path": None, "why": f"{name} 을 찾지 못했다" + (f"({var}={chosen})" if chosen else "")}


def check(runner, env=None, which=shutil.which):
    """계획에 싣는 도구 표. 게시에 꼭 필요한 openssl·gh 가 없으면 부르는 쪽이 live 를 막는다."""
    tools = {"openssl": find_openssl(runner, env, which),
             "gh": find_tool(runner, "gh", GH_CANDIDATES, env=env, which=which),
             "cargo": find_tool(runner, "cargo", CARGO_CANDIDATES, env=env, which=which),
             "git-lfs": find_tool(runner, "git-lfs", GIT_LFS_CANDIDATES, version_args=("version",), env=env, which=which)}
    if tools["gh"]["path"]:
        auth = runner.run([tools["gh"]["path"], "auth", "status"], timeout=30)
        tools["gh"]["logged_in"] = auth.ok
    for name, path in SYSTEM.items():
        tools[name] = {"path": path} if os.path.exists(path) else {"path": None, "why": f"{path} 가 없다"}
    return tools


def uses_lfs(repo):
    try:
        return "filter=lfs" in (Path(repo) / ".gitattributes").read_text()
    except OSError:
        return False


def tool_env(tools, names=("git-lfs",), base=None):
    """찾은 도구의 폴더를 PATH 앞에 붙인 env. git 은 LFS 필터를, build-app.sh 는 cargo·gh 를 이름으로 부른다."""
    env = dict(os.environ if base is None else base)
    dirs = []
    for name in names:
        path = (tools.get(name) or {}).get("path")
        if path and os.path.isabs(path) and os.path.dirname(path) not in dirs:
            dirs.append(os.path.dirname(path))
    if dirs:
        env["PATH"] = os.pathsep.join(dirs + [env.get("PATH", "")])
    return env


def git_env(tools, base=None):
    return tool_env(tools, ("git-lfs",), base)


def blocks(tools):
    """live 를 막는 도구 문제. cargo 는 검사·굽기에만 쓰이니 그 단계가 스스로 멈춘다."""
    out = []
    if not tools["openssl"]["path"]:
        out.append(tools["openssl"]["why"])
    if not tools["gh"]["path"]:
        out.append(tools["gh"]["why"] + " — CI·릴리스를 읽을 수 없다")
    elif tools["gh"].get("logged_in") is False:
        out.append("gh 로그인이 없다 — CI·릴리스를 읽을 수 없다(로그인은 사람이 한다)")
    for name in SYSTEM:
        if not tools[name]["path"]:
            out.append(tools[name]["why"])
    return out
