import { useEffect, useState } from 'react';
import { listDir, roomCd, type DirListing } from '@/lib/mcp';

export interface RoomPathModalProps {
  initialPath: string;
  onClose: () => void;
  onChanged?: (path: string) => void;
}

// 방 경로 변경 — SCHALE(블루아카이브) 테마 디렉터리 브라우저 모달. 하위 폴더로
// 들어가고(상위로 버튼), '이 방으로' 확정 시 active pane 셸을 cd(터미널 백엔드).
export function RoomPathModal({ initialPath, onClose, onChanged }: RoomPathModalProps) {
  const [listing, setListing] = useState<DirListing>({ path: initialPath, parent: null, dirs: [] });
  const [loading, setLoading] = useState(true);
  const [applying, setApplying] = useState(false);

  const load = (path?: string) => {
    setLoading(true);
    void listDir(path).then((d) => { setListing(d); setLoading(false); });
  };
  useEffect(() => { load(initialPath); }, [initialPath]);

  const apply = async () => {
    setApplying(true);
    const ok = await roomCd(listing.path);
    setApplying(false);
    if (ok) { onChanged?.(listing.path); onClose(); }
  };

  return (
    <div
      onClick={onClose}
      style={{
        position: 'fixed', inset: 0, zIndex: 50,
        background: 'rgba(21, 41, 74, 0.42)', backdropFilter: 'blur(2px)',
        display: 'flex', alignItems: 'center', justifyContent: 'center',
      }}
    >
      <div
        onClick={(e) => e.stopPropagation()}
        style={{
          width: 460, maxWidth: '92vw', maxHeight: '80vh',
          display: 'flex', flexDirection: 'column',
          background: 'var(--cth-cream-50)', borderRadius: 16,
          boxShadow: '0 16px 48px rgba(21,41,74,0.32)', overflow: 'hidden',
          border: '1px solid var(--cth-cream-200)',
        }}
      >
        {/* 헤더 — SCHALE 블루 밴드 */}
        <div style={{
          background: 'linear-gradient(180deg, #4A90E2, #3A78C2)',
          padding: '14px 18px', color: '#fff',
          display: 'flex', alignItems: 'center', gap: 10,
        }}>
          <span style={{ fontFamily: 'var(--cth-font-display)', fontSize: 16, fontWeight: 800, letterSpacing: 0.3 }}>
            방 경로 변경
          </span>
          <span style={{
            fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700, letterSpacing: 0.5,
            padding: '2px 7px', borderRadius: 5, background: 'rgba(255,255,255,0.22)',
          }}>SCHALE</span>
          <div style={{ flex: 1 }} />
          <button onClick={onClose} title="닫기" style={{
            width: 26, height: 26, borderRadius: 7, border: 'none', cursor: 'pointer',
            background: 'rgba(255,255,255,0.22)', color: '#fff', fontSize: 15, lineHeight: 1,
          }}>×</button>
        </div>

        {/* 현재 경로 + 상위로 */}
        <div style={{
          display: 'flex', alignItems: 'center', gap: 8,
          padding: '10px 14px', borderBottom: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-100)',
        }}>
          <button
            onClick={() => listing.parent && load(listing.parent)}
            disabled={!listing.parent}
            style={{
              flexShrink: 0, padding: '5px 10px', borderRadius: 8, border: 'none',
              cursor: listing.parent ? 'pointer' : 'not-allowed',
              background: listing.parent ? 'var(--cth-sky)' : 'var(--cth-cream-300)',
              color: '#fff', fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 600,
              opacity: listing.parent ? 1 : 0.5,
            }}
          >↑ 상위로</button>
          <span title={listing.path} style={{
            flex: 1, minWidth: 0, fontFamily: 'var(--cth-font-mono)', fontSize: 12,
            color: 'var(--cth-ink-700)', overflow: 'hidden', textOverflow: 'ellipsis',
            whiteSpace: 'nowrap', direction: 'rtl', textAlign: 'left',
          }}>{listing.path}</span>
        </div>

        {/* 하위 폴더 리스트 */}
        <div style={{ flex: 1, overflowY: 'auto', padding: 8, minHeight: 120 }}>
          {loading ? (
            <div style={{ textAlign: 'center', color: 'var(--cth-ink-300)', fontFamily: 'var(--cth-font-ui)', fontSize: 13, marginTop: 30 }}>불러오는 중…</div>
          ) : listing.dirs.length === 0 ? (
            <div style={{ textAlign: 'center', color: 'var(--cth-ink-300)', fontFamily: 'var(--cth-font-ui)', fontSize: 13, marginTop: 30 }}>하위 폴더 없음</div>
          ) : listing.dirs.map((name) => (
            <button
              key={name}
              onClick={() => load(`${listing.path.replace(/\/$/, '')}/${name}`)}
              style={{
                display: 'flex', alignItems: 'center', gap: 9, width: '100%',
                padding: '8px 10px', border: 'none', borderRadius: 9, cursor: 'pointer',
                background: 'transparent', textAlign: 'left',
                fontFamily: 'var(--cth-font-ui)', fontSize: 13, color: 'var(--cth-ink-900)',
              }}
              onMouseEnter={(e) => (e.currentTarget.style.background = 'var(--cth-cream-100)')}
              onMouseLeave={(e) => (e.currentTarget.style.background = 'transparent')}
            >
              <svg width="16" height="16" viewBox="0 0 16 16" style={{ flexShrink: 0 }}>
                <path d="M1.5 4a1 1 0 0 1 1-1h3l1.5 1.5h6a1 1 0 0 1 1 1v6a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1V4Z"
                  fill="var(--cth-sky-light, #9DC1E8)" stroke="var(--cth-sky)" strokeWidth="1" strokeLinejoin="round" />
              </svg>
              {name}
            </button>
          ))}
        </div>

        {/* 확정 */}
        <div style={{
          display: 'flex', alignItems: 'center', gap: 8,
          padding: '10px 14px', borderTop: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-50)',
        }}>
          <div style={{ flex: 1 }} />
          <button onClick={onClose} style={{
            padding: '8px 14px', borderRadius: 9, border: '1px solid var(--cth-cream-200)', cursor: 'pointer',
            background: '#fff', color: 'var(--cth-ink-500)', fontFamily: 'var(--cth-font-ui)', fontSize: 13, fontWeight: 600,
          }}>취소</button>
          <button
            onClick={() => void apply()}
            disabled={applying}
            style={{
              padding: '8px 16px', borderRadius: 9, border: 'none',
              cursor: applying ? 'not-allowed' : 'pointer',
              background: 'linear-gradient(180deg, #6BB0F0, #4A90E2)', color: '#fff',
              boxShadow: 'inset 0 1px 0 rgba(255,255,255,0.5)',
              fontFamily: 'var(--cth-font-ui)', fontSize: 13, fontWeight: 700, opacity: applying ? 0.5 : 1,
            }}
          >{applying ? '이동 중…' : '이 방으로'}</button>
        </div>
      </div>
    </div>
  );
}
