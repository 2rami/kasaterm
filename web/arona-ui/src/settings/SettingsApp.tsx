import { useCallback, useState } from 'react';

/// 이 페이지가 붙은 인스턴스의 포트. 웹뷰가 same-origin 으로 로드되므로
/// `location.port` 가 곧 그 인스턴스다 — 네이티브의 `mcp_panel_port()` 는 8765
/// 폴백을 가지고 있어 **남의 인스턴스를 가리킬 수 있다.** 설정 화면은 파일을
/// 쓰므로 어느 프로세스에 말하는지가 화면에 보여야 한다.
const PORT = location.port || '8765';

/// Step 1 의 걷는 뼈대 — 창이 뜨고, 어느 인스턴스에 붙었는지 보이고, POST 가
/// origin guard 를 통과한다. 읽기 전용 UI 는 Step 4 부터.
export function SettingsApp() {
  const [ping, setPing] = useState<string>('아직 안 눌렀습니다');
  const [busy, setBusy] = useState(false);

  const doPing = useCallback(async () => {
    setBusy(true);
    try {
      const res = await fetch('/settings/ping', { method: 'POST' });
      const text = await res.text();
      setPing(`${res.status} ${text}`);
    } catch (e) {
      setPing(`실패: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
    }
  }, []);

  return (
    <div className="min-h-screen bg-background text-foreground p-8 font-[var(--cth-font-ui)]">
      <header className="mb-6">
        <h1 className="text-[var(--cth-text-display-lg)] leading-[var(--cth-lh-display-lg)] font-semibold">
          설정
        </h1>
        <p className="text-[var(--cth-text-body-sm)] text-muted-foreground mt-1">
          127.0.0.1:{PORT} — 이 포트의 kasaterm 에 말합니다
        </p>
      </header>

      <section className="rounded-lg bg-card p-5 shadow-[var(--cth-panel-border)]">
        <h2 className="text-[var(--cth-text-display-sm)] font-medium mb-3">배선 확인</h2>
        <button
          type="button"
          disabled={busy}
          onClick={doPing}
          className="rounded-md bg-[var(--cth-sky)] px-4 py-2 text-white text-[var(--cth-text-body-md)] disabled:opacity-50"
        >
          POST /settings/ping
        </button>
        <pre className="mt-3 text-[var(--cth-text-mono-sm)] font-[var(--cth-font-mono)] whitespace-pre-wrap">
          {ping}
        </pre>
      </section>
    </div>
  );
}
