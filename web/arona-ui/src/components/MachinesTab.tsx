import { useEffect, useState } from 'react';
import { useStore } from '@/store';
import { fetchMachines, postMigrate, type MachineInfo } from '@/lib/mcp';
import { SpritePortrait } from './SpritePortrait';
import { assignSprites } from '@/lib/sprites';

// 이사 탭 — 기계(맥미니 등)별 학생 목록을 보고 버튼으로 보내고 데려온다(pane-migrate).
// 로컬 학생 목록은 이미 폴링되는 store.agents 를 그대로 쓴다 — /board 를 또 부르지 않는다.

interface MigrateOutcome { ok: boolean; text: string }

const agoLabel = (secs?: number | null) => {
  if (secs == null) return '한 번도 못 닿았어요';
  if (secs < 60) return `${secs}초 전까지 닿았어요`;
  if (secs < 3600) return `${Math.floor(secs / 60)}분 전까지 닿았어요`;
  return `${Math.floor(secs / 3600)}시간 전까지 닿았어요`;
};

const SectionLabel = ({ children }: { children: React.ReactNode }) => (
  <div style={{
    fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700, color: 'var(--cth-ink-500)',
    textTransform: 'uppercase', letterSpacing: 0.5, margin: '2px 4px 8px',
    display: 'flex', alignItems: 'center', gap: 6,
  }}>{children}</div>
);

// 작업중=sky(펄스) / 확인대기=coral / 그 외=초록 — 보드 행의 상태점과 같은 문법.
function StatusDot({ status }: { status?: string | null }) {
  const busy = status === 'working' || status === 'thinking';
  const wait = status === 'waiting' || status === 'blocked';
  return (
    <span style={{
      width: 8, height: 8, borderRadius: 999, flexShrink: 0,
      background: wait ? 'var(--cth-coral)' : busy ? 'var(--cth-sky)' : 'var(--cth-status-success)',
      animation: busy ? 'cth-dot-pulse 1.3s ease-in-out infinite' : undefined,
    }} />
  );
}

const migrateBtn = (disabled: boolean): React.CSSProperties => ({
  flexShrink: 0, padding: '3px 8px', borderRadius: 6, border: '1px solid var(--cth-cream-200)',
  background: 'var(--cth-cream-100)', color: disabled ? 'var(--cth-ink-300)' : 'var(--cth-ink-700)',
  fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700, whiteSpace: 'nowrap',
  cursor: disabled ? 'not-allowed' : 'pointer', opacity: disabled ? 0.6 : 1,
});

function StudentRow({ character, name, title, status, busy, result, buttons }: {
  character: string;
  name: string;
  title?: string | null;
  status?: string | null;
  busy: boolean;
  result?: MigrateOutcome;
  buttons: React.ReactNode;
}) {
  return (
    <div style={{ padding: '6px 0', borderBottom: '1px solid var(--cth-cream-200)' }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
        <div style={{ width: 26, height: 26, borderRadius: 7, overflow: 'hidden', flexShrink: 0, background: 'var(--cth-cream-100)', display: 'flex', alignItems: 'flex-end', justifyContent: 'center' }}>
          <SpritePortrait character={character} scale={1.2} bust />
        </div>
        <StatusDot status={status} />
        <div style={{ flex: 1, minWidth: 0 }}>
          <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 600, color: 'var(--cth-ink-900)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{name}</div>
          {!!title && (
            <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-500)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{title}</div>
          )}
        </div>
        {busy ? (
          <span style={{ flexShrink: 0, fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700, color: 'var(--cth-sky)' }}>이사 중…</span>
        ) : (
          <div style={{ display: 'flex', gap: 4, flexShrink: 0 }}>{buttons}</div>
        )}
      </div>
      {!busy && result && (
        <div style={{
          marginLeft: 34, marginTop: 3, fontFamily: 'var(--cth-font-ui)', fontSize: 10,
          color: result.ok ? 'var(--cth-mint)' : 'var(--cth-coral)', opacity: result.ok ? 1 : 0.85,
          overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap',
        }}>{result.text}</div>
      )}
    </div>
  );
}

export function MachinesTab() {
  const agents = useStore((s) => s.agents);
  // null = 첫 응답 전(로딩) — 빈 배열(기계 미등록)과 구분해야 빈 상태 문구가 안 깜빡인다.
  const [machines, setMachines] = useState<MachineInfo[] | null>(null);
  const [migrating, setMigrating] = useState<Record<string, boolean>>({});
  const [results, setResults] = useState<Record<string, MigrateOutcome>>({});

  useEffect(() => {
    let stop = false;
    const tick = () => { void fetchMachines().then((ms) => { if (!stop) setMachines(ms); }); };
    tick();
    const iv = setInterval(tick, 2500);
    return () => { stop = true; clearInterval(iv); };
  }, []);

  const sprited = assignSprites(agents);
  const spriteOf = new Map(sprited.map((a) => [a.id, a.spriteChar || a.character]));

  const migrate = async (paneId: string, target: string, doneText: string) => {
    if (migrating[paneId]) return;
    setMigrating((m) => ({ ...m, [paneId]: true }));
    setResults((r) => { const n = { ...r }; delete n[paneId]; return n; });
    const res = await postMigrate(paneId, target);
    setMigrating((m) => { const n = { ...m }; delete n[paneId]; return n; });
    setResults((r) => ({ ...r, [paneId]: res.ok ? { ok: true, text: doneText } : { ok: false, text: res.error || '실패했어요' } }));
  };

  if (machines === null) {
    return <Center text="기계 목록을 불러오는 중…" />;
  }
  if (machines.length === 0) {
    return <Center text={'등록된 기계가 없어요 —\n~/.config/kasaterm/machines.json 에 적으면 여기 떠요'} />;
  }

  const localAgents = agents.filter((a) => !a.machine);

  return (
    <div style={{ flex: 1, display: 'flex', flexDirection: 'column', minHeight: 0, background: 'var(--cth-cream-100)' }}>
      <div style={{ flex: 1, overflowY: 'auto', padding: 10, minHeight: 0 }}>
        {/* 이 맥북 — machine 없는 로컬 학생. 기계 수만큼 보내기 버튼. */}
        <SectionLabel>이 맥북</SectionLabel>
        {localAgents.length === 0 ? (
          <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-300)', margin: '0 4px 8px' }}>학생 없음</div>
        ) : localAgents.map((a) => (
          <StudentRow
            key={a.id}
            character={spriteOf.get(a.id) || a.character}
            name={a.character}
            title={a.title}
            status={a.status}
            busy={!!migrating[a.id]}
            result={results[a.id]}
            buttons={machines.map((m) => (
              <button
                key={m.label}
                disabled={!m.online}
                title={m.online ? `${m.label}(으)로 보내기` : `연결이 안 닿아요 — ${agoLabel(m.ago_secs)}`}
                onClick={() => void migrate(a.id, m.label, `${m.label}(으)로 보냈어요`)}
                style={migrateBtn(!m.online)}
              >{`→ ${m.label}`}</button>
            ))}
          />
        ))}

        {/* 기계별 섹션 — 그 기계 panes + 이사 간 학생의 로컬 미러(store, machine==라벨). */}
        {machines.map((m) => {
          const mirrored = agents.filter((a) => a.machine === m.label);
          // 같은 학생이 두 번 보이면 안 된다 — machine 있는 store 행을 우선하고,
          // 원격 panes 는 id 가 겹치면 뺀다.
          const mirroredIds = new Set(mirrored.map((a) => a.id));
          const panes = m.panes.filter((p) => !p.id || !mirroredIds.has(p.id));
          return (
            <div key={m.label} style={{ marginTop: 16 }}>
              <SectionLabel>
                <span>{m.label}</span>
                <span style={{
                  textTransform: 'none', letterSpacing: 0, padding: '1px 7px', borderRadius: 5,
                  fontSize: 9, fontWeight: 700,
                  color: m.online ? '#fff' : 'var(--cth-ink-300)',
                  background: m.online ? 'var(--cth-mint)' : 'var(--cth-cream-200)',
                }}>{m.online ? '연결됨' : '연결 안 닿아요'}</span>
                {!m.online && (
                  <span style={{ textTransform: 'none', letterSpacing: 0, fontWeight: 500, color: 'var(--cth-ink-300)' }}>{agoLabel(m.ago_secs)}</span>
                )}
              </SectionLabel>
              {mirrored.length === 0 && panes.length === 0 ? (
                <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-300)', margin: '0 4px' }}>학생 없음</div>
              ) : (
                <>
                  {mirrored.map((a) => (
                    <StudentRow
                      key={a.id}
                      character={spriteOf.get(a.id) || a.character}
                      name={a.character}
                      title={a.title}
                      status={a.status}
                      busy={!!migrating[a.id]}
                      result={results[a.id]}
                      buttons={
                        <button
                          title="이 맥북으로 데려오기"
                          onClick={() => void migrate(a.id, 'local', '데려왔어요')}
                          style={migrateBtn(false)}
                        >← 데려오기</button>
                      }
                    />
                  ))}
                  {panes.map((p, i) => (
                    <StudentRow
                      key={p.id || `${m.label}-${i}`}
                      character={p.name || ''}
                      name={p.name || p.id || '이름 없는 학생'}
                      title={p.title}
                      status={p.status}
                      busy={false}
                      buttons={null}
                    />
                  ))}
                </>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

function Center({ text }: { text: string }) {
  return (
    <div style={{
      flex: 1, display: 'flex', alignItems: 'center', justifyContent: 'center', padding: 20,
      textAlign: 'center', whiteSpace: 'pre-line', color: 'var(--cth-ink-300)',
      fontFamily: 'var(--cth-font-ui)', fontSize: 12, lineHeight: 1.6,
    }}>{text}</div>
  );
}
