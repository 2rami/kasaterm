"""One-time signed Mac receiver bootstrap; never terminates or relaunches an app.

The copied LaunchAgent helper is intentionally standard-library-only. It waits
for process-exit events, not recurring shell/ps polling, and preserves both apps.
"""

import argparse
import ctypes
import errno
import fcntl
import hashlib
import json
import os
from pathlib import Path
import plistlib
import pwd
import re
import select
import stat
import subprocess
import sys
import tempfile
import time
import uuid
from contextlib import contextmanager

TEAM = "L366799VND"
SCHEMA = "kasaterm-receiver-bootstrap/1"
KINDS = {"kasaterm.app": ("com.kasa.kasaterm", "kasaterm"),
         "kasaviewer.app": ("com.kasa.kasaterm.viewer", "kasaterm-viewer")}
TERMINAL = {"installed", "failed", "expired", "rollback_required"}


class Refused(Exception):
    pass


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def file_hash(path):
    hashed = hashlib.sha256()
    with open(path, "rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            hashed.update(block)
    return hashed.hexdigest()


def safe_path(value, exists=True):
    path = Path(value)
    if not path.is_absolute() or ".." in path.parts or path == Path("/"):
        raise Refused("absolute, narrow paths are required")
    if any(part.is_symlink() for part in (path, *path.parents)):
        raise Refused("symlink paths are not allowed")
    if exists and not path.exists():
        raise Refused("required path is missing")
    if path.resolve() != path:
        raise Refused("canonical paths are required")
    return path


def private_directory(path):
    path = safe_path(path, exists=False)
    path.mkdir(parents=True, exist_ok=True, mode=0o700)
    info = path.stat()
    if not path.is_dir() or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o700:
        raise Refused("bootstrap directory must be user-owned and mode 0700")
    return path


def atomic_bytes(path, body, mode=0o600):
    path = safe_path(path, exists=False)
    fd, temporary = tempfile.mkstemp(prefix="." + path.name + "-", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as target:
            os.fchmod(target.fileno(), mode)
            target.write(body)
            target.flush()
            os.fsync(target.fileno())
        os.replace(temporary, path)
        directory = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def atomic_json(path, value):
    atomic_bytes(path, json.dumps(value, ensure_ascii=False, indent=2).encode())


def read_private(path):
    path = safe_path(path)
    info = path.stat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o600:
        raise Refused("bootstrap record must be user-owned and mode 0600")
    try:
        return json.loads(path.read_bytes())
    except (ValueError, UnicodeError):
        raise Refused("bootstrap record is invalid") from None


def version(value):
    if not isinstance(value, str) or not re.fullmatch(r"\d+(?:\.\d+){0,3}", value):
        raise Refused("numeric bundle versions are required")
    parts = tuple(map(int, value.split(".")))
    return parts + (0,) * (4 - len(parts))


def safe_bundle_tree(app):
    for root, directories, files in os.walk(app, followlinks=False):
        for name in directories + files:
            item = Path(root) / name
            if item.is_symlink():
                if not item.exists() or not item.resolve().is_relative_to(app):
                    raise Refused("bundle contains an escaping or broken symlink")
            elif not (item.is_dir() or item.is_file()):
                raise Refused("bundle contains a special file")


class Runtime:
    def __init__(self):
        self.home = Path(pwd.getpwuid(os.getuid()).pw_dir).resolve()

    def machine(self):
        path = safe_path(self.home / ".config/kasaterm/machine-id")
        identity = path.read_text().strip()
        if not re.fullmatch(r"[A-Za-z0-9_-]{1,128}", identity):
            raise Refused("machine identity is invalid")
        return identity

    def command(self, argv):
        try:
            result = subprocess.run(argv, capture_output=True, text=True, timeout=120, check=False)
        except (OSError, subprocess.SubprocessError):
            raise Refused("bootstrap system command failed") from None
        if result.returncode:
            raise Refused("bootstrap system command failed")
        return result.stdout + result.stderr

    def signature(self, app):
        self.command(["/usr/bin/codesign", "--verify", "--deep", "--strict", str(app)])
        details = self.command(["/usr/bin/codesign", "--display", "--verbose=4", str(app)])
        teams = re.findall(r"^TeamIdentifier=(.+)$", details, re.M)
        if teams != [TEAM] or "Signature=adhoc" in details:
            raise Refused("bundle is not signed by the required team")

    def copy(self, source, destination):
        self.command(["/usr/bin/ditto", str(source), str(destination)])

    def processes(self):
        output = self.command(["/bin/ps", "-axww", "-o", "pid=,comm="])
        found = {}
        for line in output.splitlines():
            match = re.fullmatch(r"\s*(\d+)\s+(.+)", line)
            if match:
                found[int(match[1])] = match[2].strip()
        if not found:
            raise Refused("process inventory is unavailable")
        return found

    def wait_for_exit(self, pids, timeout):
        if not hasattr(select, "kqueue"):
            raise Refused("process-exit waiting requires macOS kqueue")
        changes = [select.kevent(pid, filter=select.KQ_FILTER_PROC,
                   flags=select.KQ_EV_ADD | select.KQ_EV_ENABLE | select.KQ_EV_ONESHOT,
                   fflags=select.KQ_NOTE_EXIT) for pid in pids]
        try:
            with select.kqueue() as queue:
                events = queue.control(changes, max(1, len(changes)), timeout)
                if any(event.flags & select.KQ_EV_ERROR and event.data not in (0, errno.ESRCH) for event in events):
                    raise Refused("process-exit observation failed")
        except OSError as error:
            if error.errno != errno.ESRCH:
                raise Refused("process-exit observation failed") from None

    def rename_atomic(self, source, destination, flags):
        if sys.platform != "darwin":
            raise Refused("atomic bundle replacement requires macOS")
        try:
            rename = ctypes.CDLL("/usr/lib/libSystem.B.dylib", use_errno=True).renamex_np
        except (OSError, AttributeError):
            raise Refused("atomic bundle replacement is unavailable") from None
        rename.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
        rename.restype = ctypes.c_int
        if rename(os.fsencode(source), os.fsencode(destination), flags):
            error = ctypes.get_errno()
            raise OSError(error, os.strerror(error))

    def exchange(self, source, destination):
        self.rename_atomic(source, destination, 0x2)  # RENAME_SWAP keeps both bundle names present.

    def rename_exclusive(self, source, destination):
        self.rename_atomic(source, destination, 0x4)  # RENAME_EXCL protects an unexpected destination.

    def launch(self, plist):
        self.command(["/bin/launchctl", "bootstrap", f"gui/{os.getuid()}", str(plist)])

    def job_registered(self, label):
        try:
            result = subprocess.run(["/bin/launchctl", "print", f"gui/{os.getuid()}/{label}"],
                                    capture_output=True, text=True, timeout=30, check=False)
        except (OSError, subprocess.SubprocessError):
            return None
        if result.returncode == 0:
            return True
        missing = f'Could not find service "{label}" in domain for user gui: {os.getuid()}'
        if result.returncode == 113 and not result.stdout and result.stderr.strip() in (
                missing, "Bad request.\n" + missing):
            return False
        return None


class Bootstrap:
    def __init__(self, state_dir=None, runtime=None):
        self.runtime = runtime or Runtime()
        home = self.runtime.home
        root = safe_path(state_dir or home / ".local/state/kasaterm-bootstrap", exists=False)
        if not root.is_relative_to(home) or any(root.is_relative_to(home / name)
                for name in ("Desktop", "Documents", "Downloads", "Applications")):
            raise Refused("bootstrap state must be outside protected folders and app bundles")
        if root in (home, home / ".config", home / ".local", home / ".local/state"):
            raise Refused("a dedicated bootstrap directory is required")
        self.root = private_directory(root)

    @contextmanager
    def locked(self):
        path = safe_path(self.root / "bootstrap.lock", exists=False)
        fd = os.open(path, os.O_CREAT | os.O_RDWR | getattr(os, "O_NOFOLLOW", 0), 0o600)
        try:
            info = os.fstat(fd)
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o600:
                raise Refused("bootstrap lock is unsafe")
            # The app and Viewer helpers may wake together; serialize the short swap, not their exit wait.
            fcntl.flock(fd, fcntl.LOCK_EX)
            yield
        finally:
            os.close(fd)

    def target(self, value):
        app = safe_path(value, exists=False)
        if app.name.lower() not in KINDS or app.parent not in (self.runtime.home / "Applications", Path("/Applications")):
            raise Refused("target must be the exact Kasaterm or KasaViewer application")
        if app.parent.stat().st_mode & 0o002:
            raise Refused("application directory must not be publicly writable")
        return app

    def inspect(self, app, kind):
        app = safe_path(app)
        if not app.is_dir():
            raise Refused("application bundle is missing")
        safe_bundle_tree(app)
        info_path = safe_path(app / "Contents/Info.plist")
        try:
            info = plistlib.loads(info_path.read_bytes())
        except (ValueError, plistlib.InvalidFileException):
            raise Refused("bundle metadata is invalid") from None
        bundle_id, executable = KINDS[kind]
        if info.get("CFBundleIdentifier") != bundle_id or info.get("CFBundleExecutable") != executable:
            raise Refused("bundle identifier or executable does not match its target")
        short, build = info.get("CFBundleShortVersionString"), info.get("CFBundleVersion")
        version(short)
        version(build)
        binary = safe_path(app / "Contents/MacOS" / executable)
        if not binary.is_file() or not os.access(binary, os.X_OK):
            raise Refused("bundle executable is invalid")
        self.runtime.signature(app)
        return {"bundle_id": bundle_id, "executable": executable, "version": short,
                "build": build, "team": TEAM, "sha256": file_hash(binary)}

    def record_path(self, plan_id):
        if not re.fullmatch(r"[0-9a-f]{24}", plan_id or ""):
            raise Refused("invalid bootstrap plan identifier")
        return self.root / (plan_id + ".json")

    def load(self, plan_id):
        record = read_private(self.record_path(plan_id))
        if not isinstance(record, dict) or not isinstance(record.get("plan"), dict):
            raise Refused("bootstrap record is invalid")
        core = record.get("plan", {})
        if record.get("seal") != digest(core) or core.get("schema") != SCHEMA or core.get("id") != plan_id:
            raise Refused("bootstrap plan was altered")
        if self.runtime.machine() != core.get("machine"):
            raise Refused("bootstrap plan belongs to another machine")
        app = self.target(core["app"])
        expected_stage, expected_backup = self.siblings(app, plan_id)
        if core.get("stage") != str(expected_stage) or core.get("backup") != str(expected_backup):
            raise Refused("bootstrap sibling paths were altered")
        if record.get("state") not in {"staged", "waiting", "swapping", "exchanged", "old_moved", "new_moved", *TERMINAL}:
            raise Refused("invalid bootstrap state")
        return record

    @staticmethod
    def siblings(app, plan_id):
        stem = app.stem.lower()
        return (app.parent / f".{stem}-bootstrap-{plan_id}.app",
                app.parent / f".{stem}-before-{plan_id}.app")

    def save(self, record, state):
        record["state"] = state
        atomic_json(self.record_path(record["plan"]["id"]), record)
        return self.summary(record)

    @staticmethod
    def summary(record):
        plan = record["plan"]
        return {"id": plan["id"], "state": record["state"], "app": plan["app"],
                "stage": plan["stage"], "backup": plan["backup"], "version": plan["new"]["version"],
                "sha256": plan["new"]["sha256"], "job": record.get("job"),
                "registration": record.get("registration", "uncertain" if record.get("job") else None)}

    def stage(self, app, source, expected_machine, expected_sha256, expected_version):
        with self.locked():
            if self.runtime.machine() != expected_machine:
                raise Refused("selected machine identity does not match")
            app, source = self.target(app), safe_path(source)
            if source == app or source.is_relative_to(app) or app.is_relative_to(source):
                raise Refused("source and target bundles must be separate")
            for path in self.root.glob("*.json"):
                pending = self.load(path.stem)
                if pending["plan"]["app"] == str(app) and pending["state"] not in TERMINAL:
                    raise Refused("target already has a pending bootstrap")
            old, new = self.inspect(app, app.name.lower()), self.inspect(source, app.name.lower())
            if new["sha256"] != expected_sha256 or new["version"] != expected_version:
                raise Refused("source differs from the explicitly pinned build")
            if version(new["version"]) < version(old["version"]) or version(new["build"]) < version(old["build"]):
                raise Refused("bootstrap must not downgrade an installed application")
            if new == old:
                raise Refused("the pinned application is already installed")
            plan_id = uuid.uuid4().hex[:24]
            staged, backup = self.siblings(app, plan_id)
            for sibling in (staged, backup):
                safe_path(sibling, exists=False)
                if sibling.exists():
                    raise Refused("bootstrap sibling already exists")
            self.runtime.copy(source, staged)
            if self.inspect(staged, app.name.lower()) != new:
                raise Refused("staged application differs from the pinned source")
            core = {"schema": SCHEMA, "id": plan_id, "machine": expected_machine, "app": str(app),
                    "stage": str(staged), "backup": str(backup), "old": old, "new": new}
            return self.save({"plan": core, "seal": digest(core)}, "staged")

    def busy(self, plan):
        executable = plan["new"]["executable"]
        exact = {str(Path(plan[key]) / "Contents/MacOS" / executable) for key in ("app", "stage", "backup")}
        needs_settings = executable == "kasaterm"
        return {pid: path for pid, path in self.runtime.processes().items()
                if path in exact or needs_settings and path.endswith("/Contents/MacOS/kasaterm")}

    def preview_settings(self):
        directory = safe_path(self.runtime.home / ".config/kasaterm", exists=False)
        directory.mkdir(parents=True, exist_ok=True, mode=0o700)
        path = safe_path(directory / "settings.json", exists=False)
        if path.exists():
            info = path.stat()
            if not path.is_file() or info.st_uid != os.getuid():
                raise Refused("settings file is not owned by this user")
            try:
                value = json.loads(path.read_bytes())
            except (ValueError, UnicodeError):
                raise Refused("settings are unreadable; no values were replaced") from None
            if not isinstance(value, dict):
                raise Refused("settings must be an object")
        else:
            value = {}
        value.update(update_channel="preview", automatic_update_on_quit=True)
        atomic_json(path, value)

    def install_when_closed(self, plan_id):
        with self.locked():
            record = self.load(plan_id)
            plan = record["plan"]
            if record["state"] in TERMINAL:
                return self.summary(record)
            app, staged, backup = (safe_path(plan[key], exists=False) for key in ("app", "stage", "backup"))
            if self.busy(plan):
                return self.save(record, "waiting")
            if backup.exists():
                # A prior interrupted swap is never treated as permission to overwrite either bundle.
                if not app.exists() and self.inspect(backup, app.name.lower()) == plan["old"]:
                    self.runtime.rename_exclusive(backup, app)
                    return self.save(record, "failed")
                if (app.exists() and not staged.exists()
                        and self.inspect(app, app.name.lower()) == plan["new"]
                        and self.inspect(backup, app.name.lower()) == plan["old"]):
                    return self.save(record, "installed")
                return self.save(record, "rollback_required")
            if (app.exists() and staged.exists()
                    and self.inspect(app, app.name.lower()) == plan["new"]
                    and self.inspect(staged, app.name.lower()) == plan["old"]):
                # A crash can precede the state write, so recovery follows both verified bundle pins.
                try:
                    return self.finish_exchange(record, app, staged, backup)
                except (OSError, Refused):
                    return self.rollback_exchange(record, app, staged, backup)
            if self.inspect(app, app.name.lower()) != plan["old"]:
                raise Refused("installed application changed after staging")
            if self.inspect(staged, app.name.lower()) != plan["new"]:
                raise Refused("staged application changed after staging")
            if self.busy(plan):
                return self.save(record, "waiting")
            if plan["new"]["executable"] == "kasaterm":
                self.preview_settings()
            if self.busy(plan):
                return self.save(record, "waiting")
            self.save(record, "swapping")
            try:
                self.runtime.exchange(app, staged)
                self.save(record, "exchanged")
                return self.finish_exchange(record, app, staged, backup)
            except (OSError, Refused):
                return self.rollback_exchange(record, app, staged, backup)

    def finish_exchange(self, record, app, staged, backup):
        plan = record["plan"]
        if self.busy(plan):
            return self.save(record, "waiting")
        if (self.inspect(app, app.name.lower()) != plan["new"]
                or self.inspect(staged, app.name.lower()) != plan["old"]):
            raise Refused("exchanged bundle verification failed")
        self.runtime.rename_exclusive(staged, backup)
        self.save(record, "new_moved")
        if (self.inspect(app, app.name.lower()) != plan["new"]
                or self.inspect(backup, app.name.lower()) != plan["old"]):
            raise Refused("installed bundle verification failed")
        return self.save(record, "installed")

    def rollback_exchange(self, record, app, staged, backup):
        plan = record["plan"]
        try:
            if self.busy(plan):
                return self.save(record, "rollback_required")
            installed = self.inspect(app, app.name.lower())
            if backup.exists():
                if (not staged.exists() and installed == plan["new"]
                        and self.inspect(backup, app.name.lower()) == plan["old"]):
                    return self.save(record, "installed")
            else:
                pending = self.inspect(staged, app.name.lower())
                if installed == plan["new"] and pending == plan["old"]:
                    self.runtime.exchange(app, staged)
                    installed = self.inspect(app, app.name.lower())
                    pending = self.inspect(staged, app.name.lower())
                if installed == plan["old"] and pending == plan["new"]:
                    return self.save(record, "failed")
        except (OSError, Refused):
            pass
        return self.save(record, "rollback_required")

    def wait(self, plan_id, seconds):
        if not 1 <= seconds <= 86400:
            raise Refused("wait duration must be between 1 second and 24 hours")
        deadline = time.monotonic() + seconds
        while True:
            result = self.install_when_closed(plan_id)
            if result["state"] != "waiting":
                return result
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                with self.locked():
                    return self.save(self.load(plan_id), "expired")
            pids = self.busy(self.load(plan_id)["plan"])
            if pids:
                self.runtime.wait_for_exit(pids, min(remaining, 60))

    def arm(self, plan_id, python, wait_seconds=86400):
        with self.locked():
            record = self.load(plan_id)
            if record["state"] in TERMINAL or record.get("job"):
                raise Refused("bootstrap plan is complete or already armed")
            python = safe_path(python)
            if not python.is_file() or not os.access(python, os.X_OK) or any(
                    python.is_relative_to(self.runtime.home / name) for name in ("Desktop", "Documents", "Downloads")):
                raise Refused("helper interpreter must be executable and outside protected folders")
            if not 1 <= wait_seconds <= 86400:
                raise Refused("invalid wait duration")
            helper_bytes = Path(__file__).read_bytes()
            helper_hash = hashlib.sha256(helper_bytes).hexdigest()
            helper = self.root / f"helper-{helper_hash}.py"
            if helper.exists():
                if file_hash(safe_path(helper)) != helper_hash:
                    raise Refused("bootstrap helper changed")
            else:
                atomic_bytes(helper, helper_bytes)
            agents = safe_path(self.runtime.home / "Library/LaunchAgents", exists=False)
            agents.mkdir(parents=True, exist_ok=True, mode=0o700)
            label = "com.kasaterm.bootstrap." + plan_id
            plist = safe_path(agents / (label + ".plist"), exists=False)
            if plist.exists():
                raise Refused("bootstrap LaunchAgent already exists")
            body = {"Label": label, "ProgramArguments": [str(python), str(helper), "--state-dir", str(self.root),
                    "install-when-closed", "--plan", plan_id, "--wait-seconds", str(wait_seconds)],
                    "RunAtLoad": True, "ProcessType": "Background", "WorkingDirectory": str(self.root),
                    "StandardOutPath": str(self.root / (plan_id + ".out")),
                    "StandardErrorPath": str(self.root / (plan_id + ".err"))}
            atomic_bytes(plist, plistlib.dumps(body))
            record["job"] = label
            record["registration"] = "uncertain"
            self.save(record, record["state"])
        # launchd may start the helper immediately; it must not inherit an occupied bootstrap lock.
        try:
            self.runtime.launch(plist)
        except (OSError, Refused):
            with self.locked():
                record = self.load(plan_id)
                if record.get("job") == label:
                    registered = self.runtime.job_registered(label)
                    if registered is False and record["state"] == "staged":
                        if safe_path(plist).read_bytes() == plistlib.dumps(body):
                            plist.unlink()
                            record.pop("job")
                            record.pop("registration", None)
                    elif registered is True:
                        record["registration"] = "registered"
                    self.save(record, record["state"])
            raise
        with self.locked():
            record = self.load(plan_id)
            record["registration"] = "registered"
            return self.save(record, record["state"])


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state-dir")
    commands = parser.add_subparsers(dest="command", required=True)
    stage = commands.add_parser("stage")
    for name in ("app", "source", "expected-machine", "expected-sha256", "expected-version"):
        stage.add_argument("--" + name, required=True)
    status = commands.add_parser("status")
    status.add_argument("--plan", required=True)
    install = commands.add_parser("install-when-closed")
    install.add_argument("--plan", required=True)
    install.add_argument("--wait-seconds", type=int, default=0)
    arm = commands.add_parser("arm")
    arm.add_argument("--plan", required=True)
    arm.add_argument("--python", required=True)
    arm.add_argument("--wait-seconds", type=int, default=86400)
    args = parser.parse_args(argv)
    try:
        bootstrap = Bootstrap(args.state_dir)
        if args.command == "stage":
            result = bootstrap.stage(args.app, args.source, args.expected_machine, args.expected_sha256, args.expected_version)
        elif args.command == "status":
            result = bootstrap.summary(bootstrap.load(args.plan))
        elif args.command == "arm":
            result = bootstrap.arm(args.plan, args.python, args.wait_seconds)
        else:
            result = bootstrap.wait(args.plan, args.wait_seconds) if args.wait_seconds else bootstrap.install_when_closed(args.plan)
        print(json.dumps(result, ensure_ascii=False))
        return 0 if result["state"] not in {"failed", "rollback_required", "expired"} else 1
    except Refused as error:
        print(json.dumps({"state": "refused", "error": str(error)}))
        return 2
    except (OSError, KeyError, TypeError, ValueError):
        # Commands and configuration can contain credentials; only fixed diagnostics leave the helper.
        print(json.dumps({"state": "refused", "error": "bootstrap safety check failed; inspect paths, signatures, identity and state"}))
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
