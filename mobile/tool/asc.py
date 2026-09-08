#!/usr/bin/env python3
"""App Store Connect API 손잡이 — TestFlight 배포에 필요한 것만.

키는 ~/.config/kasaterm/asc/key.json ({"key_id","issuer_id","p8"}) 또는 환경변수
ASC_KEY_ID / ASC_ISSUER_ID / ASC_KEY_PATH. 키·토큰은 출력하지 않는다.

  asc.py team                         팀 ID(App ID 의 seedId) — 서명에 쓴다
  asc.py bundle-id <id> <이름>        App ID 등록(이미 있으면 그대로)
  asc.py app <bundle id>              앱 레코드 id (없으면 빈 줄 — 웹에서 만들어야 한다)
  asc.py builds <app id>              최근 빌드와 처리 상태
  asc.py wait-build <app id> <번호>   그 빌드 번호가 VALID 가 될 때까지 기다린다(최대 40분)
  asc.py public-link <app id> <그룹>  공개 링크 그룹(없으면 만든다) → 링크 출력
  asc.py add-build <group id> <build id>  그룹에 빌드 넣기(외부 그룹은 베타 심사로 간다)
  asc.py internal-group <app id> <그룹> <이메일>  내부 테스터 그룹(모든 빌드 자동) + 팀원 초대
  asc.py beta-info <app id> <build id> <연락 이메일> <이름> <성> <전화>  베타 심사에 필요한 글
  asc.py submit-review <build id>     베타 앱 심사 제출(공개 링크 테스터에게 풀리려면 필요)
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
    # 팀 ID 는 API 에 곧장 없다 — 등록된 App ID 의 seedId 가 그 값이다(altool 의
    # list-providers 는 API 키 인증을 안 받는다, 2026-09-08 실측).
    r = call("GET", "/bundleIds", params={"limit": 1})
    if not r.get("data"):
        sys.exit("등록된 App ID 가 없다 — 먼저 bundle-id 로 하나 등록해 달라")
    print(r["data"][0]["attributes"]["seedId"])


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


def cmd_internal_group(app, name, email):
    r = call("GET", "/betaGroups", params={"filter[app]": app, "filter[name]": name})
    g = r["data"][0] if r.get("data") else None
    if g is None:
        g = call("POST", "/betaGroups", {"data": {"type": "betaGroups", "attributes": {"name": name, "isInternalGroup": True, "hasAccessToAllBuilds": True}, "relationships": {"app": {"data": {"type": "apps", "id": app}}}}})["data"]
    r = call("GET", "/betaTesters", params={"filter[email]": email, "filter[betaGroups]": g["id"]})
    if not r.get("data"):
        call("POST", "/betaTesters", {"data": {"type": "betaTesters", "attributes": {"email": email}, "relationships": {"betaGroups": {"data": [{"type": "betaGroups", "id": g["id"]}]}}}})
    print(g["id"])


def cmd_beta_info(app, build, email, first, last, phone):
    # 외부(공개 링크) 테스트는 베타 앱 심사를 거치고, 심사는 연락처·앱 설명·이 판의 변경점을 요구한다.
    r = call("GET", "/betaAppReviewDetails", params={"filter[app]": app})
    d = r["data"][0]
    call("PATCH", f"/betaAppReviewDetails/{d['id']}", {"data": {"type": "betaAppReviewDetails", "id": d["id"], "attributes": {"contactEmail": email, "contactFirstName": first, "contactLastName": last, "contactPhone": phone, "demoAccountRequired": False, "notes": "터미널 원격 조종 앱. 사용자가 자기 맥의 주소를 QR 로 받아 붙는다 — 심사용 서버는 별도로 없어 첫 화면(주소 입력)까지가 검토 범위다."}}})
    r = call("GET", "/betaAppLocalizations", params={"filter[app]": app})
    desc = "내 맥에서 도는 kasaterm 터미널과 AI 캐릭터들을 폰에서 보고 답장하는 앱."
    if not r.get("data"):
        call("POST", "/betaAppLocalizations", {"data": {"type": "betaAppLocalizations", "attributes": {"locale": "ko", "description": desc, "feedbackEmail": email}, "relationships": {"app": {"data": {"type": "apps", "id": app}}}}})
    r = call("GET", "/betaBuildLocalizations", params={"filter[build]": build})
    if not r.get("data"):
        call("POST", "/betaBuildLocalizations", {"data": {"type": "betaBuildLocalizations", "attributes": {"locale": "ko", "whatsNew": "첫 TestFlight 판."}, "relationships": {"build": {"data": {"type": "builds", "id": build}}}}})
    print("ok")


def cmd_submit_review(build):
    r = call("POST", "/betaAppReviewSubmissions", {"data": {"type": "betaAppReviewSubmissions", "relationships": {"build": {"data": {"type": "builds", "id": build}}}}})
    print(r["data"]["attributes"].get("betaReviewState"))


if __name__ == "__main__":
    a = sys.argv[1:]
    if not a:
        sys.exit(__doc__)
    {"team": cmd_team, "bundle-id": cmd_bundle_id, "app": cmd_app, "builds": cmd_builds, "wait-build": cmd_wait_build, "public-link": cmd_public_link, "add-build": cmd_add_build, "internal-group": cmd_internal_group, "beta-info": cmd_beta_info, "submit-review": cmd_submit_review}[a[0]](*a[1:])
