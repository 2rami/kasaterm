"""Observe OS app epochs and verified build artifacts without restarting them."""

from datetime import datetime, timezone
import json
from pathlib import Path
import re
import subprocess
import urllib.parse
import urllib.request

from .build_manifest import file_hash, git, validate_manifest, verify_signature, _inventory
from .collector import NoRedirect, loopback_url
from .store import utc_now


class RuntimeObserver:
    def __init__(self, store, project, base_url="http://127.0.0.1:8765", *, machine="local", manifest_paths=None):
        self.store = store
        self.project = str(Path(project).expanduser().resolve())
        self.base_url = loopback_url(base_url)
        self.machine = machine
        self.manifest_paths = [Path(p) for p in (manifest_paths or [Path(self.project) / "dist/kasaterm.build.json"])]
        self._manifest_cache = {}
        self._git_cache = None
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())

    @staticmethod
    def _command(args):
        return subprocess.run(args, check=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                              timeout=10, env={"LC_ALL": "C", "PATH": "/usr/bin:/bin:/usr/sbin:/sbin"}).stdout.decode()

    def _listener_pid(self):
        port = urllib.parse.urlsplit(self.base_url).port or 80
        try:
            output = self._command(["/usr/sbin/lsof", "-nP", f"-iTCP:{port}", "-sTCP:LISTEN", "-Fp"])
            pids = {int(line[1:]) for line in output.splitlines() if re.fullmatch(r"p\d+", line)}
            return next(iter(pids)) if len(pids) == 1 else None
        except (OSError, ValueError, subprocess.SubprocessError):
            return None

    def _process(self, pid):
        try:
            output = self._command(["/bin/ps", "-ww", "-p", str(pid), "-o", "pid=,lstart=,comm="])
            fields = output.strip().split(None, 6)
            if len(fields) != 7 or int(fields[0]) != pid:
                return None
            executable = fields[6]
            if Path(executable).name != "kasaterm":
                return None
            local_start = datetime.strptime(" ".join(fields[1:6]), "%a %b %d %H:%M:%S %Y").astimezone()
            stamp = local_start.astimezone(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")
            return {"pid": pid, "started_at": stamp, "executable": executable,
                    "evidence": {"kind": "os_process", "start_source": "ps_lstart", "start_precision": "seconds"}}
        except (OSError, ValueError, subprocess.SubprocessError):
            return None

    def _version(self):
        with self.opener.open(self.base_url + "/version", timeout=5) as response:
            body = response.read(8193)
        if len(body) > 8192:
            raise ValueError("Version metadata too large")
        value = json.loads(body)
        if not isinstance(value, dict) or value.get("ok") is not True:
            raise ValueError("Version endpoint is unavailable")
        return {key: value.get(key) if isinstance(value.get(key), str) else None for key in ("build", "version", "machine_id")}

    def _running_component(self, process):
        """Hash only when OS mapped inode and unchanged pre-start file agree."""
        result = {"confidence": "unknown", "reason": "mapped_file_not_verified"}
        try:
            output = self._command(["/usr/sbin/lsof", "-a", "-p", str(process["pid"]), "-d", "txt", "-Ffni"])
            records, current = [], {}
            for line in output.splitlines():
                if line.startswith("f"):
                    if current:
                        records.append(current)
                    current = {}
                elif line.startswith("i"):
                    current["inode"] = int(line[1:])
                elif line.startswith("n"):
                    current["path"] = line[1:]
            if current:
                records.append(current)
            mapped = [r for r in records if r.get("path") == process["executable"]]
            if len(mapped) != 1:
                return result
            path = Path(process["executable"])
            stat = path.stat()
            started = datetime.fromisoformat(process["started_at"].replace("Z", "+00:00")).timestamp()
            if mapped[0].get("inode") != stat.st_ino:
                return dict(result, reason="executable_path_replaced")
            if stat.st_mtime > started:
                return dict(result, reason="executable_modified_after_start_or_same_second")
            return dict(file_hash(path), confidence="mapped_inode_and_prestart_file", inode=stat.st_ino,
                        observed_at=utc_now(), applies_to="main_executable_only")
        except (OSError, ValueError, subprocess.SubprocessError):
            return result

    def _builds(self):
        artifacts = []
        for path in self.manifest_paths:
            bundle = path.with_name("kasaterm.app")
            if not path.is_file():
                artifacts.append({"manifest_path": str(path), "bundle_path": str(bundle),
                                  "status": "legacy_unknown" if bundle.exists() else "not_present", "source_commit": None})
                continue
            try:
                stat = path.stat()
                key = (stat.st_ino, stat.st_size, stat.st_mtime_ns, tuple(_inventory(bundle)))
                cached = self._manifest_cache.get(str(path))
                if cached and cached[0] == key:
                    manifest = cached[1]
                else:
                    if stat.st_size > 8 * 1024 * 1024:
                        raise ValueError("Manifest is too large")
                    manifest = json.loads(path.read_text())
                    if not isinstance(manifest, dict) or manifest.get("project") != self.project or not validate_manifest(manifest, bundle) or verify_signature(bundle).get("verified") is not True:
                        raise ValueError("Ready manifest does not match the signed artifact")
                    self._manifest_cache[str(path)] = (key, manifest)
                self.store.record_build(manifest)
                artifacts.append({"manifest_path": str(path), "bundle_path": str(bundle), "status": "verified_ready",
                                  "build_id": manifest["id"], "source_commit": manifest["source"].get("source_commit"),
                                  "source_status": manifest["source"]["status"], "completed_at": manifest["completed_at"]})
            except (OSError, ValueError, KeyError, subprocess.SubprocessError):
                artifacts.append({"manifest_path": str(path), "bundle_path": str(bundle), "status": "unverified", "source_commit": None})
        return artifacts

    def _git(self, since):
        try:
            head = git(self.project, "rev-parse", "HEAD").decode().strip()
            dirty = bool(git(self.project, "status", "--porcelain=v1", "-z"))
            key = (head, dirty, since)
            if self._git_cache and self._git_cache[0] == key:
                return self._git_cache[1]
            commits = []
            if since:
                raw = git(self.project, "log", f"--since={since}", "--format=%H%x00%cI%x00%s").decode()
                for line in raw.splitlines():
                    fields = line.split("\0", 2)
                    if len(fields) == 3:
                        commits.append({"commit": fields[0], "committed_at": fields[1], "subject": fields[2], "evidence_kind": "git_commit_only"})
            value = {"head": head, "dirty": dirty, "commits_since_start": commits,
                     "since": since, "evidence_kind": "working_tree_not_running_code"}
            self._git_cache = (key, value)
            return value
        except (OSError, ValueError, subprocess.SubprocessError):
            return {"head": None, "dirty": None, "commits_since_start": [], "evidence_kind": "git_unavailable"}

    def poll_once(self):
        artifacts = self._builds()
        previous = self.store.get_last_run(self.project, self.machine)
        pid = self._listener_pid()
        process = self._process(pid) if pid else (self._process(previous["pid"]) if previous else None)
        reachable, version = False, {}
        if pid and process:
            try:
                version = self._version()
                # A listener replacement while querying must not mix two runs.
                check = self._process(pid)
                reachable = self._listener_pid() == pid and check is not None and check["started_at"] == process["started_at"]
            except (OSError, ValueError):
                pass
        if process is None:
            self.store.record_observation("runtime_unavailable", {"process_alive": False if previous and not self._process(previous["pid"]) else None,
                                          "reason": "no_verified_app_process", "artifacts": artifacts}, machine=self.machine, project=self.project)
            return {"observed": False, "backend_reachable": False, "artifacts": artifacts}
        same_run = previous and process["pid"] == previous["pid"] and process["started_at"] == previous["started_at"]
        component = ({"sha256": previous["component_sha256"], "confidence": "preserved_run_evidence"}
                     if same_run and previous.get("component_sha256") else self._running_component(process))
        builds = self.store.list_builds(self.project)
        candidates = [b["id"] for b in builds if component.get("sha256") and b.get("components", {}).get("app", {}).get("sha256") == component["sha256"]]
        linked = candidates[0] if len(candidates) == 1 else None
        process.update(project=self.project, machine=self.machine, backend_reachable=reachable,
                       build_id=version.get("build") if reachable and version.get("build") != "unknown" else None,
                       component_sha256=component.get("sha256"), linked_build_id=linked)
        process["evidence"].update(api=version if reachable else {}, running_component=component,
                                   linked_build_scope="main_executable_only", matching_build_count=len(candidates),
                                   build_id_confidence="runtime_api_stamp_only" if reachable else "unknown")
        run = self.store.record_app_run(process)
        evidence = {"run_id": run["id"], "backend_reachable": reachable, "process_alive": True,
                    "api": version if reachable else {}, "artifacts": artifacts, "git": self._git(run["started_at"])}
        self.store.record_observation("runtime", evidence, machine=self.machine, project=self.project)
        return {"observed": True, "run_id": run["id"], "backend_reachable": reachable, "artifacts": artifacts}
