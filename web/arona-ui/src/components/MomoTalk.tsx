import { useEffect, useMemo, useRef, useState } from 'react';
import { fetchMessages, sendToPane, sendToGod, type MessageEntry } from '@/lib/mcp';
import { SpritePortrait } from './SpritePortrait';

// from_pane 이 'sensei' 면 선생님(우측 카톡 노랑), 그 외는 학생/아로나(좌측 아바타).
const SENSEI = 'sensei';
const nameOf = (pane: string, name: string) => (pane === SENSEI ? '선생님' : name || pane);

const hhmm = (ts: number) => {
  const d = new Date(ts * 1000);
  const h = d.getHours().toString().padStart(2, '0');
  const m = d.getMinutes().toString().padStart(2, '0');
  return `${h}:${m}`;
};

// done:summary|meta → {tag:'완료', body:summary}. 그 외는 그대로.
function parseText(raw: string): { tag?: string; body: string } {
  if (raw.startsWith('done:')) {
    const summary = raw.slice('done:'.length).split('|')[0].trim();
    return { tag: '완료', body: summary || '작업 완료' };
  }
  return { body: raw };
}

interface Participant { pane: string; name: string; }

export function MomoTalk() {
  const [msgs, setMsgs] = useState<MessageEntry[]>([]);
  const [filter, setFilter] = useState<string | null>(null); // pane id 또는 null(전체)
  const [input, setInput] = useState('');
  const [sending, setSending] = useState(false);
  const [flash, setFlash] = useState<'ok' | 'err' | null>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  const atBottomRef = useRef(true);

  useEffect(() => {
    let stopped = false;
    const tick = async () => {
      const list = await fetchMessages(60);
      if (stopped) return;
      // 백엔드는 ts 내림차순 → 단톡방은 오래된→최신(아래로) 이라 뒤집는다.
      setMsgs(list.slice().reverse());
    };
    void tick();
    const iv = setInterval(tick, 1500);
    return () => { stopped = true; clearInterval(iv); };
  }, []);

  // 참가자 목록(필터 칩) — sensei 제외하고 등장한 pane 들.
  const participants = useMemo<Participant[]>(() => {
    const seen = new Map<string, string>();
    for (const m of msgs) {
      if (m.from_pane && m.from_pane !== SENSEI) seen.set(m.from_pane, m.from_name);
      if (m.to_pane && m.to_pane !== SENSEI) seen.set(m.to_pane, m.to_name);
    }
    return [...seen.entries()].map(([pane, name]) => ({ pane, name }));
  }, [msgs]);

  const shown = filter
    ? msgs.filter((m) => m.from_pane === filter || m.to_pane === filter)
    : msgs;

  // 새 메시지 도착 시 바닥이었으면 자동 스크롤.
  useEffect(() => {
    const el = bodyRef.current;
    if (el && atBottomRef.current) el.scrollTop = el.scrollHeight;
  }, [shown]);

  const onScroll = () => {
    const el = bodyRef.current;
    if (!el) return;
    atBottomRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
  };

  // 선생님 발신 — 받는 사람 = 활성 필터(전체→아로나=god, 특정 학생→그 학생).
  // 두 경로 모두 백엔드가 persist_sensei_msg 로 messages.jsonl 에 기록 → 다음 폴링에 노란 버블.
  const targetName = filter ? (participants.find((p) => p.pane === filter)?.name ?? filter) : '아로나';
  const send = async () => {
    const text = input.trim();
    if (!text || sending) return;
    setSending(true);
    const ok = filter ? await sendToPane(filter, text, true) : await sendToGod(text);
    setSending(false);
    setFlash(ok ? 'ok' : 'err');
    setTimeout(() => setFlash(null), 1200);
    if (ok) { setInput(''); atBottomRef.current = true; }
  };

  return (
    <div style={{ flex: 1, display: 'flex', flexDirection: 'column', minHeight: 0, background: 'var(--cth-cream-100)' }}>
      {/* 참가자 필터 칩 */}
      <div style={{
        display: 'flex', gap: 5, padding: '8px 10px', flexWrap: 'wrap',
        borderBottom: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-50)'
      }}>
        <FilterChip label="전체" active={filter === null} onClick={() => setFilter(null)} />
        {participants.map((p) => (
          <FilterChip key={p.pane} label={p.name} active={filter === p.pane} onClick={() => setFilter(p.pane)} />
        ))}
      </div>

      {/* 단톡방 피드 */}
      <div ref={bodyRef} onScroll={onScroll} style={{ flex: 1, overflowY: 'auto', padding: '12px 12px 16px', minHeight: 0 }}>
        {shown.length === 0 ? (
          <div style={{ color: 'var(--cth-ink-300)', fontFamily: 'var(--cth-font-ui)', fontSize: 12, textAlign: 'center', marginTop: 40 }}>
            아직 오간 메시지가 없어요
          </div>
        ) : shown.map((m, i) => {
          const sensei = m.from_pane === SENSEI;
          const prev = shown[i - 1];
          // 같은 발신자 연속이면 아바타/이름 생략(말풍선만 붙임).
          const grouped = prev && prev.from_pane === m.from_pane && prev.to_pane === m.to_pane;
          const { tag, body } = parseText(m.text);
          return (
            <div key={m.id || i} style={{
              display: 'flex', justifyContent: sensei ? 'flex-end' : 'flex-start',
              gap: 8, alignItems: 'flex-end', marginTop: grouped ? 3 : 11
            }}>
              {!sensei && (
                <div style={{ width: 32, flexShrink: 0 }}>
                  {!grouped && (
                    <div style={{
                      width: 32, height: 32, borderRadius: 10, overflow: 'hidden',
                      background: 'var(--cth-cream-50)', border: '1px solid var(--cth-cream-200)',
                      display: 'flex', alignItems: 'flex-end', justifyContent: 'center'
                    }}>
                      <SpritePortrait character={m.from_name} scale={1.4} />
                    </div>
                  )}
                </div>
              )}

              <div style={{ maxWidth: '74%', display: 'flex', flexDirection: 'column', alignItems: sensei ? 'flex-end' : 'flex-start' }}>
                {!grouped && (
                  <div style={{
                    fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: 600,
                    color: 'var(--cth-ink-500)', marginBottom: 3, padding: '0 2px',
                    display: 'flex', gap: 4, alignItems: 'center'
                  }}>
                    <span>{nameOf(m.from_pane, m.from_name)}</span>
                    <span style={{ color: 'var(--cth-ink-300)', fontWeight: 400 }}>→</span>
                    <span style={{ color: 'var(--cth-ink-300)', fontWeight: 400 }}>{nameOf(m.to_pane, m.to_name)}</span>
                  </div>
                )}
                <div style={{ display: 'flex', alignItems: 'flex-end', gap: 5, flexDirection: sensei ? 'row-reverse' : 'row' }}>
                  <div style={{
                    padding: '8px 12px', borderRadius: 14,
                    borderTopLeftRadius: sensei ? 14 : (grouped ? 14 : 4),
                    borderTopRightRadius: sensei ? (grouped ? 14 : 4) : 14,
                    background: sensei ? '#FEE500' : '#fff',
                    color: sensei ? '#3A2E00' : 'var(--cth-ink-900)',
                    border: sensei ? 'none' : '1px solid var(--cth-cream-200)',
                    boxShadow: '0 1px 2px rgba(21, 41, 74, 0.06)',
                    fontFamily: 'var(--cth-font-ui)', fontSize: 13, lineHeight: 1.5,
                    whiteSpace: 'pre-wrap', wordBreak: 'break-word'
                  }}>
                    {tag && (
                      <span style={{
                        display: 'inline-block', marginRight: 6, padding: '1px 6px', borderRadius: 5,
                        background: 'var(--cth-mint)', color: '#fff', fontSize: 10, fontWeight: 700,
                        verticalAlign: 'middle'
                      }}>{tag}</span>
                    )}
                    {body}
                  </div>
                  <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 9, color: 'var(--cth-ink-300)', flexShrink: 0 }}>{hhmm(m.ts)}</span>
                </div>
              </div>
            </div>
          );
        })}
      </div>

      {/* 선생님 입력 — 모모톡 단톡방에 직접 글 올리기(받는 사람 = 활성 필터) */}
      <div style={{
        display: 'flex', alignItems: 'center', gap: 8, padding: '8px 10px',
        borderTop: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-50)'
      }}>
        <span style={{
          fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: 700, flexShrink: 0,
          color: flash === 'ok' ? 'var(--cth-mint)' : flash === 'err' ? 'var(--cth-coral)' : 'var(--cth-ink-300)'
        }}>→ {targetName}</span>
        <input
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => { if (e.key === 'Enter') { e.preventDefault(); void send(); } }}
          disabled={sending}
          placeholder="모모톡에 글 올리기 — Enter 전송"
          style={{
            flex: 1, fontFamily: 'var(--cth-font-ui)', fontSize: 12,
            background: '#fff', border: '1px solid var(--cth-cream-200)', borderRadius: 9,
            padding: '7px 11px', outline: 'none', color: 'var(--cth-ink-900)', opacity: sending ? 0.5 : 1
          }}
        />
        <button
          onClick={() => void send()}
          disabled={!input.trim() || sending}
          style={{
            fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 600,
            padding: '7px 13px', border: 'none', borderRadius: 9,
            cursor: !input.trim() || sending ? 'not-allowed' : 'pointer',
            background: 'linear-gradient(180deg, #6BB0F0, #4A90E2)', color: '#fff',
            boxShadow: 'inset 0 1px 0 rgba(255,255,255,0.5)', opacity: !input.trim() || sending ? 0.4 : 1
          }}
        >전송</button>
      </div>
    </div>
  );
}

function FilterChip({ label, active, onClick }: { label: string; active: boolean; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      style={{
        padding: '4px 11px', borderRadius: 999, cursor: 'pointer',
        fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: 600,
        border: active ? 'none' : '1px solid var(--cth-cream-200)',
        background: active ? 'var(--cth-sky)' : '#fff',
        color: active ? '#fff' : 'var(--cth-ink-500)',
        transition: 'background 120ms ease, color 120ms ease'
      }}
    >{label}</button>
  );
}
