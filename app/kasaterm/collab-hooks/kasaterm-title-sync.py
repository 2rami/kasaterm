#!/usr/bin/env python3
# Stop hook 부속: 세션 제목(custom-title)을 claude(haiku)가 지은 요약 제목으로
# 자동 갱신. 거노: "클로드가 정해주는 것만" — 마지막 프롬프트 원문 스탬프(v1)를
# 폐기하고 LLM 재생성으로 교체. claude 자체 ai-title 은 최초 1회 생성 후 값이
# 안 바뀌는 것 실측(같은 제목 37회 재스탬프) — 그래서 우리가 재생성한다.
#
# 구조(호출 2상):
#   기본(Stop, stdin=payload): 갱신 필요 판단만 하고 즉시 종료 — 필요하면
#     자신을 --generate 로 detach 스폰(Stop 흐름 무지연, 실측 haiku 14s).
#   --generate <transcript> <sid> <srclen>: 백그라운드. flock 으로 세션당
#     1개만, junk cwd 에서 headless claude 호출 후 제목 검증·스탬프.
#
# 규칙:
#   1. 수동 개명 보호 — 마지막 custom-title 에 nameSource:"auto" 가 없으면
#      (claude /rename·kasaterm-cli rename 산) 절대 덮지 않는다.
#   2. 스로틀 — 직전 auto 스탬프(genTs) 후 60s 이상 && 그 뒤 실사용자 턴이
#      1개 이상 있을 때만 재생성(auto 레코드의 srcLen 오프셋 기준).
#   3. headless claude 는 shim PATH 를 우회해 실바이너리 직접 호출(shim 을
#      타면 pane 의 --session-id 가 붙어 라이브 세션과 충돌) + 전용 junk
#      cwd($TMPDIR/kasaterm-title-gen)라 -p 가 남기는 세션 파일이 실제
#      프로젝트 피커에 안 섞인다(1일 지난 것 자동 청소).
#   4. append 는 라인 단위라 라이브 transcript 에 안전(claude 도 같은 방식).
import fcntl
import json
import os
import subprocess
import sys
import tempfile
import time

CLIP = 48
TAIL = 128 * 1024
MIN_INTERVAL_S = 60
CTX_PROMPTS = 6
CTX_CHARS = 1400


def read_tail(tp):
    size = os.path.getsize(tp)
    with open(tp, "rb") as f:
        if size > TAIL:
            f.seek(-TAIL, 2)
        data = f.read().decode("utf-8", "replace")
    lines = data.splitlines()
    if size > TAIL and lines:
        lines = lines[1:]  # seek 이 줄 중간에 떨어졌을 수 있어 부분줄 스킵
    return size, lines


def user_text(v):
    c = (v.get("message") or {}).get("content")
    if isinstance(c, str):
        return c
    if isinstance(c, list):
        for b in c:
            if isinstance(b, dict) and b.get("type") == "text":
                return b.get("text") or ""
    return ""


def assistant_text(v):
    c = (v.get("message") or {}).get("content")
    out = []
    if isinstance(c, list):
        for b in c:
            if isinstance(b, dict) and b.get("type") == "text":
                out.append(b.get("text") or "")
    return " ".join(out)


def scan(lines):
    """tail 라인들 → (마지막 custom-title 레코드 dict|None, 실사용자 발화 [(줄번호, 텍스트)], assistant 텍스트 [(줄번호, 텍스트)])."""
    title = None
    users = []
    asst = []
    for i, ln in enumerate(lines):
        if '"custom-title"' in ln:
            try:
                v = json.loads(ln)
            except Exception:
                continue
            if v.get("type") == "custom-title" and (v.get("customTitle") or "").strip():
                title = v
            continue
        if '"user"' in ln:
            try:
                v = json.loads(ln)
            except Exception:
                continue
            if v.get("type") != "user" or v.get("isMeta"):
                continue
            txt = " ".join(user_text(v).split())
            if len(txt) < 6 or txt[0] in "<[" or txt.startswith("Caveat:"):
                continue
            users.append((i, txt))
        elif '"assistant"' in ln:
            try:
                v = json.loads(ln)
            except Exception:
                continue
            if v.get("type") != "assistant":
                continue
            txt = " ".join(assistant_text(v).split())
            if txt:
                asst.append((i, txt))
    return title, users, asst


def decide():
    try:
        payload = json.load(sys.stdin)
    except Exception:
        return
    tp = payload.get("transcript_path") or ""
    sid = payload.get("session_id") or ""
    if not tp or not sid or not os.path.isfile(tp):
        return
    size, lines = read_tail(tp)
    title, users, _ = scan(lines)
    if not users:
        return
    if title is not None:
        if title.get("nameSource") != "auto":
            return  # 수동 개명 존중
        if time.time() - float(title.get("genTs") or 0) < MIN_INTERVAL_S:
            return
        # 직전 스탬프 라인 이후 새 실사용자 턴이 없으면 재생성 불필요.
        # (tail 재파싱이라 라인 인덱스 비교가 아닌 '스탬프 레코드 뒤' 판단:
        #  custom-title 은 scan 에서 가장 마지막 것이므로, 그보다 뒤 user 만 유효)
        t_idx = None
        for i, ln in enumerate(lines):
            if '"custom-title"' in ln and (title.get("customTitle") or "") in ln:
                t_idx = i
        if t_idx is not None and not any(i > t_idx for i, _ in users):
            return
    # detach 스폰 — Stop 흐름을 haiku 지연(실측 ~14s)에 묶지 않는다.
    subprocess.Popen(
        [sys.executable, os.path.abspath(__file__), "--generate", tp, sid, str(size)],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
    )


def real_claude():
    """shim 을 우회한 실제 claude 바이너리 — shim 경유는 pane 의 --session-id 가
    붙어 라이브 세션과 충돌한다(shim 과 같은 CLEAN_PATH 원리)."""
    home = os.path.expanduser("~/.local/bin/claude")
    if os.access(home, os.X_OK):
        return home
    for d in os.environ.get("PATH", "").split(":"):
        if "kasaterm-shim" in d:
            continue
        c = os.path.join(d, "claude")
        if os.access(c, os.X_OK):
            return c
    return None


def generate(tp, sid, srclen):
    lockdir = os.path.join(tempfile.gettempdir(), "kasaterm-title-gen")
    os.makedirs(lockdir, exist_ok=True)
    lock = open(os.path.join(lockdir, f"{sid}.lock"), "w")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        return  # 같은 세션 생성이 이미 진행 중
    if not os.path.isfile(tp):
        return
    _, lines = read_tail(tp)
    title, users, asst = scan(lines)
    if title is not None and title.get("nameSource") != "auto":
        return  # 스폰 사이에 수동 개명이 들어왔으면 물러난다
    claude = real_claude()
    if not claude:
        return
    # 컨텍스트: 최근 실사용자 프롬프트 위주 + 마지막 응답 한 줌.
    ctx = []
    for _, t in users[-CTX_PROMPTS:]:
        ctx.append(f"[사용자] {t[:200]}")
    if asst:
        ctx.append(f"[claude] {asst[-1][1][:200]}")
    ctx_s = "\n".join(ctx)[-CTX_CHARS:]
    prompt = (
        "다음 대화 발췌를 보고 이 세션이 지금 하고 있는 작업을 한국어 6~16자 "
        "제목으로 지어줘. 제목만 한 줄로 출력, 따옴표·마침표 없이.\n\n" + ctx_s
    )
    try:
        out = subprocess.run(
            [claude, "-p", "--model", "haiku", prompt],
            capture_output=True,
            text=True,
            timeout=120,
            cwd=lockdir,  # -p 가 남기는 세션 파일을 junk 프로젝트로 격리
        ).stdout.strip()
    except Exception:
        return
    new = " ".join(out.split()).strip("\"'“”‘’ ").rstrip(".")
    if not new or len(new) > CLIP or "\n" in out.strip():
        return  # 형식 밖 출력(거부·수다)은 버린다 — 다음 턴에 재시도
    if title is not None and (title.get("customTitle") or "") == new:
        return
    rec = json.dumps(
        {
            "type": "custom-title",
            "customTitle": new,
            "sessionId": sid,
            "nameSource": "auto",
            "srcLen": int(srclen),
            "genTs": int(time.time()),
        },
        ensure_ascii=False,
    )
    with open(tp, "a", encoding="utf-8") as f:
        f.write(rec + "\n")
    # junk cwd 의 -p 세션 잔재 청소(1일 초과분).
    proj = os.path.expanduser(
        "~/.claude/projects/" + lockdir.replace("/", "-").replace(".", "-")
    )
    try:
        cutoff = time.time() - 86400
        for n in os.listdir(proj):
            p = os.path.join(proj, n)
            if os.path.isfile(p) and os.path.getmtime(p) < cutoff:
                os.unlink(p)
    except Exception:
        pass


if __name__ == "__main__":
    if len(sys.argv) >= 5 and sys.argv[1] == "--generate":
        generate(sys.argv[2], sys.argv[3], sys.argv[4])
    else:
        decide()
