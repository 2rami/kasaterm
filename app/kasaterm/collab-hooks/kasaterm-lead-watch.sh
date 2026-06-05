#!/bin/bash
# 팀장(orchestrator) pane용 Monitor 워처. board-watch가 "형제가 뭘 하나"를
# 알린다면, 이건 한 발 더 나아가 "형제가 사람 입력을 기다리며 멈췄나"를
# 감지해 그 화면째 팀장에게 넘긴다. 팀장 claude는 받아서 선택지를 읽고
# 판단한 뒤 대신 답한다(kasaterm-cli send --surface %N "<번호>").
#
# 멈춤 판정: status=idle  +  화면에 AskUserQuestion/프롬프트 선택 UI 마커
# ("Enter to select" · "to navigate" · "❯"). 같은 대기 상태를 반복 emit하지
# 않도록 직전 키(어느 pane이 무슨 화면으로 대기인지의 해시)와 비교한다.
# 팀장은 이 스크립트를 Monitor에 persistent로 걸기만 하면 된다.
CLI="${KASATERM_CLI:-kasaterm-cli}"
ME="${KASATERM_PANE_ID:-}"
INTERVAL="${KASATERM_LEAD_WATCH_INTERVAL:-3}"

prev=""
while true; do
  out=$("$CLI" board 2>/dev/null | ME="$ME" CLI="$CLI" python3 -c '
import sys, json, os, subprocess, hashlib
me  = os.environ.get("ME", "")
cli = os.environ.get("CLI", "kasaterm-cli")
try:
    board = json.load(sys.stdin)["result"]["board"]
except Exception:
    print(json.dumps({"key": "", "display": ""})); sys.exit(0)

keys, blocks = [], []
for a in sorted(board, key=lambda x: x.get("surface_id", "")):
    sid = a.get("surface_id")
    if sid == me:                       # 팀장 자신은 제외
        continue
    if a.get("status") != "idle":       # 일하는 중이면 멈춘 게 아님
        continue
    intent = a.get("intent", "")
    if "Question" not in intent:        # 질문류 흔적 없으면 peek 안 함(부하 절감)
        continue
    try:                                # 화면 실물로 대기 UI 확증
        r = subprocess.run([cli, "peek", sid, "30"],
                           capture_output=True, text=True, timeout=5)
        screen = json.loads(r.stdout)["result"]["text"]
    except Exception:
        continue
    if not any(m in screen for m in ("Enter to select", "to navigate", "❯")):
        continue
    # 키 = pane + 화면해시. 화면이 그대로면 같은 대기 → 재알림 안 함.
    h = hashlib.sha1(screen.encode()).hexdigest()[:8]
    keys.append(f"{sid}:{h}")
    blocks.append(f"### {sid} — 사람 입력 대기\n{screen.rstrip()}")

print(json.dumps({"key": "|".join(keys), "display": "\n\n".join(blocks)}))
' 2>/dev/null)

  key=$(printf '%s' "$out" | jq -r '.key // ""' 2>/dev/null)
  display=$(printf '%s' "$out" | jq -r '.display // ""' 2>/dev/null)

  if [ -n "$display" ] && [ "$key" != "$prev" ]; then
    echo "── 사람 입력 대기 pane (팀장 개입 필요) ──"
    echo "$display"
    echo ""
    echo "[팀장 행동] 선택지를 읽고 판단해서 대신 답하라: kasaterm-cli send --surface <%N> \"<번호>\" (AskUserQuestion은 숫자키 즉시 선택, Enter 불필요). 애매하면 사람에게 물어라."
    prev="$key"
  fi
  sleep "$INTERVAL"
done
