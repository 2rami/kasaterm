#!/bin/bash
# UserPromptSubmit(start) · Stop(end) · PreCompact(compact_start) 훅 — 이 pane 의 턴 경계를
# 그 순간 앱에 알린다. pane 상태(일하는 중·압축 중·끝남)의 정본이 이 경계다: 화면의 스피너
# 글리프를 읽어 알아내던 시절엔 하네스 UI 가 바뀔 때마다 판정이 깨졌다(2026-09-17).
#
# 첫 인자가 phase 다. 페이로드의 `permission_mode` 도 함께 실어 bypass 여부를 앱이 안다.
#
# start 때 tell 이 「지금 일」(`--title`)과 함께 들어온 턴이면 CLI 가 `sessionTitle` JSON 한 줄을 낸다 —
# claude 세션 이름(`/resume`·agents 목록)을 맞추는 공식 창구다. 그것만 stdout 으로 넘긴다.
#
# ⚠️ 프롬프트 핫패스다 — bash 와 sed 만 쓰고 인터프리터를 안 띄운다. 무슨 일이 있어도
# exit 0(훅이 실패하면 claude 의 턴이 막힌다).
[ -z "$KASATERM_PANE_ID" ] && exit 0
phase="$1"
[ -z "$phase" ] && exit 0
payload="$(cat 2>/dev/null)"
mode="$(printf '%s' "$payload" | sed -n 's/.*"permission_mode"[[:space:]]*:[[:space:]]*"\([A-Za-z_-]*\)".*/\1/p' | head -n 1)"
if [ -n "$mode" ]; then
  out="$(kasaterm-cli turn "$phase" --permission-mode "$mode" 2>/dev/null)"
else
  out="$(kasaterm-cli turn "$phase" 2>/dev/null)"
fi
case "$phase:$out" in
  'start:{"hookSpecificOutput"'*) printf '%s\n' "$out" ;;
esac
exit 0
