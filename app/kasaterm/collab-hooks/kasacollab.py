#!/usr/bin/env python3
"""kasacollab — pane 간 협업 공유 상태 CLI (빌드 무관, 파일 기반).

board(감지)·conflict-guard(차단) 위에 얹는 '분담·대화' 층. 두 가지 공유 상태를
/tmp/kasaterm-collab/<cwd>/ 아래 파일로 관리한다:
  - tasks.json     : 작업판 — 누가 무슨 일을 맡았나(중복 분담 방지)
  - messages.jsonl : 메시지함 — pane끼리 주고받는 메모/지시/질문

매 턴 자동 주입(옛 board-context)은 폐기 — `kasacollab task list`/`inbox` 로
직접 조회한다. 모니터링은 팀장이 lead-watch 를 Monitor 에 거는 방식(아래 lead).

사용:
  kasacollab task add "BSP 레이아웃 리팩터"   # 내 pane이 이 일 맡음(선언)
  kasacollab task list                          # 전체 작업판
  kasacollab task done <id>                      # 완료
  kasacollab task drop <id>                      # 취소
  kasacollab msg %2 "이 파일 곧 끝나"           # %2에게 메시지
  kasacollab inbox                               # 내게 온 미읽 메시지(읽음 처리)
"""
import sys, os, json, time, subprocess


def base():
    enc = os.getcwd().replace("/", "-").replace(".", "-")
    d = os.path.join("/tmp/kasaterm-collab", enc)
    os.makedirs(d, exist_ok=True)
    return d


def me():
    return os.environ.get("KASATERM_PANE_ID", "?")


def tasks_path():
    return os.path.join(base(), "tasks.json")


def msgs_path():
    return os.path.join(base(), "messages.jsonl")


def load_tasks():
    try:
        return json.load(open(tasks_path()))
    except Exception:
        return []


def save_tasks(t):
    json.dump(t, open(tasks_path(), "w"), ensure_ascii=False)


def short_id():
    # time+pane 기반 4hex. hashlib.md5는 이 머신 framework python 3.14에서
    # import 자체가 행한다(OpenSSL 로드 블록) — id 생성에만 쓰던 의존을 끊어
    # 어느 python3 에서도 즉시 돌게 한다. 한 ms 안의 충돌은 pane id XOR로 분산.
    return f"{(int(time.time() * 1000) ^ hash(me())) & 0xFFFF:04x}"


def cmd_task(args):
    if not args:
        print("kasacollab task add|list|done|drop")
        return
    sub, t = args[0], load_tasks()
    if sub == "add":
        desc = " ".join(args[1:]).strip()
        if not desc:
            print("desc 필요: task add \"<무슨 일>\"")
            return
        item = {"id": short_id(), "pane": me(), "desc": desc,
                "status": "doing", "ts": time.time()}
        t.append(item)
        save_tasks(t)
        print(f"맡음 [{item['id']}] {desc}")
    elif sub == "list":
        live = [i for i in t if i.get("status") == "doing"]
        if not live:
            print("(진행 중 작업 없음)")
            return
        for i in live:
            print(f"[{i['id']}] {i['pane']}: {i['desc']}")
    elif sub in ("done", "drop"):
        if len(args) < 2:
            print(f"{sub} <id>")
            return
        tid = args[1]
        hit = False
        for i in t:
            if i["id"] == tid:
                i["status"] = "done" if sub == "done" else "dropped"
                hit = True
        save_tasks(t)
        print(f"{tid} {sub}" if hit else f"{tid} 없음")
    else:
        print("kasacollab task add|list|done|drop")


def read_msgs():
    p = msgs_path()
    if not os.path.exists(p):
        return []
    return [json.loads(l) for l in open(p).read().splitlines() if l.strip()]


def write_msgs(msgs):
    with open(msgs_path(), "w") as f:
        for m in msgs:
            f.write(json.dumps(m, ensure_ascii=False) + "\n")


def cmd_msg(args):
    if len(args) < 2:
        print('msg <pane> "<text>"')
        return
    to, text = args[0], " ".join(args[1:])
    if to == me():
        print(f"자기 자신({to})에게는 못 보냄 — 내 pane id가 {me()}다. "
              f"받는 상대 id를 board에서 다시 확인해라.")
        return
    m = {"id": short_id(), "from": me(), "to": to, "text": text,
         "ts": time.time(), "read": False}
    with open(msgs_path(), "a") as f:
        f.write(json.dumps(m, ensure_ascii=False) + "\n")
    # inbox는 상대 '다음 턴'에야 자동 주입돼 working 중이면 한참 뒤에 본다.
    # 보낸 즉시 tell 로 깨워 그 턴의 board-context hook 이 이 메시지를 바로
    # 끌어가게 한다. tell 본문은 트리거일 뿐 — 실제 내용은 inbox 주입이 싣는다.
    cli = os.environ.get("KASATERM_CLI", "kasaterm-cli")
    woke = False
    try:
        r = subprocess.run(
            [cli, "tell", to, f"[inbox] {me()} 메시지 도착 — 위 '받은 메시지' 확인하고 "
             f"필요하면 답장(kasacollab msg {me()} \"...\")"],
            capture_output=True, text=True, timeout=3)
        woke = r.returncode == 0
    except Exception:
        pass
    print(f"→ {to}: {text}" + (" · tell 로 깨움" if woke else " · (tell 실패, inbox 에만 쌓임)"))


def cmd_inbox(args):
    msgs = read_msgs()
    mine = [m for m in msgs
            if m.get("to") == me() and m.get("from") != me() and not m.get("read")]
    if not mine:
        print("(새 메시지 없음)")
        return
    for m in mine:
        print(f"{m['from']}: {m['text']}")
        m["read"] = True
    write_msgs(msgs)


def lead_path():
    return os.path.join(base(), "lead")


def cmd_lead(args):
    # 이 cwd 협업방의 '팀장' 마커. 팀장 = lead-watch를 Monitor에 걸고 다른
    # pane이 사람 입력 대기로 멈추면 대신 답하는 오케스트레이터. 한 방에
    # 한 명만(중복 답변 충돌 방지). 워커는 팀장 존재를 몰라도 되고, 팀장이
    # board pull로 멈춘 pane을 능동 감지해 대신 답한다.
    sub = args[0] if args else "set"
    if sub == "claim":
        # god 자리 원자적 선점. O_EXCL 이라 동시 경쟁 시 정확히 한 pane만 성공
        # (exit 0), 나머지는 양보(exit 1). stale lead(죽은 god) 정리는 god-elect.sh
        # 가 list surfaces 로 판정해 off 후 재호출한다 — 여기선 순수 원자 선점만.
        try:
            fd = os.open(lead_path(), os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o644)
        except FileExistsError:
            try:
                cur = open(lead_path()).read().strip()
            except OSError:
                cur = "?"
            print(f"이미 god: {cur}")
            sys.exit(1)
        with os.fdopen(fd, "w") as f:
            f.write(me())
        print(f"god 획득 = {me()}")
        return
    if sub in ("off", "clear"):
        try:
            os.remove(lead_path())
        except OSError:
            pass
        print("팀장 해제")
    elif sub == "who":
        try:
            print("팀장:", open(lead_path()).read().strip())
        except OSError:
            print("(팀장 없음)")
    else:  # set (default): 실행한 pane이 팀장이 된다
        with open(lead_path(), "w") as f:
            f.write(me())
        print(f"팀장 = {me()}. lead-watch 를 Monitor 에 persistent 로 걸어 통솔 시작:")
        print('  Monitor(command="bash ~/.claude/hooks/kasaterm-lead-watch.sh", '
              'description="사람 입력 대기 pane", persistent=true)')


def main():
    a = sys.argv[1:]
    if not a:
        print("kasacollab task|msg|inbox|lead")
        return
    {"task": cmd_task, "msg": cmd_msg, "inbox": cmd_inbox, "lead": cmd_lead}.get(
        a[0], lambda _: print("kasacollab task|msg|inbox|lead")
    )(a[1:])


if __name__ == "__main__":
    main()
