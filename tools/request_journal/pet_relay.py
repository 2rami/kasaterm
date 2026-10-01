"""펫과 나쵸 사이의 짧은 중계 — 우편함(`/api/pet/poll`) 넘기기와 먼저 거는 말(`chatter.py`)이 쓰는 공용 손.

펫은 나쵸에게 묻지 않는다(2026-10-01 지시 「펫에서 대화나 그런 건 다 걷어줘」). 대화는 나쵸 앱이
맡고, 펫은 나쵸가 건네는 말만 띄운다. 그래서 여기 남은 것은 두 가지다.
- 나쵸 우편함을 이 기계 이름으로 끌어와 그대로 넘긴다(`relay_poll`).
- 먼저 거는 말이 쓰는 성격 원본·CLI·모델 통로.

나쵸 자리는 서술자 `~/.config/kasaterm/nacho-ask.json` `{"version":1,"url":"http://…"}`(또는 env
`NACHO_ASK_URL`)이 정하고, 그 파일이 없으면 넘기지 않는다.

성격 원본은 나쵸 레포(`prompts/system.md`)에서 읽어 오고 여기에는 베끼지 않는다 — 베끼면 두 벌이
되어 한쪽만 고쳐지는 날이 온다.
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import re
import sys
import threading
import time
from pathlib import Path
from urllib.request import Request, urlopen

NACHO_REPO = Path(__file__).resolve().parents[3] / "nacho-neko"
# 나쵸 창구. 서술자가 없으면 넘기지 않는다.
NACHO_ASK_DESCRIPTOR = Path(os.environ.get(
    "NACHO_ASK_DESCRIPTOR", str(Path.home() / ".config/kasaterm/nacho-ask.json")))
NACHO_ASK_TOKEN_FILE = Path(os.environ.get(
    "NACHO_ASK_TOKEN_FILE", str(Path.home() / ".config/nacho-ask.key")))
# 성격 원본은 이 제목 앞까지만 쓴다 — 그 뒤는 메모리 볼트·도구 사용법이라 펫 자리에서는
# 존재하지 않는 경로를 가리키는 틀린 지시가 된다.
PERSONA_STOP = "\n# 메모리"
FALLBACK_PERSONA = (
    "너는 나쵸네코 — 고양이 모티프의 메이트 봇이다. 조용조용하고 귀여운 반말로 말한다. "
    "어미는 부드럽게(~야·~지·~네·~당·~겡), 카오모지는 환영하고 유니코드 이모지는 쓰지 않는다. "
    "존댓말은 쓰지 않는다. 확신이 없으면 「음... 확실하진 않은데」처럼 솔직하게 말한다."
)

CLI_TIMEOUT = 8
PEEK_LINES = 40
# 펫이 소식을 기다리며 붙잡는 시간의 상한과, 그 위에 얹는 왕복 여유. 나쵸 쪽 상한과 같아야
# 여기서 먼저 끊겨 「받았는데 못 받은 것으로」 남지 않는다.
POLL_WAIT_CAP = 25.0
POLL_SLACK = 10.0


def persona(repo: Path = NACHO_REPO) -> str:
    """나쵸 성격 원본의 앞부분(정체·톤·답 길이·예시)만 떼어 온다."""
    try:
        text = (repo / "prompts/system.md").read_text(encoding="utf-8")
    except OSError:
        return FALLBACK_PERSONA
    head = text.split(PERSONA_STOP, 1)[0].strip()
    return head or FALLBACK_PERSONA


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


def _clip(text, limit: int) -> str:
    text = " ".join(str(text or "").split())
    return text if len(text) <= limit else text[: limit - 1] + "…"


def nacho_base() -> str:
    """나쵸 물음 창구 주소. env → 서술자 파일 순. 없으면 빈 문자열(넘기지 않는다)."""
    url = os.environ.get("NACHO_ASK_URL", "").strip()
    if url:
        return url.rstrip("/")
    try:
        raw = NACHO_ASK_DESCRIPTOR.read_bytes()
    except OSError:
        return ""
    if len(raw) > 8192:
        return ""
    try:
        descriptor = json.loads(raw)
    except ValueError:
        return ""
    if not isinstance(descriptor, dict) or descriptor.get("version") != 1:
        return ""
    url = str(descriptor.get("url") or "").strip().rstrip("/")
    return url if url.startswith(("http://", "https://")) else ""


def _note(line: str) -> None:
    """나쵸에 못 닿은 까닭을 남긴다 — 펫은 조용히 물러나므로 사람 눈에는 안 보인다."""
    print(f"[nacho-ask] {line}", file=sys.stderr, flush=True)


def nacho_call(route: str, payload: dict, timeout: float) -> tuple[int, dict | None]:
    """나쵸 창구에 한 번. (상태, 몸통). 못 닿으면 (0, None) — 부르는 쪽이 사정을 정한다."""
    base = nacho_base()
    if not base:
        return 0, None
    body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    headers = {"Content-Type": "application/json", "X-Journal-Request": "1"}
    try:
        token = NACHO_ASK_TOKEN_FILE.read_text(encoding="utf-8").strip()
    except OSError:
        token = ""
    if token:
        headers["X-Nacho-Token"] = token
    try:
        with urlopen(Request(f"{base}{route}", data=body, headers=headers, method="POST"),
                     timeout=timeout) as resp:
            got = json.loads(resp.read(1024 * 1024))
            return resp.status, got if isinstance(got, dict) else None
    except Exception as exc:
        _note(f"나쵸에 못 닿았다({type(exc).__name__}: {str(exc)[:80]}) — {route}")
        return 0, None


# 이 기계의 이름. 판을 읽어 알아내고 잠시 들고 있는다 — 펫이 몇 초마다 소식을 끌어가는데
# 그때마다 `board --all` 을 돌리면 그 한 줄 때문에 CLI 가 쉴 새 없이 뜬다.
_MACHINE: dict = {"label": "", "at": 0.0}
MACHINE_TTL = 300.0


def machine_label() -> str:
    """판이 스스로 말하는 이 기계 이름. 못 읽으면 빈 문자열(그때는 자리를 안 정한다)."""
    now = time.monotonic()
    if _MACHINE["label"] and now - _MACHINE["at"] < MACHINE_TTL:
        return _MACHINE["label"]
    ok, raw = run_cli("board", "--all")
    label = ""
    if ok:
        label = next((str(src.get("label") or "") for src in _json_result(raw).get("sources", []) or []
                      if isinstance(src, dict) and src.get("is_local")), "")
    if label:
        _MACHINE.update(label=label, at=now)
    return label


def relay_poll(body: dict) -> tuple[int, dict]:
    """펫이 끌어가는 소식을 나쵸에서 받아 그대로 넘긴다.

    ★**이 기계 이름을 여기서 채운다.** 펫은 자기가 어느 바탕화면인지 모르고, 나쵸는 여러
    기계의 펫을 받으므로 그 이름이 곧 대화 자리(`kasapet:<기계>`)다. 이름을 못 읽으면
    **아무 자리나 고르지 않고** 물러난다 — 틀린 자리에 붙으면 남의 바탕화면 소식을 받는다.

    나쵸에 못 닿으면 503 이다. 펫은 조용히 다음에 다시 들른다 — 옛 파일 인박스(`pet-say`)는
    그대로 살아 있어서 이 기계의 소식은 여전히 흐른다.
    """
    machine = str(body.get("machine") or "").strip() or machine_label()
    if not machine:
        return 503, {"error": "machine_unknown"}
    if not nacho_base():
        return 503, {"error": "nacho_not_configured"}
    wait = max(0.0, min(POLL_WAIT_CAP, float(body.get("wait") or 0)))
    acks = [str(i)[:40] for i in (body.get("ack") or [])][:32] if isinstance(body.get("ack"), list) else []
    status, payload = nacho_call("/api/pet/poll", {"machine": machine, "ack": acks, "wait": wait},
                                 wait + POLL_SLACK)
    if status != 200 or payload is None:
        return 503, {"error": "nacho_unreachable", "status": status}
    payload["machine"] = machine        # 펫이 다음 들를 때 그대로 들고 온다
    return 200, payload


def llm_client(provider_factory):
    """모델 통로 — 장부 채팅이 쓰는 provider 에 클라이언트가 있으면 그것, 없으면(이 기계는
    미니 터널로 요약을 시키는 구성이라 없다) 나쵸의 LLM 클라이언트를 이 자리에서 직접 연다.
    키는 나쵸와 같은 `~/.config/opengateway.key`(600) 에서 읽고 이 프로세스 환경에만 둔다."""
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


def plain(text: str) -> str:
    """말풍선은 글자를 그대로 찍는다 — 모델이 습관처럼 붙이는 굵게·제목·코드펜스 기호를 걷는다."""
    text = re.sub(r"```[a-zA-Z]*\n?", "", text)
    text = re.sub(r"\*\*(.+?)\*\*", r"\1", text)
    text = re.sub(r"(?m)^\s{0,3}#{1,6}\s+", "", text)
    text = re.sub(r"(?m)^\s*[-*]\s+", "· ", text)
    return text.strip()
