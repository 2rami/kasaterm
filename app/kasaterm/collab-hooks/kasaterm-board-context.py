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
import sys, os, json, subprocess, time, hashlib, glob

# inbox 읽음 처리는 kasacollab 의 공용 drain_unread()를 쓴다 — 락+atomic 로
# lost-update 를 막는 단일 임계구역을 두 파일이 공유해야 동작이 일치한다(인라인
# 복사는 락 없는 옛 재작성이라 마킹 유실의 원인이었다). import-safe(main 가드).
# .pyc 생성 금지 — 이 파일은 서명된 .app 번들 Resources 에서 돌므로 import 가
# __pycache__ 를 번들 안에 쓰면 codesign seal 이 깨진다(실측).
sys.dont_write_bytecode = True
_HD = os.path.dirname(os.path.abspath(__file__))
if _HD not in sys.path:
    sys.path.insert(0, _HD)
try:
    import kasacollab
except Exception:
    kasacollab = None

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
    """내게 온 미읽 메시지. 읽음 처리(턴에 실으면 본 것)는 kasacollab 의 공용
    drain_unread()(락+atomic)에 위임 — 동시 재작성에 마킹이 유실되던 race 해소.
    없으면 None."""
    if kasacollab is None:
        return None
    mine = kasacollab.drain_unread()
    if not mine:
        return None
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


def _live_surface_ids():
    """현재 board 에 떠 있는 pane id 집합. roster 의 죽은 세션 판정에 쓴다."""
    cli = os.environ.get("KASATERM_CLI", "kasaterm-cli")
    try:
        r = subprocess.run([cli, "board"], capture_output=True, text=True, timeout=3)
        board = (json.loads(r.stdout).get("result") or {}).get("board") or []
        return {row.get("surface_id") for row in board if row.get("surface_id")}
    except Exception:
        return set()


def _surface_pane_ids():
    """list surfaces 의 pane id 집합 — board(bind+프롬프트 필요)와 달리 pane
    존재 자체를 보므로 사각지대가 좁다. bound 마커 생존 판정용."""
    cli = os.environ.get("KASATERM_CLI", "kasaterm-cli")
    try:
        r = subprocess.run([cli, "list", "surfaces"], capture_output=True, text=True, timeout=3)
        surfs = (json.loads(r.stdout).get("result") or {}).get("surfaces") or []
        return {s.get("id") for s in surfs if s.get("id")}
    except Exception:
        return set()


def _bound_live_sids(surface_ids, marker_glob="/tmp/kasaterm-bound-*"):
    """bind 마커가 가리키는 sid 중 그 pane 이 지금 살아있는 것 — god-elect.sh 의
    god 생존 판정과 같은 규칙. 마커는 bind 직후 생기므로 roster upsert·board
    등록(첫 프롬프트 후)보다 이른 신호다. 마커 파일명 ↔ pane 복원은 bind 쪽
    치환(% → _)의 역."""
    sids = set()
    for bm in glob.glob(marker_glob):
        bp = os.path.basename(bm).split("kasaterm-bound-", 1)[-1]
        pane = "%" + (bp[1:] if bp.startswith("_") else bp)
        if pane not in surface_ids:
            continue
        try:
            tp = open(bm).read().split(":", 1)[-1].strip()
        except OSError:
            continue
        sid = os.path.splitext(os.path.basename(tp))[0]
        if sid:
            sids.add(sid)
    return sids


def _recovery_candidates(roster, live, bound_sids):
    """복구 후보 필터(순수 함수 — 시뮬 검증용으로 IO 와 분리).
    live pane 이 잡은 sid + bound 마커로 살아있다고 확인된 sid 는 제외 —
    같은 세션을 pane 두 개에 이중 attach 하는 사고 방지. roster 가드만으로는
    rebind upsert 전(첫 프롬프트 전)의 산 세션을 못 거른다(06-10 실측:
    1f2685f3 가 %1 에 떠 있는데 옛 위치 %3 엔트리가 후보로 떠 이중 resume)."""
    live_sids = {v.get("session_id") for v in roster.values()
                 if v.get("pane_id") in live} | bound_sids
    return [v for v in roster.values()
            if v.get("pane_id") not in live and v.get("session_id") not in live_sids]


def _rel_age(ts):
    s = max(0, int(time.time() - ts))
    if s < 3600:
        return f"{s // 60}분 전"
    if s < 86400:
        return f"{s // 3600}시간 전"
    return f"{s // 86400}일 전"


def roster_recovery():
    """god 전용: roster(영속)에 기록된 과거 에이전트 중 지금 board 에 안 떠 있는
    세션을 복구 후보로 나열. 재시작하면 워커 pane 이 다 사라지고 god 만 남는데,
    이 주입을 보고 god 이 `claude --resume` 로 워커들을 부활시킨다.
    pane_id 기준 live 판정(드물게 pane_id 재사용 시 누락 가능 — 정확 매칭은
    board 에 session_id 노출 후속)."""
    slug = os.getcwd().replace("/", "-").replace(".", "-")
    p = os.path.expanduser(f"~/.config/kasaterm/agent-roster/{slug}.json")
    try:
        roster = json.load(open(p))
    except Exception:
        return None
    if not isinstance(roster, dict) or not roster:
        return None
    live = _live_surface_ids()
    dead = _recovery_candidates(roster, live, _bound_live_sids(_surface_pane_ids()))
    # board(bind 기반) 사각지대 보완: 방금 `claude --resume <sid>` 로 부활했지만
    # 아직 프롬프트를 안 받아 bind 가 안 돈 세션은 board 에 없어 '죽음'으로
    # 오판된다 — 실행 중 claude 프로세스의 cmdline 에 sid 가 보이면 산 것.
    if dead:
        try:
            r = subprocess.run(["pgrep", "-fl", "claude.*--resume"],
                               capture_output=True, text=True, timeout=3)
            dead = [v for v in dead if v.get("session_id", "\0") not in r.stdout]
        except Exception:
            pass
    if not dead:
        return None
    dead.sort(key=lambda v: v.get("ts", 0), reverse=True)
    lines = ["[복구 가능 에이전트] 이 방에서 돌던 세션 중 지금 안 떠 있는 것 — "
             "워커를 부활시키려면 새 pane 띄우고 그 세션을 resume:"]
    for v in dead[:8]:
        lines.append(f"  session {v.get('session_id','?')} "
                     f"(마지막 {v.get('pane_id','?')}, {_rel_age(v.get('ts',0))})")
    lines.append("  복구 절차: `kasaterm-cli split right` (출력의 새 pane id 확인) "
                 "→ `kasaterm-cli tell <새id> \"claude --resume <session_id>\"`. "
                 "부활한 pane 이 다시 bind 되면 roster 가 자동 갱신된다.")
    return "\n".join(lines)


def god_section():
    """협업 규약 — 모드별. 기본 solo(팀장 없음, 거노 직접 오케스트레이션):
    커밋은 각자(자기 작업 파일 명시), 승인은 사용자 직행, 파일 겹침은
    conflict-guard 가 차단. god(옵트인): lead 파일로 god 파악 — 내가 god 이면
    커밋 책임+변경점 종합, 워커면 커밋 금지(god 에게 done). 복구 후보(roster)는
    **모드 무관 유지** — 재시작 후 워커 부활은 어느 모드든 필요하다."""
    mode = kasacollab.current_mode() if kasacollab else "solo"
    if mode != "god":
        recovery = roster_recovery()
        solo = ("[solo 모드] 협업방에 팀장 없음. 커밋은 각자 — git add 는 자기 작업 "
                "파일을 명시 나열한다(-A/-u 금지, 다른 pane WIP 섞임 방지). 막힘/승인은 "
                "사용자에게 직행. 파일 겹침은 conflict-guard 가 차단한다.")
        return solo + (("\n" + recovery) if recovery else "")
    try:
        god = open(os.path.join(collab_dir(), "lead")).read().strip()
    except OSError:
        god = ""
    if not god:
        return roster_recovery()
    if god == me:
        base = ("[god 역할] 너 = god. 워커가 'done:' 보고하면 변경을 검토하고 너가 "
                "단독으로 git add/commit/push 한다(워커는 커밋 안 함). 부하가 많으면 "
                "split 로 워커를 더 띄워 위임한다.")
        digest = god_fleet_digest()
        recovery = roster_recovery()
        return base + (("\n" + digest) if digest else "") + (("\n" + recovery) if recovery else "")
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
