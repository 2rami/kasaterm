import hashlib
import re
from pathlib import Path


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


def fetch_feed(http, src):
    """피드 원문 bytes. 파일 경로도 받는다(검사·로컬 확인용). 못 읽으면 None."""
    if not src:
        return None
    if re.match(r"https?://", src):
        status, raw = http.get(src, timeout=15)
        return raw if status == 200 else None
    try:
        return Path(src).read_bytes()
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
