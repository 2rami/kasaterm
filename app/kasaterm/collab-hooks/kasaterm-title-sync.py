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
#   1. 수동 개명 보호 — **우리가 지은 것(nameSource "auto")만 갈아치운다.** 사람이
#      붙인 이름은 표시를 안 남길 수 있는데(claude 의 `/rename` 이 그렇다), 그걸
#      갱신 대상으로 보면 한 턴 뒤 조용히 덮여 「이름을 붙였는데 안 붙는다」가 된다
#      (2026-08-26 지적). 한동안 "user 만 보호"였는데, 그 표시를 남기는 통로가
#      kasaterm-cli rename 하나뿐이라 `/rename` 이 통째로 무력했다.
#      ⚠️ 이 규칙의 대가: 사람이 한 번 이름을 붙이면 주제가 바뀌어도 자동 갱신이
#      다시 손대지 않는다. 그게 「손으로 붙인 이름」의 뜻이므로 의도된 것이다.
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
CTX_CHARS = 2000
# 전 머신 동시 생성 상한. 세션당 flock 만으로는 못 막는다 — 락이 sid 별이라
# 세션이 200개면 락도 200개고 전부 동시에 통과한다(2026-08-05 실사고: 벤치마크가
# 만든 junk 세션 수백 개가 저마다 제목 생성을 띄워 로드 348, 코어 14). 여기서
# 막히면 그냥 물러난다 — 다음 Stop 턴에 어차피 다시 시도한다.
MAX_CONCURRENT = 2


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


def lockdir_path():
    return os.path.join(tempfile.gettempdir(), "kasaterm-title-gen")


def decide():
    try:
        payload = json.load(sys.stdin)
    except Exception:
        return
    tp = payload.get("transcript_path") or ""
    sid = payload.get("session_id") or ""
    if not tp or not sid or not os.path.isfile(tp):
        return
    # 제 꼬리를 물지 않는다. 우리가 띄운 headless claude 는 그 자체가 세션이고,
    # claude 는 **그 세션에도** 제목을 지어주려 또 하나를 띄운다. 그 손자는 shim
    # settings 를 물려받아 Stop 훅이 발화하고, 그러면 여기로 돌아와 또 스폰한다.
    # 매번 새 sid 라 60초 스로틀도 세션당 flock 도 하나도 안 걸린다 — 5초에
    # 한 개씩 무한증식했다(2026-08-05: 로드 348, MCP 47개, 앱 재시작으로도 안 죽음.
    # detach 라 앱 수명 밖이다). 우리 세션은 cwd 가 junk 락디렉터리라 transcript
    # 프로젝트 경로에 그 이름이 박힌다 — 그게 유일하게 믿을 만한 표식이다.
    if os.path.basename(lockdir_path()) in tp.replace(os.sep, "-"):
        return
    size, lines = read_tail(tp)
    title, users, _ = scan(lines)
    if not users:
        return
    if title is not None:
        if title.get("nameSource") != "auto":
            return  # **우리가 지은 것(auto)만 갈아치운다.** 사람이 붙인 이름은
            # `/rename` 이든 무엇이든 표시를 안 남길 수 있는데, 그때 갱신 대상으로
            # 보면 한 턴 뒤 조용히 덮인다 — 이름을 붙였는데 안 붙는 것처럼 보이던
            # 원인이다(2026-08-26 지적). 판정을 뒤집어 「auto 면 갱신」으로 못 박는다.
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
    lockdir = lockdir_path()
    os.makedirs(lockdir, exist_ok=True)
    lock = open(os.path.join(lockdir, f"{sid}.lock"), "w")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        return  # 같은 세션 생성이 이미 진행 중
    # 슬롯을 하나 못 잡으면 물러난다. 변수에 붙들어 둬야 한다 — 파일 객체가
    # GC 되면 fd 가 닫히며 flock 도 풀려, 상한이 있으나 마나가 된다.
    slot = None
    for i in range(MAX_CONCURRENT):
        f = open(os.path.join(lockdir, f"slot-{i}.lock"), "w")
        try:
            fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
            slot = f
            break
        except OSError:
            f.close()
    if slot is None:
        return
    if not os.path.isfile(tp):
        return
    _, lines = read_tail(tp)
    title, users, asst = scan(lines)
    if title is not None and title.get("nameSource") != "auto":
        return  # 스폰 사이에 사람이 붙인 이름이 들어왔으면 물러난다(위와 같은 규칙)
    claude = real_claude()
    if not claude:
        return
    # 전체 대화 흐름(오래된→최근 user 프롬프트)을 맥락으로 주고, 마지막 프롬프트를 따로
    # 강조해 "지금 하는 작업"을 콕 집어 제목으로 만든다. 최근 몇 개만 주면 맥락이 없어
    # 엉뚱하게 요약됐다(거노: 그지같이 요약). 흐름은 짧게 많이, 지금 프롬프트는 길게.
    # asst 응답은 안 넣는다 — 제목의 기준은 사용자 의도.
    flow = [t.replace("\n", " ").strip()[:70] for _, t in users if t.strip()]
    recent = (users[-1][1].replace("\n", " ").strip()[:350]) if users else ""
    flow_s = "\n".join(f"- {t}" for t in flow[-15:])
    ctx_s = f"[대화 흐름 · 오래된 순]\n{flow_s}\n\n[지금 프롬프트]\n{recent}"[-CTX_CHARS:]
    prompt = (
        "아래는 한 개발 작업 세션의 사용자 프롬프트들이야. [대화 흐름]으로 전체 맥락을 "
        "파악한 뒤, [지금 프롬프트]가 가리키는 '지금 하고 있는 작업'을 한국어 제목으로 "
        "지어줘. 세션 전체의 큰 주제가 아니라 가장 최근에 착수한 구체적 작업을 콕 집어서. "
        "6~18자, 제목만 한 줄, 따옴표·마침표·번호 없이.\n\n" + ctx_s
    )
    # KASATERM_* 를 제거한 env 로 실행 — 안 그러면 이 headless claude(및 그 내부
    # 제목생성 서브세션)가 부모 pane 의 KASATERM_PANE_ID 를 물려받아 SessionStart
    # bind-transcript hook 이 발화, pane_claude_sid 를 이 title-gen 세션으로 덮어써
    # 입력박스 인레이에 "아래 대화의 주제를…" 메타프롬프트가 유출된다(거노 실측).
    # PANE_ID 가 없으면 hook 이 첫 줄에서 즉시 no-op.
    clean_env = {k: v for k, v in os.environ.items() if not k.startswith("KASATERM_")}
    # `--strict-mcp-config` 없이 부르면 이 한 줄짜리 제목 생성이 사용자의 MCP
    # 스택을 통째로 띄운다 — exa·playwright·context7 이 매번 node 프로세스로
    # 붙어 제목 하나에 4프로세스가 됐다(2026-08-05: 고아 MCP 43개). 제목 짓는 데
    # 툴은 하나도 안 쓴다. `--mcp-config` 를 안 준 채로 strict 면 서버 0개다.
    try:
        out = subprocess.run(
            [claude, "-p", "--strict-mcp-config", "--model", "haiku", prompt],
            capture_output=True,
            text=True,
            timeout=60,
            cwd=lockdir,  # -p 가 남기는 세션 파일을 junk 프로젝트로 격리
            env=clean_env,
        ).stdout.strip()
    except Exception:
        return
    new = " ".join(out.split()).strip("\"'“”‘’ ").rstrip(".")
    if not new or len(new) > CLIP or "\n" in out.strip():
        return  # 형식 밖 출력(거부·수다)은 버린다 — 다음 턴에 재시도
    # 발췌가 부실하면 haiku 가 제목 대신 거부/설명을 뱉는다("대화 발췌가 제공되지
    # 않았어요…"). CLIP 을 통과하는 짧은 거부문도 있어 시그니처로 한 번 더 거른다.
    if new.startswith("대화 발췌") or "제공되지 않" in new:
        return
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
    # Windows 는 드라이브 콜론·역슬래시까지 같이 접힌다(`C:\\Users\\x` →
    # `C--Users-x`, 실측). `/`·`.` 만 접으면 없는 폴더라 청소가 무음 정지한다.
    # 추가 두 글자는 nt 에서만 — unix 폴더 이름엔 실제로 들어갈 수 있다.
    slug = lockdir
    for ch in ("/", ".", "\\", ":") if os.name == "nt" else ("/", "."):
        slug = slug.replace(ch, "-")
    proj = os.path.expanduser("~/.claude/projects/" + slug)
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
