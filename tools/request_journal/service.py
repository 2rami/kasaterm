"""Local service lifecycle; launchd mutations require an explicit --apply."""

import argparse
import json
import os
from pathlib import Path
import plistlib
import signal
import subprocess
import sys
import threading
import tempfile

from .server import JournalServer

LABEL = "com.kasaterm.request-journal"
ROOT = Path(__file__).resolve().parents[2]
DEFAULT_DATA = Path.home() / ".config/kasaterm/request-journal"


def launchd_spec(project, data_dir, port, interval, base_url="http://127.0.0.1:8765", llm=False, nacho_repo=None):
    spec = {
        "Label": LABEL,
        "ProgramArguments": [str(Path(sys.executable).resolve()), "-m", "tools.request_journal", "run", "--project", str(Path(project).resolve()), "--data-dir", str(Path(data_dir).resolve()), "--port", str(port), "--interval", str(interval), "--base-url", base_url, "--collect"],
        "WorkingDirectory": str(ROOT),
        "RunAtLoad": True,
        "KeepAlive": {"SuccessfulExit": False},
        "ThrottleInterval": 10,
        "ProcessType": "Background",
        "StandardOutPath": str(Path(data_dir).resolve() / "service.log"),
        "StandardErrorPath": str(Path(data_dir).resolve() / "service.log"),
    }
    if llm:
        spec["ProgramArguments"].append("--llm")
    if nacho_repo:
        spec["ProgramArguments"].extend(["--nacho-repo", str(Path(nacho_repo).resolve())])
    return spec


def install(args):
    target = Path.home() / "Library/LaunchAgents" / f"{LABEL}.plist"
    spec = launchd_spec(args.project, args.data_dir, args.port, args.interval, args.base_url, args.llm, args.nacho_repo)
    if not args.apply:
        print(plistlib.dumps(spec).decode())
        return 0
    if sys.platform != "darwin":
        raise RuntimeError("launchd requires macOS")
    Path(args.data_dir).mkdir(parents=True, exist_ok=True, mode=0o700)
    target.parent.mkdir(parents=True, exist_ok=True)
    # Refuse to replace another running installation before its owner stops it.
    if target.exists():
        raise RuntimeError("service is already installed; stop it before changing its installation")
    with target.open("xb") as file:
        file.write(plistlib.dumps(spec))
    target.chmod(0o600)
    result = subprocess.run(["launchctl", "bootstrap", f"gui/{os.getuid()}", str(target)], capture_output=True)
    if result.returncode:
        target.unlink()
        raise RuntimeError("launchd bootstrap failed; installation rolled back")
    print(f"Installed {LABEL}")
    return 0


def stop(args):
    if not args.apply:
        print(f"Would stop {LABEL}; pass --apply to stop it")
        return 0
    if sys.platform != "darwin":
        raise RuntimeError("launchd requires macOS")
    result = subprocess.run(["launchctl", "bootout", f"gui/{os.getuid()}/{LABEL}"], capture_output=True)
    if result.returncode:
        raise RuntimeError("service was not running or could not be stopped")
    print(f"Stopped {LABEL}; the launch agent file is retained")
    return 0


def run(args):
    from .store import Store
    data_dir = Path(args.data_dir).resolve()
    data_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
    db = data_dir / "journal.sqlite3"
    store = Store(db_path=db)
    factory = lambda: store
    server = JournalServer(factory, args.project, args.port)
    discovery = {"version": 1, "base_url": f"http://127.0.0.1:{server.server_port}", "project": str(Path(args.project).resolve())}
    fd, temporary = tempfile.mkstemp(prefix=".service-", dir=data_dir)
    with os.fdopen(fd, "w") as file:
        json.dump(discovery, file)
    os.replace(temporary, data_dir / "service.json")
    stopped = threading.Event()
    worker = None
    if args.collect:
        from .collector import Collector
        def collect():
            server.collector_status = "starting"
            try:
                collector = Collector(factory(), project=str(Path(args.project).resolve()), base_url=args.base_url)
            except Exception:
                server.collector_status = "failed"
                return
            while not stopped.is_set():
                try:
                    collector.poll_once()
                    server.collector_status = "running"
                except Exception:
                    server.collector_status = "retrying"
                    print("request-journal: collection failed; retrying", file=sys.stderr, flush=True)
                stopped.wait(args.interval)
        worker = threading.Thread(target=collect, name="journal-collector", daemon=True)
        worker.start()
    def summarize():
        try:
            from .summarizer import Summarizer
            from .nacho import NachoProvider
            provider = NachoProvider.from_environment(repo=args.nacho_repo) if args.llm else None
            summarizer = Summarizer(provider=provider)
        except Exception:
            server.summarizer_status = {"provider": "unavailable", "error": "summary_initialization_failed"}
            return
        while not stopped.is_set():
            try:
                result = summarizer.update(factory(), project=server.project, limit=8)
                server.summarizer_status = {key: result[key] for key in ("provider", "updated", "skipped") if key in result}
                if result.get("error"):
                    server.summarizer_status["error"] = "summary_unavailable"
            except Exception:
                server.summarizer_status = {"provider": "unavailable", "error": "summary_unavailable"}
            stopped.wait(30)
    summary_worker = threading.Thread(target=summarize, name="journal-summarizer", daemon=True)
    summary_worker.start()
    def shutdown(_signum, _frame):
        stopped.set()
        threading.Thread(target=server.shutdown, daemon=True).start()
    signal.signal(signal.SIGTERM, shutdown)
    signal.signal(signal.SIGINT, shutdown)
    print(json.dumps({"service": LABEL, "url": f"http://127.0.0.1:{server.server_port}", "collecting": args.collect}), flush=True)
    try:
        server.serve_forever(poll_interval=0.25)
    finally:
        stopped.set()
        server.server_close()
        if worker:
            worker.join(timeout=2)
        summary_worker.join(timeout=2)
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(description="Local request journal")
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("run", "install"):
        command = commands.add_parser(name)
        command.add_argument("--project", default=str(Path.cwd()))
        command.add_argument("--data-dir", type=Path, default=DEFAULT_DATA)
        command.add_argument("--port", type=int, default=0)
        command.add_argument("--interval", type=float, default=5)
        command.add_argument("--base-url", default="http://127.0.0.1:8765")
        command.add_argument("--llm", action="store_true")
        command.add_argument("--nacho-repo", type=Path)
        command.add_argument("--collect" if name == "run" else "--apply", action="store_true")
    commands.add_parser("stop").add_argument("--apply", action="store_true")
    args = parser.parse_args(argv)
    if getattr(args, "port", 0) not in range(0, 65536):
        parser.error("port must be between 0 (automatic) and 65535")
    if getattr(args, "interval", 1) < 0.5:
        parser.error("interval must be at least 0.5 seconds")
    try:
        return {"run": run, "install": install, "stop": stop}[args.command](args)
    except (OSError, RuntimeError) as exc:
        # Messages here describe lifecycle errors, never transcript content.
        print(f"request-journal: {exc}", file=sys.stderr)
        return 1
