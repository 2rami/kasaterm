"""릴리스 승인은 나쵸 승인 장부에서만 온다 — 로컬 파일의 「승인됨」은 믿지 않는다.

앱 재시작과 같은 틀이다(docs/app-restart.md, 나쵸 `approvals.py`): 나쵸가 `plan --json` 의 `approval_scope` 를 그대로
요청으로 만들고, 주인이 확인 단추로 decide 한다(10분, 한 번). 카사텀은 게시 첫 단계 직전에 `GET` 으로 읽고
`consume` 으로 한 번 가져간다 — 서버가 받은 scope 로 해시를 다시 재고, 소비 기기가 scope 의 조종 기기이면서 나쵸가
도는 기기일 때만 성공한다. 동작마다 HTTP 창구를 따로 켜므로(`capabilities.approvals.http_actions`), 거기에
`kasaterm_release` 가 없으면 묻기 전에 거절한다.
"""

import hashlib
import json
import os
from pathlib import Path
import re

ACTION = "kasaterm_release"
SCOPE_KEYS = ("action", "plan", "controller", "tag", "commit", "version", "channel", "feed_base", "platforms", "devices")
_AID = re.compile(r"^ap_[0-9a-f]{32}$")
# 나쵸는 승인 기록을 7일 두고 지운다(KEEP_SEC) — 그 뒤의 재개는 새 승인이다.
KEEP_MS = 7 * 86400 * 1000


class Denied(Exception):
    """나쵸가 준 거절 낱말(no_approval·already_used·scope_changed…) 또는 여기서 본 까닭."""


def valid_approval_id(aid):
    return bool(_AID.match(aid or ""))


def scope_hash(scope):
    raw = json.dumps(scope, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    return "sha256:" + hashlib.sha256(raw.encode()).hexdigest()


def local_machine_id():
    """나쵸 `inbox.local_machine_id` 와 같은 자리 — 소비 기기 이름은 여기서만 나온다."""
    v = os.environ.get("KASATERM_MACHINE_ID", "").strip()
    if v:
        return v
    try:
        return (Path.home() / ".config/kasaterm/machine-id").read_text().strip()
    except OSError:
        return ""


def release_scope(plan, controller, feed_base):
    """계약 키 10개 그대로. 계획의 칸을 옮겨 적을 뿐 새로 짓지 않는다."""
    return {"action": ACTION, "plan": plan["plan_id"], "controller": controller, "tag": plan["tag"],
            "commit": plan["commit"], "version": plan["version"], "channel": plan["channel"],
            "feed_base": feed_base, "platforms": sorted(plan["platforms"]), "devices": sorted(plan["device_ids"])}


def scope_problem(scope):
    """나쵸 `validate_release_scope` 가 거절할 모양을 미리 걸러 낸다(나쵸 쪽이 정본이다)."""
    if tuple(sorted(scope)) != tuple(sorted(SCOPE_KEYS)):
        return "scope 칸이 계약과 다르다"
    if not re.fullmatch(r"[0-9a-f]{16}", scope["plan"]):
        return "계획 id 모양이 아니다"
    if not re.fullmatch(r"\d+\.\d+\.\d+", scope["version"]) or scope["tag"] != "v" + scope["version"]:
        return "버전·태그가 계약 모양이 아니다"
    if not re.fullmatch(r"[0-9a-f]{40}", scope["commit"]):
        return "커밋이 소문자 40자가 아니다"
    if scope["channel"] != "stable":
        return "채널은 stable 만 계약에 있다"
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", scope["feed_base"] or ""):
        return "feed_base 모양이 아니다"
    p = scope["platforms"]
    if not p or p != sorted(set(p)) or not set(p) <= {"macos", "windows", "ios"}:
        return "게시할 플랫폼이 없거나 모양이 틀렸다"
    d = scope["devices"]
    if len(d) > 16 or d != sorted(set(d)) or not all(re.fullmatch(r"[A-Za-z0-9_-]{1,128}", x) for x in d):
        return "기기 목록 모양이 틀렸다"
    if not re.fullmatch(r"[A-Za-z0-9_-]{1,128}", scope["controller"] or ""):
        return "조종 기기 id 가 없다"
    return None


class NachoAuthority:
    """나쵸 앱 창구. 주소·키는 데스크톱 앱과 같은 서술자·키 파일(kasa-mcp `nacho_relay::app_target`)."""

    def __init__(self, base, key, http):
        self.base, self._key, self.http = base.rstrip("/"), key, http

    @classmethod
    def from_env(cls, http):
        url = os.environ.get("NACHO_ASK_URL", "").strip()
        if not url:
            desc = Path(os.environ.get("NACHO_ASK_DESCRIPTOR") or Path.home() / ".config/kasaterm/nacho-ask.json")
            try:
                url = str(json.loads(desc.read_text()).get("url") or "").strip()
            except (OSError, ValueError):
                url = ""
        if not re.match(r"https?://", url):
            raise Denied("나쵸 자리 정보가 없다")
        key_path = Path(os.environ.get("NACHO_APP_TOKEN_FILE") or Path.home() / ".config/nacho-app.key")
        try:
            key = key_path.read_text().strip()
        except OSError:
            key = ""
        if not key:
            raise Denied("이 기기에 나쵸 앱 키가 없다 — 승인은 나쵸가 도는 조종 기기에서만 소비된다")
        return cls(url, key, http)

    def _headers(self, post=False):
        h = {"X-Nacho-Token": self._key, "X-Kasa-Owner": "1", "X-Kasa-User": "release-cli"}
        if post:
            h.update({"Content-Type": "application/json", "X-Journal-Request": "1"})
        return h

    def _call(self, method, path, body=None):
        status, raw = self.http.request(method, self.base + path, self._headers(body is not None), body, timeout=15)
        try:
            doc = json.loads(raw.decode() or "{}")
        except ValueError:
            doc = {}
        if status == 200:
            return doc
        if status == 0:
            raise Denied("나쵸에 닿지 못했다")
        raise Denied(str(doc.get("error") or f"나쵸가 {status} 로 답했다"))

    def http_actions(self):
        return set((self._call("GET", "/api/app/capabilities").get("approvals") or {}).get("http_actions") or [])

    def get(self, aid):
        if not _AID.match(aid or ""):
            raise Denied("approval id 모양이 아니다")
        return self._call("GET", f"/api/app/approvals/{aid}")["approval"]

    def consume(self, aid, scope, consumer):
        return self._call("POST", f"/api/app/approvals/{aid}/consume",
                          {"scope": scope, "consumer_machine_id": consumer})["approval"]

    def resume(self, aid, scope, consumer, stage, remote):
        """이미 소비한 승인으로 게시 단계를 잇는다 — 나쵸가 소비 기기·해시·7일·원격 사실을 보고 기록만 남긴다(다시 소비 안 함)."""
        if not _AID.match(aid or ""):
            raise Denied("approval id 모양이 아니다")
        return self._call("POST", f"/api/app/approvals/{aid}/resume",
                          {"scope": scope, "consumer_machine_id": consumer, "stage": stage, "remote": remote})["approval"]


RESUME_STAGES = ("tag", "release", "feed", "devices")


def acquire(authority, aid, scope, controller, record, now_ms, stage=None, remote=None):
    """게시 전 한 번. [record] 는 카사텀 작업 기록에 남긴 소비(`{id, plan, scope_hash, consumed_at_ms}`) — 있으면 재개다.

    새 소비: 창구가 열렸나 → 승인됨·안 씀·안 지남·해시 같음을 먼저 보고 → consume(서버가 다시 잰다).
    재개: 다시 소비하지 않는다. 작업 기록의 id·계획·해시·7일을 여기서 보고, 나쵸 `resume` 에 이을 단계와 원격 사실
    (`{main, tag_parent}`)을 실어 서버 기록과 대조한다 — 소비 안 된 승인·다른 기기·다른 해시·7일 초과·원격 불일치면
    나쵸가 거절한다(만료된 미소비 승인은 절대 되살아나지 않는다).
    """
    problem = scope_problem(scope)
    if problem:
        raise Denied(problem)
    want = scope_hash(scope)
    if record:
        if record.get("id") != aid:
            raise Denied(f"이 계획은 이미 다른 승인({record.get('id')})으로 소비됐다")
        if record.get("plan") != scope["plan"] or record.get("scope_hash") != want:
            raise Denied("작업 기록의 계획·범위가 지금과 다르다 — 새 계획·새 승인이 필요하다")
        if now_ms - int(record.get("consumed_at_ms") or 0) > KEEP_MS:
            raise Denied("소비한 지 7일이 넘었다 — 나쵸 기록이 지워졌으니 새 승인이 필요하다")
        if stage not in RESUME_STAGES:
            raise Denied(f"이을 수 없는 단계다({stage})")
        view = authority.resume(aid, scope, controller, stage, remote or {})
        if view.get("consumed_by") != controller or view.get("scope_hash") != want or not view.get("consumed_at_ms"):
            raise Denied("나쵸 기록이 이 기기의 소비와 맞지 않는다")
        return record
    if ACTION not in authority.http_actions():
        raise Denied("no_approval — 나쵸가 kasaterm_release 승인 창구를 열지 않았다")
    view = authority.get(aid)
    if view.get("action") != ACTION:
        raise Denied("릴리스 승인이 아니다")
    if view.get("state") != "approved":
        raise Denied(f"not_approved:{view.get('state')}")
    if view.get("consumed_at_ms"):
        raise Denied("already_used")
    if int(view.get("expires_at_ms") or 0) <= now_ms:
        raise Denied("expired")
    if view.get("scope_hash") != want:
        raise Denied("scope_changed — 승인한 뒤 대상(커밋·피드·기기)이 바뀌었다")
    got = authority.consume(aid, scope, controller)
    if got.get("consumed_by") != controller or got.get("scope_hash") != want:
        raise Denied("나쵸가 돌려준 소비 기록이 이 기기·이 범위가 아니다")
    return {"id": aid, "plan": scope["plan"], "scope_hash": want, "consumed_at_ms": got.get("consumed_at_ms") or now_ms}
