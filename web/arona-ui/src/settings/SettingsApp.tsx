import { useCallback, useEffect, useState } from 'react';

/// `GET /design-tokens` 응답. 색은 CSS hex 문자열이라 그대로 var() 에 꽂힌다.
type DesignTokens = {
  theme: string;
  accent_name: string;
  palette: Record<string, string>;
  ansi: string[];
  shape: Record<string, number | boolean>;
};

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
  const [tokens, setTokens] = useState<DesignTokens | null>(null);
  const [tokenErr, setTokenErr] = useState<string | null>(null);

  const loadTokens = useCallback(async () => {
    try {
      const res = await fetch('/design-tokens');
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      setTokens((await res.json()) as DesignTokens);
      setTokenErr(null);
    } catch (e) {
      setTokenErr(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    void loadTokens();
  }, [loadTokens]);

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

      <section className="mt-4 rounded-lg bg-card p-5 shadow-[var(--cth-panel-border)]">
        <div className="flex items-center justify-between mb-3">
          <h2 className="text-[var(--cth-text-display-sm)] font-medium">
            네이티브가 지금 쓰는 색
          </h2>
          <button
            type="button"
            onClick={() => void loadTokens()}
            className="text-[var(--cth-text-body-sm)] underline opacity-70 hover:opacity-100"
          >
            다시 읽기
          </button>
        </div>
        {tokenErr && (
          <p className="text-[var(--cth-text-body-sm)] text-[var(--cth-coral)]">
            /design-tokens 실패: {tokenErr}
          </p>
        )}
        {tokens && (
          <>
            <p className="text-[var(--cth-text-body-sm)] text-muted-foreground mb-3">
              테마 {tokens.theme} · accent {tokens.accent_name}
            </p>
            <ul className="grid grid-cols-2 gap-x-6 gap-y-1">
              {Object.entries(tokens.palette).map(([name, hex]) => (
                <li key={name} className="flex items-center gap-2">
                  <span
                    className="inline-block h-4 w-4 rounded-sm shrink-0 shadow-[var(--cth-panel-border-inset)]"
                    style={{ background: hex }}
                  />
                  <span className="text-[var(--cth-text-body-sm)]">{name}</span>
                  <span className="text-[var(--cth-text-mono-sm)] font-[var(--cth-font-mono)] opacity-60 ml-auto">
                    {hex}
                  </span>
                </li>
              ))}
            </ul>
            <div className="mt-3 flex gap-1">
              {tokens.ansi.map((hex, i) => (
                <span
                  key={i}
                  title={`ansi ${i} ${hex}`}
                  className="inline-block h-4 w-4 rounded-sm"
                  style={{ background: hex }}
                />
              ))}
            </div>
          </>
        )}
      </section>
    </div>
  );
}
