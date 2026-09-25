#!/bin/bash
# 앱 재시작 ↔ 나쵸 승인 창구 상호운용 검사(docs/app-restart.md 「확인 방법」).
#
# 나쵸 레포의 실제 승인 서버를 임시 폴더·가짜 앱 키로 띄우고, 카사텀의 실제 러스트 코드로 붙는다:
#   1) 고정 자료(approval.kasaterm_restart.implemented.json)를 카사텀 해시·계획·판정 함수로 대조
#   2) 러스트가 세운 scope 로 실제 HTTP 소비 → 나쵸에서 읽은 승인으로 대상 판정(스위치 on)
#   3) 스위치 off 면 승인된 요청도 503, 키가 틀리면 403
# 운영 키·운영 승인 저장소·카사텀 앱은 건드리지 않는다. 띄운 서버는 잡아 둔 PID 로만 거둔다.
#
# 쓰는 법: scripts/nacho-restart-interop.sh [나쵸 레포 경로]   (기본: 이 레포 옆 nacho-neko)
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
NACHO=$(cd "${1:-$ROOT/../nacho-neko}" && pwd)
PY="$NACHO/.venv/bin/python"
FIXTURES="$NACHO/docs/development/api/fixtures"
FIXTURE="$FIXTURES/approval.kasaterm_restart.implemented.json"
[ -x "$PY" ] || { echo "나쵸 venv 가 없다: $PY" >&2; exit 2; }
[ -f "$FIXTURE" ] || { echo "고정 자료가 없다: $FIXTURE" >&2; exit 2; }

WORK=$(mktemp -d "${TMPDIR:-/tmp}/kasaterm-nacho-interop.XXXXXX")
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
"$PY" -c 'import secrets; print(secrets.token_hex(24))' > "$WORK/wrong.key"
CONTROLLER=$("$PY" -c 'import json,sys; print(json.load(open(sys.argv[1]))["plan"]["controller"])' "$FIXTURE")

# 나쵸 검사(tests/adapters/test_app_restart_approval.py)와 같은 격리 — 저장소·장부·키를 전부 임시 폴더로.
serve() {
  local mode=$1 dir="$WORK/$1"
  mkdir -p "$dir"
  env WORKLOG_STATE="$dir/worklog.json" WORKLOG_ARCHIVE="$dir/archive.jsonl" NACHO_APP_DIR="$dir/appdesk" \
    PETBOX_STATE="$dir/petbox.json" NACHO_APPROVALS_STATE="$dir/approvals.json" NACHO_INBOX_DIR="$dir/inbox" \
    NACHO_INBOX_LEDGER="$dir/inbox_ledger.json" NACHO_ASK_TOKEN_FILE="$dir/nacho-ask.key" NACHO_ORCH_DIR="$dir/orchestra" \
    NACHO_APP_TOKEN_FILE="$WORK/app.key" OWNER_ID=777 SLACK_OWNER_ID=UOWNER KASATERM_MACHINE_ID="$CONTROLLER" \
    NACHO_APPROVALS="$mode" \
    "$PY" - "$NACHO" "$FIXTURE" "$dir/case.json" "$mode" > "$dir/server.log" 2>&1 <<'PY' &
import asyncio, json, os, secrets, sys
nacho, fixture, out, mode = sys.argv[1:5]
sys.path.insert(0, nacho)
from aiohttp import web
from nacho.adapters import askserve
from nacho.adapters.appserve import AppRoutes
from nacho.services import approvals

fx = json.load(open(fixture))
scope = fx["plan"]["scope"]
why = approvals.validate_restart_scope(scope, local_machine=scope["controller"],
                                       registered={t["machine_id"] for t in scope["targets"]})
assert not why, why

def approved():
    a = approvals.create("interop", approvals.RESTART_ACTION, scope, "상호운용 검사")
    code, _ = approvals.decide(a["id"], "approve", scope_hash_seen=a["scope_hash"], rev_seen="1", rev_now="1",
                               nonce=secrets.token_hex(6), device_ok=True)
    assert code == 200
    return a["id"]

ids = {"main": approved(), "wrong_consumer": approved(), "changed": approved(),
       "pending": approvals.create("interop", approvals.RESTART_ACTION, scope, "상호운용 검사")["id"]}

async def main():
    async def no_board():
        return []
    server = askserve.AskServer(None, owner_id="777", mounts=[AppRoutes(None, board_fetch=no_board)])
    runner = web.AppRunner(server.app)
    await runner.setup()
    site = web.TCPSite(runner, "127.0.0.1", 0)
    await site.start()
    port = runner.addresses[0][1]
    json.dump({"mode": mode, "fixture": fixture, "ids": ids, "port": port}, open(out + ".tmp", "w"))
    os.replace(out + ".tmp", out)
    await asyncio.Event().wait()

asyncio.run(main())
PY
  PIDS+=($!)
  local pid=$!
  for _ in $(seq 1 100); do
    [ -f "$dir/case.json" ] && return 0
    kill -0 "$pid" 2>/dev/null || { echo "나쵸 서버($mode)가 죽었다:" >&2; cat "$dir/server.log" >&2; exit 1; }
    sleep 0.1
  done
  echo "나쵸 서버($mode)가 10초 안에 안 떴다" >&2; exit 1
}

roundtrip() { # 케이스 폴더, 키 파일, 기대 모드
  local dir="$WORK/$1" key=$2 mode=$3
  local port
  port=$("$PY" -c 'import json,sys; print(json.load(open(sys.argv[1]))["port"])' "$dir/case.json")
  "$PY" - "$dir/case.json" "$mode" <<'PY'
import json, sys
case = json.load(open(sys.argv[1])); case["mode"] = sys.argv[2]
json.dump(case, open(sys.argv[1] + "." + sys.argv[2], "w"))
PY
  echo "== 실제 나쵸 HTTP 왕복 ($mode)"
  (cd "$ROOT" && env NACHO_ASK_URL="http://127.0.0.1:$port" NACHO_APP_TOKEN_FILE="$key" NACHO_ASK_DESCRIPTOR="$WORK/none.json" \
    KASATERM_RESTART_INTEROP="$dir/case.json.$mode" \
    cargo test -q -p kasaterm app_restart::tests::nacho_restart_round_trip_against_isolated_nacho -- --ignored --exact 2>&1 \
    | grep -E 'test result|panicked|assertion|^ +(left|right):|^error' )
}

echo "== 고정 자료 ↔ 러스트 해시·계획·판정"
(cd "$ROOT" && NACHO_DESK_FIXTURES="$FIXTURES" cargo test -q -p kasa-socket --lib app_restart::tests::nacho_restart_fixture_matches_rust -- --ignored --exact 2>&1 \
  | grep -E 'test result|panicked|assertion|^ +(left|right):|^error')

serve on
serve off
roundtrip on "$WORK/app.key" on
roundtrip off "$WORK/app.key" off
roundtrip on "$WORK/wrong.key" bad_key
echo "== 끝"
