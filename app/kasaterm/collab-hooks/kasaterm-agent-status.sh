#!/bin/bash
# PreToolUse / PostToolUse / Stop 훅 — 이 pane 이 **지금 무엇을 돌리고 있는지** 를
# 일어난 그 순간 앱에 밀어 넣는다. 진행 표시(헤더 바·사이드바 펄스)의 정본.
#
# 왜 필요했나: 진행 표시는 원래 claude 의 transcript 꼬리 **64KB** 를 읽어 「런치
# (tool_use)」와 「회수(tool_result)」를 짝지어 알아냈다. 그 방식은 세션이 커지면
# 조용히 눈이 먼다 — 런치 기록이 꼬리 밖으로 밀려나면 짝이 안 맞아 in-flight 인
# 줄을 모른다. 2026-08-11 실측: 3.8MB 세션은 7건이 잡혔는데 8.3MB·24MB 세션은
# 0건이었다. 하필 **오래 기다리는 작업일수록 안 보이는** 쪽으로 틀려서, 진행
# 표시가 가장 필요한 자리에서 꺼져 있었다.
#
# 훅은 그 순간 한 번 오고 끝이라 세션 크기와 무관하다. Orca 가 「상태는 훅에서
# 온다, 터미널 타이틀에서 추론하지 않는다」로 정한 것과 같은 자리다.
#
# ⚠️ 이 훅은 **모든 도구 호출마다** 돈다. 그래서 python3 를 띄우기 전에 bash 만으로
# 관심 밖을 걷어낸다 — Task 도 백그라운드도 아닌 호출이 압도적 다수라, 여기서 안
# 거르면 Edit 하나하나에 인터프리터 기동 비용이 그대로 얹힌다.
#
# ⚠️ 무슨 일이 있어도 **exit 0, stdout 은 비운다**. 훅이 실패하거나 뭔가를 출력해
# claude 의 도구 호출이 막히는 것보다, 진행 표시가 한 번 빠지는 편이 훨씬 낫다.
[ -z "$KASATERM_PANE_ID" ] && exit 0
payload="$(cat 2>/dev/null)"

# payload 실물 덤프. 훅 스펙은 문서가 끝까지 알려 주지 않는 부분이 있어(PostToolUse
# 의 tool_use_id 유무, 백그라운드 Bash 의 발화 시점) 실물을 봐야 할 때가 온다.
if [ -n "$KASATERM_HOOK_DEBUG" ]; then
  printf '%s\n' "$payload" >>"${TMPDIR:-/tmp}/kasaterm-hook-payload.jsonl" 2>/dev/null
fi

# 턴이 끝나면 훅이 쥐고 있던 것을 **통째로 놓는다**. 여기가 이 설계의 이음매다:
# 훅은 그 턴 동안만 정본이고, 턴이 끝나면 판정을 transcript 폴백에 넘긴다.
#
# 그래야 하는 이유는 백그라운드 셸이다. 서브에이전트는 시작·종료가 훅 한 쌍으로
# 닫히지만, 백그라운드 명령은 **끝났다고 알려 주는 훅이 없다**(도구 호출이 끝난
# 뒤에도 계속 돈다). 놓지 않으면 이미 끝난 셸이 「도는 중」으로 영영 화면에 남는다.
# 놓아도 안전한 것은, 방금 끝난 턴의 런치라면 transcript 꼬리 맨 끝에 있어서 폴백이
# 확실히 본다는 점이다 — 훅이 메우려던 「꼬리에서 밀려난 옛 런치」와는 반대 상황이다.
case "$payload" in
  *'"hook_event_name":"Stop"'* | *'"hook_event_name": "Stop"'*)
    kasaterm-cli agent-status clear subagent - >/dev/null 2>&1 || true
    kasaterm-cli agent-status clear background - >/dev/null 2>&1 || true
    exit 0
    ;;
esac

# 빠른 거르기 — 둘 중 하나도 아니면 인터프리터를 안 띄우고 나간다.
# `tool_name` 이 정확히 무엇인지(`Task`/`Agent`)는 문서가 안 정해 줘서 둘 다 본다.
case "$payload" in
  *'"Task"'* | *'"Agent"'*) ;;
  *'"run_in_background":true'* | *'"run_in_background": true'*) ;;
  *) exit 0 ;;
esac

out="$(printf '%s' "$payload" | python3 -c '
import json, sys

try:
    d = json.load(sys.stdin)
except Exception:
    raise SystemExit(0)

ev = d.get("hook_event_name") or ""

# 서브에이전트 **안에서** 난 도구 호출도 부모 세션의 같은 훅을 발화시킨다(문서 명시:
# 그때 agent_id·agent_type 이 실린다). 그것까지 세면 한 작업이 두 번 잡히므로 끊는다.
if d.get("agent_id"):
    raise SystemExit(0)

name = d.get("tool_name") or ""
ti = d.get("tool_input") or {}
if not isinstance(ti, dict):
    ti = {}
key = d.get("tool_use_id") or ""


def emit(phase, kind, label):
    print(f"{phase} {kind} {key}")
    print(" ".join((label or "").split())[:40])


# 서브에이전트 — 시작과 끝이 훅 한 쌍으로 완결된다. tool_use_id 가 그대로 짝이 된다.
if name in ("Task", "Agent"):
    if not key:
        raise SystemExit(0)
    label = ti.get("description") or ti.get("subagent_type") or "서브에이전트"
    emit("start" if ev == "PreToolUse" else "end", "subagent", label)
    raise SystemExit(0)

# 백그라운드 셸 — 시작만 잡는다. 끝을 알려 주는 훅이 없어서(명령은 도구 호출이 끝난
# 뒤에도 계속 돈다) 회수는 위의 `Stop` 이 통째로 한다.
#
# `PostToolUse` 에서 잡는 이유: `PreToolUse` 는 아직 안 뜬 셸이라, 권한 거부나 실패로
# 끝내 안 뜨면 시작만 있고 끝이 없는 유령이 남는다.
if name == "Bash" and ev == "PostToolUse":
    bg = ti.get("run_in_background")
    if bg is True or bg == "true":
        if not key:
            raise SystemExit(0)
        emit("start", "background", ti.get("description") or ti.get("command") or "백그라운드")

raise SystemExit(0)
' 2>/dev/null)"

[ -z "$out" ] && exit 0
head="$(printf '%s' "$out" | sed -n 1p)"
label="$(printf '%s' "$out" | sed -n 2p)"
# $head 는 `phase kind key` 세 토큰이라 일부러 quote 하지 않는다 — 셋 다 공백이 없다
# (phase·kind 는 고정 어휘, key 는 `toolu_…` 형식).
# shellcheck disable=SC2086
kasaterm-cli agent-status $head "$label" >/dev/null 2>&1 || true
exit 0
