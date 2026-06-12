import { useEffect, useState } from 'react';
import { listDir, type DirListing } from '@/lib/mcp';

export interface FolderBrowserProps {
  /** 시작 경로(없으면 현재 폴더). */
  initialPath?: string | null;
  /** 탐색으로 현재 경로가 바뀔 때마다(상위로/폴더 진입) 호출 — 부모가 선택값으로 쓴다. */
  onPathChange?: (path: string) => void;
  /** 폴더 리스트 스크롤 높이(px). */
  height?: number;
}

// 디렉터리 브라우저 — 현재 경로 + 상위로 + 하위 폴더 클릭 탐색(방 경로 변경·학생
// 부르기 공용). "선택"은 곧 "현재 탐색 경로" — onPathChange 로 부모에 보고한다.
export function FolderBrowser({ initialPath, onPathChange, height = 200 }: FolderBrowserProps) {
  const [listing, setListing] = useState<DirListing>({ path: initialPath ?? '', parent: null, dirs: [] });
  const [loading, setLoading] = useState(true);

  const load = (path?: string) => {
    setLoading(true);
    void listDir(path).then((d) => { setListing(d); setLoading(false); onPathChange?.(d.path); });
  };
  // initialPath 가 정해지면 1회 로드(이후엔 내부 탐색).
  useEffect(() => { load(initialPath ?? undefined); }, [initialPath]);

  return (
    <div style={{
      display: 'flex', flexDirection: 'column',
      borderRadius: 10, overflow: 'hidden', border: '1px solid var(--cth-cream-200)',
      background: 'var(--cth-cream-50)',
    }}>
      {/* 현재 경로 + 상위로 */}
      <div style={{
        display: 'flex', alignItems: 'center', gap: 8,
        padding: '8px 10px', borderBottom: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-100)',
      }}>
        <button
          type="button"
          onClick={() => listing.parent && load(listing.parent)}
          disabled={!listing.parent}
          style={{
            flexShrink: 0, padding: '4px 9px', borderRadius: 7, border: 'none',
            cursor: listing.parent ? 'pointer' : 'not-allowed',
            background: listing.parent ? 'var(--cth-sky)' : 'var(--cth-cream-300)',
            color: '#fff', fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: 600,
            opacity: listing.parent ? 1 : 0.5,
          }}
        >↑ 상위로</button>
        <span title={listing.path} style={{
          flex: 1, minWidth: 0, fontFamily: 'var(--cth-font-mono)', fontSize: 11,
          color: 'var(--cth-ink-700)', overflow: 'hidden', textOverflow: 'ellipsis',
          whiteSpace: 'nowrap', direction: 'rtl', textAlign: 'left',
        }}>{listing.path || '…'}</span>
      </div>

      {/* 하위 폴더 리스트 */}
      <div style={{ height, overflowY: 'auto', padding: 6 }}>
        {loading ? (
          <div style={{ textAlign: 'center', color: 'var(--cth-ink-300)', fontFamily: 'var(--cth-font-ui)', fontSize: 12, marginTop: 24 }}>불러오는 중…</div>
        ) : listing.dirs.length === 0 ? (
          <div style={{ textAlign: 'center', color: 'var(--cth-ink-300)', fontFamily: 'var(--cth-font-ui)', fontSize: 12, marginTop: 24 }}>하위 폴더 없음</div>
        ) : listing.dirs.map((name) => (
          <button
            type="button"
            key={name}
            onClick={() => load(`${listing.path.replace(/\/$/, '')}/${name}`)}
            style={{
              display: 'flex', alignItems: 'center', gap: 9, width: '100%',
              padding: '7px 9px', border: 'none', borderRadius: 8, cursor: 'pointer',
              background: 'transparent', textAlign: 'left',
              fontFamily: 'var(--cth-font-ui)', fontSize: 12, color: 'var(--cth-ink-900)',
            }}
            onMouseEnter={(e) => (e.currentTarget.style.background = 'var(--cth-cream-100)')}
            onMouseLeave={(e) => (e.currentTarget.style.background = 'transparent')}
          >
            <svg width="15" height="15" viewBox="0 0 16 16" style={{ flexShrink: 0 }}>
              <path d="M1.5 4a1 1 0 0 1 1-1h3l1.5 1.5h6a1 1 0 0 1 1 1v6a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1V4Z"
                fill="var(--cth-sky-light, #9DC1E8)" stroke="var(--cth-sky)" strokeWidth="1" strokeLinejoin="round" />
            </svg>
            {name}
          </button>
        ))}
      </div>
    </div>
  );
}
