#!/usr/bin/env bash
# 맥미니(또는 어느 기계든)에서 맥북에 「구워라」 — 나쵸 역터널의 관문을 통과하는 유일한 모양으로.
#
#   scripts/macbook-bake.sh status          # 맥북 판 보기
#   scripts/macbook-bake.sh app [--force]   # pull → 앱 전체 굽기(반영은 거노가 앱을 껐다 켜야)
#   scripts/macbook-bake.sh pet             # pull → 펫만 갈아 끼우고 다시 띄움(즉시)
#   scripts/macbook-bake.sh journal         # pull → 펫의 뇌 재시작
#
# 관문(nacho-tunnel-guard)은 argv[0] 이 맥북의 `~/.local/bin/kasaterm-remote` **절대 경로**일 때만
# 통과시킨다 — 이름만 보내면 거부된다(2026-09-17 실측). 그래서 경로를 여기 박아 둔다.
# 실제 동작은 맥북 레포의 scripts/remote-bake.sh — 동사를 바꾸려면 그쪽을 고치면 pull 로 따라온다.
set -uo pipefail
HOST="${MACBOOK_HOST:-macbook-kasa}"
REMOTE="${MACBOOK_REMOTE:-/Users/kasa/.local/bin/kasaterm-remote}"
[[ $# -gt 0 ]] || { sed -n 4,8p "$0"; exit 2; }
exec ssh -T -o BatchMode=yes -o ConnectTimeout=8 "$HOST" "$REMOTE" bake "$@"
