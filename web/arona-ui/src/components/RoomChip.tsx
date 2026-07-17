// '이 방' 경로 칩 — 어느 폴더(cwd)를 방으로 다루는지 사용자에게 보여준다.
// 보기메뉴 아로나 모드가 활성 pane cwd 를 따라가 엉뚱한 방을 잡던
// 버그(거노 실측)의 UX 보완: 화면에 방 경로를 표시해 오선택을 알아채게.
export interface RoomChipProps {
  cwd: string | null;
  /** 클릭 시 방 경로 변경 모달 열기. */
  onClick?: () => void;
}

function shortCwd(cwd: string): string {
  const segs = cwd.replace(/\/+$/, '').split('/').filter(Boolean);
  if (segs.length <= 2) return cwd;
  return '…/' + segs.slice(-2).join('/');
}

export function RoomChip({ cwd, onClick }: RoomChipProps) {
  if (!cwd) return null;
  return (
    <button
      title={`${cwd} — 클릭하면 방 경로 변경`}
      onClick={onClick}
      style={{
        fontFamily: 'var(--cth-font-ui)',
        fontSize: 'var(--cth-text-body-sm)',
        color: 'var(--cth-ink-700)',
        background: 'var(--cth-cream-200)',
        padding: '3px 8px', border: 'none', borderRadius: 6, cursor: 'pointer',
        boxShadow: 'inset 0 0 0 1px var(--cth-ink-300)',
        display: 'inline-flex', alignItems: 'center', gap: 5,
        whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis', maxWidth: 260
      }}
    >
      이 방: {shortCwd(cwd)}
      <span style={{ color: 'var(--cth-ink-300)', fontSize: 10 }}>▾</span>
    </button>
  );
}
