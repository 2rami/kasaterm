import base64
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest


ACTION = Path(__file__).resolve().parent
URL = "https://mini.invalid/lfs"
VERSION = b"version https://git-lfs.github.com/spec/v1\n"


def pointer(data):
    return VERSION + "oid sha256:{}\nsize {}\n".format(
        hashlib.sha256(data).hexdigest(), len(data),
    ).encode()


def fake_command():
    command, args = sys.argv[2], sys.argv[3:]
    root = Path(os.environ["LFS_FIXTURE"])
    config = json.loads((root / "fixture.json").read_text())
    with (root / "calls.jsonl").open("a") as log:
        log.write(json.dumps([command] + args) + "\n")
    if command == "curl":
        assert args[-1] == URL + "/objects/batch"
        print(config["health"], end="")
        return 7 if config["health"] == "000" else 0
    if command == "sleep":
        return 0
    if args == ["config", "-f", ".lfsconfig", "--get", "lfs.url"]:
        print(URL)
        return 0
    settings = {}
    while args[:1] == ["-c"]:
        key, value = args[1].split("=", 1)
        settings[key] = value
        args = args[2:]
    if args == ["lfs", "install", "--local"]:
        return 0
    if args == ["lfs", "ls-files", "--json", "HEAD"]:
        assert settings == {"lfs.fetchinclude": "", "lfs.fetchexclude": ""}
        print(json.dumps({"files": config["files"]}))
        return 0
    if args[:3] == ["lfs", "checkout", "--"]:
        names = args[3:]
    elif args[:2] == ["lfs", "pull"]:
        assert settings["lfs.url"] == settings["remote.origin.lfsurl"] == URL
        assert settings["lfs.remote.autodetect"] == settings["lfs.remote.searchall"] == "false"
        assert settings["credential.helper"] == ""
        assert args[-1] == "origin" and "--include=" in args
        excludes = next(arg[len("--exclude="):] for arg in args if arg.startswith("--exclude="))
        prefixes = [part[:-2] for part in excludes.split(",") if part]
        names = [item["name"] for item in config["files"]
                 if not any(item["name"].startswith(prefix) for prefix in prefixes)]
        if config.get("pull_failure"):
            return 1
        for item in config["files"]:
            cached = root / "objects" / item["oid"]
            if (item["name"] in names and not cached.exists()
                    and not config.get("leave_missing")):
                cached.write_bytes(base64.b64decode(config["payloads"][item["name"]]))
    else:
        raise AssertionError("unexpected fake git command: {!r}".format(args))
    for item in config["files"]:
        target = root / item["name"]
        cached = root / "objects" / item["oid"]
        if item["name"] in names and cached.exists():
            if not target.exists() or target.read_bytes().startswith(VERSION):
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(cached, target)
    return 0


class ActionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="lfs-action-fixture-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.payloads = {
            "assets/one.bin": b"first required asset",
            "assets/그림 with spaces.bin": b"second required asset",
            "mobile/skip.bin": b"optional mobile asset",
        }
        self.config = {
            "health": "000",
            "files": [],
            "payloads": {},
        }
        (self.root / "objects").mkdir()
        (self.root / "bin").mkdir()
        for name, data in self.payloads.items():
            oid = hashlib.sha256(data).hexdigest()
            self.config["files"].append({
                "name": name, "oid": oid, "oid_type": "sha256", "size": len(data),
                "checkout": True, "downloaded": True,
            })
            self.config["payloads"][name] = base64.b64encode(data).decode()
            target = self.root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(pointer(data))
            (self.root / "objects" / oid).write_bytes(data)
        for name in ("git", "curl", "sleep"):
            wrapper = self.root / "bin" / name
            wrapper.write_text('#!/bin/sh\nexec "$LFS_FIXTURE_PYTHON" "$LFS_FIXTURE_DRIVER" --fake-command '
                               + name + ' "$@"\n')
            wrapper.chmod(0o755)
        self.env = dict(os.environ, LFS_FIXTURE=str(self.root),
                        LFS_FIXTURE_PYTHON=sys.executable, LFS_FIXTURE_DRIVER=str(Path(__file__).resolve()),
                        LFS_ACTION_PATH=str(ACTION), SKIP="",
                        PATH=str(self.root / "bin") + os.pathsep + os.environ["PATH"])

    def cached(self, name):
        return self.root / "objects" / hashlib.sha256(self.payloads[name]).hexdigest()

    def run_action(self):
        (self.root / "fixture.json").write_text(json.dumps(self.config))
        script = textwrap.dedent((ACTION / "action.yml").read_text(encoding="utf-8").split("      run: |\n", 1)[1])
        return subprocess.run(["bash", "--noprofile", "--norc", "-e", "-o", "pipefail", "-c", script],
                              cwd=self.root, env=self.env, stdout=subprocess.PIPE,
                              stderr=subprocess.STDOUT, encoding="utf-8", timeout=90)

    def calls(self):
        return [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]

    def test_complete_cache_is_verified_without_network(self):
        result = self.run_action()
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertFalse(any(call[0] == "curl" or "pull" in call for call in self.calls()))
        for name, data in self.payloads.items():
            self.assertEqual((self.root / name).read_bytes(), data)

    def test_python_command_is_used_when_python3_cannot_run(self):
        unavailable = self.root / "bin" / "python3"
        unavailable.write_text("#!/bin/sh\nexit 1\n")
        unavailable.chmod(0o755)
        fallback = self.root / "bin" / "python"
        fallback.write_text('#!/bin/sh\nexec "$LFS_FIXTURE_PYTHON" "$@"\n')
        fallback.chmod(0o755)
        result = self.run_action()
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertFalse(any(call[0] == "curl" for call in self.calls()))

    def test_missing_cache_requires_health_and_fails_when_offline(self):
        self.cached("assets/one.bin").unlink()
        result = self.run_action()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("HTTP 000", result.stdout)
        self.assertTrue(any(call[0] == "curl" for call in self.calls()))
        self.assertFalse(any("pull" in call for call in self.calls()))

    def test_missing_cache_downloads_only_from_configured_mini(self):
        self.cached("assets/one.bin").unlink()
        self.config["health"] = "200"
        result = self.run_action()
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertEqual(sum("pull" in call for call in self.calls()), 1)
        self.assertEqual((self.root / "assets/one.bin").read_bytes(), self.payloads["assets/one.bin"])

    def test_same_size_corrupt_cache_is_rejected_after_pull(self):
        self.cached("assets/one.bin").write_bytes(b"x" * len(self.payloads["assets/one.bin"]))
        self.config["health"] = "200"
        result = self.run_action()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("SHA256 mismatch", result.stdout)
        self.assertTrue(any("pull" in call for call in self.calls()))

    def test_truncated_cache_is_rejected(self):
        self.cached("assets/one.bin").write_bytes(b"short")
        result = self.run_action()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("size mismatch", result.stdout)

    def test_non_ascii_failure_is_reported_under_windows_console_encoding(self):
        name = "assets/그림 with spaces.bin"
        self.cached(name).write_bytes(b"x" * len(self.payloads[name]))
        self.config["health"] = "200"
        self.env["PYTHONIOENCODING"] = "cp1252"
        result = self.run_action()
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn(name, result.stdout)
        self.assertIn("SHA256 mismatch", result.stdout)
        self.assertNotIn("UnicodeEncodeError", result.stdout)

    def test_modified_worktree_is_not_trusted_or_overwritten(self):
        bad = b"x" * len(self.payloads["assets/one.bin"])
        (self.root / "assets/one.bin").write_bytes(bad)
        self.config["health"] = "200"
        result = self.run_action()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("SHA256 mismatch", result.stdout)
        self.assertEqual((self.root / "assets/one.bin").read_bytes(), bad)

    def test_skip_keeps_excluded_pointer_without_network(self):
        self.env["SKIP"] = "mobile/"
        self.cached("mobile/skip.bin").unlink()
        result = self.run_action()
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertTrue((self.root / "mobile/skip.bin").read_bytes().startswith(VERSION))
        self.assertFalse(any(call[0] == "curl" for call in self.calls()))

    def test_skip_cannot_make_an_empty_verification_succeed(self):
        self.env["SKIP"] = "assets/ mobile/"
        result = self.run_action()
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn("no required LFS files", result.stdout)
        self.assertFalse(any(call[0] == "curl" for call in self.calls()))

    def test_skip_is_a_literal_directory_prefix(self):
        self.env["SKIP"] = "asset/"
        self.cached("assets/one.bin").unlink()
        self.assertNotEqual(self.run_action().returncode, 0)

    def test_successful_pull_cannot_hide_a_remaining_pointer(self):
        self.cached("assets/one.bin").unlink()
        self.config.update(health="200", leave_missing=True)
        result = self.run_action()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("LFS pointer remains", result.stdout)

    def test_failed_download_retries_and_still_fails_verification(self):
        self.cached("assets/one.bin").unlink()
        self.config.update(health="200", pull_failure=True)
        result = self.run_action()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(sum("pull" in call for call in self.calls()), 3)
        self.assertEqual(sum(call[0] == "sleep" for call in self.calls()), 2)


class RealLfsTests(unittest.TestCase):
    def test_real_checkout_and_manifest_reject_modified_content(self):
        with tempfile.TemporaryDirectory(prefix="lfs-real-fixture-") as directory:
            root = Path(directory)
            env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
            env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull,
                       GIT_CONFIG_SYSTEM=os.devnull, GIT_TERMINAL_PROMPT="0", GIT_LFS_SKIP_SMUDGE="1", SKIP="")
            hooks = root / "empty-hooks"
            hooks.mkdir()

            def git(*args):
                return subprocess.run(
                    ["git", "-c", "core.hooksPath=" + str(hooks), "-c", "commit.gpgsign=false",
                     "-c", "user.name=LFS Fixture", "-c", "user.email=fixture@example.invalid",
                     "-c", "core.autocrlf=false"] + list(args),
                    cwd=root, env=env, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                ).stdout

            git("init", "--quiet")
            git("lfs", "install", "--local", "--skip-repo")
            data = b"actual local git-lfs fixture asset"
            oid = hashlib.sha256(data).hexdigest()
            (root / ".gitattributes").write_text("*.bin filter=lfs -text\n")
            target = root / "asset.bin"
            target.write_bytes(pointer(data))
            git("add", ".gitattributes", "asset.bin")
            git("commit", "--quiet", "-m", "LFS fixture")
            cached = root / ".git" / "lfs" / "objects" / oid[:2] / oid[2:4] / oid
            cached.parent.mkdir(parents=True, exist_ok=True)
            cached.write_bytes(data)

            def verify(*args):
                return subprocess.run([sys.executable, str(ACTION / "verify.py")] + list(args),
                                      cwd=root, env=env, stdout=subprocess.PIPE,
                                      stderr=subprocess.STDOUT, encoding="utf-8", timeout=90)

            result = verify("--checkout")
            self.assertEqual(result.returncode, 0, result.stdout)
            self.assertEqual(target.read_bytes(), data)
            target.write_bytes(b"x" * len(data))
            entry = json.loads(git("lfs", "ls-files", "--json", "HEAD"))["files"][0]
            self.assertEqual((entry["oid"], entry["size"]), (oid, len(data)))
            changed = verify()
            self.assertIn("SHA256 mismatch", changed.stdout)
            self.assertEqual(changed.returncode, 1)
            target.write_bytes(pointer(data))
            cached.write_bytes(b"x" * len(data))
            self.assertEqual(verify("--checkout").returncode, 1)


if __name__ == "__main__":
    if sys.argv[1:2] == ["--fake-command"]:
        sys.exit(fake_command())
    unittest.main()
