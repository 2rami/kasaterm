import { useEffect, useState } from 'react';
import { PixelPanel } from './PixelPanel';
import { PixelButton } from './PixelButton';
import { SpritePortrait } from './SpritePortrait';
import { fetchCharacters, spawnAgent, type CharacterDef } from '@/lib/mcp';

// 학생(워커) 부르기 — munder AddAgentModal 을 우리 MCP(/characters·/spawn)에 맞게
// 단순화. provider/command 자유주입은 백엔드가 막았으므로(claude 전용) 캐릭터+모델
// +cwd 만 고른다. 캐릭터는 /characters 의 members 에서만(오타→백엔드가 unknown 거부).
const MODELS = ['opus', 'sonnet', 'haiku'] as const;

export interface AddAgentModalProps {
  onClose: () => void;
  onSpawned?: (surfaceId?: string) => void;
}

export function AddAgentModal({ onClose, onSpawned }: AddAgentModalProps) {
  const [members, setMembers] = useState<CharacterDef[]>([]);
  const [character, setCharacter] = useState<string>('');
  const [model, setModel] = useState<string>('sonnet');
  const [cwd, setCwd] = useState<string>('');
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    fetchCharacters().then((c) => {
      if (!alive || !c?.members?.length) return;
      setMembers(c.members);
      setCharacter(c.members[0].name);
    });
    return () => { alive = false; };
  }, []);

  const submit = async () => {
    setBusy(true);
    setErr(null);
    const res = await spawnAgent({ character, model, cwd });
    setBusy(false);
    if (res.ok) {
      onSpawned?.(res.surface_id);
      onClose();
    } else {
      setErr(res.notes ?? 'spawn 실패');
    }
  };

  return (
    <div
      onClick={onClose}
      style={{
        position: 'fixed', inset: 0, zIndex: 20,
        background: 'rgba(26,19,32,0.45)',
        display: 'flex', alignItems: 'center', justifyContent: 'center'
      }}
    >
      <div onClick={(e) => e.stopPropagation()} style={{ width: 380 }}>
        <PixelPanel variant="dialog" title="학생 부르기">
          <div style={{ display: 'flex', flexDirection: 'column', gap: 'var(--cth-space-4)' }}>
            {/* 캐릭터 픽커 */}
            <div style={{ display: 'flex', flexWrap: 'wrap', gap: 8 }}>
              {members.map((m) => {
                const picked = m.name === character;
                return (
                  <button
                    key={m.name}
                    onClick={() => setCharacter(m.name)}
                    style={{
                      display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 2,
                      padding: 6, cursor: 'pointer', border: 'none',
                      background: picked ? `var(--cth-sky-light)` : 'var(--cth-cream-200)',
                      boxShadow: `inset 0 0 0 ${picked ? 2 : 1}px var(--cth-ink-900)`
                    }}
                  >
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

            {/* cwd (선택) */}
            <label style={{ fontSize: 'var(--cth-text-body-sm)', color: 'var(--cth-ink-700)' }}>
              작업 폴더 <span style={{ color: 'var(--cth-ink-300)' }}>(선택 · 절대경로)</span>
              <input
                value={cwd}
                onChange={(e) => setCwd(e.target.value)}
                placeholder="비우면 현재 폴더"
                style={{
                  display: 'block', marginTop: 4, width: '100%', padding: '4px 8px',
                  background: 'var(--cth-cream-50)', boxShadow: 'inset 0 0 0 1px var(--cth-ink-700)',
                  border: 'none'
                }}
              />
            </label>

            {err && (
              <div style={{ color: 'var(--cth-coral)', fontSize: 'var(--cth-text-body-sm)' }}>{err}</div>
            )}

            <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
              <PixelButton onClick={onClose}>취소</PixelButton>
              <PixelButton variant="primary" onClick={submit} disabled={busy || !character}>
                {busy ? '부르는 중…' : '부르기'}
              </PixelButton>
            </div>
          </div>
        </PixelPanel>
      </div>
    </div>
  );
}
