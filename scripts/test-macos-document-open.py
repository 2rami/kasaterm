#!/usr/bin/env python3
"""Exercise the production Apple Event handler through an isolated app bundle."""

import argparse
import os
from pathlib import Path
import plistlib
import shutil
import signal
import subprocess
import tempfile
import time
import unicodedata
import uuid


def wait_for(log, predicate, deadline):
    while time.monotonic() < deadline:
        lines = log.read_text().splitlines() if log.exists() else []
        if predicate(lines):
            return lines
        time.sleep(0.05)
    raise RuntimeError(f"document-open probe timed out: {log.read_text() if log.exists() else 'no process log'}")


def document_paths(lines):
    # Apple Event file URLs can resolve /var to /private/var and decompose
    # Hangul. Compare file identities without mistaking normalization for loss.
    return [normalized_path(line.removeprefix("open ")) for line in lines if line.startswith("open ")]


def normalized_path(path):
    return unicodedata.normalize("NFC", os.path.realpath(path))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--resumed-only", action="store_true", help="reproduce the former late handler registration")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    rig = Path(tempfile.mkdtemp(prefix="kasaterm-open-probe-")).resolve()
    app = rig / "DocumentOpenProbe.app"
    executable = app / "Contents/MacOS/macos_open_probe"
    executable.parent.mkdir(parents=True)
    shutil.copy2(binary, executable)
    with (app / "Contents/Info.plist").open("wb") as output:
        plistlib.dump({
            "CFBundleIdentifier": f"com.kasa.document-open-probe.{uuid.uuid4().hex}",
            "CFBundleName": "DocumentOpenProbe",
            "CFBundleExecutable": executable.name,
            "CFBundlePackageType": "APPL",
            "NSPrincipalClass": "NSApplication",
            "LSUIElement": True,
            "CFBundleDocumentTypes": [{
                "CFBundleTypeName": "Markdown Document",
                "CFBundleTypeRole": "Viewer",
                "LSHandlerRank": "None",
                "LSItemContentTypes": ["net.daringfireball.markdown"],
                "CFBundleTypeExtensions": ["md", "markdown", "mdown", "mkd"],
            }],
        }, output)
    paths = [rig / "첫 문서.md", rig / "다음 문서.markdown", rig / "세 번째.mdown"]
    for path in paths:
        path.write_text("# Isolated open-document fixture\n", encoding="utf-8")
    log = rig / "events.log"
    deadline = time.monotonic() + 12
    command = ["/usr/bin/open", "-n", "-g", "--env", f"KASATERM_OPEN_PROBE_LOG={log}"]
    if args.resumed_only:
        command += ["--env", "KASATERM_OPEN_PROBE_RESUMED_ONLY=1"]
    command += ["-a", str(app), str(paths[0])]
    pid = None
    try:
        subprocess.run(command, check=True)
        lines = wait_for(log, lambda lines: any(line.startswith("pid ") for line in lines), deadline)
        pid = int(next(line.removeprefix("pid ") for line in lines if line.startswith("pid ")))
        wait_for(log, lambda lines: normalized_path(paths[0]) in document_paths(lines), deadline)
        subprocess.run(["/usr/bin/open", "-g", "-a", str(app), *(str(path) for path in paths[1:])], check=True)
        lines = wait_for(log, lambda lines: sum(line.startswith("open ") for line in lines) == 3, deadline)
        assert document_paths(lines) == [normalized_path(path) for path in paths], lines
        print(f"PASS: cold launch, warm launch, multiple files, Unicode and spaces; {log}")
    finally:
        # Only the PID reported by this fresh, uniquely identified test bundle
        # can be stopped; production apps are never matched by process name.
        if pid is not None:
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass


if __name__ == "__main__":
    main()
