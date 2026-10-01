#!/usr/bin/env python3
"""Ad Hoc 판을 관문 기계(미니)에서 다시 서명하고 새 기기를 등록한다 — 서명 키는 이 기계 밖으로 안 나간다.

  adhoc_sign.py setup                             배포 인증서·전용 키체인 만들기(기계마다 한 번)
  adhoc_sign.py resign <판 폴더>                  등록된 기기 전부로 프로파일을 맞추고 ipa 를 다시 서명
  adhoc_sign.py register <판 폴더> <UDID> <이름>  기기 등록(있으면 그대로) 뒤 resign
  adhoc_sign.py counts <설치 폴더>                기기 수만 다시 적는다(devices.json)
  adhoc_sign.py sign-profile                      표준입력의 .mobileconfig 를 같은 인증서로 서명해 표준출력으로

관문(gateway_install.rs)이 KASA_INSTALL_SIGNER 로 부르고, adhoc.sh 가 올릴 때 resign 을 부른다. 출력 마지막 줄은
JSON 한 줄이다. 키·암호는 출력하지 않는다. Python 3.9(미니 기본)에서 돈다.

상태: ~/.config/kasaterm/asc/adhoc/(cert.json·프로파일, 700), 키체인 ~/Library/Keychains/ios-adhoc.keychain-db
(암호 ~/.config/kasaterm/asc/ios-adhoc.pw, 사용자 검색 목록에 올린다). 키체인을 로그인 키체인과 따로 두는 까닭: 로그인 키체인은 파티션 목록을
로그인 암호로만 열 수 있어 codesign 이 접근 창에서 멈춘다(testflight.sh 머리말, 10-01 실측).
"""
import base64, fcntl, hashlib, json, os, plistlib, re, secrets, shutil, subprocess, sys, tempfile, time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import asc  # noqa: E402

ASC_DIR = Path.home() / ".config/kasaterm/asc"
STATE = ASC_DIR / "adhoc"
KC = Path.home() / "Library/Keychains/ios-adhoc.keychain-db"
PW = ASC_DIR / "ios-adhoc.pw"
IPA = "kasaterm.ipa"
UDID = re.compile(r"^([0-9A-Fa-f]{8}-[0-9A-Fa-f]{16}|[0-9A-Fa-f]{40})$")
# 애플은 기기 종류마다 멤버십 해에 100대까지 받는다. 해지한 기기도 갱신 전까지 센다.
YEARLY_LIMIT = 100


def run(*cmd, data=None):
    r = subprocess.run(cmd, input=data, capture_output=True)
    if r.returncode != 0:
        raise RuntimeError(f"{Path(cmd[0]).name} {cmd[1] if len(cmd) > 1 else ''} 실패: {r.stderr.decode(errors='replace').strip()[:300]}")
    return r.stdout


def cert():
    p = STATE / "cert.json"
    if not p.exists():
        raise RuntimeError("서명 인증서가 없다 — 이 기계에서 adhoc_sign.py setup 을 먼저")
    return json.loads(p.read_text())


def unlock():
    run("security", "unlock-keychain", "-p", PW.read_text().strip(), str(KC))
    # codesign 은 --keychain 을 줘도 검색 목록에 없는 키체인에서는 신원을 못 찾는다(10-01 미니 실측 「no identity
    # found」). 목록은 통째로 덮어쓰는 명령이라 기존 항목을 함께 넘긴다.
    listed = re.findall(r'"([^"]+)"', run("security", "list-keychains", "-d", "user").decode())
    if str(KC) not in listed:
        run("security", "list-keychains", "-d", "user", "-s", *listed, str(KC))


def cmd_setup():
    STATE.mkdir(parents=True, exist_ok=True)
    os.chmod(STATE, 0o700)
    if (STATE / "cert.json").exists():
        return {"ok": True, "cert": cert()["id"], "already": True}
    if not PW.exists():
        PW.write_text(secrets.token_urlsafe(24))
        os.chmod(PW, 0o600)
    pw = PW.read_text().strip()
    if not KC.exists():
        run("security", "create-keychain", "-p", pw, str(KC))
        run("security", "set-keychain-settings", str(KC))
    unlock()
    with tempfile.TemporaryDirectory() as tmp:
        key, csr = Path(tmp) / "key.pem", Path(tmp) / "csr.pem"
        run("openssl", "genrsa", "-out", str(key), "2048")
        run("openssl", "req", "-new", "-key", str(key), "-subj", "/CN=kasaterm adhoc relay", "-out", str(csr))
        r = asc.call("POST", "/certificates", {"data": {"type": "certificates", "attributes": {"certificateType": "DISTRIBUTION", "csrContent": csr.read_text()}}})
        der = base64.b64decode(r["data"]["attributes"]["certificateContent"])
        (Path(tmp) / "cert.cer").write_bytes(der)
        run("security", "import", str(key), "-k", str(KC), "-t", "priv", "-f", "openssl", "-T", "/usr/bin/codesign", "-T", "/usr/bin/security")
        run("security", "import", str(Path(tmp) / "cert.cer"), "-k", str(KC))
    run("security", "set-key-partition-list", "-S", "apple-tool:,apple:,codesign:", "-s", "-k", pw, str(KC))
    info = {"id": r["data"]["id"], "sha1": hashlib.sha1(der).hexdigest().upper(), "expires": r["data"]["attributes"].get("expirationDate")}
    (STATE / "cert.json").write_text(json.dumps(info))
    return {"ok": True, "cert": info["id"]}


def all_devices():
    return asc.call("GET", "/devices", params={"filter[platform]": "IOS", "limit": 200}).get("data", [])


def profile_for(bid, cert_id, device_ids):
    """그 번들의 Ad Hoc 프로파일 — 기기·인증서가 그대로고 한 주 넘게 남았으면 받아 둔 것을 쓴다."""
    book_path = STATE / "profiles.json"
    book = json.loads(book_path.read_text()) if book_path.exists() else {}
    saved = STATE / f"{bid}.mobileprovision"
    rec = book.get(bid)
    if rec and saved.exists() and rec["cert"] == cert_id and rec["devices"] == device_ids and rec["until"] > time.time() + 7 * 86400:
        return saved
    name = f"kasaterm adhoc {bid}"
    old = asc.call("GET", "/profiles", params={"filter[name]": name, "limit": 20}).get("data", [])
    for p in old:
        asc.call("DELETE", f"/profiles/{p['id']}")
    b = next(x for x in asc.call("GET", "/bundleIds", params={"filter[identifier]": bid}).get("data", []) if x["attributes"]["identifier"] == bid)
    r = asc.call("POST", "/profiles", {"data": {"type": "profiles", "attributes": {"name": name, "profileType": "IOS_APP_ADHOC"}, "relationships": {
        "bundleId": {"data": {"type": "bundleIds", "id": b["id"]}},
        "certificates": {"data": [{"type": "certificates", "id": cert_id}]},
        "devices": {"data": [{"type": "devices", "id": d} for d in device_ids]}}}})
    saved.write_bytes(base64.b64decode(r["data"]["attributes"]["profileContent"]))
    until = time.mktime(time.strptime(r["data"]["attributes"]["expirationDate"][:19], "%Y-%m-%dT%H:%M:%S"))
    book[bid] = {"cert": cert_id, "devices": device_ids, "until": until}
    book_path.write_text(json.dumps(book))
    return saved


# 서명 없이 아카이브한 판(관문 기계에서 구울 때)은 권한을 프로파일에서 고른다. keychain-access-groups 는 빼야 한다 —
# 넣으면 키체인 기본 그룹이 바뀌어 폰에 저장된 로그인을 못 읽는다.
PROFILE_KEYS = ("application-identifier", "com.apple.developer.team-identifier", "get-task-allow",
                "aps-environment", "com.apple.developer.usernotifications.communication")


def entitlements(bundle, profile, out):
    try:
        out.write_bytes(run("codesign", "-d", "--entitlements", "-", "--xml", str(bundle)))
        if plistlib.loads(out.read_bytes()):
            return
    except (RuntimeError, plistlib.InvalidFileException, ValueError):
        pass
    ent = plistlib.loads(run("security", "cms", "-D", "-i", str(profile)))["Entitlements"]
    out.write_bytes(plistlib.dumps({k: ent[k] for k in PROFILE_KEYS if k in ent}))


def cmd_resign(release):
    release = Path(release)
    c = cert()
    device_ids = sorted(d["id"] for d in all_devices() if d["attributes"]["status"] == "ENABLED")
    unlock()
    with tempfile.TemporaryDirectory(dir=release.parent) as tmp:
        work = Path(tmp)
        run("unzip", "-q", str(release / IPA), "-d", str(work / "x"))
        app = next((work / "x/Payload").glob("*.app"))
        bundles = sorted(app.glob("PlugIns/*.appex")) + [app]
        sign = ["codesign", "-f", "-s", c["sha1"], "--keychain", str(KC)]
        for b in bundles:
            for fw in sorted(b.glob("Frameworks/*.framework")) + sorted(b.glob("Frameworks/*.dylib")):
                run(*sign, str(fw))
            bid = plistlib.loads((b / "Info.plist").read_bytes())["CFBundleIdentifier"]
            profile = profile_for(bid, c["id"], device_ids)
            ent = work / f"{bid}.entitlements"
            entitlements(b, profile, ent)
            shutil.copyfile(profile, b / "embedded.mobileprovision")
            run(*sign, "--entitlements", str(ent), "--generate-entitlement-der", str(b))
        run("codesign", "--verify", "--deep", "--strict", str(app))
        out = work / IPA
        subprocess.run(["zip", "-qry", str(out), "."], cwd=work / "x", check=True)
        os.replace(out, release / IPA)
    counts(release.parent)
    return {"ok": True, "devices": len(device_ids)}


def cmd_register(release, udid, name):
    if not UDID.match(udid):
        raise RuntimeError("UDID 꼴이 아니다")
    name = re.sub(r"[^\w .'\-()가-힣]", "", name).strip()[:50] or "kasaterm device"
    found = asc.call("GET", "/devices", params={"filter[udid]": udid}).get("data", [])
    new = not found
    if new:
        asc.call("POST", "/devices", {"data": {"type": "devices", "attributes": {"name": name, "udid": udid, "platform": "IOS"}}})
    elif found[0]["attributes"]["status"] != "ENABLED":
        d = found[0]
        asc.call("PATCH", f"/devices/{d['id']}", {"data": {"type": "devices", "id": d["id"], "attributes": {"status": "ENABLED"}}})
    out = cmd_resign(release)
    out["new"] = new
    return out


def counts(install):
    classes = {}
    for d in all_devices():
        a = d["attributes"]
        c = classes.setdefault(a["deviceClass"], {"enabled": 0, "disabled": 0})
        c["enabled" if a["status"] == "ENABLED" else "disabled"] += 1
    p = Path(install) / "devices.json"
    tmp = p.with_suffix(".tmp")
    tmp.write_text(json.dumps({"updated": int(time.time()), "limit": YEARLY_LIMIT, "classes": classes}))
    os.replace(tmp, p)
    return {"ok": True, "classes": classes}


def cmd_sign_profile():
    c = cert()
    unlock()
    with tempfile.TemporaryDirectory() as tmp:
        src, dst = Path(tmp) / "in", Path(tmp) / "out"
        src.write_bytes(sys.stdin.buffer.read())
        name = run("security", "find-certificate", "-Z", "-a", str(KC)).decode()
        label = re.search(r'"labl"<blob>="([^"]+)"', name)
        if not label:
            raise RuntimeError("키체인에서 인증서 이름을 못 찾았다")
        run("security", "cms", "-S", "-N", label.group(1), "-k", str(KC), "-i", str(src), "-o", str(dst))
        sys.stdout.buffer.write(dst.read_bytes())
    return None


def main():
    a = sys.argv[1:]
    if not a:
        sys.exit(__doc__)
    cmds = {"setup": cmd_setup, "resign": cmd_resign, "register": cmd_register, "counts": counts, "sign-profile": cmd_sign_profile}
    if a[0] not in cmds:
        sys.exit(__doc__)
    STATE.mkdir(parents=True, exist_ok=True)
    # 관문의 등록 작업과 adhoc.sh 의 올리기가 같은 프로파일·ipa 를 만질 수 있어 한 번에 하나만.
    with open(STATE / "lock", "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        try:
            out = cmds[a[0]](*a[1:])
        except SystemExit as e:  # asc.call 이 HTTP 오류를 SystemExit 로 낸다
            out = {"ok": False, "error": str(e.code)[:400]}
        except Exception as e:
            out = {"ok": False, "error": str(e)[:400]}
    if out is not None:
        print(json.dumps(out, ensure_ascii=False))
        sys.exit(0 if out.get("ok") else 1)


if __name__ == "__main__":
    main()
