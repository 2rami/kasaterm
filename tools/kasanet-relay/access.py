#!/usr/bin/env python3
"""국내 중계 접근 확인 — iroh-relay `access.http` 가 접속마다 묻는다. 허용 목록 파일을 그때그때 읽어 답한다.

목록을 고쳐도 중계를 다시 켤 필요가 없고(붙은 기기가 안 끊긴다), 거절된 id 가 기록(journal)에 남아 새 기기(폰)를
넣을 때 그 id 를 고를 수 있다 — 중계 자체 로그에는 거절된 id 가 안 남는다.
"""
import http.server
import re

ALLOWLIST = "/etc/kasanet-relay/allowlist"
ID = re.compile(r"^[0-9a-f]{64}$")


def allowed_ids():
    try:
        with open(ALLOWLIST) as f:
            return {line.split()[0] for line in f if line.strip() and not line.startswith("#")}
    except OSError:
        return set()


class Check(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        node = (self.headers.get("X-Iroh-NodeId") or "").strip().lower()
        ok = bool(ID.match(node)) and node in allowed_ids()
        print(f"{'허용' if ok else '거절'} {node or '-'}", flush=True)
        body = b"true" if ok else b"false"
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


if __name__ == "__main__":
    http.server.ThreadingHTTPServer(("127.0.0.1", 9101), Check).serve_forever()
