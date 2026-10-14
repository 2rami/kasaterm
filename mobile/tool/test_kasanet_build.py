"""A stale static library must not hide a failed iOS compile."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class BuildFailureTest(unittest.TestCase):
    def test_failed_cargo_keeps_existing_framework_and_never_packages_stale_archive(self):
        with tempfile.TemporaryDirectory(prefix="kasa-ios-build-test-") as tmp:
            root = Path(tmp)
            script = root / "mobile/tool/kasanet.sh"
            script.parent.mkdir(parents=True)
            shutil.copyfile(Path(__file__).with_name("kasanet.sh"), script)
            old = root / "target/ios/aarch64-apple-ios/release/libkasa_net_ffi.a"
            old.parent.mkdir(parents=True)
            old.write_bytes(b"stale library")
            framework = root / "mobile/ios/KasaNet/KasaNet.xcframework/fixture"
            framework.parent.mkdir(parents=True)
            framework.write_bytes(b"previous framework")
            commands = root / "bin"
            commands.mkdir()
            for name, body in {
                "rustup": "exit 0",
                "cargo": "exit 17",
                "lipo": 'touch "$TEST_PACKAGED"; exit 0',
                "xcodebuild": 'touch "$TEST_PACKAGED"; exit 0',
            }.items():
                command = commands / name
                command.write_text(f"#!/bin/sh\n{body}\n", encoding="utf-8")
                command.chmod(0o700)
            marker = root / "packaged"
            result = subprocess.run(["bash", str(script)], capture_output=True, env={
                **os.environ, "PATH": f"{commands}:{os.environ['PATH']}",
                "TEST_PACKAGED": str(marker),
            })
            self.assertEqual(result.returncode, 17, result.stderr.decode())
            self.assertFalse(marker.exists())
            self.assertEqual(framework.read_bytes(), b"previous framework")


if __name__ == "__main__":
    unittest.main()
