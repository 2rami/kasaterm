import { useEffect, useState } from 'react';
import { createPortal } from 'react-dom';
import { PixelPanel } from './PixelPanel';
import { PixelButton } from './PixelButton';
import { SpritePortrait } from './SpritePortrait';
import { FolderBrowser } from './FolderBrowser';
import { fetchCharacters, spawnAgent, type CharacterDef } from '@/lib/mcp';
import { useStore } from '@/store';

// 학생(워커) 부르기 — munder AddAgentModal 을 우리 MCP(/characters·/spawn)에 맞게
// 단순화. provider/command 자유주입은 백엔드가 막았으므로(claude 전용) 캐릭터+모델
// +cwd 만 고른다. 캐릭터는 /characters 의 members 에서만(오타→백엔드가 unknown 거부).
const MODELS = ['opus', 'sonnet', 'haiku'] as const;

export interface AddAgentModalProps {
  onClose: () => void;
  onSpawned?: (surfaceId?: string) => void;
  /** 현재 방 경로 — cwd 필드 프리필(여기서 바로 편집). 비우면 현재 폴더. */
  defaultCwd?: string | null;
}

export function AddAgentModal({ onClose, onSpawned, defaultCwd }: AddAgentModalProps) {
  const [leader, setLeader] = useState<CharacterDef | null>(null);
  const [members, setMembers] = useState<CharacterDef[]>([]);
  const [character, setCharacter] = useState<string>('');
  const [model, setModel] = useState<string>('sonnet');
  const [cwd, setCwd] = useState<string>(defaultCwd ?? '');
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  // 이미 교실에 있는 캐릭터는 빼고(중복 방지), 리더(아로나)는 없을 때만 부를 수 있게.
  const agents = useStore((s) => s.agents);
  const taken = new Set(agents.map((a) => a.character));
  const available = [leader, ...members].filter((c): c is CharacterDef => !!c && !taken.has(c.name));
  const selected = available.some((c) => c.name === character) ? character : (available[0]?.name ?? '');

  useEffect(() => {
    let alive = true;
    fetchCharacters().then((c) => {
      if (!alive || !c) return;
      setLeader(c.leader ?? null);
      setMembers(c.members ?? []);
    });
    return () => { alive = false; };
  }, []);

  const submit = async () => {
    setBusy(true);
    setErr(null);
    const res = await spawnAgent({ character: selected, model, cwd });
    setBusy(false);
    if (res.ok) {
      onSpawned?.(res.surface_id);
      onClose();
    } else {
      setErr(res.notes ?? 'spawn 실패');
    }
  };

  // document.body 로 portal — 교실(가구 z 높음) 등 어떤 중첩 stacking context 도
  // 벗어나 항상 최상위에 뜬다(거노: 모달 위로 책상이 올라오던 버그 확실 차단).
  return createPortal(
    <div
      onClick={onClose}
      style={{
        position: 'fixed', inset: 0, zIndex: 1000,
        background: 'rgba(26,19,32,0.45)',
        display: 'flex', alignItems: 'center', justifyContent: 'center'
      }}
    >
      <div onClick={(e) => e.stopPropagation()} style={{ width: 380 }}>
        <PixelPanel variant="dialog" title="학생 부르기">
          <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--cth-space-4)' }}>
            {/* 캐릭터 픽커 — 리더(아로나)는 없을 때만 맨 앞에, ★ 표시 */}
            <div style={{ display: 'flex', flexWrap: 'wrap', gap: 8 }}>
              {available.length === 0 ? (
                <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 'var(--cth-text-body-sm)', color: 'var(--cth-ink-500)' }}>
                  부를 수 있는 캐릭터가 다 차 있어요
                </span>
              ) : available.map((m) => {
                const picked = m.name === selected;
                const isLeader = m.name === leader?.name;
                return (
                  <button
                    key={m.name}
                    onClick={() => setCharacter(m.name)}
                    title={isLeader ? '아로나 — 리더(god)' : m.name}
                    style={{
                      position: 'relative',
                      display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 2,
                      padding: 6, cursor: 'pointer', border: 'none',
                      background: picked ? `var(--cth-sky-light)` : 'var(--cth-cream-200)',
                      boxShadow: `inset 0 0 0 ${picked ? 2 : 1}px ${isLeader ? 'var(--cth-lemon)' : 'var(--cth-ink-900)'}`
                    }}
                  >
                    {isLeader && (
                      <span style={{ position: 'absolute', top: 1, right: 2, fontSize: 9, color: 'var(--cth-lemon)', fontWeight: 800 }}>★</span>
                    )}
                    <SpritePortrait character={m.name} scale={1} />
                    <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 'var(--cth-text-body-sm)' }}>
                      {m.name}
                    </span>
                  </button>
                );
              })}
            </div>

            {/* 모델 */}
            <label style={{ fontSize: 'var(--cth-text-body-sm)', color: 'var(--cth-ink-700)' }}>
              모델
              <select
                value={model}
                onChange={(e) => setModel(e.target.value)}
                style={{
                  display: 'block', marginTop: 4, width: '100%', padding: '4px 8px',
                  background: 'var(--cth-cream-50)', boxShadow: 'inset 0 0 0 1px var(--cth-ink-700)',
                  border: 'none'
                }}
              >
                {MODELS.map((m) => <option key={m} value={m}>{m}</option>)}
              </select>
            </label>

            {/* 작업 폴더 — 방 경로 변경처럼 폴더 클릭 탐색(상위로/하위 진입) */}
            <div style={{ fontSize: 'var(--cth-text-body-sm)', color: 'var(--cth-ink-700)' }}>
              <div style={{ marginBottom: 4 }}>
                작업 폴더 <span style={{ color: 'var(--cth-ink-300)' }}>(탐색해서 선택)</span>
              </div>
              <FolderBrowser initialPath={defaultCwd} onPathChange={setCwd} height={150} />
              <div style={{ marginTop: 4, fontFamily: 'var(--cth-font-mono)', fontSize: 11, color: 'var(--cth-ink-500)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', direction: 'rtl', textAlign: 'left' }} title={cwd}>
                선택: {cwd || '현재 폴더'}
              </div>
            </div>

            {err && (
              <div style={{ color: 'var(--cth-coral)', fontSize: 'var(--cth-text-body-sm)' }}>{err}</div>
            )}

            <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
              <PixelButton onClick={onClose}>취소</PixelButton>
              <PixelButton variant="primary" onClick={submit} disabled={busy || !selected}>
                {busy ? '부르는 중…' : '부르기'}
              </PixelButton>
            </div>
          </div>
        </PixelPanel>
      </div>
    </div>,
    document.body,
  );
}
