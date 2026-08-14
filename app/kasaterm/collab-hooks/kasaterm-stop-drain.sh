#!/bin/bash
# Stop hook: 이 pane 의 claude 가 턴을 끝내려 한다. 두 가지를 한다.
#   1. 세션 제목을 마지막 사용자 프롬프트로 갱신 — 피커·board 라벨이 첫 프롬프트에
#      멈춰 뭘 하는 세션인지 헷갈리는 문제. 수동 개명(/rename·kasaterm-cli rename)은
#      건드리지 않는다(auto 마커로 가른다).
#   2. 작업 완료 데스크탑 알림.
#
# **인박스 drain 은 2026-08-15 에 걷어냈다.** 여기서 보던 쪽지함(`kasacollab msg`)은
# pane 협업이 세션 소켓(SendMessage)으로 옮겨 가면서 아무도 안 쓰게 됐다 — 실측으로
# 이 레포 방에는 그 파일이 만들어진 적조차 없고, 마지막으로 뭔가 쌓인 건 나흘 전 다른
# 레포였다. 그런데도 매 턴 파이썬을 하나 띄워 빈손을 확인하고 있었다.
#
# 파일 이름이 하는 일과 안 맞게 됐지만 그대로 둔다 — 지금 도는 pane 들이 이미 옛 이름을
# 가리키는 훅 설정을 물고 있어서, 이름을 바꾸면 그 pane 들이 턴마다 「파일 없음」으로
# 실패한다. 이름 정리는 pane 이 전부 새로 뜬 뒤에.
#
# $KASATERM_PANE_ID 는 pty-backend 가 pane 스폰 때 주입. Stop payload 는 stdin.
[ -z "$KASATERM_PANE_ID" ] && exit 0

input=$(cat 2>/dev/null)

# 재진입(다른 훅이 한 번 막아 claude 가 더 돈 뒤 다시 멈추려는 경우) — 완료 알림이
# 두 번 뜨지 않게 조용히 나간다. 우리는 더 이상 막지 않으므로 우리 탓으로는 여기 안
# 온다. grep 으로 보는 이유는 이 판정 하나에 파이썬을 띄울 값이 없어서다.
printf '%s' "$input" | grep -q '"stop_hook_active"[[:space:]]*:[[:space:]]*true' && exit 0

HOOKS_DIR="$(cd "$(dirname "$0")" && pwd)"
printf '%s' "$input" | python3 "$HOOKS_DIR/kasaterm-title-sync.py" >/dev/null 2>&1 || true

dir="${PWD##*/}"
kasaterm-cli notify "✓ ${dir} — claude 완료" "작업을 마쳤어" >/dev/null 2>&1 || true
exit 0
