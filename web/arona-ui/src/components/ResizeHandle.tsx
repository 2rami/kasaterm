import { useRef } from 'react';

// 드래그 리사이즈 핸들(거노: 각 영역 크기조절) — dir='col'=세로 막대(좌우 폭 조절),
// 'row'=가로 막대(상하 높이 조절). onDrag(delta) 는 직전 프레임 대비 이동량(px) — 호출자가
// width/height state 에 누적. 드래그 중 전역 커서·선택 방지로 매끄럽게.
export function ResizeHandle({ dir, onDrag }: { dir: 'col' | 'row'; onDrag: (delta: number) => void }) {
  const last = useRef(0);
  const onMouseDown = (e: React.MouseEvent) => {
    e.preventDefault();
    last.current = dir === 'col' ? e.clientX : e.clientY;
    const move = (ev: MouseEvent) => {
      const cur = dir === 'col' ? ev.clientX : ev.clientY;
      onDrag(cur - last.current);
      last.current = cur;
    };
    const up = () => {
      window.removeEventListener('mousemove', move);
      window.removeEventListener('mouseup', up);
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
    };
    window.addEventListener('mousemove', move);
    window.addEventListener('mouseup', up);
    document.body.style.cursor = dir === 'col' ? 'col-resize' : 'row-resize';
    document.body.style.userSelect = 'none';
  };
  return (
    <div
      onMouseDown={onMouseDown}
      className={`cth-resize-handle cth-resize-handle-${dir}`}
      title="드래그해서 크기 조절"
      style={{
        flexShrink: 0,
        width: dir === 'col' ? 10 : undefined,
        height: dir === 'row' ? 10 : undefined,
        cursor: dir === 'col' ? 'col-resize' : 'row-resize',
        position: 'relative', zIndex: 5,
        display: 'flex', alignItems: 'center', justifyContent: 'center',
      }}
    />
  );
}
