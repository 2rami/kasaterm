import { useEffect, useRef, useState } from 'react';
import { fetchPeek, fetchTranscript, sendToPane, type Turn } from '@/lib/mcp';
import { SegmentedTabs } from './GameKit';
import { SpritePortrait } from './SpritePortrait';

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
type Tab = 'chat' | 'screen';

export function TerminalPeekPanel({ surfaceId, title, onClose }: TerminalPeekPanelProps) {
  const [tab, setTab] = useState<Tab>('chat');
  const [raw, setRaw] = useState('');
  const [turns, setTurns] = useState<Turn[]>([]);
  const [input, setInput] = useState('');
  const [sending, setSending] = useState(false);
  const [flash, setFlash] = useState<'ok' | 'err' | null>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  // 대화 탭 — 구조화된 transcript 폴링(선생님 ②: 명령어 말고 대화내용).
  useEffect(() => {
    if (tab !== 'chat') return;
    let stopped = false;
    const tick = async () => {
      const ts = await fetchTranscript(surfaceId, 30);
      if (!stopped) setTurns(ts);
    };
    void tick();
    const iv = setInterval(tick, 1500);
    return () => { stopped = true; clearInterval(iv); };
  }, [surfaceId, tab]);

  // 화면 탭 — raw 터미널(ANSI). 필요할 때만 폴링.
  useEffect(() => {
    if (tab !== 'screen') return;
    let stopped = false;
    setRaw('');
    const tick = async () => {
      const t = await fetchPeek(surfaceId, LINES, true);
      if (!stopped) setRaw(t);
    };
    void tick();
    const iv = setInterval(tick, 1000);
    return () => { stopped = true; clearInterval(iv); };
  }, [surfaceId, tab]);

  useEffect(() => {
    const el = bodyRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [raw, turns]);

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

  const dark = tab === 'screen';

  return (
    <div style={{
      width: 340, flexShrink: 0, height: '100%',
      display: 'flex', flexDirection: 'column',
      background: dark ? '#16121c' : 'var(--cth-cream-50)',
      borderLeft: '1px solid var(--cth-cream-200)',
      overflow: 'hidden'
    }}>
      {/* 헤더: 캐릭터명 + 대화/화면 탭 + 닫기 */}
      <div style={{
        display: 'flex', alignItems: 'center', gap: 10,
        padding: '10px 12px',
        background: 'var(--cth-cream-50)',
        borderBottom: '1px solid var(--cth-cream-200)'
      }}>
        <span style={{ fontFamily: 'var(--cth-font-display)', fontSize: 15, fontWeight: 700, color: 'var(--cth-ink-900)' }}>
          {title} <span style={{ color: 'var(--cth-ink-300)', fontWeight: 400, fontSize: 13 }}>{surfaceId}</span>
        </span>
        <SegmentedTabs<Tab>
          options={[{ value: 'chat', label: '대화' }, { value: 'screen', label: '화면' }]}
          value={tab}
          onChange={setTab}
          size="sm"
        />
        <div style={{ flex: 1 }} />
        <button
          onClick={onClose}
          title="닫기"
          style={{
            width: 28, height: 28, borderRadius: 8, border: 'none', cursor: 'pointer',
            background: 'var(--cth-cream-100)', color: 'var(--cth-ink-500)',
            fontFamily: 'var(--cth-font-ui)', fontSize: 16, lineHeight: 1,
            display: 'inline-flex', alignItems: 'center', justifyContent: 'center'
          }}
        >×</button>
      </div>

      {/* 본문 — 대화(채팅 버블) or 화면(ANSI) */}
      {tab === 'chat' ? (
        <div ref={bodyRef} style={{ flex: 1, overflow: 'auto', padding: '14px 16px', background: 'var(--cth-cream-100)' }}>
          {turns.length === 0 ? (
            <div style={{ color: 'var(--cth-ink-300)', fontFamily: 'var(--cth-font-ui)', fontSize: 13, textAlign: 'center', marginTop: 40 }}>
              대화를 불러오는 중…
            </div>
          ) : turns.map((t, i) => {
            const mine = t.role === 'user';
            // 모모톡 스타일: 학생(assistant)=좌측 아바타+흰 말풍선, 선생님(user)=우측 카톡 노란 말풍선.
            return (
              <div key={i} style={{ display: 'flex', justifyContent: mine ? 'flex-end' : 'flex-start', marginBottom: 10, gap: 8, alignItems: 'flex-end' }}>
                {!mine && (
                  <div style={{ width: 34, height: 34, borderRadius: 11, overflow: 'hidden', flexShrink: 0, background: 'var(--cth-cream-100)', border: '1px solid var(--cth-cream-200)', display: 'flex', alignItems: 'flex-end', justifyContent: 'center' }}>
                    <SpritePortrait character={title} scale={1.5} />
                  </div>
                )}
                <div style={{
                  maxWidth: '72%', padding: '8px 12px',
                  borderRadius: 14,
                  borderTopLeftRadius: mine ? 14 : 4,
                  borderTopRightRadius: mine ? 4 : 14,
                  background: mine ? '#FEE500' : '#fff',
                  color: mine ? '#3A2E00' : 'var(--cth-ink-900)',
                  border: mine ? 'none' : '1px solid var(--cth-cream-200)',
                  boxShadow: '0 1px 3px rgba(21, 41, 74, 0.08)',
                  fontFamily: 'var(--cth-font-ui)', fontSize: 13, lineHeight: 1.55,
                  whiteSpace: 'pre-wrap', wordBreak: 'break-word'
                }}>{t.text}</div>
              </div>
            );
          })}
        </div>
      ) : (
        <pre
          // ANSI 화면은 div 가 아닌 pre 이지만 같은 ref 타입(HTMLElement)로 스크롤 제어.
          ref={bodyRef as unknown as React.RefObject<HTMLPreElement>}
          style={{
            flex: 1, margin: 0, padding: '10px 14px', overflow: 'auto',
            fontFamily: '"JetBrains Mono", "D2Coding", Menlo, ui-monospace, monospace',
            fontSize: 13, lineHeight: 1.3, letterSpacing: 0,
            color: '#e6e0ee', whiteSpace: 'pre', tabSize: 4, background: '#16121c'
          }}
        >
          {raw ? <AnsiText raw={raw} /> : '화면을 불러오는 중…'}
        </pre>
      )}

      {/* 입력창 — 학생에게 직접 전송(양방향) */}
      <div style={{
        display: 'flex', alignItems: 'center', gap: 8,
        padding: '8px 12px',
        background: 'var(--cth-cream-50)',
        borderTop: '1px solid var(--cth-cream-200)'
      }}>
        <span style={{
          fontFamily: 'var(--cth-font-ui)', fontSize: 13, fontWeight: 700,
          color: flash === 'ok' ? 'var(--cth-mint)' : flash === 'err' ? 'var(--cth-coral)' : 'var(--cth-sky)',
          flexShrink: 0
        }}>{flash === 'err' ? '!' : '›'}</span>
        <input
          ref={inputRef}
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={onKey}
          disabled={sending}
          placeholder="학생에게 지시 — Enter 전송 · Ctrl+C 인터럽트"
          style={{
            flex: 1,
            fontFamily: 'var(--cth-font-ui)', fontSize: 13,
            background: '#fff', border: '1px solid var(--cth-cream-200)', borderRadius: 9,
            padding: '7px 11px', outline: 'none',
            color: 'var(--cth-ink-900)', opacity: sending ? 0.5 : 1
          }}
        />
        <button
          onClick={() => void send(true)}
          disabled={!input || sending}
          style={{
            fontFamily: 'var(--cth-font-ui)', fontSize: 13, fontWeight: 600,
            padding: '7px 14px', border: 'none', borderRadius: 9,
            cursor: !input || sending ? 'not-allowed' : 'pointer',
            background: 'linear-gradient(180deg, #6BB0F0, #4A90E2)', color: '#fff',
            boxShadow: 'inset 0 1px 0 rgba(255,255,255,0.5)',
            opacity: !input || sending ? 0.4 : 1
          }}
        >전송</button>
      </div>
    </div>
  );
}
