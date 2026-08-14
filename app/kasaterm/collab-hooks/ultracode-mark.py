#!/usr/bin/env python3
"""UserPromptSubmit 훅 — 이 세션이 ultracode 인지 마커로 남긴다.

claude 는 ultracode 를 **어디에도 저장하지 않는다**. 바이너리 문자열이 못을 박아 뒀다 —
"interactive toggles never persist it". settings.json 의 effortLevel 도, statusline
payload 의 effort 도, board 의 effort_default 도 ultracode 를 켠 순간 xhigh 그대로다.
디스크에 남는 유일한 흔적이 transcript 의 attachment 레코드 한 줄이라, 그걸 본다.

판정은 두 갈래의 OR 다:
  ① 프롬프트 본문에 "ultracode" — 예전부터 있던 턴 단위 신호. 그대로 둔다.
  ② transcript 의 마지막 effort 마커 — `/effort` 로 켜는 세션 단위 신호.
②가 없던 시절엔 `/effort` 로 켜면 프롬프트에 그 글자가 없어 아예 안 켜졌고, 켜져 있어도
다음 프롬프트에서 지워졌다.

②는 세 가지 니들을 마지막 위치가 이기는(last-wins) 방식으로 본다:
  "type":"ultra_effort_enter"                              → ON
  "type":"ultra_effort_exit"                               → OFF
  "content":"<local-command-stdout>Set effort level to …"  → ultracode 면 ON, 아니면 OFF
셋 다 **따옴표를 이스케이프하지 않은 형태**라 산문에는 못 나온다 — 메시지 본문은 전부 JSON
문자열이라 `"` 가 `\\"` 로 박힌다. 낱말 `ultracode` 로 찾으면 안 되는 이유가 이것이다(이 기능을
만든 세션 transcript 에만 그 낱말이 534번 나온다).

★ 프로세스 시작 시각으로 자르는 게 핵심이다. ultracode 는 프로세스를 못 넘는데
(`this session only`) `claude --resume` 은 **같은 jsonl 에 이어 쓴다**. 게다가 재개 시
exit 가 안 찍히기도 한다 — 실측(8cd6f2a2): 재개 뒤 227 레코드를 쌓는 동안 enter 도 exit 도
없었고 마지막 마커는 재개 9시간 전의 enter 였다. 그래서 마커 timestamp 가 그 세션을 돌리는
claude 의 startedAt(~/.claude/sessions/<pid>.json) 보다 앞서면 유령으로 보고 버린다.

IO: 훅은 프롬프트마다 도니 통째로 읽지 않는다. 처음 한 번만 EOF 에서 1MB 씩 거슬러 올라가
첫 마커에서 멈추고(실측 5~56ms), 그 뒤로는 직전에 본 크기 이후 **새로 붙은 바이트만** 본다
(수 KB, 1ms 미만). 캐시는 .state/<sid>.json 이며 inode·startedAt 이 바뀌면 버린다.
"""
import calendar
import json
import os
import pathlib
import re
import subprocess
import sys

MARK_DIR = pathlib.Path(os.environ.get("KASATERM_ULTRACODE_DIR") or "/tmp/kasaterm-collab/ultracode")
STATE_DIR = MARK_DIR / ".state"
SESSIONS_DIR = pathlib.Path(os.path.expanduser("~/.claude/sessions"))
CACHE_V = 2  # argv 기준선 도입(2026-08-15) — 옛 캐시의 on=False 를 그대로 믿으면 안 된다

CHUNK = 1 << 20
OVERLAP = 512  # 청크 경계에 걸친 니들을 양쪽에서 다 보게 하는 여유
MAX_SCAN = 16 << 20  # 여기까지 거슬러도 마커가 없으면 OFF. 세션당 한 번만 내는 비용이다
LINE_TAIL = 8192  # 마커 줄에서 timestamp 까지 읽을 만큼

ENTER = b'"type":"ultra_effort_enter"'
EXIT = b'"type":"ultra_effort_exit"'
CMD = b'"content":"<local-command-stdout>Set effort level to '
NEEDLES = ((ENTER, True), (EXIT, False), (CMD, None))
TS_RE = re.compile(rb'"timestamp":"(\d{4})-(\d\d)-(\d\d)T(\d\d):(\d\d):(\d\d)')


def _proc_start(sid):
    """이 sid 를 돌리는 살아 있는 claude 의 (시작 시각 epoch 초, argv 에
    `--effort ultracode` 가 있나). 못 찾으면 (0, False).

    argv 를 보는 이유: kasaterm 세션 복원은 ultracode 였던 pane 을
    `claude --resume … --effort ultracode` 로 되살리는데, 플래그 launch 는
    transcript 에 enter attachment 를 **안 남긴다**(2026-08-15 실측 — print 로
    켜고 뒤져도 0건). 옛 enter 마커는 아래 시작-시각 게이트가 유령으로
    버리므로, argv 에서 기준선을 얻지 않으면 재개 첫 프롬프트에서 마커가
    지워지고 그다음 재시작은 xhigh 로 풀린다.
    """
    best = 0.0
    ultra = False
    try:
        names = os.listdir(SESSIONS_DIR)
    except OSError:
        return best, ultra
    for name in names:
        if not name.endswith(".json"):
            continue
        try:
            pid = int(name[:-5])
        except ValueError:
            continue
        try:
            with open(SESSIONS_DIR / name, "rb") as f:
                rec = json.load(f)
        except (OSError, ValueError):
            continue
        if rec.get("sessionId") != sid:
            continue
        started = rec.get("startedAt")
        if not isinstance(started, (int, float)):
            continue
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            continue  # 죽은 pid 의 명부가 남아 살아 있는 것보다 새 시각을 주면 오판한다
        except OSError:
            pass
        best = max(best, started / 1000.0)
        try:
            args = subprocess.run(
                ["ps", "-o", "args=", "-p", str(pid)],
                capture_output=True,
                text=True,
                timeout=2,
            ).stdout
        except Exception:
            args = ""
        if "--effort ultracode" in args:
            ultra = True
    return best, ultra


def _pick(buf, base):
    """버퍼 안에서 **가장 뒤에 있는** 니들. (절대오프셋, ON여부 또는 None)."""
    best, kind = -1, None
    for needle, verdict in NEEDLES:
        i = buf.rfind(needle)
        if i > best:
            best, kind = i, verdict
    return (base + best, kind) if best >= 0 else None


def _read_marker(f, hit, kind):
    """마커가 든 줄을 통째로 떠서 (ON여부, epoch) 로 만든다.

    timestamp 는 레코드 안 어디에 있어도 되게 니들 양옆을 다 읽는다 — 한쪽만 읽으면
    필드 순서가 바뀌는 순간 게이트가 조용히 무력화된다.
    """
    lo = max(0, hit - LINE_TAIL)
    f.seek(lo)
    win = f.read((hit - lo) + LINE_TAIL)
    at = hit - lo
    line = win[win.rfind(b"\n", 0, at) + 1:]
    cut = line.find(b"\n")
    if cut >= 0:
        line = line[:cut]
    if kind is None:
        i = line.find(CMD)
        kind = i >= 0 and line[i + len(CMD):].startswith(b"ultracode")
    m = TS_RE.search(line)
    if not m:
        return bool(kind), 0.0
    y, mo, d, h, mi, s = (int(x) for x in m.groups())
    return bool(kind), float(calendar.timegm((y, mo, d, h, mi, s, 0, 0, 0)))


def _scan_back(f, size):
    end, read = size, 0
    while end > 0 and read < MAX_SCAN:
        start = max(0, end - CHUNK)
        f.seek(start)
        buf = f.read(min(size, end + OVERLAP) - start)
        if not buf:
            break
        read += len(buf)
        hit = _pick(buf, start)
        if hit:
            return hit
        end = start
    return None


def _scan_forward(f, frm, size):
    found = None
    pos = max(0, frm)
    while pos < size:
        f.seek(pos)
        buf = f.read(CHUNK)
        if not buf:
            break
        hit = _pick(buf, pos)
        if hit:
            found = hit
        if len(buf) < CHUNK:
            break
        pos += len(buf) - OVERLAP
    return found


def _session_on(tpath, sid, safe):
    try:
        st = os.stat(tpath)
    except OSError:
        return False
    started, argv_ultra = _proc_start(sid)
    cache = STATE_DIR / f"{safe}.json"
    prev = None
    try:
        with open(cache, "rb") as f:
            prev = json.load(f)
    except (OSError, ValueError):
        prev = None
    warm = (
        isinstance(prev, dict)
        and prev.get("v") == CACHE_V
        and prev.get("ino") == st.st_ino
        and prev.get("start") == started
        and isinstance(prev.get("size"), int)
        and 0 < prev["size"] <= st.st_size
    )
    with open(tpath, "rb") as f:
        if warm:
            on = bool(prev.get("on"))
            hit = _scan_forward(f, prev["size"] - OVERLAP, st.st_size)
        else:
            # 마커가 하나도 없을 때의 기준선 — 이 프로세스가 `--effort ultracode`
            # 로 떴다면 시작 시점 상태는 ON 이다(kasaterm 복원 경로).
            on = argv_ultra
            hit = _scan_back(f, st.st_size)
        if hit:
            kind, ts = _read_marker(f, hit[0], hit[1])
            # 유령(이 프로세스 시작 전 마커)은 판정을 못 바꾼다 — 기준선/캐시 유지.
            # 이 프로세스 안에서 찍힌 마커만 last-wins 로 상태를 덮는다.
            if not (started and ts and ts < started):
                on = kind
    try:
        STATE_DIR.mkdir(parents=True, exist_ok=True)
        tmp = cache.with_suffix(".tmp")
        tmp.write_text(
            json.dumps({"v": CACHE_V, "ino": st.st_ino, "start": started, "size": st.st_size, "on": on})
        )
        os.replace(tmp, cache)
    except OSError:
        pass
    return on


def main() -> None:
    try:
        d = json.load(sys.stdin)
    except Exception:
        return
    # title-sync 가 제목을 지으려고 띄우는 claude(-p, cwd=$TMPDIR/kasaterm-title-gen)는
    # **대화 전문**을 프롬프트로 받는다. 그 안에 "ultracode" 글자가 한 번이라도 있으면
    # 남의 턴을 자기 세션 마커로 찍고, 그 세션은 다음 턴이 없어 지울 기회조차 없다 —
    # /tmp 에 영영 쌓인다(2026-08-11 실측: 남아 있던 마커 4개가 전부 이것이었다).
    # 같은 가드를 kasaterm-bind-transcript.sh·sessions.rs 가 이미 갖고 있다.
    junk = str(d.get("transcript_path") or "") + str(d.get("cwd") or "") + os.getcwd()
    if "kasaterm-title-gen" in junk:
        return

    sid = d.get("session_id")
    if not isinstance(sid, str) or not sid:
        return
    # 경로 조작 방지 — session_id 는 uuid 지만 남이 준 값이므로 그대로 믿지 않는다.
    safe = "".join(c for c in sid if c.isalnum() or c in "-_")
    if not safe:
        return

    prompt = d.get("prompt")
    on = isinstance(prompt, str) and "ultracode" in prompt.lower()
    if not on:
        tpath = d.get("transcript_path")
        if isinstance(tpath, str) and tpath:
            try:
                on = _session_on(tpath, sid, safe)
            except Exception:
                on = False  # 마커는 표시용이라 어떤 실패도 턴을 막지 않는다

    path = MARK_DIR / f"{safe}.on"
    try:
        if on:
            MARK_DIR.mkdir(parents=True, exist_ok=True)
            path.write_text("1")
        else:
            path.unlink(missing_ok=True)
    except OSError:
        pass


if __name__ == "__main__":
    main()
