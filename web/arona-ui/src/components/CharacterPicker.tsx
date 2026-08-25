import { useEffect, useState } from 'react';
import { fetchCharacters, fetchThemeRoster, fetchThemesList, characterPool, characterFaceUrl, type CharacterDef } from '@/lib/mcp';
import { SpritePortrait } from './SpritePortrait';

// 테마 묶음 하나 — theme 이 없으면 활성 로스터(프사도 활성 경로에서 온다).
interface Group {
  key: string;
  label: string;
  theme?: string;
  pool: CharacterDef[];
}

// 프사 한 칸 — 테마별 /character-face 를 먼저 쓰고, 없으면(404) 기존 도트
// 초상 폴백. slug 가 없는 항목은 처음부터 폴백이다.
function Face({ c, theme }: { c: CharacterDef; theme?: string }) {
  const [broken, setBroken] = useState(false);
  useEffect(() => setBroken(false), [c.slug, theme]);
  if (!c.slug || broken) return <SpritePortrait character={c.name} scale={1.5} bust />;
  return (
    <img
      src={characterFaceUrl(c.slug, theme)}
      onError={() => setBroken(true)}
      style={{ width: '100%', height: '100%', objectFit: 'cover', imageRendering: 'pixelated' }}
    />
  );
}

// 학생 추가('+ 학생')·재배정(캐릭터 변경) 공통 선택 팝업. 활성 테마만이 아니라
// 기본(번들)·설치 테마 전부를 묶음으로 보여 준다 — 진행 중 pane 을 어느 테마
// 캐릭터로든 바꿀 수 있어야 해서다(2026-08-24 지시).
export function CharacterPicker({ title, note, onPick, onClose }: {
  title: string;
  note?: string;
  onPick: (name: string) => void;
  onClose: () => void;
}) {
  const [groups, setGroups] = useState<Group[]>([]);
  const [loaded, setLoaded] = useState(false);
  useEffect(() => {
    void (async () => {
      const [act, meta] = await Promise.all([fetchCharacters(), fetchThemesList()]);
      const gs: Group[] = [];
      if (act) {
        const label = meta.active
          ? (meta.themes.find((t) => t.id === meta.active)?.label ?? meta.active)
          : '기본';
        gs.push({ key: 'active', label, pool: characterPool(act) });
      }
      // 테마가 활성일 때만 「기본」 묶음이 따로 생긴다 — 미선택이면 활성 = 기본.
      if (meta.active) {
        const base = await fetchThemeRoster('__base');
        if (base) gs.push({ key: '__base', label: '기본', pool: characterPool(base) });
      }
      for (const t of meta.themes) {
        if (t.id === meta.active) continue;
        const r = await fetchThemeRoster(t.id);
        if (r) gs.push({ key: t.id, label: t.label, theme: t.id, pool: characterPool(r) });
      }
      setGroups(gs.filter((g) => g.pool.length > 0));
      setLoaded(true);
    })();
  }, []);
  return (
    <div onClick={onClose} style={{
      position: 'fixed', inset: 0, zIndex: 200, background: 'rgba(21,41,74,0.35)',
      display: 'flex', alignItems: 'center', justifyContent: 'center',
    }}>
      <div onClick={(e) => e.stopPropagation()} style={{
        // 390px 폰에서 340 은 좌우 여백이 25px 씩밖에 안 남는다. 데스크톱에선 그대로 340.
        background: 'var(--cth-cream-50)', borderRadius: 16, padding: 18, width: 'min(340px, 92vw)',
        maxHeight: '72vh', display: 'flex', flexDirection: 'column',
        boxShadow: '0 8px 32px rgba(21,41,74,0.25)',
      }}>
        <div style={{ fontFamily: 'var(--cth-font-display)', fontSize: 15, fontWeight: 700, color: 'var(--cth-ink-900)', marginBottom: note ? 4 : 12 }}>{title}</div>
        {note && <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-500)', marginBottom: 12 }}>{note}</div>}
        <div style={{ overflowY: 'auto', minHeight: 0 }}>
          {!loaded ? (
            <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 12, color: 'var(--cth-ink-300)', padding: 8 }}>불러오는 중…</div>
          ) : groups.map((g) => (
            <div key={g.key}>
              <div style={{
                fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: 700,
                color: 'var(--cth-ink-500)', margin: '10px 2px 6px',
              }}>{g.label}</div>
              <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 8 }}>
                {g.pool.map((c) => (
                  <button key={`${g.key}:${c.name}`} onClick={() => onPick(c.name)} className="cth-titlebar-nodrag" style={{
                    display: 'flex', alignItems: 'center', gap: 8, padding: '8px 10px', borderRadius: 10,
                    border: '1px solid var(--cth-cream-200)', background: '#fff', cursor: 'pointer', textAlign: 'left',
                  }}>
                    <div style={{ width: 32, height: 32, borderRadius: 8, overflow: 'hidden', flexShrink: 0, background: 'var(--cth-cream-100)', display: 'flex', alignItems: 'flex-end', justifyContent: 'center' }}>
                      <Face c={c} theme={g.theme} />
                    </div>
                    <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 13, fontWeight: 700, color: 'var(--cth-ink-900)' }}>{c.name}</span>
                  </button>
                ))}
              </div>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
