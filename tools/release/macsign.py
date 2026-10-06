"""mac 판을 조종 기기(미니)에서 Developer ID 로 서명·공증하는 길 — 키를 CI 로 옮기지 않는다.

설치본은 Developer ID(팀 서명)라, CI 가 굽는 자체 서명 판은 기기 업데이트 창구가 팀·공증 검사에서 거절한다(의도).
그렇다고 서명 키를 CI 비밀로 복사하면 신뢰 범위가 넓어진다. 그래서 release.yml 의 `MAC_ARTIFACT: local` 에서는:

  미니: 버전 커밋으로 굽기(hardened runtime·보안 타임스탬프·Developer ID) → dmg 서명 → [승인 뒤] 애플 공증·staple
        → 검증 → 태그 push → 그 dmg 를 릴리스에 올림
  CI : dmg 를 굽지 않고 받아서 팀·공증·staple·판 번호를 검증 → 검증한 그 해시에만 EdDSA 를 달아 appcast

이 모듈은 읽기·판정만 한다. 공증 자격 증명도 읽지 않는다 — notarytool 이 열쇠고리 프로필 이름(`AC_NOTARY`)으로
스스로 찾는다. 열쇠고리 풀기는 부르는 쪽이 `KASATERM_RELEASE_UNLOCK=1` 로 맡겼을 때만, 이미 있는 풀기 도우미
(`~/bin/unlock-signing`)를 서명·공증 바로 앞에서 부른다(backend). 맡기지 않았으면 잠긴 채로 시도해 빨리 실패한다.
"""

import json
import os
from pathlib import Path
import plistlib
import re

DEFAULT_KEYCHAIN = str(Path.home() / "Library/Keychains/codesign.keychain-db")
DEFAULT_PROFILE = "AC_NOTARY"
UNLOCK_HELPER = str(Path.home() / "bin/unlock-signing")
DEVID = "Developer ID Application: "
# release.yml 이 로컬 판을 실제로 검증하는지 — 이 표식이 빠지면 CI 가 검증 없이 appcast 를 낼 수 있어 계획이 막는다.
WORKFLOW_MARKERS = ("Verify locally signed DMG", "source=Notarized Developer ID", "stapler validate", "VERIFIED_SHA")
# build-app.sh 의 KASATERM_SIGN_HARDENED=1 이 하나씩 서명하는 자리(안쪽부터). 이 밖의 Mach-O 는 공증에서 거절된다.
# kasa-op(1Password 실행기)는 굽는 기계에 Go 가 있을 때만 들어간다 — 없으면 목록에 있어도 검사할 조각이 없을 뿐이다.
HARDENED_SIGNED = ("Contents/Frameworks/Sparkle.framework", "Contents/MacOS/kasaterm-cli", "Contents/MacOS/kasa-serve-web",
                   "Contents/MacOS/kasa-op", "Contents/Resources/kasapet", "Contents/MacOS/kasaterm")
_MACHO = {b"\xfe\xed\xfa\xce", b"\xfe\xed\xfa\xcf", b"\xce\xfa\xed\xfe", b"\xcf\xfa\xed\xfe", b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca"}
_RUNTIME = 0x10000


def workflow_mode(text):
    mode = re.search(r"^\s*MAC_ARTIFACT:\s*['\"]?([\w-]+)", text or "", re.M)
    team = re.search(r"^\s*MAC_TEAM:\s*['\"]?([A-Z0-9]{10})\b", text or "", re.M)
    return {"mode": mode.group(1) if mode else "ci", "team": team.group(1) if team else None,
            "verifies": all(m in (text or "") for m in WORKFLOW_MARKERS)}


def repo_ready(repo):
    """계획 커밋의 굽기 스크립트가 공증 가능한 서명을 할 줄 아는가."""
    repo = Path(repo)
    bake = (repo / "scripts/build-app.sh").read_text(errors="ignore") if (repo / "scripts/build-app.sh").exists() else ""
    out = []
    if "KASATERM_SIGN_HARDENED" not in bake:
        out.append("scripts/build-app.sh 에 KASATERM_SIGN_HARDENED(hardened runtime 서명)가 없다")
    if not (repo / "scripts/kasaterm.entitlements").exists():
        out.append("scripts/kasaterm.entitlements 가 없다")
    return out


def parse_identities(text):
    """`security find-identity -v -p codesigning` 의 Developer ID Application 줄 — 인증서 지문과 이름(공개 정보)만."""
    rows = []
    for m in re.finditer(r'^\s*\d+\)\s+([0-9A-F]{40})\s+"(' + re.escape(DEVID) + r'.+ \(([A-Z0-9]{10})\))"', text or "", re.M):
        if m.group(1) not in [r["sha1"] for r in rows]:
            rows.append({"sha1": m.group(1), "name": m.group(2), "team": m.group(3)})
    return rows


def local_signing(runner, tools, env=None, exists=os.path.exists):
    """이 기기의 서명 준비 — 열쇠고리 파일·Developer ID 신원(잠겨 있어도 보인다)·공증 프로필 이름·풀기 도우미의 자리.

    열쇠고리 잠김 여부는 묻지 않는다(`show-keychain-info` 는 화면에 암호 창을 띄우고 멈출 수 있다).
    """
    env = os.environ if env is None else env
    keychain = (env.get("KASATERM_SIGN_KEYCHAIN") or "").strip() or DEFAULT_KEYCHAIN
    info = {"mode": "local", "keychain": keychain, "notary_profile": (env.get("KASATERM_NOTARY_PROFILE") or "").strip()
            or DEFAULT_PROFILE, "identity": None, "unlock_helper": UNLOCK_HELPER if exists(UNLOCK_HELPER) else None,
            "problems": []}
    security = (tools.get("security") or {}).get("path")
    for name in ("security", "xcrun", "ditto"):
        if not (tools.get(name) or {}).get("path"):
            info["problems"].append((tools.get(name) or {}).get("why") or f"{name} 이 도구 표에 없다")
    if not exists(keychain):
        info["problems"].append(f"서명 열쇠고리 {keychain} 가 없다")
        return info
    if not security:
        return info
    r = runner.run([security, "find-identity", "-v", "-p", "codesigning", keychain], timeout=30)
    ids = parse_identities(r.out)
    want = (env.get("KASATERM_SIGN_ID") or "").strip()
    if want:
        ids = [i for i in ids if want in (i["sha1"], i["name"])]
    if not ids:
        info["problems"].append(f"{keychain} 에 쓸 수 있는 Developer ID Application 신원이 없다" + (f"(KASATERM_SIGN_ID={want})" if want else ""))
    elif len(ids) > 1:
        info["problems"].append("Developer ID Application 신원이 여럿이다 — KASATERM_SIGN_ID 로 지문을 골라라: "
                                + ", ".join(f"{i['sha1'][:8]} {i['name']}" for i in ids))
    else:
        info["identity"] = ids[0]
    return info


def plan_problems(local, workflow, installed):
    """로컬 판으로 게시해도 되는 설정인가. 하나라도 있으면 태그 전에 막는다."""
    out = list(local["problems"])
    ident = local.get("identity") or {}
    if not workflow["verifies"]:
        out.append("release.yml 이 로컬 dmg 를 검증하지 않는다(공증·staple·팀·판 번호) — 검증 없는 appcast 를 막는다")
    if ident and workflow["team"] != ident.get("team"):
        out.append(f"release.yml 의 MAC_TEAM({workflow['team'] or '없음'})이 서명 신원 팀({ident.get('team')})과 다르다")
    if ident and installed and installed.get("team") and installed["team"] != ident.get("team"):
        out.append(f"서명 신원 팀({ident.get('team')})이 설치본 팀({installed['team']})과 다르다")
    return out


def predicted_identity(local):
    """계획이 내다보는 게시 신원. 공증은 태그 단계가 하고, 태그·CI·릴리스 단계가 각자 실제 파일로 다시 잰다."""
    ident = local.get("identity") or {}
    return {"authority": ident.get("name"), "team": ident.get("team"), "notarized": bool(ident), "predicted": True,
            "source": "local"}


def is_macho(path):
    try:
        with open(path, "rb") as f:
            return f.read(4) in _MACHO
    except OSError:
        return False


def machos(app):
    app = Path(app)
    found = []
    for root, _dirs, files in os.walk(app):
        for name in files:
            p = Path(root) / name
            if not p.is_symlink() and is_macho(p):
                found.append(p)
    return sorted(found)


def covered(rel):
    return any(rel == c or rel.startswith(c + "/") for c in HARDENED_SIGNED)


def parse_signature(text):
    leaf = re.search(r"^Authority=(.+)$", text or "", re.M)
    team = re.search(r"^TeamIdentifier=(.+)$", text or "", re.M)
    flags = re.search(r"\bflags=0x([0-9a-f]+)", text or "")
    t = team.group(1).strip() if team else None
    return {"authority": leaf.group(1).strip() if leaf else None, "team": None if t in (None, "not set") else t,
            "runtime": bool(flags and int(flags.group(1), 16) & _RUNTIME),
            "timestamp": bool(re.search(r"^Timestamp=", text or "", re.M)), "adhoc": "Signature=adhoc" in (text or "")}


def scan(runner, codesign, app):
    """번들 안 Mach-O 하나하나의 서명 — 공증은 한 조각이라도 빠지면 통째로 거절한다."""
    rows = []
    for p in machos(app):
        rel = str(p.relative_to(app))
        r = runner.run([codesign, "-dvv", str(p)], timeout=60)
        sig = parse_signature((r.err or "") + "\n" + (r.out or "")) if r.ok else {"unsigned": True}
        rows.append({"path": rel, "covered": covered(rel), **sig})
    ent = runner.run([codesign, "-d", "--entitlements", "-", "--xml", str(app)], timeout=60)
    return rows, "get-task-allow" in ((ent.out or "") + (ent.err or ""))


def readiness_problems(rows, team, debuggable=False):
    out = [] if rows else ["번들에서 Mach-O 를 찾지 못했다"]
    if debuggable:
        out.append("번들 권한에 get-task-allow 가 있다 — 애플이 공증을 거절한다")
    for r in rows:
        why = []
        if r.get("unsigned"):
            why.append("서명 없음")
        else:
            if not (r.get("authority") or "").startswith(DEVID):
                why.append(f"Developer ID 가 아님({r.get('authority') or 'adhoc'})")
            if r.get("team") != team:
                why.append(f"팀 {r.get('team') or '없음'}")
            if not r.get("runtime"):
                why.append("hardened runtime 없음")
            if not r.get("timestamp"):
                why.append("보안 타임스탬프 없음")
        if not r.get("covered"):
            why.append("build-app.sh 의 hardened 서명 목록 밖")
        if why:
            out.append(f"{r['path']}: {', '.join(why)}")
    return out


def bundle_version(app):
    try:
        with open(Path(app) / "Contents/Info.plist", "rb") as f:
            info = plistlib.load(f)
    except (OSError, plistlib.InvalidFileException):
        return None
    return info.get("CFBundleShortVersionString")


def dmg_commands(tools, stage, dmg, sign=None):
    """dmg 는 release.yml(ci) 과 같은 모양 — 앱과 /Applications 바로가기, UDZO. 로컬 판은 dmg 자체도 서명한다."""
    out = [[tools["hdiutil"]["path"], "create", "-volname", "kasaterm", "-srcfolder", str(stage), "-ov", "-format", "UDZO", str(dmg)]]
    if sign:
        out.append([tools["codesign"]["path"], "--force", "--sign", sign["identity"]["sha1"], "--timestamp",
                    "--keychain", sign["keychain"], str(dmg)])
    return out


def probe_command(codesign, target, local):
    """열쇠가 지금 쓰이는가 — 작은 사본 하나를 서명해 본다. 잠겼으면 긴 굽기 전에 실패한다(타임스탬프 없이, 밖으로 안 나감)."""
    return [codesign, "--force", "--sign", local["identity"]["sha1"], "--keychain", local["keychain"], "--timestamp=none", str(target)]


def notarize_command(xcrun, dmg, local):
    return [xcrun, "notarytool", "submit", str(dmg), "--keychain-profile", local["notary_profile"],
            "--keychain", local["keychain"], "--wait", "--timeout", "45m", "--output-format", "json"]


def parse_notary(text):
    try:
        doc = json.loads(text or "{}")
    except ValueError:
        return {"id": None, "status": None}
    return {"id": doc.get("id"), "status": doc.get("status"), "message": doc.get("message")}


def dmg_notarized(runner, tools, dmg):
    """공증 표 — spctl 이 dmg 서명을 「Notarized Developer ID」로 받고, staple 된 티켓이 확인되는가."""
    gate = runner.run([tools["spctl"]["path"], "-a", "-t", "open", "--context", "context:primary-signature", "-vv", str(dmg)],
                      timeout=120)
    staple = runner.run([tools["xcrun"]["path"], "stapler", "validate", str(dmg)], timeout=120)
    return {"notarized": "source=Notarized Developer ID" in ((gate.err or "") + (gate.out or "")), "stapled": staple.ok}
