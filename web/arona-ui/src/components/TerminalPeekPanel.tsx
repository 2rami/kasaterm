import { useEffect, useRef, useState } from 'react';
import { fetchPeek } from '@/lib/mcp';

// 교실 터미널 뷰 — 학생(pane) 클릭 시 그 화면을 교실 안에서 그대로 본다.
// GET /peek?surface=&lines=40 을 1s 폴링(열린 동안만). 픽셀 프레임 + 모노 텍스트.
export interface TerminalPeekPanelProps {
  surfaceId: string;
  title: string;
  onClose: () => void;
}

const LINES = 40;

export function TerminalPeekPanel({ surfaceId, title, onClose }: TerminalPeekPanelProps) {
  const [text, setText] = useState('');
  const bodyRef = useRef<HTMLPreElement>(null);

  useEffect(() => {
    let stopped = false;
    setText('');
    const tick = async () => {
      if (stopped) return;
      const t = await fetchPeek(surfaceId, LINES);
      if (!stopped) setText(t);
    };
    void tick();
    const iv = setInterval(tick, 1000);
    return () => { stopped = true; clearInterval(iv); };
  }, [surfaceId]);

  // 새 텍스트가 들어오면 항상 맨 아래(최신 행)로 — 터미널처럼 따라간다.
  useEffect(() => {
    const el = bodyRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [text]);

  return (
    <div
      style={{
        // 교실(좌측) 옆 우측 공간을 꽉 채워 80~120컬럼이 잘리지 않게 — 진짜
        // 터미널처럼 넓게. 좁은 화면은 left 가 줄며 가로 스크롤로 흡수.
        position: 'fixed', top: 80, left: 500, right: 16, bottom: 16,
        display: 'flex', flexDirection: 'column',
        background: '#16121c',
        boxShadow: '0 0 0 2px var(--cth-ink-900), 0 0 0 4px var(--cth-cream-200)',
        zIndex: 50
      }}
    >
      {/* 헤더 — 캐릭터명 + surface + 닫기 */}
      <div
        style={{
          display: 'flex', alignItems: 'center', justifyContent: 'space-between',
          padding: '6px 10px', background: 'var(--cth-cream-200)',
          boxShadow: 'inset 0 -2px 0 var(--cth-ink-900)'
        }}
      >
        <span style={{ fontFamily: 'var(--cth-font-display)', fontSize: 'var(--cth-text-display-sm)', color: 'var(--cth-ink-900)' }}>
          {title} <span style={{ color: 'var(--cth-ink-500)' }}>{surfaceId}</span>
        </span>
        <button
          onClick={onClose}
          title="닫기"
          style={{
            fontFamily: 'var(--cth-font-display)', fontSize: 'var(--cth-text-display-sm)',
            border: 'none', cursor: 'pointer', padding: '2px 8px',
            background: 'var(--cth-coral)', color: 'var(--cth-cream-50)',
            boxShadow: 'inset 0 0 0 1px var(--cth-ink-900)'
          }}
        >
          ✕
        </button>
      </div>
      {/* 본문 — pane 화면 모노 텍스트. 픽셀폰트 대신 코딩 모노로 80컬럼 정렬을
          살리고, 줄간격을 터미널처럼 타이트하게. */}
      <pre
        ref={bodyRef}
        style={{
          flex: 1, margin: 0, padding: '10px 14px', overflow: 'auto',
          fontFamily: '"JetBrains Mono", "D2Coding", Menlo, ui-monospace, monospace',
          fontSize: 13, lineHeight: 1.3, letterSpacing: 0,
          color: '#e6e0ee', whiteSpace: 'pre', tabSize: 4,
          textRendering: 'optimizeLegibility'
        }}
      >
        {text || '화면을 불러오는 중…'}
      </pre>
    </div>
  );
}
