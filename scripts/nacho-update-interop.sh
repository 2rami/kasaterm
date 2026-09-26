#!/bin/bash
# 앱 업데이트 ↔ 나쵸 승인 창구 상호운용 검사(docs/app-update.md 「검사」).
#
# 나쵸 레포의 실제 승인 서버를 임시 폴더·가짜 앱 키로 띄우고, 카사텀의 실제 코드로 붙는다:
#   1) 고정 자료(approval.kasaterm_update.implemented.json)를 러스트 rollout·범위·해시·창·거둠 판정으로 대조
#   2) 파이썬(tools/release/devices)이 지은 rollout 으로 승인을 만들고, 러스트 러너가 실제 HTTP 로 한 번 소비 → 기기 쪽 판정은
#      나쵸에서 읽은 승인으로 → 소비 뒤 창(대상 × 35분)·두 번째 소비 거절·기록으로 다시 굴리기·주인이 거둔 승인
#   3) update 창구가 닫혀 있으면(NACHO_APPROVAL_HTTP_ACTIONS 에 없음) no_approval, 키가 틀리면 bad_token
# 운영 키·운영 승인 저장소·카사텀 앱은 건드리지 않는다. 받기·설치는 없다(기기 진행은 흉내). 띄운 서버는 잡아 둔 PID 로만 거둔다.
#
# 쓰는 법: scripts/nacho-update-interop.sh [나쵸 레포 경로]   (기본: 이 레포 옆 nacho-neko)
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
NACHO=$(cd "${1:-$ROOT/../nacho-neko}" && pwd)
PY="$NACHO/.venv/bin/python"
FIXTURES="$NACHO/docs/development/api/fixtures"
FIXTURE="$FIXTURES/approval.kasaterm_update.implemented.json"
[ -x "$PY" ] || { echo "나쵸 venv 가 없다: $PY" >&2; exit 2; }
[ -f "$FIXTURE" ] || { echo "고정 자료가 없다: $FIXTURE" >&2; exit 2; }
"$PY" -c "import sys; sys.path.insert(0, sys.argv[1]); from nacho.services import approvals; approvals.UPDATE_ACTION; approvals.validate_update_scope" "$NACHO" \
  || { echo "이 나쵸에는 kasaterm_update 승인이 없다" >&2; exit 2; }

WORK=$(mktemp -d "${TMPDIR:-/tmp}/kasaterm-update-interop.XXXXXX")
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
CONTROLLER=$("$PY" -c 'import json,sys; print(json.load(open(sys.argv[1]))["plan"]["approval_scope"]["controller"])' "$FIXTURE")

# 나쵸 검사와 같은 격리 — 저장소·장부·키를 전부 임시 폴더로. 거두기 도구(revoke.sh)도 같은 저장소를 본다.
nacho_env() {
  local dir=$1
  echo WORKLOG_STATE="$dir/worklog.json" WORKLOG_ARCHIVE="$dir/archive.jsonl" NACHO_APP_DIR="$dir/appdesk" \
    PETBOX_STATE="$dir/petbox.json" NACHO_APPROVALS_STATE="$dir/approvals.json" NACHO_INBOX_DIR="$dir/inbox" \
    NACHO_INBOX_LEDGER="$dir/inbox_ledger.json" NACHO_ASK_TOKEN_FILE="$dir/nacho-ask.key" NACHO_ORCH_DIR="$dir/orchestra" \
    NACHO_APP_TOKEN_FILE="$WORK/app.key" OWNER_ID=777 SLACK_OWNER_ID=UOWNER KASATERM_MACHINE_ID="$CONTROLLER" NACHO_APPROVALS=on
}

serve() {
  local mode=$1 actions=$2 dir="$WORK/$1"
  mkdir -p "$dir"
  cat > "$dir/revoke.sh" <<SH
#!/bin/sh
exec env $(nacho_env "$dir") "$PY" -c 'import sys; sys.path.insert(0, sys.argv[1]); from nacho.services import approvals; ok, why = approvals.revoke(sys.argv[2], by="interop"); sys.exit(0 if ok else (print(why) or 1))' "$NACHO" "\$1"
SH
  env $(nacho_env "$dir") NACHO_APPROVAL_HTTP_ACTIONS="$actions" \
    "$PY" - "$NACHO" "$ROOT" "$FIXTURE" "$dir/case.json" "$mode" > "$dir/server.log" 2>&1 <<'PY' &
import asyncio, json, os, secrets, sys, time
nacho, root, fixture, out, mode = sys.argv[1:6]
sys.path.insert(0, nacho)
sys.path.insert(0, root)
from aiohttp import web
from nacho.adapters import askserve
from nacho.adapters.appserve import AppRoutes
from nacho.services import approvals
from tools.release import devices
from tools.release.nacho import scope_hash

fx = json.load(open(fixture))
base = fx["plan"]
controller = base["approval_scope"]["controller"]
machines = [t["machine_id"] for t in base["approval_scope"]["targets"]]
app = "/Users/x/Applications/kasaterm.app"
facts = {mid: {"schema": "kasaterm-restart/1", "machine_id": mid, "label": mid[:8], "os": "macos", "capability": 1, "app_path": app,
               "pid": 4000 + n, "running_exe": app + "/Contents/MacOS/kasaterm",
               "binary": {"inode": 7 + n, "mtime_ms": 1, "build": "8933a0ca"}, "update_capability": 1, "update_enabled": True}
         for n, mid in enumerate(machines)}
created = int(time.time() * 1000)
tmpl = base["jobs"][0]
sha = tmpl["asset"]["sha256"]
rid = devices.rollout_id(base["release_plan"], sha, [(m, devices.target_hash(facts[m])) for m in machines], created)
jobs = [{**tmpl, "machine_id": m, "target_hash": devices.target_hash(facts[m]), "plan_hash": rid,
         "job_id": devices.update_job_id(rid, m, sha), "old_pid": facts[m]["pid"], "created_at_ms": created} for m in machines]
scope = devices.rollout_scope(jobs, controller)
why = approvals.validate_update_scope(scope, local_machine=controller, registered=set(machines))
assert not why, why
rollout = {"id": rid, "release_plan": base["release_plan"], "created_at_ms": created, "approval_scope": scope,
           "approval_scope_hash": scope_hash(scope), "jobs": jobs, "excluded": []}

def approved():
    a = approvals.create("interop", approvals.UPDATE_ACTION, scope, "상호운용 검사")
    code, _ = approvals.decide(a["id"], "approve", scope_hash_seen=a["scope_hash"], rev_seen="1", rev_now="1",
                               nonce=secrets.token_hex(6), device_ok=True)
    assert code == 200, code
    return a["id"]

ids = {"main": approved(), "pending": approvals.create("interop", approvals.UPDATE_ACTION, scope, "상호운용 검사")["id"]}

async def main():
    async def no_board():
        return []
    server = askserve.AskServer(None, owner_id="777", mounts=[AppRoutes(None, board_fetch=no_board)])
    runner = web.AppRunner(server.app)
    await runner.setup()
    site = web.TCPSite(runner, "127.0.0.1", 0)
    await site.start()
    port = runner.addresses[0][1]
    json.dump({"mode": mode, "ids": ids, "port": port, "rollout": rollout, "facts": facts}, open(out + ".tmp", "w"))
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
    KASATERM_UPDATE_INTEROP="$dir/case.json.$mode" KASATERM_UPDATE_INTEROP_REVOKE="$dir/revoke.sh" \
    cargo test -q -p kasaterm app_update::tests::nacho_update_round_trip_against_isolated_nacho -- --ignored --exact 2>&1 \
    | grep -E 'test result|panicked|assertion|^ +(left|right):|^error' )
}

echo "== 고정 자료 ↔ 러스트 rollout·범위·해시·창·거둠"
(cd "$ROOT" && NACHO_DESK_FIXTURES="$FIXTURES" cargo test -q -p kasa-socket --lib app_update::tests::nacho_update_fixture_matches_rust -- --ignored --exact 2>&1 \
  | grep -E 'test result|panicked|assertion|^ +(left|right):|^error')

serve on "kasaterm_restart,kasaterm_update"
serve off "kasaterm_restart"
roundtrip on "$WORK/app.key" on
roundtrip off "$WORK/app.key" off
roundtrip on "$WORK/wrong.key" bad_key
echo "== 끝"
