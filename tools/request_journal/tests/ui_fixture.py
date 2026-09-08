"""Synthetic UI fixture; never reads live transcripts or user journal data."""
from pathlib import Path
import tempfile

from tools.request_journal.server import JournalServer
from tools.request_journal.store import Store
from tools.request_journal.summarizer import Summarizer
from tools.request_journal.chat import ChatManager
from tools.request_journal.tests.test_chat import JSONProvider


def main():
    with tempfile.TemporaryDirectory(prefix="journal-ui-") as root:
        project = str((Path(root) / "데모 프로젝트").resolve())
        store = Store(Path(root) / "journal.sqlite3")
        for i, (prompt, final, status) in enumerate([
            ('펫 우클릭에서 행동을 고르고 설정에서 크기와 알림을 바꾸고 싶어요. <img src=x onerror="window.fixtureUnsafe=true">', '우클릭 메뉴와 상세 설정을 연결했습니다. 앱 재시작 후 반영을 확인해야 합니다.', 'reported_done'),
            ('다른 창에 포커스가 있어도 펫 커서가 손 모양으로 바뀌게 해 주세요.', '커서가 바뀌는 경로를 확인하고 있습니다.', 'working'),
            ('요청과 완료 보고를 한 화면에서 확인할 수 있게 해 주세요.', '', 'received'),
        ]):
            events = [{"event_key": f"u{i}", "kind": "user", "text": prompt, "created_at": f"2026-09-08T12:0{i}:00Z"}]
            if final:
                events.append({"event_key": f"f{i}", "kind": "assistant_final", "text": final, "created_at": f"2026-09-08T12:0{i}:30Z"})
            result = store.ingest(f"fixture-{i}", events, 100, {"project": project})
            if status != "received":
                store.set_reported_status(result["last_request_id"], status, {"fixture": True})
        Summarizer().update(store, project=project)
        store.record_app_run({"project": project, "machine": "local", "pid": 777777, "started_at": "2026-09-08T04:13:51.000Z", "executable": "/synthetic/kasaterm", "evidence": {"kind": "synthetic_fixture"}})
        server = JournalServer(lambda: store, project, port=0)
        server.chat = ChatManager(store, project, provider_factory=lambda _cancel: JSONProvider())
        print(f"http://127.0.0.1:{server.server_port}/", flush=True)
        try:
            server.serve_forever()
        finally:
            server.chat.close()
            server.server_close()


if __name__ == "__main__":
    main()
