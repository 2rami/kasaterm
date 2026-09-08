"""Tool-free access to Nacho's existing LLM transport, without importing the bot."""
from __future__ import annotations

import asyncio
import base64
import json
from pathlib import Path
import re
import subprocess
import threading
import time
from urllib.parse import urlencode, urlsplit
from urllib.request import Request, build_opener, ProxyHandler
import uuid

from .remote_helper import SYSTEM, bounded_payload, completion_payload, load_client, summarize_with_client



class NachoProvider:
    name = "nacho-llm"

    def __init__(self, client):
        self.client = client

    @classmethod
    def from_environment(cls, repo: str | Path | None = None):
        root = Path(repo) if repo else Path(__file__).resolve().parents[3] / "nacho-neko"
        try:
            client = load_client(root)
        except (ImportError, OSError):
            return None
        return cls(client) if client else None

    def summarize(self, payload: str) -> str:
        return asyncio.run(summarize_with_client(self.client, payload))

    def complete(self, payload: str) -> str:
        return self.summarize(completion_payload(payload))


class SSHNachoProvider:
    """Opt-in only after remote helper installation; no implicit network probing."""
    name = "nacho-ssh"
    command = 'cd "$HOME/nacho-neko" && set -a && { [ ! -f .env ] || . ./.env; } && set +a && exec .venv/bin/python bin/request-journal-summary.py'

    def __init__(self, host="nachoneko"):
        if host not in ("nachoneko", "nachoneko-via05"):
            raise ValueError("unknown Nacho host")
        self.host = host

    def summarize(self, payload):
        complete = json.loads(payload).get("mode") in ("chat", "checklist")
        result = subprocess.run(
            ["ssh", "-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5", self.host, self.command],
            input=bounded_payload(payload), text=True, capture_output=True, timeout=130 if complete else 45,
        )
        if result.returncode or len(result.stdout) > 96000:
            raise RuntimeError("remote summary unavailable")
        response = json.loads(result.stdout)
        if response.get("ok") is not True or not isinstance(response.get("text"), str) or not response["text"].strip():
            raise RuntimeError("remote summary unavailable")
        text = response["text"].strip()
        if complete and len(text) > 12000:
            raise RuntimeError("completion response too large")
        return text if complete else text[:400]

    def complete(self, payload: str) -> str:
        return self.summarize(completion_payload(payload))


class HTTPNachoProvider:
    """A private, short-lived web terminal; never addresses an existing student."""
    name = "nacho-http"

    def __init__(self, base_url="http://127.0.0.1:18795", cancel_event=None):
        parsed = urlsplit(base_url)
        if parsed.scheme != "http" or parsed.hostname != "127.0.0.1" or not parsed.port or parsed.path not in ("", "/") or parsed.query or parsed.fragment or parsed.username:
            raise ValueError("existing loopback tunnel required")
        self.base_url = base_url.rstrip("/")
        self.cancel_event = cancel_event or threading.Event()
        self.opener = build_opener(ProxyHandler({}))

    def _call(self, method, route, data=None, raw=False):
        if isinstance(data, dict):
            data = json.dumps(data, ensure_ascii=False).encode()
        elif isinstance(data, str):
            data = data.encode()
        request = Request(self.base_url + route, data=data, method=method)
        with self.opener.open(request, timeout=4) as response:
            body = response.read(262145)
        if len(body) > 262144:
            raise RuntimeError("response too large")
        return body.decode() if raw else json.loads(body)

    def _input(self, pane, command):
        if not re.fullmatch(r"web-[0-9a-f-]{36}", pane):
            raise ValueError("unowned terminal")
        if not self._call("POST", "/term/input?" + urlencode({"pane": pane}), command).get("ok"):
            raise RuntimeError("terminal input unavailable")

    def _screen(self, pane):
        return self._call("GET", "/term/screen?" + urlencode({"pane": pane, "lines": 1000}), raw=True)

    def _wait(self, pane, pattern, seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if self.cancel_event.is_set():
                raise RuntimeError("summary cancelled")
            match = re.search(pattern, self._screen(pane), re.MULTILINE | re.DOTALL)
            if match:
                return match
            self.cancel_event.wait(0.3)
        raise RuntimeError("summary timed out")

    def summarize(self, payload):
        payload = bounded_payload(payload)
        complete = json.loads(payload).get("mode") in ("chat", "checklist")
        if self.cancel_event.is_set():
            raise RuntimeError("summary cancelled")
        pane = directory = None
        marker = "RJ_" + uuid.uuid4().hex
        cleanup_error = False
        try:
            spawned = self._call("POST", "/term/spawn?cwd=%2Ftmp&cols=120&rows=32")
            pane = spawned.get("id")
            if not spawned.get("ok") or not isinstance(pane, str) or not re.fullmatch(r"web-[0-9a-f-]{36}", pane):
                pane = None
                raise RuntimeError("private terminal unavailable")
            # Suppress terminal echo so neither the input payload nor transport markers
            # can be mistaken for completed output when the screen is polled.
            self._input(pane, "stty -echo; umask 077; journal_temp_dir=$(mktemp -d /tmp/kasaterm-journal.XXXXXXXX); "
                        "trap 'rm -f \"$journal_temp_dir/helper.py\" \"$journal_temp_dir/input.json\" \"$journal_temp_dir/output.json\"; rmdir \"$journal_temp_dir\" 2>/dev/null' EXIT HUP TERM; "
                        f"printf '\\n{marker}_DIR:%s\\n' \"$journal_temp_dir\"\n")
            found = self._wait(pane, rf"^{marker}_DIR:(/tmp/kasaterm-journal\.[A-Za-z0-9]+)\r?$", 8)
            directory = found.group(1)
            for filename, content in (("helper.py", Path(__file__).with_name("remote_helper.py").read_text()), ("input.json", payload)):
                if not self._call("POST", "/save-markdown", {"path": directory + "/" + filename, "content": content}).get("ok"):
                    raise RuntimeError("private input unavailable")
            clean = f"rm -f {directory}/helper.py {directory}/input.json {directory}/output.json; rmdir {directory}"
            command = (
                f"(set +x; set +v; trap '{clean}' EXIT; "
                "cd \"$HOME/nacho-neko\" && set -a && { [ ! -f .env ] || . ./.env >/dev/null 2>&1; } && set +a && "
                f".venv/bin/python -B {directory}/helper.py <{directory}/input.json >{directory}/output.json 2>/dev/null; "
                f"printf '\\n{marker}_BEGIN\\n'; base64 <{directory}/output.json; printf '\\n{marker}_END\\n')\n"
            )
            self._input(pane, command)
            found = self._wait(pane, rf"^{marker}_BEGIN\r?\n(.*?)\r?\n{marker}_END\r?$", 120 if complete else 45)
            encoded = "".join(found.group(1).split())
            if len(encoded) > 96000:
                raise RuntimeError("invalid summary")
            response = json.loads(base64.b64decode(encoded, validate=True))
            if response.get("ok") is not True or not isinstance(response.get("text"), str) or not response["text"].strip():
                stage = response.get("stage")
                stage = stage if stage in ("invalid_input", "runtime_unavailable", "credentials_unavailable", "inference_unavailable") else "unknown"
                raise RuntimeError("remote summary unavailable: " + stage)
            text = response["text"].strip()
            if complete and len(text) > 12000:
                raise RuntimeError("completion response too large")
            return text if complete else text[:400]
        finally:
            if pane:
                try:
                    self._input(pane, "\x03")
                    if directory:
                        self._input(pane, f"rm -f {directory}/helper.py {directory}/input.json {directory}/output.json; rmdir {directory} 2>/dev/null; [ ! -d {directory} ] && printf '\\n{marker}_CLEAN\\n'\n")
                        # Cleanup still runs after cancellation. Only our UUID is addressed.
                        deadline = time.monotonic() + 5
                        while time.monotonic() < deadline:
                            if re.search(rf"^{marker}_CLEAN\r?$", self._screen(pane), re.MULTILINE):
                                break
                            time.sleep(0.2)
                        else:
                            cleanup_error = True
                except Exception:
                    cleanup_error = True
                finally:
                    try:
                        result = self._call("DELETE", "/term/session?" + urlencode({"pane": pane}))
                        cleanup_error = cleanup_error or result.get("ok") is not True
                    except Exception:
                        cleanup_error = True
                if cleanup_error:
                    raise RuntimeError("private summary cleanup unconfirmed")

    def complete(self, payload: str) -> str:
        return self.summarize(completion_payload(payload))
