#!/usr/bin/env python3
"""UserPromptSubmit hook: 매 턴 시작 시 board(다른 pane 활동) + inbox(내게 온
메시지)를 프롬프트에 주입.

모든 pane 이 자기 턴에 "다른 pane 이 뭐 하나"(board) 와 "나한테 온 메시지"(inbox)를
자동으로 본다(pull). 따로 Monitor·조회 없이 claude 턴이라는 자연스러운 시점에
한 번 당겨 컨텍스트로 넣는다. 둘 다 없으면(혼자+메시지 없음) 조용. pane 밖이면 no-op.

- board: `kasaterm-cli board` = 호출 시점 pull. 제목(ai-title)·상태·시킨 일만 간결히.
- inbox: kasacollab 의 messages.jsonl 에서 to==나·미읽을 띄우고 읽음 처리(본 것).
  답장은 claude 가 `kasacollab msg <상대> "..."` 로.
- diff 주입: board/god 섹션은 안정 키(pane 구성·제목·시킴·git 변경·god 신원)가 직전
  주입과 같으면 스킵 — 대화 이력에 이미 있어 매 턴 반복은 토큰 낭비다(munder-difflin
  의 "additionalContext 는 개입 전용" 절약 차용). status(working/idle)는 매 턴
  팔랑거려 키에서 제외. 컨텍스트 압축에 유실될 수 있어 30분마다 무조건 재주입.
  inbox 는 원래 one-shot(미읽만)이라 항상 주입.
"""
import sys, os, json, subprocess, time, hashlib

me = os.environ.get("KASATERM_PANE_ID")
if not me:
    sys.exit(0)
try:
    sys.stdin.read()  # payload 소비
except Exception:
    pass

# god 선출/표시 자가치유 — 매 턴 백그라운드 fire-and-forget. 이 hook 의 stdout
# (board/inbox 주입 JSON)은 건드리지 않는다. god-elect 는 pane 2개+ 일 때만 동작.
try:
    _hd = os.path.dirname(os.path.abspath(__file__))
    subprocess.Popen(["bash", os.path.join(_hd, "god-elect.sh")],
                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
except Exception:
    pass


def collab_dir():
    enc = os.getcwd().replace("/", "-").replace(".", "-")
    return os.path.join("/tmp/kasaterm-collab", enc)


def board_section():
    """다른 pane 활동 (렌더 텍스트, 안정 키). 형제 없으면 (None, "").
    안정 키는 status 를 뺀 구성·제목·시킴 — diff 주입 판정용."""
    cli = os.environ.get("KASATERM_CLI", "kasaterm-cli")
    try:
        out = subprocess.run([cli, "board"], capture_output=True, text=True, timeout=3).stdout
        board = json.loads(out)["result"]["board"]
    except Exception:
        return None, ""
    sibs = [p for p in board if p.get("surface_id") != me]
    if not sibs:
        return None, ""
    lines, stable = [], []
    for p in sorted(sibs, key=lambda x: x.get("surface_id", "")):
        sid = p.get("surface_id", "?")
        st = p.get("status", "")
        title = (p.get("title") or "").strip() or "(제목 없음)"
        prompt = (p.get("last_prompt") or "").strip()
        line = f"  {sid} [{st}] {title}"
        if prompt:
            line += f" — 시킴: {prompt[:60]}"
        lines.append(line)
        stable.append(f"{sid}|{title}|{prompt[:60]}")
    return (f"[협업 보드] 너 = {me}. 같은 레포를 동시에 만지는 다른 pane:\n"
            + "\n".join(lines)), "\n".join(stable)


def inbox_section():
    """내게 온 미읽 메시지. 띄우면서 읽음 처리(턴에 실으면 본 것). 없으면 None."""
    p = os.path.join(collab_dir(), "messages.jsonl")
    if not os.path.exists(p):
        return None
    try:
        msgs = [json.loads(l) for l in open(p).read().splitlines() if l.strip()]
    except Exception:
        return None
    mine = [m for m in msgs
            if m.get("to") == me and m.get("from") != me and not m.get("read")]
    if not mine:
        return None
    for m in mine:
        m["read"] = True
    try:
        with open(p, "w") as f:
            for m in msgs:
                f.write(json.dumps(m, ensure_ascii=False) + "\n")
    except OSError:
        pass
    lines = [f"  {m.get('from', '?')}: {m.get('text', '')}" for m in mine]
    return ("[받은 메시지] 나한테 온 말 (답장: kasacollab msg <상대> \"...\"):\n"
            + "\n".join(lines))


def god_fleet_digest():
    """god 전용 변경점 종합 — 살아있는 pane 수 + 미커밋 변경(git status).
    god 이 '누가 뭘 바꿨고 아직 커밋 안 됐나'를 매 턴 본다(P2 변경점 추적)."""
    out = []
    cli = os.environ.get("KASATERM_CLI", "kasaterm-cli")
    try:
        r = subprocess.run([cli, "list", "surfaces"], capture_output=True, text=True, timeout=3)
        n = len(json.loads(r.stdout)["result"]["surfaces"])
        out.append(f"  pane {n}개(너 god 포함)")
    except Exception:
        pass
    try:
        r = subprocess.run(["git", "status", "--short"], capture_output=True, text=True, timeout=3)
        lines = [l for l in r.stdout.splitlines() if l.strip()]
        if lines:
            out.append(f"  미커밋 변경 {len(lines)}개 — done 받으면 너가 커밋:")
            out += [f"    {l}" for l in lines[:12]]
        else:
            out.append("  워킹트리 깨끗(미커밋 없음)")
    except Exception:
        pass
    return "\n".join(out) if out else None


def god_section():
    """god 체제 규약. lead 파일로 god 파악 — 내가 god 이면 커밋 책임 + 변경점
    종합, 워커면 커밋 금지(god 에게 done 보고). god 없으면(혼자/미선출) None."""
    try:
        god = open(os.path.join(collab_dir(), "lead")).read().strip()
    except OSError:
        return None
    if not god:
        return None
    if god == me:
        base = ("[god 역할] 너 = god. 워커가 'done:' 보고하면 변경을 검토하고 너가 "
                "단독으로 git add/commit/push 한다(워커는 커밋 안 함). 부하가 많으면 "
                "split 로 워커를 더 띄워 위임한다.")
        digest = god_fleet_digest()
        return base + (("\n" + digest) if digest else "")
    return (f"[god 체제] god = {god}. 너는 워커 — 직접 git commit/push 하지 마라. "
            f"작업이 끝나면 `kasacollab msg {god} \"done: <요약> | files: a,b\"` 로 "
            f"보고하면 god 이 검토 후 단독 커밋한다.")


REINJECT_SECS = 1800  # 변화 없어도 30분마다 재주입(컨텍스트 압축 유실 대비)


def ctx_cache_path():
    return os.path.join(collab_dir(), f"ctx-cache-{me.lstrip('%')}")


def ambient_changed(stable_key):
    """board/god 안정 키가 직전 주입과 다르거나 TTL 지났으면 True + 캐시 갱신."""
    h = hashlib.sha1(stable_key.encode()).hexdigest()
    p = ctx_cache_path()
    try:
        prev = json.load(open(p))
        if prev.get("hash") == h and time.time() - prev.get("ts", 0) < REINJECT_SECS:
            return False
    except Exception:
        pass
    try:
        json.dump({"hash": h, "ts": time.time()}, open(p, "w"))
    except OSError:
        pass
    return True


god = god_section()
board, board_stable = board_section()
inbox = inbox_section()

# board/god = 상황인지(ambient) — 변화 없으면 스킵. inbox = 신호 — 항상 주입.
ambient = [s for s in (god, board) if s]
if ambient and not ambient_changed((god or "") + " " + board_stable):
    ambient = []

parts = ambient + ([inbox] if inbox else [])
if not parts:
    sys.exit(0)

ctx = "\n".join(parts)
if ambient:
    ctx += (f"\n(협업 규약: 너 = {me} 다 — board/inbox 에 뜬 다른 id 가 상대다(자기 자신에겐 "
            "못 보낸다). ① 대화·조율은 `kasacollab msg %N \"...\"` — 메시지를 상대 inbox 에 쌓고 "
            "그 즉시 tell 로 깨운다. board·inbox 는 자동 주입이라 상대가 자기 턴에 바로 본다"
            "(모니터링 불필요, 변화 없으면 생략될 수 있음 — 최신 확인: kasaterm-cli board). "
            "② `kasaterm-cli tell %N \"...\"` 단독은 inbox 없이 그냥 깨우거나 "
            "즉시 행동시킬 때 — 강제 제출이라 바쁜 상대 입력창엔 누적된다. 겹치면 피하거나 합류. "
            "자세히: kasaterm-cli transcript %N / peek %N.)")
print(json.dumps({
    "hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": ctx}
}))
sys.exit(0)
