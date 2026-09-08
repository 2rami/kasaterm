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
import time

from .server import JournalServer

LABEL = "com.kasaterm.request-journal"
ROOT = Path(__file__).resolve().parents[2]
DEFAULT_DATA = Path.home() / ".config/kasaterm/request-journal"


def launchd_spec(project, data_dir, port, interval, base_url="http://127.0.0.1:8765", llm=False, nacho_repo=None, nacho_http=None):
    if llm and nacho_http:
        raise ValueError("choose either --llm or --nacho-http")
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
    if nacho_http:
        spec["ProgramArguments"].extend(["--nacho-http", nacho_http])
    return spec


def write_private(path, content):
    fd, temporary = tempfile.mkstemp(prefix=".journal-", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as file:
            file.write(content)
            file.flush()
            os.fsync(file.fileno())
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def matching_installation(previous, spec):
    old_args, new_args = previous.get("ProgramArguments", []), spec["ProgramArguments"]
    def value(args, flag):
        try:
            return args[args.index(flag) + 1]
        except (ValueError, IndexError):
            return None
    return (previous.get("Label") == LABEL and previous.get("WorkingDirectory") == str(ROOT)
            and value(old_args, "-m") == "tools.request_journal"
            and all(value(old_args, flag) == value(new_args, flag) for flag in ("--project", "--data-dir")))


def bootstrap(target):
    # launchd may acknowledge bootout before its old registration is fully gone.
    for attempt in range(6):
        result = subprocess.run(["launchctl", "bootstrap", f"gui/{os.getuid()}", str(target)], capture_output=True)
        if not result.returncode:
            return result
        if attempt < 5:
            time.sleep(.2 * (attempt + 1))
    return result


def install(args):
    target = Path.home() / "Library/LaunchAgents" / f"{LABEL}.plist"
    spec = launchd_spec(args.project, args.data_dir, args.port, args.interval, args.base_url, args.llm, args.nacho_repo, getattr(args, "nacho_http", None))
    if not args.apply:
        print(plistlib.dumps(spec).decode())
        return 0
    if sys.platform != "darwin":
        raise RuntimeError("launchd requires macOS")
    Path(args.data_dir).mkdir(parents=True, exist_ok=True, mode=0o700)
    log_file = Path(args.data_dir) / "service.log"
    fd = os.open(log_file, os.O_CREAT | os.O_WRONLY, 0o600)
    os.close(fd)
    log_file.chmod(0o600)
    target.parent.mkdir(parents=True, exist_ok=True)
    previous = None
    if target.exists():
        if not getattr(args, "replace", False):
            raise RuntimeError("service is already installed; use --replace for this project's journal")
        previous = target.read_bytes()
        if not matching_installation(plistlib.loads(previous), spec):
            raise RuntimeError("existing launch agent belongs to another installation")
        stopped = subprocess.run(["launchctl", "bootout", f"gui/{os.getuid()}/{LABEL}"], capture_output=True)
        if stopped.returncode:
            raise RuntimeError("could not stop the existing journal; configuration unchanged")
    write_private(target, plistlib.dumps(spec))
    result = bootstrap(target)
    if result.returncode:
        if previous is not None:
            write_private(target, previous)
            restored = bootstrap(target)
            raise RuntimeError("replacement failed; previous journal restored" if not restored.returncode else "replacement failed; previous configuration restored but service is stopped")
        target.unlink()
        raise RuntimeError("launchd bootstrap failed; installation rolled back")
    print(f"Installed {LABEL}")
    return 0


def summary_provider(args, stopped):
    from .nacho import HTTPNachoProvider, NachoProvider
    if getattr(args, "nacho_http", None):
        return HTTPNachoProvider(base_url=args.nacho_http, cancel_event=stopped)
    return NachoProvider.from_environment(repo=args.nacho_repo) if args.llm else None


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
    stopped = threading.Event()
    from .chat import ChatManager
    server.chat = ChatManager(store, server.project, provider_factory=lambda cancel: summary_provider(args, cancel))
    worker = None
    runtime_worker = None
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
        def observe_runtime():
            try:
                from .runtime import RuntimeObserver
                observer = RuntimeObserver(store, project=server.project, base_url=args.base_url, machine="local")
                while not stopped.is_set():
                    try:
                        observer.poll_once()
                        server.runtime_status = "running"
                    except Exception:
                        server.runtime_status = "retrying"
                    stopped.wait(10)
            except Exception:
                server.runtime_status = "unavailable"
        runtime_worker = threading.Thread(target=observe_runtime, name="journal-runtime", daemon=True)
        runtime_worker.start()
    def summarize():
        try:
            from .summarizer import Summarizer
            provider = summary_provider(args, stopped)
            screen = getattr(provider, "_screen", None)
            code = getattr(screen, "__code__", None)
            server.summary_transport = {"provider": getattr(provider, "name", "structured-fallback"), "screen_lines": max((value for value in code.co_consts if type(value) is int and value >= 100), default=None) if code else None}
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
    # Discovery is also the readiness signal; publish only after cleanup handlers
    # can safely stop every initialized worker.
    write_private(data_dir / "service.json", json.dumps(discovery).encode())
    print(json.dumps({"service": LABEL, "url": f"http://127.0.0.1:{server.server_port}", "collecting": args.collect}), flush=True)
    try:
        server.serve_forever(poll_interval=0.25)
    finally:
        stopped.set()
        server.chat.close()
        server.server_close()
        if worker:
            worker.join(timeout=2)
        # HTTP summaries own a temporary remote terminal and must have time to
        # delete it after cancellation before the process exits.
        summary_worker.join(timeout=15)
        if runtime_worker:
            runtime_worker.join(timeout=2)
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
        command.add_argument("--nacho-http")
        command.add_argument("--nacho-repo", type=Path)
        command.add_argument("--collect" if name == "run" else "--apply", action="store_true")
        if name == "install":
            command.add_argument("--replace", action="store_true")
    commands.add_parser("stop").add_argument("--apply", action="store_true")
    args = parser.parse_args(argv)
    if getattr(args, "port", 0) not in range(0, 65536):
        parser.error("port must be between 0 (automatic) and 65535")
    if getattr(args, "interval", 1) < 0.5:
        parser.error("interval must be at least 0.5 seconds")
    if getattr(args, "llm", False) and getattr(args, "nacho_http", None):
        parser.error("choose either --llm or --nacho-http")
    try:
        return {"run": run, "install": install, "stop": stop}[args.command](args)
    except (OSError, RuntimeError) as exc:
        # Messages here describe lifecycle errors, never transcript content.
        print(f"request-journal: {exc}", file=sys.stderr)
        return 1
