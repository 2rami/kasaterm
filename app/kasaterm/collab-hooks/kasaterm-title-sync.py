#!/usr/bin/env python3
# Stop hook 부속: 세션 제목(custom-title)을 '지금 하는 작업'으로 자동 갱신.
# 피커/board 라벨이 첫 프롬프트·초기 ai-title 에 멈춰 세션이 뭘 하는지
# 헷갈리는 문제(거노) — 턴이 끝날 때마다 마지막 실사용자 프롬프트를 제목으로
# append 한다. 규칙:
#   1. 수동 개명 보호 — 마지막 custom-title 에 nameSource:"auto" 마커가 없으면
#      (claude /rename·kasaterm-cli rename 산) 절대 덮지 않는다.
#   2. 같은 제목이면 append 안 함(턴마다 중복 라인 방지).
#   3. transcript 꼬리 128KB 만 읽는다(대형 파일 방어). append 는 라인 단위라
#      라이브 세션에도 안전(claude 자신도 같은 방식으로 append).
# stdin = Stop hook payload JSON (transcript_path, session_id).
import json
import os
import sys

CLIP = 48
MIN_LEN = 6
TAIL = 128 * 1024


def user_text(v):
    c = (v.get("message") or {}).get("content")
    if isinstance(c, str):
        return c
    if isinstance(c, list):
        for b in c:
            if isinstance(b, dict) and b.get("type") == "text":
                return b.get("text") or ""
    return ""


def main():
    try:
        payload = json.load(sys.stdin)
    except Exception:
        return
    tp = payload.get("transcript_path") or ""
    sid = payload.get("session_id") or ""
    if not tp or not sid or not os.path.isfile(tp):
        return
    size = os.path.getsize(tp)
    with open(tp, "rb") as f:
        if size > TAIL:
            f.seek(-TAIL, 2)
        data = f.read().decode("utf-8", "replace")
    lines = data.splitlines()
    if size > TAIL and lines:
        lines = lines[1:]  # seek 이 줄 중간에 떨어졌을 수 있어 부분줄 스킵

    last_title = None
    title_auto = False
    last_user = None
    for ln in lines:
        if '"custom-title"' in ln:
            try:
                v = json.loads(ln)
            except Exception:
                continue
            if v.get("type") == "custom-title":
                t = (v.get("customTitle") or "").strip()
                if t:
                    last_title = t
                    title_auto = v.get("nameSource") == "auto"
            continue
        if '"user"' not in ln:
            continue
        try:
            v = json.loads(ln)
        except Exception:
            continue
        if v.get("type") != "user" or v.get("isMeta"):
            continue
        txt = " ".join(user_text(v).split())
        # 슬래시 명령·시스템 주입·teammate-message·인터럽트 마커·짧은 맞장구는
        # 제목감이 아니다(sessions.rs is_meta_user_text 와 같은 취지).
        if len(txt) < MIN_LEN or txt[0] in "<[" or txt.startswith("Caveat:"):
            continue
        last_user = txt

    if not last_user:
        return
    if last_title and not title_auto:
        return  # 수동 개명 존중
    new = last_user[:CLIP]
    if last_title == new:
        return
    rec = json.dumps(
        {"type": "custom-title", "customTitle": new, "sessionId": sid, "nameSource": "auto"},
        ensure_ascii=False,
    )
    with open(tp, "a", encoding="utf-8") as f:
        f.write(rec + "\n")


if __name__ == "__main__":
    main()
