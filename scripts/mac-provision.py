#!/usr/bin/env python3
"""Decide whether the bundle may carry the communication-notification entitlement.

usage: mac-provision.py <profile> <app> <team> <signing sha1> <base entitlements | -> <out entitlements>

Communication notifications (the sender's face as the banner's large icon) are a
restricted entitlement: the kernel kills a bundle at launch when it claims one
without an embedded profile that grants it to this team, bundle id and signing
certificate. So this checks every one of those against the profile before the
caller embeds it. Exit 0 after writing the merged entitlements; exit 1 with the
reason, and the caller signs without the entitlement as before.
"""
import datetime
import hashlib
import pathlib
import plistlib
import subprocess
import sys

KEY = "com.apple.developer.usernotifications.communication"


def fail(reason: str) -> None:
    print(f"[mac-provision] {reason} — 대화 알림 권한 없이 서명한다", file=sys.stderr)
    sys.exit(1)


def main(profile: str, app: str, team: str, sha1: str, base: str, out: str) -> None:
    if not pathlib.Path(profile).is_file():
        fail(f"프로파일이 없다({profile})")
    decoded = subprocess.run(["security", "cms", "-D", "-i", profile], capture_output=True)
    if decoded.returncode != 0:
        fail("프로파일을 풀 수 없다")
    body = plistlib.loads(decoded.stdout)
    granted = body.get("Entitlements", {})
    bundle_id = plistlib.loads(pathlib.Path(app, "Contents/Info.plist").read_bytes())["CFBundleIdentifier"]
    expires = body.get("ExpirationDate")
    now = datetime.datetime.now(datetime.timezone.utc).replace(tzinfo=None)
    if not isinstance(expires, datetime.datetime) or expires.replace(tzinfo=None) <= now:
        fail("프로파일이 만료됐다")
    if team not in body.get("TeamIdentifier", []):
        fail(f"프로파일 팀이 {team} 이 아니다")
    if granted.get("com.apple.application-identifier") != f"{team}.{bundle_id}":
        fail(f"프로파일이 {bundle_id} 것이 아니다")
    if granted.get(KEY) is not True:
        fail("프로파일에 대화 알림 권한이 없다")
    certs = {hashlib.sha1(der).hexdigest().upper() for der in body.get("DeveloperCertificates", [])}
    if sha1.upper() not in certs:
        fail("서명 인증서가 프로파일에 없다")
    merged = plistlib.loads(pathlib.Path(base).read_bytes()) if base != "-" else {}
    merged.update({
        "com.apple.application-identifier": f"{team}.{bundle_id}",
        "com.apple.developer.team-identifier": team,
        KEY: True,
    })
    pathlib.Path(out).write_bytes(plistlib.dumps(merged))


if __name__ == "__main__":
    if len(sys.argv) != 7:
        print(__doc__, file=sys.stderr)
        sys.exit(2)
    main(*sys.argv[1:])
