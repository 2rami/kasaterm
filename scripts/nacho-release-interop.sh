#!/bin/bash
# 패치 릴리스 ↔ 나쵸 승인 창구 상호운용 검사(docs/fast-patch-release.md 「나쵸와의 계약」).
#
# 나쵸 레포의 실제 승인 서버를 임시 폴더·가짜 앱 키로 띄우고, 카사텀의 실제 클라이언트(tools/release/nacho.py)로 붙는다:
#   1) 창구 on — 정본 고정 자료의 approval_scope 로 GET → consume 한 번 → 재소비 거절 → resume(원격 사실) →
#      remote_mismatch·bad_remote·not_consumed·not_approved·scope_changed·wrong_consumer, 틀린 키 bad_token
#   2) 창구 off(기본: kasaterm_restart 만) — capabilities 에 없고 acquire 가 no_approval 로 멈춤, GET 404
# 운영 키·운영 승인 저장소·카사텀 앱은 건드리지 않는다. 띄운 서버는 잡아 둔 PID 로만 거둔다.
#
# 쓰는 법: scripts/nacho-release-interop.sh [나쵸 레포 경로]   (기본: 이 레포 옆 nacho-neko)
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
NACHO=$(cd "${1:-$ROOT/../nacho-neko}" && pwd)
PY="$NACHO/.venv/bin/python"
FIXTURE="$NACHO/docs/development/api/fixtures/approval.kasaterm_release.implemented.json"
[ -x "$PY" ] || { echo "나쵸 venv 가 없다: $PY" >&2; exit 2; }
[ -f "$FIXTURE" ] || { echo "고정 자료가 없다: $FIXTURE" >&2; exit 2; }

WORK=$(mktemp -d "${TMPDIR:-/tmp}/kasaterm-release-interop.XXXXXX")
PIDS=()
cleanup() {
  for pid in "${PIDS[@]:-}"; do
    [ -n "$pid" ] || continue
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  done
  rm -rf "$WORK"
}
trap cleanup EXIT

"$PY" -c 'import secrets; print(secrets.token_hex(24))' > "$WORK/app.key"
CONTROLLER=$("$PY" -c 'import json,sys; print(json.load(open(sys.argv[1]))["plan"]["controller"])' "$FIXTURE")

serve() {
  local name=$1 actions=$2 dir="$WORK/$1"
  mkdir -p "$dir"
  env WORKLOG_STATE="$dir/worklog.json" WORKLOG_ARCHIVE="$dir/archive.jsonl" NACHO_APP_DIR="$dir/appdesk" \
    PETBOX_STATE="$dir/petbox.json" NACHO_APPROVALS_STATE="$dir/approvals.json" NACHO_INBOX_DIR="$dir/inbox" \
    NACHO_INBOX_LEDGER="$dir/inbox_ledger.json" NACHO_ASK_TOKEN_FILE="$dir/nacho-ask.key" NACHO_ORCH_DIR="$dir/orchestra" \
    NACHO_APP_TOKEN_FILE="$WORK/app.key" OWNER_ID=777 SLACK_OWNER_ID=UOWNER KASATERM_MACHINE_ID="$CONTROLLER" \
    NACHO_APPROVALS=on NACHO_APPROVAL_HTTP_ACTIONS="$actions" \
    "$PY" - "$NACHO" "$FIXTURE" "$dir/case.json" > "$dir/server.log" 2>&1 <<'PY' &
import asyncio, json, secrets, sys
nacho, fixture, out = sys.argv[1:4]
sys.path.insert(0, nacho)
from aiohttp import web
from nacho.adapters import askserve
from nacho.adapters.appserve import AppRoutes
from nacho.services import approvals

scope = json.load(open(fixture))["plan"]["approval_scope"]
why = approvals.validate_release_scope(scope, local_machine=scope["controller"], registered=set(scope["devices"]))
assert not why, why

def approved(s=scope):
    a = approvals.create("interop", approvals.RELEASE_ACTION, s, "상호운용 검사")
    code, _ = approvals.decide(a["id"], "approve", scope_hash_seen=a["scope_hash"], rev_seen="1", rev_now="1",
                               nonce=secrets.token_hex(6), device_ok=True)
    assert code == 200
    return a["id"]

ids = {"main": approved(), "unconsumed": approved(), "changed": approved(),
       "pending": approvals.create("interop", approvals.RELEASE_ACTION, scope, "상호운용 검사")["id"]}

async def main():
    async def no_board():
        return []
    server = askserve.AskServer(None, owner_id="777", mounts=[AppRoutes(None, board_fetch=no_board)])
    runner = web.AppRunner(server.app)
    await runner.setup()
    site = web.TCPSite(runner, "127.0.0.1", 0)
    await site.start()
    json.dump({"ids": ids, "port": runner.addresses[0][1]}, open(out + ".tmp", "w"))
    import os; os.replace(out + ".tmp", out)
    await asyncio.Event().wait()

asyncio.run(main())
PY
  PIDS+=($!)
  for _ in $(seq 1 100); do
    [ -f "$dir/case.json" ] && return 0
    kill -0 "${PIDS[${#PIDS[@]}-1]}" 2>/dev/null || { cat "$dir/server.log" >&2; exit 1; }
    sleep 0.1
  done
  echo "나쵸 서버가 안 떴다: $dir/server.log" >&2
  exit 1
}

serve on "kasaterm_restart,kasaterm_release"
serve off ""

cd "$ROOT"
python3 - "$FIXTURE" "$WORK" "$CONTROLLER" <<'PY'
import json, sys, time
from pathlib import Path
from tools.release import nacho
from tools.release.proc import Http

fixture, work, controller = sys.argv[1], Path(sys.argv[2]), sys.argv[3]
fx = json.load(open(fixture))
scope = fx["plan"]["approval_scope"]
key = (work / "app.key").read_text().strip()
on, off = (json.loads((work / m / "case.json").read_text()) for m in ("on", "off"))
auth = nacho.NachoAuthority(f"http://127.0.0.1:{on['port']}", key, Http())
now = int(time.time() * 1000)
fails = []

def check(label, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + label + (f" — {detail}" if detail and not ok else ""))
    if not ok:
        fails.append(label)

def denied(label, fn, word):
    try:
        fn()
    except nacho.Denied as e:
        check(label, word in str(e), str(e))
    else:
        check(label, False, "거절되지 않았다")

check("창구 on: capabilities 에 kasaterm_release", nacho.ACTION in auth.http_actions())
view = auth.get(on["ids"]["main"])
check("GET 이 approved·같은 해시", view["state"] == "approved" and view["scope_hash"] == nacho.scope_hash(scope))
record = nacho.acquire(auth, on["ids"]["main"], scope, controller, None, now)
check("acquire 가 한 번 소비", record["id"] == on["ids"]["main"] and record["scope_hash"] == nacho.scope_hash(scope))
denied("같은 승인 재소비 거절", lambda: nacho.acquire(auth, on["ids"]["main"], scope, controller, None, now), "already_used")
remote = {"main": "e" * 40, "tag_parent": scope["commit"]}
again = nacho.acquire(auth, on["ids"]["main"], scope, controller, record, now, stage="release", remote=remote)
check("resume 으로 재개(다시 소비 안 함)", again == record)
check("태그 전 resume(tag_parent 없음)", nacho.acquire(auth, on["ids"]["main"], scope, controller, record, now,
                                                  stage="tag", remote={"main": scope["commit"]}) == record)
denied("태그 부모가 계획 커밋이 아니면 remote_mismatch",
       lambda: nacho.acquire(auth, on["ids"]["main"], scope, controller, record, now, stage="feed",
                             remote={"main": "e" * 40, "tag_parent": "f" * 40}), "remote_mismatch")
denied("모르는 원격 칸 bad_remote",
       lambda: auth.resume(on["ids"]["main"], scope, controller, "feed", {"main": "e" * 40, "extra": 1}), "bad_remote")
denied("이을 수 없는 단계 bad_stage", lambda: auth.resume(on["ids"]["main"], scope, controller, "verify", {}), "bad_stage")
fake_record = {"id": on["ids"]["unconsumed"], "plan": scope["plan"], "scope_hash": nacho.scope_hash(scope), "consumed_at_ms": now}
denied("소비 안 한 승인은 재개 안 됨(not_consumed)",
       lambda: nacho.acquire(auth, on["ids"]["unconsumed"], scope, controller, fake_record, now, stage="release", remote={}),
       "not_consumed")
denied("대기 중 승인 not_approved", lambda: nacho.acquire(auth, on["ids"]["pending"], scope, controller, None, now), "not_approved")
changed = {**scope, "feed_base": "sha256:" + "c" * 64}
denied("범위가 바뀌면 scope_changed(서버 재계산)", lambda: auth.consume(on["ids"]["changed"], changed, controller), "scope_changed")
denied("다른 소비 기기 wrong_consumer", lambda: auth.consume(on["ids"]["changed"], scope, "someone-else"), "wrong_consumer")
denied("틀린 키 bad_token", lambda: nacho.NachoAuthority(auth.base, "wrong", Http()).get(on["ids"]["main"]), "bad_token")

closed = nacho.NachoAuthority(f"http://127.0.0.1:{off['port']}", key, Http())
check("창구 off: capabilities 에 없음", nacho.ACTION not in closed.http_actions())
denied("창구 off: acquire 가 no_approval 로 멈춤",
       lambda: nacho.acquire(closed, off["ids"]["main"], scope, controller, None, now), "no_approval")
denied("창구 off: GET 404 no_approval", lambda: closed.get(off["ids"]["main"]), "no_approval")
print(f"\n{'실패 ' + str(len(fails)) + '건' if fails else '전부 통과'}")
sys.exit(1 if fails else 0)
PY
