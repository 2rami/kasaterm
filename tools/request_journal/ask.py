"""펫 머리 위 유리 바가 묻는 「지금 보는 창」 질문 — 짧게 답하고, 시키면 움직인다.

`/api/chat` 은 시킨 일의 장부를 검산하는 느린 길이고 모델 출력을 실행하지 않는다.
이 길은 반대다 — 지금 포커스한 pane 의 화면·상태를 실어 몇 초 안에 답하고, 「이 창
미니로 이사해줘」 같은 말은 kasaterm-cli 로 실제로 옮긴다(2026-09-14 지시). 실행은
모델이 아래 도구를 골랐을 때만, 그것도 이 목록의 것만이다. 화면 글자는 그대로 모델에
가지만 디스크에는 남기지 않는다.
"""
from __future__ import annotations

import asyncio
import json
import os
import shutil
import subprocess
import threading
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
]
TOOL_NAMES = tuple(t["name"] for t in TOOLS)

SYSTEM = (
    "너는 카사텀의 나쵸다. 사용자는 지금 보고 있는 터미널 창(pane) 하나를 두고 묻는다. "
    "아래 맥락만 근거로 한국어 반말로 두세 문장 안에 답해라. 코드 식별자·경로·명령은 빼고, "
    "무엇을 하고 있는지·어디까지 됐는지·막힌 게 있는지·사람이 할 게 있는지로 말해라. "
    "모르면 모른다고 해라. 사용자가 옮기라·데려오라·앞으로·전달하라처럼 분명히 시킬 때만 도구를 쓰고, "
    "묻기만 하면 도구 없이 답만 해라. 도구를 썼으면 무엇을 했는지 한 문장으로 말해라."
)

CLI_TIMEOUT = 8
PEEK_LINES = 40
MAX_CHARS = 6000


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
    if done.returncode != 0:
        return False, (done.stderr or out or "실패").strip()[:400]
    try:
        parsed = json.loads(out)
        if isinstance(parsed, dict) and parsed.get("ok") is False:
            return False, str(parsed.get("error", {}).get("message", "실패"))[:400]
    except ValueError:
        pass
    return True, out


def _json_result(raw: str) -> dict:
    try:
        value = json.loads(raw)
    except ValueError:
        return {}
    return value.get("result", value) if isinstance(value, dict) else {}


def pane_context(pane: str) -> dict:
    """모델에 실을 맥락 — 판(누가·무슨 일·상태), 화면 꼬리, 기계 목록."""
    who = {}
    ok, raw = run_cli("board")
    if ok:
        for row in _json_result(raw).get("board", []) or []:
            if isinstance(row, dict) and row.get("surface_id") == pane:
                who = row
                break
    screen = ""
    ok, raw = run_cli("peek", pane)
    if ok:
        text = str(_json_result(raw).get("text", ""))
        lines = [line.rstrip() for line in text.splitlines() if line.strip()]
        screen = "\n".join(lines[-PEEK_LINES:])
    machines = []
    try:
        with urlopen(Request("http://127.0.0.1:8765/machines"), timeout=3) as resp:
            for m in json.load(resp).get("machines", []):
                machines.append(f"{m.get('label')}({'연결됨' if m.get('online') else '끊김'})")
    except Exception:
        pass
    return {"who": who, "screen": screen, "machines": machines}


def build_prompt(text: str, pane: str, ctx: dict) -> str:
    who = ctx["who"]
    head = (
        f"[질문] {text}\n\n"
        f"[창] {pane} · 학생 {who.get('character') or '미배정'} · 세션 {who.get('peer_name') or '-'} · "
        f"상태 {who.get('status') or '-'} · 기계 {who.get('machine') or '이 기기'} · 폴더 {who.get('cwd') or '-'}\n"
        f"[마지막 지시] {(who.get('last_prompt') or '')[:600]}\n"
        f"[마지막 답] {(who.get('last_reply') or '')[:800]}\n"
        f"[기계 목록] {', '.join(ctx['machines']) or '없음'}\n"
        f"[화면 끝 부분]\n"
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
            results.append({"kind": name, "ok": ok, "detail": f"{machine}로 이사" if ok else detail})
        elif name == "focus_pane":
            ok, detail = run_cli("focus", pane)
            results.append({"kind": name, "ok": ok, "detail": "앞으로 가져옴" if ok else detail})
        elif name == "send_text":
            line = str(args.get("text", "")).strip()
            ok, detail = (False, "보낼 글이 없다") if not line else run_cli("send", "--surface", pane, line + "\n")
            results.append({"kind": name, "ok": ok, "detail": f"전달: {line[:60]}" if ok else detail})
    return results


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

    repo = Path(__file__).resolve().parents[3] / "nacho-neko"
    try:
        return load_client(repo)
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
    ctx = pane_context(pane)
    prompt = build_prompt(text, pane, ctx)

    async def call():
        return await asyncio.wait_for(
            client.messages(
                system=SYSTEM,
                messages=[{"role": "user", "content": prompt}],
                tools=TOOLS,
                max_tokens=500,
                temperature=0.2,
            ),
            timeout=25.0,
        )

    try:
        resp = asyncio.run(call())
    except Exception as exc:
        return 502, {"error": "model_failed", "detail": str(exc)[:200]}
    reply = (client.extract_text(resp, TOOL_NAMES) or "").strip()
    actions = execute(pane, client.extract_tool_uses(resp))
    if not reply and actions:
        reply = " · ".join(a["detail"] for a in actions)
    if not reply:
        reply = "답을 못 만들었어."
    return 200, {"answer": reply[:600], "actions": actions}
