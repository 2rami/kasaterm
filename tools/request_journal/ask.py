"""펫 머리 위 유리 바가 묻는 질문 — 지금 보는 창이든 모든 기기든, 판을 근거로 답하고 시키면 움직인다.

`/api/chat` 은 시킨 일의 장부를 검산하는 느린 길이고 모델 출력을 실행하지 않는다.
이 길은 반대다 — 판(`board --all`)과 포커스한 pane 의 화면을 실어 몇 초 안에 답하고,
「이 창 미니로 이사해줘」 같은 말은 kasaterm-cli 로 실제로 옮긴다(2026-09-14 지시).
실행은 모델이 아래 도구를 골랐을 때만, 그것도 이 목록의 것만이다. 화면 글자는 그대로
모델에 가지만 디스크에는 남기지 않는다.

답은 펫 말풍선에 그대로 뜬다(2026-09-17 지시 「답변이 채팅창 밖으로 나와야 한다」).
그래서 말투가 곧 캐릭터다 — 성격 원본은 나쵸 레포(`prompts/system.md`)에서 읽어 오고,
여기에는 베끼지 않는다. 베끼면 두 벌이 되어 한쪽만 고쳐지는 날이 온다.
"""
from __future__ import annotations

import asyncio
import json
import os
import shutil
import subprocess
import re
import threading
from collections import Counter
from pathlib import Path
from urllib.request import Request, urlopen

TOOLS = [
    {
        "name": "migrate_pane",
        "description": "지금 보는 pane(학생)을 다른 기계로 옮긴다. machine 은 기계 이름(예: 나쵸네코, 맥북) "
        "또는 'local'(이 기계로 데려오기). 사용자가 옮기라·이사시켜라·데려오라고 분명히 말했을 때만.",
        "input_schema": {"type": "object", "properties": {"machine": {"type": "string"}}, "required": ["machine"]},
    },
    {
        "name": "focus_pane",
        "description": "그 pane 을 화면 앞으로 가져온다. 사용자가 보여 달라·앞으로 가져와 달라고 했을 때만.",
        "input_schema": {"type": "object", "properties": {}},
    },
    {
        "name": "send_text",
        "description": "그 pane 의 학생에게 한 줄 지시를 넣는다(Enter 포함). 사용자가 전달해 달라·시켜 달라고 했을 때만.",
        "input_schema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]},
    },
    {
        "name": "homepc",
        "description": "집 데스크톱 전원. on 은 켜기(30초쯤 걸림), off 는 끄기, status 는 켜져 있는지 확인. "
        "사용자가 집컴·내 컴퓨터·데스크탑을 켜라·꺼라·켜져 있냐고 했을 때만.",
        "input_schema": {"type": "object", "properties": {"action": {"type": "string", "enum": ["on", "off", "status"]}}, "required": ["action"]},
    },
]
TOOL_NAMES = tuple(t["name"] for t in TOOLS)

NACHO_REPO = Path(__file__).resolve().parents[3] / "nacho-neko"
# 성격 원본은 이 제목 앞까지만 쓴다 — 그 뒤는 메모리 볼트·도구 사용법이라 펫 자리에서는
# 존재하지 않는 경로를 가리키는 틀린 지시가 된다.
PERSONA_STOP = "\n# 메모리"
FALLBACK_PERSONA = (
    "너는 나쵸네코 — 고양이 모티프의 메이트 봇이다. 조용조용하고 귀여운 반말로 말한다. "
    "어미는 부드럽게(~야·~지·~네·~당·~겡), 카오모지는 환영하고 유니코드 이모지는 쓰지 않는다. "
    "존댓말은 쓰지 않는다. 확신이 없으면 「음... 확실하진 않은데」처럼 솔직하게 말한다."
)

PET_RULES = """
# 지금 자리 — 카사텀 바탕화면 펫
너는 지금 바탕화면 펫으로 떠 있고, 사용자가 펫 머리 위 바에 한 마디 묻는다. 네 답은 펫 말풍선에 그대로 뜬다.
근거는 아래 자료뿐이다. 자료에 없는 것은 지어내지 말고 모른다고 해라.
- 「이 창」「얘」처럼 하나를 물으면 [지금 보는 창] 으로 답한다 — 무엇을 하는지·어디까지 됐는지·막힌 게 있는지·사람이 할 게 있는지. 두세 문장.
- 「전체」「모든 기기」「다들 뭐 해」처럼 판 전체를 물으면 [모든 기기] 로 답한다. 이때는 짧게 답하는 규칙의 예외다.
  기계마다 한 단락: 첫 줄에 기계 이름과 연결 상태, 그 아래 학생마다 한 줄(이름 · 무슨 일 · 어디까지 · 막힘이나 기다림).
  사람 손이 필요한 학생을 맨 앞에 두고, 끊긴 기계는 「끊김」 한 줄로, 쉬는 학생이 많으면 이름만 묶어 한 줄로. 기계당 3~6줄, 전체 700자 안.
- 마크다운 기호(**, #, 코드펜스)는 쓰지 마라 — 말풍선은 글자를 그대로 보여 준다. 줄 앞은 「·」 정도면 된다.
- 코드 식별자·경로·명령·창 번호는 빼고 사람 말로 옮긴다. 학생이 보고한 것과 실제 반영은 다르니 「~했대」로 귀속한다.
- 도구는 사용자가 옮기라·데려오라·앞으로 가져오라·전달하라고 분명히 시킬 때만 쓴다. 묻기만 하면 답만 한다. 도구를 썼으면 무엇을 했는지 한 문장.
- 답 맨 첫 줄에 이번 답의 연출을 적어라: `#연출 동작=<그룹> 표정=<이름>`. [할 수 있는 동작]·[지을 수 있는 표정] 목록의
  이름만 쓰고, 안 고르면 `없음`. 답의 기분에 맞춰 고른다 — 궁금·확인 중이면 생각(Think), 반갑거나 잘됐으면 대화(Talk),
  곤란·실패·막힘이면 곤란(Error), 나른하거나 조용하면 졸기(Sleep), 별일 없으면 대기(Idle). 표정은 어울릴 때만.
"""

ACT_LINE = re.compile(r"^\s*#\s*연출\s*동작\s*=\s*(\S+)\s*표정\s*=\s*(\S+)\s*$", re.MULTILINE)
NONE_WORDS = {"없음", "none", "null", "-", "x"}

CLI_TIMEOUT = 8
# 집컴 전원 명령 — 이 기계(펫이 도는 맥북)에 이미 깔린 것을 그대로 부른다. launchd 밑에는
# PATH 에 ~/.local/bin 이 없어 절대 경로로 간다.
HOMEPC_BIN = Path(os.environ.get("HOMEPC_BIN", str(Path.home() / ".local/bin/homepc")))
HOMEPC_TIMEOUT = 40
PEEK_LINES = 40
MAX_CHARS = 9000
FLEET_CHARS = 5500
MACHINE_CHARS = 2200
LINE_CHARS = 240
NEEDS_HUMAN = ("waiting", "blocked", "error", "needs_attention", "dead")


def persona(repo: Path = NACHO_REPO) -> str:
    """나쵸 성격 원본의 앞부분(정체·톤·답 길이·예시)만 떼어 온다."""
    try:
        text = (repo / "prompts/system.md").read_text(encoding="utf-8")
    except OSError:
        return FALLBACK_PERSONA
    head = text.split(PERSONA_STOP, 1)[0].strip()
    return head or FALLBACK_PERSONA


def system_prompt(repo: Path = NACHO_REPO) -> str:
    return persona(repo) + "\n" + PET_RULES


def cli_path() -> str | None:
    found = shutil.which("kasaterm-cli")
    if found:
        return found
    for candidate in (
        Path.home() / "Applications/kasaterm.app/Contents/MacOS/kasaterm-cli",
        Path("/Applications/kasaterm.app/Contents/MacOS/kasaterm-cli"),
    ):
        if candidate.is_file():
            return str(candidate)
    return None


def socket_path() -> str | None:
    """도는 앱의 소켓 — launchd 밑에는 pane 이 물려주는 KASATERM_SOCKET_PATH 가 없고
    `/tmp/cmux.sock` 도 없다. 앱은 사용자 임시 폴더에 `kasaterm-<pid>.sock` 을 두므로
    가장 새것을 고른다(껐다 켜면 pid 가 바뀌어 옛 파일이 남을 수 있다)."""
    given = os.environ.get("KASATERM_SOCKET_PATH") or os.environ.get("CMUX_SOCKET_PATH")
    if given and Path(given).exists():
        return given
    try:
        tmp = subprocess.run(["getconf", "DARWIN_USER_TEMP_DIR"], capture_output=True, text=True, timeout=3).stdout.strip()
    except (subprocess.TimeoutExpired, OSError):
        tmp = ""
    socks = sorted(Path(tmp or "/tmp").glob("kasaterm-*.sock"), key=lambda p: p.stat().st_mtime, reverse=True)
    return str(socks[0]) if socks else None


def run_cli(*args: str, timeout: int = CLI_TIMEOUT) -> tuple[bool, str]:
    cli = cli_path()
    if not cli:
        return False, "kasaterm-cli 를 못 찾았다"
    env = dict(os.environ)
    if sock := socket_path():
        env["KASATERM_SOCKET_PATH"] = sock
    try:
        done = subprocess.run([cli, *args], capture_output=True, text=True, timeout=timeout, env=env)
    except subprocess.TimeoutExpired:
        return False, "kasaterm-cli 가 응답하지 않는다"
    out = (done.stdout or "").strip()
    # 파이프로 부르면 CLI 는 실패도 JSON 봉투로 찍고 1 로 끝난다 — 봉투째 돌려주면
    # 말풍선에 `{"id":"cli-3","ok":false,…}` 가 그대로 뜬다(2026-09-17 실측). 이유만 꺼낸다.
    try:
        parsed = json.loads(out)
    except ValueError:
        parsed = None
    if isinstance(parsed, dict) and parsed.get("ok") is False:
        error = parsed.get("error") or {}
        return False, str(error.get("message") if isinstance(error, dict) else error or "실패")[:400]
    if done.returncode != 0:
        return False, (done.stderr.strip() or out or "실패")[:400]
    return True, out


def _json_result(raw: str) -> dict:
    try:
        value = json.loads(raw)
    except ValueError:
        return {}
    return value.get("result", value) if isinstance(value, dict) else {}


def _machines_http() -> list[dict]:
    """앱의 기계 목록 — 다른 기계 pane 의 대기 사유·쉰 시간처럼 판에는 없는 것을 여기서 채운다."""
    try:
        with urlopen(Request("http://127.0.0.1:8765/machines"), timeout=3) as resp:
            rows = json.load(resp).get("machines", [])
    except Exception:
        return []
    return [m for m in rows if isinstance(m, dict)]


def _clip(text, limit: int) -> str:
    text = " ".join(str(text or "").split())
    return text if len(text) <= limit else text[: limit - 1] + "…"


def _state(status: str, waiting: str) -> tuple[int, str]:
    """사람이 읽을 상태와 정렬 순위 — 사람 손이 필요한 학생이 맨 앞이다."""
    if waiting or status in NEEDS_HUMAN:
        return 0, "사람 손 필요"
    if status == "working":
        return 1, "일하는 중"
    if status == "idle":
        return 2, "쉬는 중"
    return 3, "불명"


def fleet(board_all: dict, local_rows: dict[str, dict], machines_http: list[dict]) -> list[dict]:
    """`board --all` 을 기계별·방별로 묶는다.

    다른 기계에 비친 거울 줄(status_reason 이 remote mirror)은 뺀다 — 같은 학생이 두 기계에
    한 번씩, 두 번 세어진다. 이 기기 줄은 로컬 판으로, 다른 기계 줄은 기계 목록의 pane 으로
    대기 사유·쉰 시간·지금 하는 일을 채운다.
    """
    remote: dict[tuple[str, str], dict] = {}
    for machine in machines_http:
        route = str(machine.get("route") or "")
        if not route.startswith("~"):
            continue
        for pane in machine.get("panes") or []:
            if isinstance(pane, dict) and pane.get("id"):
                remote[(route[1:], str(pane["id"]))] = pane
    machines: dict[str, dict] = {}
    for source in board_all.get("sources") or []:
        if not isinstance(source, dict) or not source.get("machine_id"):
            continue
        machines[source["machine_id"]] = {
            "label": source.get("label") or source["machine_id"],
            "online": source.get("state") == "online",
            "is_local": bool(source.get("is_local")),
            "rooms": {},
        }
    for pane in board_all.get("panes") or []:
        if not isinstance(pane, dict) or str(pane.get("status_reason") or "").startswith("remote mirror"):
            continue
        address = pane.get("address") or {}
        machine_id = address.get("machine_id") or ""
        machine = machines.get(machine_id)
        if machine is None:
            continue
        surface = str(address.get("surface_id") or "")
        extra = local_rows.get(surface, {}) if machine["is_local"] else remote.get((machine_id, surface), {})
        waiting = _clip(extra.get("waiting_for"), 80)
        rank, state = _state(str(pane.get("status") or ""), waiting)
        idle = extra.get("idle_secs")
        entry = {
            "pane": surface,
            "name": pane.get("character") or extra.get("name") or extra.get("character") or "(캐릭터 없음)",
            "rank": rank,
            "state": state,
            "title": _clip(pane.get("title") or extra.get("title"), 60),
            "request": _clip(pane.get("request") or extra.get("last_prompt"), 70),
            "progress": _clip(pane.get("progress") or extra.get("last_reply"), 100),
            "doing": _clip(extra.get("doing") or extra.get("intent"), 70),
            "waiting": waiting,
            "idle_min": int(idle // 60) if isinstance(idle, (int, float)) and idle >= 60 else 0,
            "stale": pane.get("freshness") not in (None, "fresh"),
        }
        machine["rooms"].setdefault(pane.get("room_label") or "방 없음", []).append(entry)
    ordered = sorted(machines.values(), key=lambda m: (not m["is_local"], not m["online"], m["label"]))
    for machine in ordered:
        for rows in machine["rooms"].values():
            rows.sort(key=lambda e: (e["rank"], e["name"]))
        # 사람 손이 필요한 학생이 있는 방이 앞이다 — 모델이 앞에서부터 읽고 앞에서부터 말한다.
        machine["rooms"] = dict(sorted(machine["rooms"].items(), key=lambda room: min(e["rank"] for e in room[1])))
    return ordered


def _entry_line(entry: dict) -> str:
    parts = [f"{entry['name']} · {entry['state']}"]
    if entry["title"]:
        parts.append(entry["title"])
    if entry["waiting"]:
        parts.append(f"기다림: {entry['waiting']}")
    if entry["request"]:
        parts.append(f"지시: {entry['request']}")
    if entry["progress"]:
        parts.append(f"진행: {entry['progress']}")
    if entry["doing"] and entry["state"] == "일하는 중":
        parts.append(f"지금: {entry['doing']}")
    if entry["idle_min"]:
        parts.append(f"{entry['idle_min']}분째 조용")
    if entry["stale"]:
        parts.append("(오래된 관측)")
    return _clip(" · ".join(parts), LINE_CHARS)


def digest(machines: list[dict]) -> str:
    """모델에 실을 판 — 기계마다 한 덩어리. 비어 있으면 그렇다고 적는다(빈 줄은 「판을
    못 읽었다」와 구분이 안 된다)."""
    if not machines:
        return "(기계 목록이 비어 있다)"
    blocks = []
    for machine in machines:
        label = machine["label"] + (" (이 기기)" if machine["is_local"] else "")
        if not machine["online"]:
            blocks.append(f"■ {label} — 끊김")
            continue
        entries = [e for rows in machine["rooms"].values() for e in rows]
        counts = Counter(e["state"] for e in entries)
        tally = " · ".join(f"{state} {counts[state]}" for state in ("사람 손 필요", "일하는 중", "쉬는 중", "불명") if counts[state])
        lines = [f"■ {label} — 연결됨 · 학생 {len(entries)}" + (f" ({tally})" if tally else "")]
        for room, rows in machine["rooms"].items():
            lines.append(f" [{room}]")
            lines.extend("  · " + _entry_line(e) for e in rows)
        block = "\n".join(lines)
        blocks.append(block if len(block) <= MACHINE_CHARS else block[: MACHINE_CHARS - 1] + "…")
    text = "\n".join(blocks)
    return text if len(text) <= FLEET_CHARS else text[: FLEET_CHARS - 1] + "…"


def context(pane: str) -> dict:
    """모델에 실을 맥락 — 포커스한 pane(판 줄·화면 꼬리)과 모든 기기의 판."""
    local_rows: dict[str, dict] = {}
    ok, raw = run_cli("board")
    if ok:
        for row in _json_result(raw).get("board", []) or []:
            if isinstance(row, dict) and row.get("surface_id"):
                local_rows[str(row["surface_id"])] = row
    ok, raw = run_cli("board", "--all")
    fleet_text = digest(fleet(_json_result(raw), local_rows, _machines_http())) if ok else f"(판을 읽지 못했다: {raw})"
    screen = ""
    ok, raw = run_cli("peek", pane)
    if ok:
        text = str(_json_result(raw).get("text", ""))
        lines = [line.rstrip() for line in text.splitlines() if line.strip()]
        screen = "\n".join(lines[-PEEK_LINES:])
    return {"who": local_rows.get(pane, {}), "screen": screen, "fleet": fleet_text}


def catalog_lines(catalog) -> str:
    """펫이 보낸 동작·표정 목록을 모델이 읽을 두 줄로. 목록이 없으면 연출 없이 답한다."""
    if not isinstance(catalog, dict):
        return ""
    def items(rows, key):
        out = []
        for row in rows if isinstance(rows, list) else []:
            if isinstance(row, dict) and row.get(key):
                name = str(row[key])[:40]
                label = str(row.get("label") or "").strip()[:20]
                out.append(f"{name}({label})" if label else name)
        return out
    motions = items(catalog.get("motions"), "group")
    expressions = items(catalog.get("expressions"), "name")
    if not motions and not expressions:
        return ""
    return f"[할 수 있는 동작] {', '.join(motions) or '없음'}\n[지을 수 있는 표정] {', '.join(expressions) or '없음'}\n"


def perform(reply: str, catalog) -> tuple[str, dict]:
    """답 첫 줄의 연출 지시를 떼어 낸다. 목록에 없는 이름은 「없음」이다 — 모델이 지어낸
    동작을 펫에 넘기면 펫이 못 찾고 조용히 무시하니, 여기서 거른다."""
    act = {"motion": None, "expression": None}
    match = ACT_LINE.search(reply)
    if not match:
        return reply, act
    def known(value, rows, key):
        if value.lower() in NONE_WORDS:
            return None
        for row in rows if isinstance(rows, list) else []:
            if isinstance(row, dict) and str(row.get(key, "")).lower() == value.lower():
                return str(row[key])
        return None
    rows = catalog if isinstance(catalog, dict) else {}
    act["motion"] = known(match.group(1), rows.get("motions"), "group")
    act["expression"] = known(match.group(2), rows.get("expressions"), "name")
    return (reply[: match.start()] + reply[match.end():]).strip(), act


def build_prompt(text: str, pane: str, ctx: dict, catalog=None) -> str:
    who = ctx["who"]
    head = (
        f"[질문] {text}\n\n"
        f"{catalog_lines(catalog)}"
        f"[지금 보는 창] {pane} · 학생 {who.get('character') or '미배정'} · 세션 {who.get('peer_name') or '-'} · "
        f"상태 {who.get('status') or '-'} · 기계 {who.get('machine') or '이 기기'} · 폴더 {who.get('cwd') or '-'}\n"
        f"[마지막 지시] {(who.get('last_prompt') or '')[:600]}\n"
        f"[마지막 답] {(who.get('last_reply') or '')[:800]}\n\n"
        f"[모든 기기]\n{ctx['fleet']}\n\n"
        f"[지금 보는 창의 화면 끝 부분]\n"
    )
    room = max(MAX_CHARS - len(head), 800)
    return head + ctx["screen"][-room:]


def execute(pane: str, uses: list[dict]) -> list[dict]:
    """모델이 고른 도구를 실행한다 — 목록 밖 이름은 무시한다."""
    results = []
    for use in uses:
        name = use.get("name")
        args = use.get("input") or {}
        if name == "migrate_pane":
            machine = str(args.get("machine", "")).strip()
            ok, detail = (False, "기계 이름이 없다") if not machine else run_cli("migrate", pane, machine, timeout=60)
            if ok:
                # 일하는 중인 학생은 바로 안 가고 턴이 끝나면 간다 — 앱이 그 말을 remote_id 자리에
                # 실어 준다. 「이사」라고만 하면 사람은 옮겨진 줄 알고 그 창을 찾아 헤맨다.
                note = str(_json_result(detail).get("remote_id") or "")
                detail = note if "예약" in note else f"{machine}로 이사"
            results.append({"kind": name, "ok": ok, "detail": detail})
        elif name == "focus_pane":
            ok, detail = run_cli("focus", pane)
            results.append({"kind": name, "ok": ok, "detail": "앞으로 가져옴" if ok else detail})
        elif name == "send_text":
            line = str(args.get("text", "")).strip()
            ok, detail = (False, "보낼 글이 없다") if not line else run_cli("send", "--surface", pane, line + "\n")
            results.append({"kind": name, "ok": ok, "detail": f"전달: {line[:60]}" if ok else detail})
        elif name == "homepc":
            ok, detail = homepc(str(args.get("action", "")).strip().lower())
            results.append({"kind": name, "ok": ok, "detail": detail})
    return results


def homepc(action: str) -> tuple[bool, str]:
    """집컴 전원 스크립트를 부르고 그 말을 그대로 돌려준다. 이 서버는 사용자 본인 기계에서만
    도니(로컬 원점 검사) 따로 사람을 가리지 않는다."""
    if action not in ("on", "off", "status"):
        return False, "action 은 on·off·status 중 하나다"
    if not HOMEPC_BIN.is_file():
        return False, f"집컴 전원 명령이 없다: {HOMEPC_BIN}"
    try:
        done = subprocess.run([str(HOMEPC_BIN), action], capture_output=True, text=True, timeout=HOMEPC_TIMEOUT)
    except subprocess.TimeoutExpired:
        return False, f"집컴 {action} 응답이 {HOMEPC_TIMEOUT}초 안에 안 왔다"
    except OSError as e:
        return False, f"집컴 전원 명령을 못 돌렸다: {e}"
    out = (done.stdout or "").strip() or (done.stderr or "").strip()
    if done.returncode != 0:
        return False, (out or f"종료 코드 {done.returncode}")[:300]
    return True, (out or {"on": "켜기 신호를 보냈다 — 30초쯤 뒤에 켜진다", "off": "끄기 신호를 보냈다", "status": "상태 응답이 비었다"}[action])[:300]


def llm_client(provider_factory):
    """모델 통로 — 장부 채팅이 쓰는 provider 에 클라이언트가 있으면 그것, 없으면(이 기계는
    미니 터널로 요약을 시키는 구성이라 없다) 나쵸의 LLM 클라이언트를 이 자리에서 직접 연다.
    키는 askimg 와 같은 `~/.config/opengateway.key`(600) 에서 읽고 이 프로세스 환경에만 둔다."""
    provider = provider_factory(threading.Event())
    client = getattr(provider, "client", None)
    if client is not None:
        return client
    if not os.environ.get("OPENGATEWAY_API_KEY"):
        keyfile = Path(os.environ.get("OG_KEY_FILE", str(Path.home() / ".config/opengateway.key")))
        try:
            key = keyfile.read_text(encoding="utf-8").strip()
        except OSError:
            key = ""
        if key:
            os.environ["OPENGATEWAY_API_KEY"] = key
    from .remote_helper import load_client

    try:
        return load_client(NACHO_REPO)
    except Exception:
        return None


def answer(provider_factory, body: dict) -> tuple[int, dict]:
    text = str(body.get("text", "")).strip()
    pane = str(body.get("pane", "")).strip()
    if not text:
        return 400, {"error": "empty_question"}
    if not pane.startswith("%"):
        return 400, {"error": "pane_required"}
    client = llm_client(provider_factory)
    if client is None:
        return 503, {"error": "llm_unavailable"}
    catalog = body.get("catalog")
    prompt = build_prompt(text, pane, context(pane), catalog)

    async def call():
        return await asyncio.wait_for(
            client.messages(
                system=system_prompt(),
                messages=[{"role": "user", "content": prompt}],
                tools=TOOLS,
                # 전체 요약은 기계마다 한 단락이라 한 창 답보다 서너 배 길다.
                max_tokens=900,
                temperature=0.3,
            ),
            timeout=35.0,
        )

    try:
        resp = asyncio.run(call())
    except Exception as exc:
        return 502, {"error": "model_failed", "detail": str(exc)[:200]}
    reply = (client.extract_text(resp, TOOL_NAMES) or "").strip()
    actions = execute(pane, client.extract_tool_uses(resp))
    if not reply and actions:
        reply = " · ".join(a["detail"] for a in actions)
    reply, act = perform(reply, catalog)
    if not reply:
        reply = "음... 답을 못 만들었엉"
    return 200, {"answer": plain(reply)[:1500], "actions": actions, "act": act}


def plain(text: str) -> str:
    """말풍선은 글자를 그대로 찍는다 — 모델이 습관처럼 붙이는 굵게·제목·코드펜스 기호를 걷는다."""
    text = re.sub(r"```[a-zA-Z]*\n?", "", text)
    text = re.sub(r"\*\*(.+?)\*\*", r"\1", text)
    text = re.sub(r"(?m)^\s{0,3}#{1,6}\s+", "", text)
    text = re.sub(r"(?m)^\s*[-*]\s+", "· ", text)
    return text.strip()
