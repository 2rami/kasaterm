import { useEffect, useState } from 'react';
import { fetchCharacters, characterPool, type CharacterDef } from '@/lib/mcp';
import { SpritePortrait } from './SpritePortrait';

// 학생 추가('+ 학생')·교체(캐릭터 변경) 공통 선택 팝업 — members(미도리~아리스) +
// leaders(아로나·프라나) 전체 풀에서 고른다(거노: 아로나/프라나도 학생처럼).
export function CharacterPicker({ title, note, onPick, onClose }: {
  title: string;
  note?: string;
  onPick: (name: string) => void;
  onClose: () => void;
}) {
  const [pool, setPool] = useState<CharacterDef[]>([]);
  useEffect(() => { void fetchCharacters().then((c) => setPool(characterPool(c))); }, []);
  return (
    <div onClick={onClose} style={{
      position: 'fixed', inset: 0, zIndex: 200, background: 'rgba(21,41,74,0.35)',
      display: 'flex', alignItems: 'center', justifyContent: 'center',
    }}>
      <div onClick={(e) => e.stopPropagation()} style={{
        background: 'var(--cth-cream-50)', borderRadius: 16, padding: 18, width: 320,
        boxShadow: '0 8px 32px rgba(21,41,74,0.25)',
      }}>
        <div style={{ fontFamily: 'var(--cth-font-display)', fontSize: 15, fontWeight: 700, color: 'var(--cth-ink-900)', marginBottom: note ? 4 : 12 }}>{title}</div>
        {note && <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-500)', marginBottom: 12 }}>{note}</div>}
        <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 8 }}>
          {pool.length === 0 ? (
            <div style={{ gridColumn: '1 / -1', fontFamily: 'var(--cth-font-ui)', fontSize: 12, color: 'var(--cth-ink-300)', padding: 8 }}>불러오는 중…</div>
          ) : pool.map((c) => (
            <button key={c.name} onClick={() => onPick(c.name)} className="cth-titlebar-nodrag" style={{
              display: 'flex', alignItems: 'center', gap: 8, padding: '8px 10px', borderRadius: 10,
              border: '1px solid var(--cth-cream-200)', background: '#fff', cursor: 'pointer', textAlign: 'left',
            }}>
              <div style={{ width: 32, height: 32, borderRadius: 8, overflow: 'hidden', flexShrink: 0, background: 'var(--cth-cream-100)', display: 'flex', alignItems: 'flex-end', justifyContent: 'center' }}>
                <SpritePortrait character={c.name} scale={1.5} bust />
              </div>
              <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 13, fontWeight: 700, color: 'var(--cth-ink-900)' }}>{c.name}</span>
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}
