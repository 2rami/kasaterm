import { useEffect, useRef, useState } from 'react';
import { fetchPeek, sendToPane } from '@/lib/mcp';

// 교실 터미널 뷰 — 학생(pane) 클릭 시 그 화면을 교실 안에서 그대로 본다.
// GET /peek?surface=&lines=40 을 1s 폴링(열린 동안만). 픽셀 프레임 + 모노 텍스트.
// 하단 입력창: POST /send?surface=<id> body:{text,submit} — 교실에서 직접 타이핑.
export interface TerminalPeekPanelProps {
  surfaceId: string;
  title: string;
  onClose: () => void;
}

const LINES = 40;

export function TerminalPeekPanel({ surfaceId, title, onClose }: TerminalPeekPanelProps) {
  const [text, setText] = useState('');
  const [input, setInput] = useState('');
  const [sending, setSending] = useState(false);
  const [flash, setFlash] = useState<'ok' | 'err' | null>(null);
  const bodyRef = useRef<HTMLPreElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    let stopped = false;
    setText('');
    const tick = async () => {
      if (stopped) return;
      const t = await fetchPeek(surfaceId, LINES);
      if (!stopped) setText(t);
    };
    void tick();
    const iv = setInterval(tick, 1000);
    return () => { stopped = true; clearInterval(iv); };
  }, [surfaceId]);

  // 새 텍스트가 들어오면 항상 맨 아래(최신 행)로 — 터미널처럼 따라간다.
  useEffect(() => {
    const el = bodyRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [text]);

  const send = async (submit: boolean) => {
    if (!input || sending) return;
    setSending(true);
    const ok = await sendToPane(surfaceId, input, submit);
    setSending(false);
    setFlash(ok ? 'ok' : 'err');
    setTimeout(() => setFlash(null), 1200);
    if (ok) setInput('');
    inputRef.current?.focus();
  };

  const onKey = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter') { e.preventDefault(); void send(true); }
    // Ctrl+C → 인터럽트 신호
    if (e.key === 'c' && e.ctrlKey) { e.preventDefault(); void sendToPane(surfaceId, '\x03', false); }
  };

  return (
    <div
      style={{
        position: 'fixed', top: 80, left: 500, right: 16, bottom: 16,
        display: 'flex', flexDirection: 'column',
        background: '#16121c',
        boxShadow: '0 0 0 2px var(--cth-ink-900), 0 0 0 4px var(--cth-cream-200)',
        zIndex: 50
      }}
    >
      {/* 헤더 */}
      <div style={{
        display: 'flex', alignItems: 'center', justifyContent: 'space-between',
        padding: '6px 10px', background: 'var(--cth-cream-200)',
        boxShadow: 'inset 0 -2px 0 var(--cth-ink-900)'
      }}>
        <span style={{ fontFamily: 'var(--cth-font-display)', fontSize: 'var(--cth-text-display-sm)', color: 'var(--cth-ink-900)' }}>
          {title} <span style={{ color: 'var(--cth-ink-500)' }}>{surfaceId}</span>
        </span>
        <button
          onClick={onClose}
          title="닫기"
          style={{
            fontFamily: 'var(--cth-font-display)', fontSize: 'var(--cth-text-display-sm)',
            border: 'none', cursor: 'pointer', padding: '2px 8px',
            background: 'var(--cth-coral)', color: 'var(--cth-cream-50)',
            boxShadow: 'inset 0 0 0 1px var(--cth-ink-900)'
          }}
        >
          X
        </button>
      </div>

      {/* 본문 */}
      <pre
        ref={bodyRef}
        style={{
          flex: 1, margin: 0, padding: '10px 14px', overflow: 'auto',
          fontFamily: '"JetBrains Mono", "D2Coding", Menlo, ui-monospace, monospace',
          fontSize: 13, lineHeight: 1.3, letterSpacing: 0,
          color: '#e6e0ee', whiteSpace: 'pre', tabSize: 4,
          textRendering: 'optimizeLegibility'
        }}
      >
        {text || '화면을 불러오는 중…'}
      </pre>

      {/* 입력창 — pane 에 직접 타이핑 */}
      <div style={{
        display: 'flex', alignItems: 'center', gap: 6,
        padding: '6px 10px',
        background: '#1e1a28',
        boxShadow: 'inset 0 2px 0 var(--cth-ink-900)'
      }}>
        {/* 프롬프트 힌트 */}
        <span style={{
          fontFamily: '"JetBrains Mono", monospace', fontSize: 12,
          color: flash === 'ok' ? '#98d7a0' : flash === 'err' ? '#f28779' : '#6c8ef5',
          flexShrink: 0, width: 14
        }}>
          {flash === 'ok' ? '>' : flash === 'err' ? '!' : '>'}
        </span>

        <input
          ref={inputRef}
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={onKey}
          disabled={sending}
          placeholder="입력 후 Enter — 그 pane 에 그대로 전송됩니다"
          style={{
            flex: 1,
            fontFamily: '"JetBrains Mono", "D2Coding", Menlo, monospace',
            fontSize: 12,
            background: 'transparent',
            border: 'none',
            outline: 'none',
            color: '#e6e0ee',
            opacity: sending ? 0.5 : 1
          }}
        />

        {/* 전송(submit) 버튼 */}
        <button
          onClick={() => void send(true)}
          disabled={!input || sending}
          title="Enter 전송 (개행 포함)"
          style={{
            fontFamily: '"JetBrains Mono", monospace', fontSize: 11,
            padding: '2px 8px', border: 'none', cursor: !input || sending ? 'not-allowed' : 'pointer',
            background: '#6c8ef5', color: '#fff',
            opacity: !input || sending ? 0.4 : 1
          }}
        >
          전송
        </button>
      </div>
    </div>
  );
}
