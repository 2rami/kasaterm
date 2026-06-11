import { useEffect, useState } from 'react';
import { useStore } from '@/store';
import { fetchSchedule, addSchedule, deleteSchedule, type ScheduleItem } from '@/lib/mcp';

type Kind = 'loop' | 'cron' | 'timer';
const KIND_LABEL: Record<Kind, string> = { loop: '반복 루프', cron: '예약', timer: '타이머' };
const KIND_COLOR: Record<Kind, string> = { loop: 'var(--cth-sky)', cron: 'var(--cth-lilac)', timer: 'var(--cth-mint)' };

function whenText(it: ScheduleItem): string {
  if (it.kind === 'loop') return `${Math.round((it.interval_sec ?? 0) / 60)}분마다`;
  const ts = it.kind === 'cron' ? it.at_ts : it.next_ts;
  if (!ts) return '';
  const d = new Date(ts * 1000);
  return `${d.getMonth() + 1}/${d.getDate()} ${d.getHours().toString().padStart(2, '0')}:${d.getMinutes().toString().padStart(2, '0')}`;
}

// 스케줄/루프 탭 — 반복 지시 루프 · 예약(크론) · 타이머/리마인더. 백엔드 타이머가
// due 항목을 학생 pane 에 자동 send. 학생을 골라 메시지 + 시각/간격 설정.
export function ScheduleTab() {
  const agents = useStore((s) => s.agents);
  const [items, setItems] = useState<ScheduleItem[]>([]);
  const [adding, setAdding] = useState(false);
  const [kind, setKind] = useState<Kind>('loop');
  const [surface, setSurface] = useState('');
  const [text, setText] = useState('');
  const [minutes, setMinutes] = useState(10); // loop 간격 / timer 후
  const [at, setAt] = useState(''); // cron datetime-local
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let stop = false;
    const tick = () => { void fetchSchedule().then((x) => { if (!stop) setItems(x); }); };
    tick();
    const iv = setInterval(tick, 3000);
    return () => { stop = true; clearInterval(iv); };
  }, []);

  useEffect(() => {
    if (!surface && agents.length) setSurface(agents[0].id);
  }, [agents, surface]);

  const submit = async () => {
    if (!surface || !text.trim() || busy) return;
    setBusy(true);
    const payload: { kind: string; surface: string; text: string; interval_sec?: number; at_ts?: number } = {
      kind, surface, text: text.trim(),
    };
    if (kind === 'cron') {
      const ts = at ? new Date(at).getTime() / 1000 : 0;
      payload.at_ts = ts;
    } else {
      payload.interval_sec = Math.max(1, Math.round(minutes)) * 60;
    }
    const ok = await addSchedule(payload);
    setBusy(false);
    if (ok) { setText(''); setAdding(false); void fetchSchedule().then(setItems); }
  };

  return (
    <div style={{ flex: 1, display: 'flex', flexDirection: 'column', minHeight: 0 }}>
      {/* 헤더 + 추가 토글 */}
      <div style={{ display: 'flex', alignItems: 'center', padding: '8px 10px', gap: 8, borderBottom: '1px solid var(--cth-cream-200)' }}>
        <span style={{ fontFamily: 'var(--cth-font-display)', fontSize: 11, color: 'var(--cth-ink-500)', flex: 1 }}>루프 · 예약 · 타이머</span>
        <button onClick={() => setAdding((v) => !v)} style={{
          padding: '4px 10px', borderRadius: 7, border: 'none', cursor: 'pointer',
          background: adding ? 'var(--cth-cream-200)' : 'var(--cth-sky)', color: adding ? 'var(--cth-ink-500)' : '#fff',
          fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: 700,
        }}>{adding ? '닫기' : '+ 추가'}</button>
      </div>

      {/* 추가 폼 */}
      {adding && (
        <div style={{ padding: 10, borderBottom: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-100)', display: 'flex', flexDirection: 'column', gap: 7 }}>
          <div style={{ display: 'flex', gap: 5 }}>
            {(['loop', 'cron', 'timer'] as Kind[]).map((k) => (
              <button key={k} onClick={() => setKind(k)} style={{
                flex: 1, padding: '5px 0', borderRadius: 7, border: 'none', cursor: 'pointer',
                background: kind === k ? KIND_COLOR[k] : '#fff', color: kind === k ? '#fff' : 'var(--cth-ink-500)',
                boxShadow: kind === k ? 'none' : 'inset 0 0 0 1px var(--cth-cream-200)',
                fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: 600,
              }}>{KIND_LABEL[k]}</button>
            ))}
          </div>
          <select value={surface} onChange={(e) => setSurface(e.target.value)} style={selStyle}>
            {agents.map((a) => <option key={a.id} value={a.id}>{a.character} ({a.id})</option>)}
          </select>
          <input value={text} onChange={(e) => setText(e.target.value)} placeholder={kind === 'timer' ? '리마인더 내용' : '보낼 지시'} style={inStyle} />
          {kind === 'cron' ? (
            <input type="datetime-local" value={at} onChange={(e) => setAt(e.target.value)} style={inStyle} />
          ) : (
            <label style={{ display: 'flex', alignItems: 'center', gap: 6, fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-500)' }}>
              {kind === 'loop' ? '간격' : '몇 분 뒤'}
              <input type="number" min={1} value={minutes} onChange={(e) => setMinutes(Number(e.target.value))} style={{ ...inStyle, width: 64 }} /> 분
            </label>
          )}
          <button onClick={() => void submit()} disabled={busy || !text.trim()} style={{
            padding: '7px 0', borderRadius: 8, border: 'none', cursor: busy || !text.trim() ? 'not-allowed' : 'pointer',
            background: 'linear-gradient(180deg, #6BB0F0, #4A90E2)', color: '#fff', fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 700,
            opacity: busy || !text.trim() ? 0.5 : 1,
          }}>{busy ? '등록 중…' : '등록'}</button>
        </div>
      )}

      {/* 목록 */}
      <div style={{ flex: 1, overflowY: 'auto', padding: 8 }}>
        {items.length === 0 ? (
          <div style={{ textAlign: 'center', color: 'var(--cth-ink-300)', fontFamily: 'var(--cth-font-ui)', fontSize: 11, marginTop: 24 }}>
            예약된 루프·작업 없음
          </div>
        ) : items.map((it) => {
          const ag = agents.find((a) => a.id === it.surface);
          const k = it.kind as Kind;
          return (
            <div key={it.id} style={{ display: 'flex', alignItems: 'center', gap: 7, padding: '7px 4px', borderBottom: '1px solid var(--cth-cream-200)', opacity: it.enabled ? 1 : 0.45 }}>
              <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 9, fontWeight: 700, color: '#fff', background: KIND_COLOR[k] ?? 'var(--cth-ink-300)', padding: '1px 6px', borderRadius: 5, flexShrink: 0 }}>{KIND_LABEL[k] ?? it.kind}</span>
              <div style={{ flex: 1, minWidth: 0 }}>
                <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 12, color: 'var(--cth-ink-900)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{it.text}</div>
                <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 10, color: 'var(--cth-ink-500)' }}>{ag?.character ?? it.surface} · {whenText(it)}</div>
              </div>
              <button onClick={() => void deleteSchedule(it.id, true).then(() => fetchSchedule().then(setItems))} title={it.enabled ? '일시정지' : '재개'} style={iconBtn}>{it.enabled ? '⏸' : '▶'}</button>
              <button onClick={() => void deleteSchedule(it.id).then(() => fetchSchedule().then(setItems))} title="삭제" style={iconBtn}>×</button>
            </div>
          );
        })}
      </div>
    </div>
  );
}

const inStyle: React.CSSProperties = {
  width: '100%', padding: '6px 9px', borderRadius: 8, border: '1px solid var(--cth-cream-200)',
  outline: 'none', fontFamily: 'var(--cth-font-ui)', fontSize: 12, background: '#fff', color: 'var(--cth-ink-900)', boxSizing: 'border-box',
};
const selStyle: React.CSSProperties = { ...inStyle };
const iconBtn: React.CSSProperties = {
  width: 24, height: 24, borderRadius: 6, border: 'none', cursor: 'pointer', flexShrink: 0,
  background: 'var(--cth-cream-100)', color: 'var(--cth-ink-500)', fontSize: 13, lineHeight: 1,
};
