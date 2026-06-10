import { useEffect, useRef, useState } from 'react';
import { fetchPeek, sendToPane } from '@/lib/mcp';

// ── ANSI SGR 파서 ──────────────────────────────────────────────────────────────
// claude TUI 에서 실제로 쓰는 색 코드만 커버(30-37/90-97 fg, 38;5;n 256색, reset).

// 다크 터미널 기준 VT100 16색 팔레트 (dark bg #16121c 기준으로 조정)
const FG16: Record<number, string> = {
  30: '#4a4458', 31: '#f28779', 32: '#87d7a8', 33: '#ffd580',
  34: '#6c8ef5', 35: '#c39fef', 36: '#5ccfe6', 37: '#c8c0d0',
  90: '#7a6e8a', 91: '#ff9f9f', 92: '#9deba0', 93: '#ffe08a',
  94: '#92a8ff', 95: '#d4b8ff', 96: '#80ddee', 97: '#f0eaf8',
};

// xterm-256 색상 기본 팔레트 (0-15는 FG16, 16-231은 6×6×6 큐브, 232-255는 그레이스케일)
function xterm256(n: number): string {
  if (n < 16) {
    const idx = n < 8 ? n + 30 : n - 8 + 90;
    return FG16[idx] ?? '#e6e0ee';
  }
  if (n >= 232) {
    const v = Math.round(((n - 232) / 23) * 255);
    const h = v.toString(16).padStart(2, '0');
    return `#${h}${h}${h}`;
  }
  const i = n - 16;
  const b = i % 6, g = Math.floor(i / 6) % 6, r = Math.floor(i / 36);
  const cv = (x: number) => (x === 0 ? 0 : Math.round(55 + x * 40));
  const hex = (x: number) => cv(x).toString(16).padStart(2, '0');
  return `#${hex(r)}${hex(g)}${hex(b)}`;
}

interface AnsiSpan { text: string; color?: string; bold?: boolean; }

function parseAnsi(raw: string): AnsiSpan[] {
  const spans: AnsiSpan[] = [];
  // 현재 상태
  let color: string | undefined;
  let bold = false;

  // ANSI escape: \x1b[ ... m
  const RE = /\x1b\[([0-9;]*)m/g;
  let last = 0;

  for (const m of raw.matchAll(RE)) {
    const idx = m.index ?? 0;
    if (idx > last) spans.push({ text: raw.slice(last, idx), color, bold });

    const params = m[1].split(';').map(Number);
    let i = 0;
    while (i < params.length) {
      const p = params[i];
      if (p === 0) { color = undefined; bold = false; }
      else if (p === 1) bold = true;
      else if (p === 22) bold = false;
      else if (p === 39) color = undefined;
      else if ((p >= 30 && p <= 37) || (p >= 90 && p <= 97)) color = FG16[p];
      else if (p === 38 && params[i + 1] === 5) { color = xterm256(params[i + 2]); i += 2; }
      else if (p === 38 && params[i + 1] === 2) {
        color = `rgb(${params[i+2]},${params[i+3]},${params[i+4]})`; i += 4;
      }
      i++;
    }

    last = idx + m[0].length;
  }
  if (last < raw.length) spans.push({ text: raw.slice(last), color, bold });
  return spans;
}

function AnsiText({ raw }: { raw: string }) {
  const spans = parseAnsi(raw);
  return (
    <>
      {spans.map((s, i) => (
        s.color || s.bold
          ? <span key={i} style={{ color: s.color, fontWeight: s.bold ? 700 : undefined }}>{s.text}</span>
          : <span key={i}>{s.text}</span>
      ))}
    </>
  );
}

// ── 컴포넌트 ──────────────────────────────────────────────────────────────────

export interface TerminalPeekPanelProps {
  surfaceId: string;
  title: string;
  onClose: () => void;
}

const LINES = 40;

export function TerminalPeekPanel({ surfaceId, title, onClose }: TerminalPeekPanelProps) {
  const [raw, setRaw] = useState('');
  const [input, setInput] = useState('');
  const [sending, setSending] = useState(false);
  const [flash, setFlash] = useState<'ok' | 'err' | null>(null);
  const bodyRef = useRef<HTMLPreElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    let stopped = false;
    setRaw('');
    const tick = async () => {
      if (stopped) return;
      const t = await fetchPeek(surfaceId, LINES, true); // ansi=1
      if (!stopped) setRaw(t);
    };
    void tick();
    const iv = setInterval(tick, 1000);
    return () => { stopped = true; clearInterval(iv); };
  }, [surfaceId]);

  useEffect(() => {
    const el = bodyRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [raw]);

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
    if (e.key === 'c' && e.ctrlKey) { e.preventDefault(); void sendToPane(surfaceId, '\x03', false); }
  };

  return (
    <div style={{
      position: 'fixed', top: 80, left: 500, right: 16, bottom: 16,
      display: 'flex', flexDirection: 'column',
      background: '#16121c',
      boxShadow: '0 0 0 2px var(--cth-ink-900), 0 0 0 4px var(--cth-cream-200)',
      zIndex: 50
    }}>
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

      {/* 본문 — ANSI 색 렌더링 */}
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
        {raw ? <AnsiText raw={raw} /> : '화면을 불러오는 중…'}
      </pre>

      {/* 입력창 */}
      <div style={{
        display: 'flex', alignItems: 'center', gap: 6,
        padding: '6px 10px',
        background: '#1e1a28',
        boxShadow: 'inset 0 2px 0 var(--cth-ink-900)'
      }}>
        <span style={{
          fontFamily: '"JetBrains Mono", monospace', fontSize: 12,
          color: flash === 'ok' ? '#87d7a8' : flash === 'err' ? '#f28779' : '#6c8ef5',
          flexShrink: 0, width: 14
        }}>
          {flash === 'err' ? '!' : '>'}
        </span>
        <input
          ref={inputRef}
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={onKey}
          disabled={sending}
          placeholder="입력 후 Enter 전송 — Ctrl+C 인터럽트"
          style={{
            flex: 1,
            fontFamily: '"JetBrains Mono", "D2Coding", Menlo, monospace',
            fontSize: 12,
            background: 'transparent', border: 'none', outline: 'none',
            color: '#e6e0ee', opacity: sending ? 0.5 : 1
          }}
        />
        <button
          onClick={() => void send(true)}
          disabled={!input || sending}
          style={{
            fontFamily: '"JetBrains Mono", monospace', fontSize: 11,
            padding: '2px 8px', border: 'none',
            cursor: !input || sending ? 'not-allowed' : 'pointer',
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
