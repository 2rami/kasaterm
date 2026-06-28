import { useEffect, useMemo, useRef, useState } from 'react';
import { fetchMessages, sendToPane, sendToInbox, sendToGod, type MessageEntry } from '@/lib/mcp';
import { useStore } from '@/store';
import { SpritePortrait } from './SpritePortrait';
import { assignSprites } from '@/lib/sprites';

// from_pane 이 'sensei' 면 선생님(우측 카톡 노랑), 그 외는 학생/아로나(좌측 아바타).
const SENSEI = 'sensei';
// 백엔드 캐릭터명 해석 실패 시 from_name 에 pane id('%3')가 박힌다(거노: 프사 %).
const looksLikePaneId = (s: string) => /^%?\d+$/.test(s.trim());

const hhmm = (ts: number) => {
  const d = new Date(ts * 1000);
  const h = d.getHours().toString().padStart(2, '0');
  const m = d.getMinutes().toString().padStart(2, '0');
  return `${h}:${m}`;
};

// 시간대별 첫인사 — 빈 모모톡(아직 메시지 0)에서 배정 학생이 선생님을 맞이한다.
function timeGreeting(): string {
  const h = new Date().getHours();
  if (h < 6) return '늦은 시간이에요 선생님, 무리하지 마세요';
  if (h < 11) return '좋은 아침이에요 선생님';
  if (h < 17) return '좋은 오후예요 선생님';
  if (h < 21) return '좋은 저녁이에요 선생님';
  return '오늘도 고생 많으셨어요 선생님';
}

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
  const agents = useStore((s) => s.agents);
  const [filter, setFilter] = useState<string | null>(null); // pane id 또는 null(전체)
  const [input, setInput] = useState('');
  const [sending, setSending] = useState(false);
  const [flash, setFlash] = useState<'ok' | 'err' | null>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  const atBottomRef = useRef(true);

  // 아바타·이름표는 board(라이브 폴링) 캐릭터명을 우선한다 — messages 의 from_name 은
  // 기록 시점 마커 상태로 고정돼 자주 pane id 로 깨져 프사가 안 떴다(거노: 대화창에도
  // 교실처럼 프사 넣어줘). board 도 pane id 면 from_name 폴백, 그것도 id 면 SpritePortrait 가 막음.
  const byPane = useMemo(() => new Map(agents.map((a) => [a.id, a.character])), [agents]);
  const charOf = (pane: string, fallback: string) => {
    const c = byPane.get(pane);
    if (c && !looksLikePaneId(c)) return c;
    return fallback;
  };
  const labelOf = (pane: string, fallback: string) => {
    if (pane === SENSEI) return '선생님';
    const c = charOf(pane, fallback);
    return looksLikePaneId(c) ? pane : (c || pane);
  };

  useEffect(() => {
    let stopped = false;
    const tick = async () => {
      // 모모톡 = inbox(messages.jsonl)만 보여준다(거노: 캡처 프록시 conversation 의
      // assistant turn 을 끌어오면, 실제 inbox 에 없는 답변이 유령처럼 떴다 — "답변이
      // 모모톡에 있는데 실제론 없다"). 에이전트 답장은 kasacollab msg(inbox)로 와야 뜬다.
      const list = await fetchMessages(25);
      if (stopped) return;
      // 백엔드는 ts 내림차순. 같은 발신자 동일 텍스트 중복 제거 후 오래된→최신으로 뒤집어 표시.
      const seen = new Set<string>();
      const deduped = list.filter((m) => {
        const key = `${m.from_pane}|${m.text.trim()}`;
        if (seen.has(key)) return false;
        seen.add(key);
        return true;
      });
      setMsgs(deduped.reverse());
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

  // 빈 화면(메시지 0)에서 맞이할 학생 — assignSprites 로 외형(spriteChar) 확보(작업명 학생도
  // 게임개발부 도트로). god 강조는 빼고(거노) 배정 학생만. 특정 학생 탭이면 그 학생만.
  const sprited = useMemo(() => assignSprites(agents), [agents]);
  const emptyStudents = useMemo(
    () => (filter ? sprited.filter((a) => a.id === filter) : sprited.filter((a) => !a.isGod)),
    [sprited, filter],
  );

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
  const targetName = filter ? labelOf(filter, participants.find((p) => p.pane === filter)?.name ?? filter) : '아로나';
  const send = async () => {
    const text = input.trim();
    if (!text || sending) return;
    setSending(true);
    // 모모톡 = 에이전트 inbox(거노). 일반 메시지는 PTY 프롬프트로 주입하지 않고
    // 상대(아로나/학생)의 inbox 에 넣는다(read=false) — 상대가 drain_unread 로 받고,
    // idle 이면 god-loop nudge 가 깨운다. 슬래시 명령(/context 등)만 예외로 그 학생
    // PTY 에 직접 주입(그 학생 기능)하고 모모톡엔 안 남긴다(nopersist).
    const isSlash = text.startsWith('/');
    const targetId = filter ?? agents.find((a) => a.isGod)?.id;
    const ok = targetId
      ? isSlash
        ? await sendToPane(targetId, text, true, false)
        : await sendToInbox(targetId, text)
      : await sendToGod(text);
    setSending(false);
    setFlash(ok ? 'ok' : 'err');
    setTimeout(() => setFlash(null), 1200);
    if (ok) {
      // 모모톡은 모모톡에만 남긴다 — 학생별 대화 탭으로 휙 넘어가지 않게(거노: 모모톡에
      // 쳤는데 대화에 들어가는 문제). 상대 답변은 conversation 폴링으로 이 단톡방에 뜬다.
      setInput(''); atBottomRef.current = true;
    }
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
          <FilterChip key={p.pane} label={labelOf(p.pane, p.name)} active={filter === p.pane} onClick={() => setFilter(p.pane)} />
        ))}
      </div>

      {/* 단톡방 피드 */}
      <div ref={bodyRef} onScroll={onScroll} style={{ flex: 1, overflowY: 'auto', padding: '12px 12px 16px', minHeight: 0 }}>
        {shown.length === 0 ? (
          <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', justifyContent: 'center', minHeight: '72%', gap: 16, padding: '24px 16px' }}>
            {emptyStudents.length > 0 && (
              <div style={{ display: 'flex', gap: 14, flexWrap: 'wrap', justifyContent: 'center', maxWidth: 300 }}>
                {emptyStudents.map((s) => (
                  <div key={s.id} style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 5 }}>
                    <div style={{ width: 52, height: 52, borderRadius: 14, overflow: 'hidden', background: 'var(--cth-cream-100)', display: 'flex', alignItems: 'flex-end', justifyContent: 'center' }}>
                      <SpritePortrait character={s.spriteChar || s.character} scale={2.4} bust />
                    </div>
                    <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 10, color: 'var(--cth-ink-500)', fontWeight: 600 }}>{s.spriteChar || s.character}</span>
                  </div>
                ))}
              </div>
            )}
            <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 13, fontWeight: 700, color: 'var(--cth-ink-700)', textAlign: 'center' }}>{timeGreeting()}</div>
            <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-300)', textAlign: 'center' }}>
              {emptyStudents.length > 0 ? '아래에 첫 메시지를 보내보세요' : '아직 배정된 학생이 없어요'}
            </div>
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
                      display: 'flex', alignItems: 'center', justifyContent: 'center'
                    }}>
                      <SpritePortrait character={charOf(m.from_pane, m.from_name)} scale={1.4} bust />
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
                    <span>{labelOf(m.from_pane, m.from_name)}</span>
                    <span style={{ color: 'var(--cth-ink-300)', fontWeight: 400 }}>→</span>
                    <span style={{ color: 'var(--cth-ink-300)', fontWeight: 400 }}>{labelOf(m.to_pane, m.to_name)}</span>
                  </div>
                )}
                <div style={{ display: 'flex', alignItems: 'flex-end', gap: 5, flexDirection: sensei ? 'row-reverse' : 'row' }}>
                  <div style={{
                    padding: '8px 12px', borderRadius: 14,
                    borderTopLeftRadius: sensei ? 14 : (grouped ? 14 : 4),
                    borderTopRightRadius: sensei ? (grouped ? 14 : 4) : 14,
                    background: sensei ? '#FEE500' : 'var(--cth-cream-50)',
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
            background: 'var(--cth-cream-50)', border: '1px solid var(--cth-cream-200)', borderRadius: 9,
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
        background: active ? 'var(--cth-sky)' : 'var(--cth-cream-50)',
        color: active ? '#fff' : 'var(--cth-ink-500)',
        transition: 'background 120ms ease, color 120ms ease'
      }}
    >{label}</button>
  );
}
