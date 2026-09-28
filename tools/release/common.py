import hashlib
import json
import os
import re
from pathlib import Path
import subprocess
import tempfile

CHANNEL_MANIFEST = ".github/release-channel.json"
CHANNEL_SCHEMA = "kasaterm-release-channel/1"
MAC_FEED = "https://2rami.github.io/kasaterm/appcast.xml"
PREVIEW_FEED = "https://2rami.github.io/kasaterm/appcast-preview.xml"
WIN_FEED = "https://2rami.github.io/kasaterm/appcast-win.xml"


class Refused(Exception):
    """단계를 움직이지 않은 까닭 — 사람에게 그대로 보인다."""


class Pending(Exception):
    """아직 차례가 아니다(CI 가 도는 중, 피드가 아직 안 올라옴). 실패가 아니고, 다시 돌리면 이어서 본다."""


def version_tuple(text):
    m = re.fullmatch(r"v?(\d+)\.(\d+)\.(\d+)", (text or "").strip())
    return tuple(int(x) for x in m.groups()) if m else None


def version_text(parts):
    return ".".join(str(x) for x in parts)


def sha256_bytes(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return "sha256:" + h.hexdigest()


def save_json_atomic(path, document):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=path.parent,
                                         prefix=f".{path.name}.", delete=False) as output:
            temporary = Path(output.name)
            os.fchmod(output.fileno(), 0o600)
            json.dump(document, output, ensure_ascii=False, indent=2)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def fetch_feed(http, src, allow_missing=False):
    """피드 원문 bytes. 파일 경로도 받는다(검사·로컬 확인용). 못 읽으면 None."""
    if not src:
        return None
    if re.match(r"https?://", src):
        status, raw = http.get(src, timeout=15)
        return raw if status == 200 else (b"" if allow_missing and status == 404 else None)
    try:
        return Path(src).read_bytes()
    except FileNotFoundError:
        return b"" if allow_missing else None
    except OSError:
        return None


def feed_item(xml):
    """appcast 의 최신 항목 — CI 는 한 건만 싣는다(release.yml). 속성 순서는 mac·win 이 다르다."""
    text = xml.decode(errors="replace") if isinstance(xml, bytes) else (xml or "")
    ver = re.search(r"<sparkle:version>([^<]+)</sparkle:version>", text)
    enc = re.search(r"<enclosure\b([^>]*)/?>", text, re.S)
    attrs = dict(re.findall(r'([\w:]+)="([^"]*)"', enc.group(1))) if enc else {}
    length = attrs.get("length", "")
    return {"version": ver.group(1).strip() if ver else None, "url": attrs.get("url"),
            "length": int(length) if length.isdigit() else None, "signature": attrs.get("sparkle:edSignature")}


def feed_fingerprint(http, mac, win=None):
    sources = {"macos": fetch_feed(http, mac, allow_missing=win is None)}
    if win is not None:
        sources["windows"] = fetch_feed(http, win)
    if any(raw is None for raw in sources.values()):
        return None, None, None
    digest = sha256_bytes(json.dumps({key: sha256_bytes(raw) for key, raw in sources.items()}, sort_keys=True).encode())
    return digest, feed_item(sources["macos"])["version"], feed_item(sources.get("windows"))["version"]


def channel_manifest(channel, tag, source_commit, platforms=None):
    allowed = ["macos"] if channel == "preview" else ["macos", "windows"]
    manifest = {"schema": CHANNEL_SCHEMA, "channel": channel, "tag": tag,
                "source_commit": source_commit, "platforms": allowed if platforms is None else platforms}
    validate_channel_manifest(manifest, tag, source_commit)
    return manifest


def validate_channel_manifest(manifest, tag, source_commit):
    if not re.fullmatch(r"v\d+\.\d+\.\d+", tag or ""):
        raise Refused("릴리스 태그는 vX.Y.Z 여야 한다")
    if manifest is None:
        return "stable", ["macos", "windows"]
    if not isinstance(manifest, dict) or set(manifest) != {"schema", "channel", "tag", "source_commit", "platforms"}:
        raise Refused("release channel manifest 형식이 다르다")
    if manifest["schema"] != CHANNEL_SCHEMA or manifest["channel"] not in ("stable", "preview"):
        raise Refused("release channel manifest 채널·schema가 다르다")
    if manifest["tag"] != tag or manifest["source_commit"] != source_commit or not re.fullmatch(r"[0-9a-f]{40}", source_commit or ""):
        raise Refused("release channel manifest가 태그·버전 커밋의 부모와 다르다")
    platforms = manifest["platforms"]
    if platforms not in (["macos"], ["macos", "windows"]) or (manifest["channel"] == "preview" and platforms != ["macos"]):
        raise Refused("preview는 macOS만 게시할 수 있다")
    return manifest["channel"], platforms


def ci_release_context(repo, tag, requested_platforms="both"):
    repo = Path(repo)
    if requested_platforms not in ("both", "macos"):
        raise Refused("모르는 릴리스 플랫폼")
    commit = subprocess.check_output(["git", "-C", str(repo), "rev-parse", "HEAD"], text=True).strip()
    channel, platforms = "stable", ["macos", "windows"]
    if tag:
        cargo = re.search(r'^version = "([^"]+)"', (repo / "Cargo.toml").read_text(), re.M)
        if not cargo or "v" + cargo.group(1) != tag or not re.fullmatch(r"v\d+\.\d+\.\d+", tag):
            raise Refused("태그와 Cargo.toml 버전이 다르다")
        manifest_path = repo / CHANNEL_MANIFEST
        if manifest_path.exists():
            try:
                manifest = json.loads(manifest_path.read_text())
            except (OSError, ValueError) as error:
                raise Refused("release channel manifest를 읽지 못했다") from error
            parent = subprocess.check_output(["git", "-C", str(repo), "rev-parse", "HEAD^"], text=True).strip()
            channel, platforms = validate_channel_manifest(manifest, tag, parent)
    effective = "macos" if channel == "preview" or platforms == ["macos"] or requested_platforms == "macos" else "both"
    return {"channel": channel, "platforms": effective, "commit": commit,
            "prerelease": "true" if channel == "preview" else "false",
            "feed_path": "docs/appcast-preview.xml" if channel == "preview" else "docs/appcast.xml"}


def stage_appcasts(repo, tag, channel, mac_source, windows_source=None, slug="2rami/kasaterm"):
    """모든 후보를 먼저 검증한다. 공개 피드는 이후 한 git 커밋의 CAS push로만 바뀐다."""
    repo = Path(repo)
    if channel not in ("stable", "preview") or not re.fullmatch(r"v\d+\.\d+\.\d+", tag or ""):
        raise Refused("게시 채널·태그가 올바르지 않다")
    if channel == "preview" and windows_source:
        raise Refused("preview 게시가 Windows 피드를 바꿀 수 없다")
    candidates = [("docs/appcast-preview.xml" if channel == "preview" else "docs/appcast.xml", mac_source, f"kasaterm-{tag}.dmg")]
    if windows_source:
        candidates.append(("docs/appcast-win.xml", windows_source, f"kasaterm-{tag}-windows-x86_64.msi"))
    replacements = []
    for relative, source, name in candidates:
        raw = Path(source).read_bytes()
        candidate = feed_item(raw)
        expected_url = f"https://github.com/{slug}/releases/download/{tag}/{name}"
        if (candidate["version"] != tag[1:] or candidate["url"] != expected_url
                or not candidate["signature"] or not candidate["length"]):
            raise Refused(f"{relative}의 버전·서명·산출물 주소가 릴리스와 다르다")
        destination = repo / relative
        existing_raw = destination.read_bytes() if destination.exists() else b""
        existing = feed_item(existing_raw)
        previous, next_version = version_tuple(existing["version"]), version_tuple(candidate["version"])
        if previous and previous >= next_version:
            if existing == candidate:
                continue
            raise Refused(f"{relative}를 같은 판의 다른 파일이나 이전 판으로 덮을 수 없다")
        replacements.append((destination, raw))
    for destination, raw in replacements:
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(raw)
    return [str(path.relative_to(repo)) for path, _ in replacements]


def main(argv=None):
    import argparse
    parser = argparse.ArgumentParser(description="릴리스 manifest·appcast 로컬 검증")
    sub = parser.add_subparsers(dest="command", required=True)
    context = sub.add_parser("ci-context")
    context.add_argument("--repo", default=".")
    context.add_argument("--tag", default="")
    context.add_argument("--platforms", default="both")
    feeds = sub.add_parser("stage-appcasts")
    feeds.add_argument("--repo", default=".")
    feeds.add_argument("--tag", required=True)
    feeds.add_argument("--channel", required=True)
    feeds.add_argument("--mac", required=True)
    feeds.add_argument("--windows")
    args = parser.parse_args(argv)
    try:
        if args.command == "ci-context":
            for key, value in ci_release_context(args.repo, args.tag, args.platforms).items():
                print(f"{key}={value}")
        else:
            for path in stage_appcasts(args.repo, args.tag, args.channel, args.mac, args.windows):
                print(path)
    except (Refused, OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
