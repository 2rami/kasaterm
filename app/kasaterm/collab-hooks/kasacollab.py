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
import sys, os, json, time, hashlib


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
    return hashlib.md5(f"{time.time()}{me()}".encode()).hexdigest()[:4]


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
    m = {"id": short_id(), "from": me(), "to": to, "text": text,
         "ts": time.time(), "read": False}
    with open(msgs_path(), "a") as f:
        f.write(json.dumps(m, ensure_ascii=False) + "\n")
    print(f"→ {to}: {text}")


def cmd_inbox(args):
    msgs = read_msgs()
    mine = [m for m in msgs if m.get("to") == me() and not m.get("read")]
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
        print(f"팀장 = {me()}. 이제 lead-watch를 Monitor에 persistent로 걸어 통솔을 시작하라:")
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
