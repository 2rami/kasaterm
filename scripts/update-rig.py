#!/usr/bin/env python3
"""Sparkle 업데이트 리그 — 시험 키로 서명한 로컬 피드와 격리 번들 두 판(0.0.1·0.0.2)을 만든다.

절차와 함정은 docs/verify-app.md 「업데이트 리그」. 번들 뼈대는 설치본을 복사만 하고, 실행파일만
갈아 끼운 뒤 번들 id·이름·공개키·LSEnvironment 를 리그 것으로 바꿔 ad-hoc 서명한다.

  python3 scripts/update-rig.py --root /tmp/<이름>-up --binary target/debug/kasaterm
  (cd /tmp/<이름>-up/serve && python3 -m http.server <port> --bind 127.0.0.1) &
"""
import argparse, base64, json, plistlib, shutil, socket, subprocess, sys
from pathlib import Path
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives import serialization

BUNDLE_ID = "com.kasa.kasaterm.updaterig"
NAME = "kasaterm-rig"


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def rig_env(root: Path, feed: str):
    cfg = root / "cfg"
    return {
        "KASATERM_SESSION_FILE": str(cfg / "session.json"),
        "KASATERM_SETTINGS_FILE": str(cfg / "settings.json"),
        "KASATERM_WINDOW_FILE": str(cfg / "window.json"),
        "KASATERM_AUTORESTORE": "fresh",
        "KASATERM_STUDENTS_DIR": str(cfg / "students"),
        "KASATERM_KASANET_KEY": str(cfg / "kasanet.key"),
        "KASATERM_KASANET": "off",
        "KASATERM_MACHINES": "[]",
        "KASATERM_COLLAB_ROOT": str(cfg / "collab"),
        "KASATERM_SOCKET_PATH": str(root / "rig.sock"),
        "TMPDIR": str(root / "tmp") + "/",
        "KASATERM_UPDATE_RIG_FEED": feed,
        "KASATERM_AUTOQUIT_MS": "900000",
        "KASATERM_CWD": str(root / "tmp"),
    }


def make_bundle(template: Path, binary: Path, dest: Path, version: str, pub: str, env: dict):
    if dest.exists():
        shutil.rmtree(dest)
    dest.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(["ditto", str(template), str(dest)], check=True)
    exe = dest / "Contents/MacOS/kasaterm"
    shutil.copy2(binary, exe)
    # 설치본이 본 레포 dist 를 보며 자기설치를 시도하지 않게.
    (dest / "Contents/Resources/build-root").unlink(missing_ok=True)
    info_path = dest / "Contents/Info.plist"
    info = plistlib.loads(info_path.read_bytes())
    info.update({
        "CFBundleIdentifier": BUNDLE_ID, "CFBundleName": NAME, "CFBundleDisplayName": NAME,
        "CFBundleVersion": version, "CFBundleShortVersionString": version,
        "SUPublicEDKey": pub, "LSEnvironment": env,
    })
    info_path.write_bytes(plistlib.dumps(info))
    subprocess.run(["codesign", "--force", "--deep", "-s", "-", str(dest)], check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", required=True)
    ap.add_argument("--binary", required=True, help="old 판 실행파일")
    ap.add_argument("--new-binary", help="new 판 실행파일(없으면 --binary)")
    ap.add_argument("--template", default=str(Path.home() / "Applications/kasaterm.app"))
    ap.add_argument("--port", type=int)
    ap.add_argument("--extra-env", action="append", default=[], help="KEY=VALUE, 새 판 LSEnvironment 에만")
    args = ap.parse_args()
    root = Path(args.root)
    root.mkdir(parents=True, exist_ok=True)
    for d in ["cfg/students", "cfg/collab", "tmp", "serve", "build"]:
        (root / d).mkdir(parents=True, exist_ok=True)
    key = Ed25519PrivateKey.generate()
    pub = base64.b64encode(key.public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw)).decode()
    port = args.port or free_port()
    feed = f"http://127.0.0.1:{port}/appcast.xml"
    env = rig_env(root, feed)
    (root / "cfg/settings.json").write_text(json.dumps(
        {"update_channel": "preview", "automatic_update_on_quit": True, "default_cwd": str(root / "tmp")}))

    template = Path(args.template)
    make_bundle(template, Path(args.binary), root / "app" / f"{NAME}.app", "0.0.1", pub, env)
    new = root / "build" / f"{NAME}.app"
    new_env = dict(env, **dict(kv.split("=", 1) for kv in args.extra_env))
    make_bundle(template, Path(args.new_binary or args.binary), new, "0.0.2", pub, new_env)
    dmg = root / "serve" / f"{NAME}-0.0.2.dmg"
    dmg.unlink(missing_ok=True)
    subprocess.run(["hdiutil", "create", "-quiet", "-volname", NAME, "-srcfolder", str(new),
                    "-format", "ULFO", "-ov", str(dmg)], check=True)
    data = dmg.read_bytes()
    sig = base64.b64encode(key.sign(data)).decode()
    (root / "serve/appcast.xml").write_text(f"""<?xml version="1.0" standalone="yes"?>
<rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" version="2.0">
    <channel>
        <title>{NAME}</title>
        <item>
            <title>0.0.2</title>
            <pubDate>Fri, 02 Oct 2026 05:00:00 +0000</pubDate>
            <sparkle:version>0.0.2</sparkle:version>
            <sparkle:shortVersionString>0.0.2</sparkle:shortVersionString>
            <sparkle:minimumSystemVersion>11.0</sparkle:minimumSystemVersion>
            <enclosure url="http://127.0.0.1:{port}/{dmg.name}" length="{len(data)}" type="application/octet-stream" sparkle:edSignature="{sig}"/>
        </item>
    </channel>
</rss>
""")
    (root / "rig.env").write_text("".join(f"export {k}={json.dumps(v)}\n" for k, v in env.items()))
    print(json.dumps({"port": port, "feed": feed, "app": str(root / "app" / f"{NAME}.app"),
                      "dmg_bytes": len(data)}))


if __name__ == "__main__":
    sys.exit(main())
