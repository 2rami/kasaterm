#!/usr/bin/env python3
"""App Store Connect API 손잡이 — TestFlight 배포에 필요한 것만.

키는 ~/.config/kasaterm/asc/key.json ({"key_id","issuer_id","p8"}) 또는 환경변수
ASC_KEY_ID / ASC_ISSUER_ID / ASC_KEY_PATH. 키·토큰은 출력하지 않는다.

  asc.py team                         팀 ID(WWDRTeamID) — 서명에 쓴다
  asc.py bundle-id <id> <이름>        App ID 등록(이미 있으면 그대로)
  asc.py app <bundle id>              앱 레코드 id (없으면 빈 줄 — 웹에서 만들어야 한다)
  asc.py builds <app id>              최근 빌드와 처리 상태
  asc.py wait-build <app id> <번호>   그 빌드 번호가 VALID 가 될 때까지 기다린다(최대 40분)
  asc.py public-link <app id> <그룹>  공개 링크 그룹(없으면 만든다) → 링크 출력
  asc.py add-build <group id> <build id>  그룹에 빌드 넣기(외부 그룹은 베타 심사로 간다)
"""
import json, os, sys, time, urllib.request, urllib.error
from pathlib import Path

API = "https://api.appstoreconnect.apple.com/v1"


def creds():
    kid, iss, p8 = os.environ.get("ASC_KEY_ID"), os.environ.get("ASC_ISSUER_ID"), os.environ.get("ASC_KEY_PATH")
    if not (kid and iss and p8):
        c = json.loads((Path.home() / ".config/kasaterm/asc/key.json").read_text())
        kid, iss, p8 = c["key_id"], c["issuer_id"], os.path.expanduser(c["p8"])
    return kid, iss, p8


def token():
    import jwt
    kid, iss, p8 = creds()
    now = int(time.time())
    return jwt.encode(
        {"iss": iss, "iat": now, "exp": now + 1100, "aud": "appstoreconnect-v1"},
        Path(p8).read_text(),
        algorithm="ES256",
        headers={"kid": kid},
    )


def call(method, path, body=None, params=None):
    url = API + path
    if params:
        url += "?" + "&".join(f"{k}={urllib.parse.quote(str(v))}" for k, v in params.items())
    req = urllib.request.Request(url, method=method, data=json.dumps(body).encode() if body else None)
    req.add_header("Authorization", f"Bearer {token()}")
    req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            raw = r.read()
            return json.loads(raw) if raw else {}
    except urllib.error.HTTPError as e:
        detail = e.read().decode(errors="replace")
        sys.exit(f"ASC {e.code} {method} {path}: {detail[:600]}")


import urllib.parse


def cmd_team():
    # 팀 ID 는 API 에 직접 없다 — altool 의 provider 목록이 준다.
    import subprocess
    kid, iss, p8 = creds()
    out = subprocess.run(
        ["xcrun", "altool", "--list-providers", "--apiKey", kid, "--apiIssuer", iss, "--output-format", "json"],
        capture_output=True, text=True, env={**os.environ, "API_PRIVATE_KEYS_DIR": str(Path(p8).parent)},
    )
    try:
        data = json.loads(out.stdout)
    except json.JSONDecodeError:
        sys.exit(f"altool: {out.stdout[-400:]} {out.stderr[-400:]}")
    for p in data.get("providers", []):
        print(p.get("WWDRTeamID"), p.get("ProviderName"))


def cmd_bundle_id(ident, name):
    r = call("GET", "/bundleIds", params={"filter[identifier]": ident})
    if r.get("data"):
        print(r["data"][0]["id"]); return
    r = call("POST", "/bundleIds", {"data": {"type": "bundleIds", "attributes": {"identifier": ident, "name": name, "platform": "IOS"}}})
    print(r["data"]["id"])


def cmd_app(ident):
    r = call("GET", "/apps", params={"filter[bundleId]": ident})
    print(r["data"][0]["id"] if r.get("data") else "")


def cmd_builds(app):
    r = call("GET", "/builds", params={"filter[app]": app, "sort": "-uploadedDate", "limit": 5})
    for b in r.get("data", []):
        a = b["attributes"]
        print(b["id"], a.get("version"), a.get("processingState"), a.get("uploadedDate"))


def cmd_wait_build(app, number):
    for _ in range(80):
        r = call("GET", "/builds", params={"filter[app]": app, "filter[version]": number, "limit": 1})
        if r.get("data"):
            b = r["data"][0]; st = b["attributes"].get("processingState")
            if st == "VALID":
                print(b["id"]); return
            if st in ("FAILED", "INVALID"):
                sys.exit(f"build {number}: {st}")
        time.sleep(30)
    sys.exit("timeout: 빌드가 40분 안에 처리되지 않았다")


def cmd_public_link(app, name):
    r = call("GET", "/betaGroups", params={"filter[app]": app, "filter[name]": name})
    g = r["data"][0] if r.get("data") else None
    if g is None:
        g = call("POST", "/betaGroups", {"data": {"type": "betaGroups", "attributes": {"name": name, "publicLinkEnabled": True, "publicLinkLimitEnabled": False, "isInternalGroup": False}, "relationships": {"app": {"data": {"type": "apps", "id": app}}}}})["data"]
    elif not g["attributes"].get("publicLinkEnabled"):
        g = call("PATCH", f"/betaGroups/{g['id']}", {"data": {"type": "betaGroups", "id": g["id"], "attributes": {"publicLinkEnabled": True, "publicLinkLimitEnabled": False}}})["data"]
    print(g["id"], g["attributes"].get("publicLink") or "")


def cmd_add_build(group, build):
    call("POST", f"/betaGroups/{group}/relationships/builds", {"data": [{"type": "builds", "id": build}]})
    print("ok")


if __name__ == "__main__":
    a = sys.argv[1:]
    if not a:
        sys.exit(__doc__)
    {"team": cmd_team, "bundle-id": cmd_bundle_id, "app": cmd_app, "builds": cmd_builds, "wait-build": cmd_wait_build, "public-link": cmd_public_link, "add-build": cmd_add_build}[a[0]](*a[1:])
