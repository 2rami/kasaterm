import { useState } from 'react';
import { createPortal } from 'react-dom';
import { FolderBrowser } from './FolderBrowser';
import { roomCd } from '@/lib/mcp';

export interface RoomPathModalProps {
  initialPath: string;
  onClose: () => void;
  onChanged?: (path: string) => void;
}

// 방 경로 변경 — SCHALE(블루아카이브) 테마 디렉터리 브라우저 모달. 하위 폴더로
// 들어가고(상위로 버튼), '이 방으로' 확정 시 active pane 셸을 cd(터미널 백엔드).
export function RoomPathModal({ initialPath, onClose, onChanged }: RoomPathModalProps) {
  const [selPath, setSelPath] = useState(initialPath);
  const [applying, setApplying] = useState(false);

  const apply = async () => {
    setApplying(true);
    const ok = await roomCd(selPath);
    setApplying(false);
    if (ok) { onChanged?.(selPath); onClose(); }
  };

  // document.body 로 portal — 중첩 stacking context 탈출, 항상 최상위.
  return createPortal(
    <div
      onClick={onClose}
      style={{
        position: 'fixed', inset: 0, zIndex: 1000,
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

        {/* 디렉터리 브라우저(학생 부르기와 공용) */}
        <div style={{ padding: 12 }}>
          <FolderBrowser initialPath={initialPath} onPathChange={setSelPath} height={240} />
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
    </div>,
    document.body,
  );
}
