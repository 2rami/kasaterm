"""Tool-free access to Nacho's existing LLM transport, without importing the bot."""
from __future__ import annotations

import asyncio
import json
from pathlib import Path
import subprocess

from .remote_helper import SYSTEM, bounded_payload, load_client, summarize_with_client



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


class SSHNachoProvider:
    """Opt-in only after remote helper installation; no implicit network probing."""
    name = "nacho-ssh"
    command = 'cd "$HOME/nacho-neko" && set -a && { [ ! -f .env ] || . ./.env; } && set +a && exec .venv/bin/python bin/request-journal-summary.py'

    def __init__(self, host="nachoneko"):
        if host not in ("nachoneko", "nachoneko-via05"):
            raise ValueError("unknown Nacho host")
        self.host = host

    def summarize(self, payload):
        result = subprocess.run(
            ["ssh", "-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5", self.host, self.command],
            input=bounded_payload(payload), text=True, capture_output=True, timeout=45,
        )
        if result.returncode or len(result.stdout) > 16000:
            raise RuntimeError("remote summary unavailable")
        response = json.loads(result.stdout)
        if response.get("ok") is not True or not isinstance(response.get("text"), str) or not response["text"].strip():
            raise RuntimeError("remote summary unavailable")
        return response["text"].strip()[:400]
