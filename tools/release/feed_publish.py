"""검증한 산출물은 고정하고 main 경합만 다시 읽는 appcast 게시자."""

import argparse
from dataclasses import dataclass
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
import xml.etree.ElementTree as ET

from tools.release import deps
from tools.release.backend import ed25519_ok
from tools.release.common import (CHANNEL_MANIFEST, Refused, sha256_file,
                                  validate_channel_manifest, version_tuple)
from tools.release.proc import Http, Runner

SLUG = "2rami/kasaterm"
SPARKLE = "{http://www.andymatuschak.org/xml-namespaces/sparkle}"
PAGES_URL = "https://2rami.github.io/kasaterm/"


class CasExhausted(Refused):
    """산출물 검사가 아니라 main 경합만 재시도 한도를 넘었다."""


class PagesUnfinished(Refused):
    """피드 커밋은 올라갔고 Pages 빌드·공개 피드 확인만 못 끝냈다 — 게시 job 만 다시 돌리면 된다."""


# legacy Pages 빌드는 가끔 까닭 없이 실패하거나 15분씩 「빌드 중」에 머문다(2026-10-02·10-05). 다음 빌드는 대개 된다.
PAGES_REBUILDS = 2
PAGES_STALL = 18


def item(raw, empty=False):
    if not raw and empty:
        return None
    try:
        root = ET.fromstring(raw)
        channels = root.findall("channel")
        if root.tag != "rss" or len(channels) != 1:
            raise ValueError("channel")
        items = channels[0].findall("item")
        if not items and empty:
            return None
        if len(items) != 1:
            raise ValueError("item")
        versions = items[0].findall(SPARKLE + "version")
        enclosures = items[0].findall("enclosure")
        if len(versions) != 1 or len(enclosures) != 1:
            raise ValueError("version/enclosure")
        enclosure = enclosures[0]
        result = {"version": versions[0].text, "url": enclosure.get("url"),
                  "signature": enclosure.get(SPARKLE + "edSignature"), "length": int(enclosure.get("length", "0"))}
        if not version_tuple(result["version"]) or not result["signature"] or result["length"] <= 0:
            raise ValueError("identity")
        return result
    except (ET.ParseError, ValueError, TypeError) as error:
        raise Refused("appcast의 단일 항목·버전·서명·길이를 읽지 못했다") from error


@dataclass(frozen=True)
class Candidate:
    platform: str
    raw: bytes
    asset: Path
    sha256: str

    @classmethod
    def read(cls, platform, feed, asset, digest):
        if not re.fullmatch(r"(?:sha256:)?[0-9a-f]{64}", digest or ""):
            raise Refused("검증된 산출물의 SHA-256이 없다")
        return cls(platform, Path(feed).read_bytes(), Path(asset).resolve(),
                   digest if digest.startswith("sha256:") else "sha256:" + digest)

    def destination(self, channel):
        return ("docs/appcast-win.xml" if self.platform == "windows" else
                "docs/appcast-preview.xml" if channel == "preview" else "docs/appcast.xml")


class Publisher:
    def __init__(self, repo, tag, channel, commit, candidates, *, remote="origin", openssl=None):
        self.repo, self.tag, self.channel, self.commit = Path(repo), tag, channel, commit
        self.candidates, self.remote = tuple(candidates), remote
        if (not re.fullmatch(r"v\d+\.\d+\.\d+", tag or "") or channel not in ("stable", "preview")
                or not re.fullmatch(r"[0-9a-f]{40}", commit or "")):
            raise Refused("게시 태그·채널·검증 커밋이 올바르지 않다")
        platforms = [candidate.platform for candidate in self.candidates]
        if platforms not in (["macos"], ["macos", "windows"]) or (channel == "preview" and platforms != ["macos"]):
            raise Refused("preview는 macOS 피드만 게시할 수 있다")
        self.runner = Runner("local")
        tool = {"path": openssl} if openssl else deps.find_openssl(self.runner)
        if not tool.get("path"):
            raise Refused(tool["why"])
        self.openssl = tool["path"]

    def git(self, *args, data=None, extra=None, check=True):
        # CI의 피드 전용 push가 사용자/LFS hook을 실행하면 고정 산출물 밖의 작업까지 게시할 수 있다.
        result = subprocess.run(["git", "-c", "core.hooksPath=/dev/null", "-C", str(self.repo), *args],
                                input=data, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=120,
                                env={**os.environ, "GIT_TERMINAL_PROMPT": "0", "GIT_LFS_SKIP_SMUDGE": "1", **(extra or {})})
        if check and result.returncode:
            raise Refused("피드 게시 git 단계 실패: " + args[0])
        return result

    def read(self, *args):
        return self.git(*args).stdout.decode().strip()

    def current_main(self):
        self.git("fetch", "-q", "--no-tags", self.remote, "refs/heads/main")
        return self.read("rev-parse", "FETCH_HEAD^{commit}")

    def file_at(self, commit, relative):
        row = self.read("ls-tree", commit, "--", relative)
        if not row:
            return b""
        if not row.startswith("100644 blob "):
            raise Refused(f"{relative}가 일반 파일이 아니다")
        return self.git("show", f"{commit}:{relative}").stdout

    def release_view(self):
        result = self.runner.run(["gh", "release", "view", self.tag, "--repo", SLUG,
                                  "--json", "assets,isDraft,isPrerelease"], timeout=60)
        if not result.ok:
            raise Refused("게시 전 릴리스 신원·산출물을 다시 읽지 못했다")
        try:
            return json.loads(result.out)
        except ValueError as error:
            raise Refused("릴리스 응답이 JSON이 아니다") from error

    def validate(self, base, scratch):
        self.git("fetch", "-q", "--no-tags", "--depth=2", self.remote, "refs/tags/" + self.tag)
        if self.read("rev-parse", "FETCH_HEAD^{commit}") != self.commit:
            raise Refused("검증 뒤 릴리스 태그 커밋이 바뀌었다")
        cargo = self.git("show", f"{self.commit}:Cargo.toml").stdout.decode()
        version = re.search(r'^version = "([^\"]+)"', cargo, re.M)
        if not version or version[1] != self.tag[1:]:
            raise Refused("릴리스 태그와 Cargo 버전이 다르다")
        raw_manifest = self.file_at(self.commit, CHANNEL_MANIFEST)
        try:
            manifest = json.loads(raw_manifest) if raw_manifest else None
        except ValueError as error:
            raise Refused("릴리스 채널 manifest가 올바르지 않다") from error
        parent = self.read("rev-parse", self.commit + "^") if manifest else None
        channel, platforms = validate_channel_manifest(manifest, self.tag, parent)
        if channel != self.channel or any(candidate.platform not in platforms for candidate in self.candidates):
            raise Refused("검증한 태그의 채널·플랫폼과 게시 요청이 다르다")
        script = self.git("show", f"{self.commit}:scripts/build-app.sh").stdout.decode()
        public = re.search(r"<key>SUPublicEDKey</key>\s*<string>([^<]+)</string>", script)
        if not public:
            raise Refused("검증한 태그의 Sparkle 공개키가 없다")
        release = self.release_view()
        if (not isinstance(release, dict) or release.get("isDraft") is not False
                or release.get("isPrerelease") is not (self.channel == "preview")
                or not isinstance(release.get("assets"), list) or any(not isinstance(asset, dict) for asset in release["assets"])):
            raise Refused("릴리스의 공개 상태·채널이 다르다")
        replacements = []
        for candidate in self.candidates:
            suffix = ".dmg" if candidate.platform == "macos" else "-windows-x86_64.msi"
            name = f"kasaterm-{self.tag}{suffix}"
            value = item(candidate.raw)
            if (value["version"] != self.tag[1:] or value["url"] != f"https://github.com/{SLUG}/releases/download/{self.tag}/{name}"
                    or value["length"] != candidate.asset.stat().st_size or sha256_file(candidate.asset) != candidate.sha256):
                raise Refused("고정한 appcast의 버전·주소·크기·산출물 해시가 다르다")
            assets = [asset for asset in release.get("assets", []) if asset.get("name") == name]
            if (len(assets) != 1 or assets[0].get("digest") != candidate.sha256 or assets[0].get("size") != value["length"]):
                raise Refused("릴리스 산출물이 검증한 해시·크기와 다르다")
            if not ed25519_ok(self.runner, self.openssl, public[1].strip(), value["signature"], candidate.asset, scratch):
                raise Refused("appcast EdDSA 서명이 고정한 산출물과 다르다")
            relative = candidate.destination(self.channel)
            previous = item(self.file_at(base, relative), empty=True)
            if previous and version_tuple(previous["version"]) >= version_tuple(value["version"]):
                if previous == value:
                    continue
                raise Refused(f"{relative}의 더 새 판이나 같은 판의 다른 파일을 덮을 수 없다")
            replacements.append((relative, candidate.raw))
        return replacements

    def commit_feeds(self, base, replacements, scratch):
        index = scratch / "index"
        index.unlink(missing_ok=True)
        env = {"GIT_INDEX_FILE": str(index), "GIT_AUTHOR_NAME": "github-actions[bot]",
               "GIT_AUTHOR_EMAIL": "github-actions[bot]@users.noreply.github.com",
               "GIT_COMMITTER_NAME": "github-actions[bot]", "GIT_COMMITTER_EMAIL": "github-actions[bot]@users.noreply.github.com"}
        self.git("read-tree", base, extra=env)
        for relative, raw in replacements:
            blob = self.git("hash-object", "-w", "--stdin", data=raw).stdout.decode().strip()
            self.git("update-index", "--add", "--cacheinfo", f"100644,{blob},{relative}", extra=env)
        tree = self.git("write-tree", extra=env).stdout.decode().strip()
        return self.git("commit-tree", tree, "-p", base, "-m", f"chore(release): {self.channel} appcast {self.tag}",
                        extra=env).stdout.decode().strip()

    def push(self, commit):
        return self.git("push", self.remote, f"{commit}:refs/heads/main", check=False).returncode == 0

    def publish(self, attempts=5):
        if type(attempts) is not int or not 1 <= attempts <= 10:
            raise Refused("게시 재시도 횟수는 1~10이어야 한다")
        with tempfile.TemporaryDirectory(prefix="kasaterm-feed-") as directory:
            scratch = Path(directory)
            base = self.current_main()
            for attempt in range(1, attempts + 1):
                replacements = self.validate(base, scratch)
                if not replacements:
                    return {"state": "already_published", "attempts": attempt, "commit": base}
                commit = self.commit_feeds(base, replacements, scratch)
                if self.push(commit):
                    return {"state": "published", "attempts": attempt, "commit": commit}
                latest = self.current_main()
                if latest == base:
                    raise Refused("피드 push가 거절됐지만 main은 그대로다 — 권한·네트워크를 확인한다")
                base = latest
            if not self.validate(base, scratch):
                return {"state": "already_published", "attempts": attempts, "commit": base}
        raise CasExhausted(f"main 경합 {attempts}회 — 이 실행의 appcast job만 명시적으로 다시 실행할 수 있다")

    def pages_api(self, method, suffix=""):
        result = Runner("live").run(
            ["gh", "api", "--method", method, f"repos/{SLUG}/pages{suffix}",
             "-H", "Accept: application/vnd.github+json", "-H", "X-GitHub-Api-Version: 2022-11-28"],
            timeout=30, kind="publish" if method == "POST" else "read")
        if not result.ok:
            raise Refused(f"Pages {method} {suffix or '/'} 실패 — 피드 커밋은 유지되며 배포 완료가 아니다")
        try:
            value = json.loads(result.out)
        except ValueError as error:
            raise Refused("Pages 응답이 JSON이 아니다") from error
        if not isinstance(value, dict):
            raise Refused("Pages 응답이 객체가 아니다")
        return value

    def deploy_pages(self, publication, attempts=60, delay=10, http=None, wait=None):
        if (type(attempts) is not int or not 1 <= attempts <= 120
                or type(delay) is not int or not 1 <= delay <= 30):
            raise Refused("Pages 확인은 1~120회, 간격은 1~30초여야 한다")
        commit = publication.get("commit")
        if (publication.get("state") not in ("published", "already_published")
                or not re.fullmatch(r"[0-9a-f]{40}", commit or "")):
            raise Refused("검증된 피드 게시 결과가 없어 Pages를 요청하지 않는다")
        expected = {candidate.destination(self.channel): item(candidate.raw) for candidate in self.candidates}
        if any(item(self.file_at(commit, path)) != value for path, value in expected.items()):
            raise Refused("게시 커밋의 피드가 검증한 후보와 다르다")
        site = self.pages_api("GET")
        if (site.get("build_type") != "legacy" or site.get("source") != {"branch": "main", "path": "/docs"}
                or site.get("html_url") != PAGES_URL):
            raise Refused("Pages가 main:/docs의 기존 사이트가 아니다 — 설정을 자동 변경하지 않는다")
        # GITHUB_TOKEN의 push는 legacy Pages를 깨우지 않아 명시적 요청이 필요하다.
        requested = self.pages_api("POST", "/builds")
        if requested.get("status") not in ("queued", "building", "built"):
            raise Refused("Pages 빌드 요청이 접수되지 않았다")
        http, wait, checked = http or Http(), wait or time.sleep, {}
        rebuilds, last, still = 0, None, 0
        for attempt in range(1, attempts + 1):
            build = self.pages_api("GET", "/builds/latest")
            status, built = build.get("status"), build.get("commit")
            seen = (status, built, build.get("created_at"))
            still = still + 1 if seen == last and status in ("queued", "building") else 0
            last = seen
            if status not in ("queued", "building", "built", "errored"):
                raise Refused("Pages 빌드 상태를 확인하지 못했다")
            if built and (not isinstance(built, str) or not re.fullmatch(r"[0-9a-f]{40}", built)):
                raise Refused("Pages 빌드 커밋이 올바르지 않다")
            if built and built not in checked:
                if self.git("cat-file", "-e", built + "^{commit}", check=False).returncode != 0:
                    self.git("fetch", "-q", "--no-tags", self.remote, built)
                checked[built] = self.git("merge-base", "--is-ancestor", commit, built, check=False).returncode == 0
            relevant = built and checked[built]
            if (relevant and status == "errored") or still >= PAGES_STALL:
                if rebuilds >= PAGES_REBUILDS:
                    if status == "errored":
                        raise PagesUnfinished("게시 커밋의 Pages 빌드 실패 — 피드 커밋은 유지된다")
                else:
                    rebuilds += 1
                    if self.pages_api("POST", "/builds").get("status") not in ("queued", "building", "built"):
                        raise Refused("Pages 빌드 재요청이 접수되지 않았다")
                    still, last = 0, None
                    if attempt < attempts:
                        wait(delay)
                    continue
            if relevant and status == "built":
                if any(item(self.file_at(built, path)) != value for path, value in expected.items()):
                    raise Refused("Pages가 빌드한 피드가 검증한 후보에서 바뀌었다")
                matches = True
                for path, value in expected.items():
                    url = PAGES_URL + path.removeprefix("docs/")
                    code, raw = http.get(url, headers={"Cache-Control": "no-cache"}, timeout=15)
                    try:
                        matches = code == 200 and item(raw) == value and matches
                    except Refused:
                        matches = False
                if matches:
                    return {"state": "deployed", "commit": built, "attempts": attempt,
                            "feeds": [PAGES_URL + path.removeprefix("docs/") for path in expected]}
            if attempt < attempts:
                wait(delay)
        raise PagesUnfinished(f"Pages 배포·공개 피드 확인 {attempts}회 소진 — 피드 커밋은 유지되며 배포 완료가 아니다")


def main(argv=None):
    parser = argparse.ArgumentParser(description="서명된 appcast의 bounded CAS 게시")
    parser.add_argument("--repo", default=".")
    parser.add_argument("--tag", required=True)
    parser.add_argument("--channel", required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--mac", required=True)
    parser.add_argument("--dmg", required=True)
    parser.add_argument("--dmg-sha256", required=True)
    parser.add_argument("--windows")
    parser.add_argument("--msi")
    parser.add_argument("--msi-sha256")
    parser.add_argument("--attempts", type=int, default=5)
    parser.add_argument("--deploy-pages", action="store_true")
    args = parser.parse_args(argv)
    try:
        candidates = [Candidate.read("macos", args.mac, args.dmg, args.dmg_sha256)]
        if any((args.windows, args.msi, args.msi_sha256)):
            if not all((args.windows, args.msi, args.msi_sha256)):
                raise Refused("Windows 피드·산출물·검증 해시가 모두 필요하다")
            candidates.append(Candidate.read("windows", args.windows, args.msi, args.msi_sha256))
        publisher = Publisher(args.repo, args.tag, args.channel, args.commit, candidates)
        result = publisher.publish(args.attempts)
        if args.deploy_pages:
            result["pages"] = publisher.deploy_pages(result)
        print(json.dumps(result))
    except CasExhausted as error:
        parser.exit(75, f"KASATERM_APPCAST_CAS_EXHAUSTED\n::error title=Appcast CAS retries exhausted::{error}\n")
    except PagesUnfinished as error:
        parser.exit(1, f"KASATERM_APPCAST_PAGES_UNFINISHED\n{error}\n")
    except (Refused, OSError, ValueError, subprocess.TimeoutExpired) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
