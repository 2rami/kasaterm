import { useEffect, useRef, useState } from 'react';
import { fetchPeek, type PaneRect } from '../lib/mcp';
import { AnsiText } from './AnsiText';

// claude 가 아닌 plain 터미널 pane 타일. board(/board)에 행이 없어 TerminalPeekPanel 이
// "아직 대화가 없어요"만 띄우던 빈창을, Warp 처럼 상단 상태바(cwd · branch · diff) +
// 라이브 터미널 화면(/peek ANSI 폴링)으로 채운다. cwd/git 은 백엔드 window_layout 이
// PaneRect 에 실어 보낸다(GUI 가 publish_pane_status 로 캐시 미러).

// 절대경로를 홈 상대(~)로. /Users/<name>/… · /home/<name>/… → ~/…. 그 외는 그대로.
function homeRelative(p?: string): string {
  if (!p) return '';
  const m = p.match(/^\/(?:Users|home)\/[^/]+(\/.*)?$/);
  return m ? '~' + (m[1] ?? '') : p;
}

const ICON = { width: 13, height: 13, viewBox: '0 0 16 16', fill: 'none' as const, stroke: 'currentColor', strokeWidth: 1.4, strokeLinecap: 'round' as const, strokeLinejoin: 'round' as const };

function FolderIcon() {
  return <svg {...ICON}><path d="M2 4.2a1 1 0 0 1 1-1h3l1.4 1.5H13a1 1 0 0 1 1 1V12a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1z" /></svg>;
}
function BranchIcon() {
  return <svg {...ICON}><circle cx="4.5" cy="3.5" r="1.6" /><circle cx="4.5" cy="12.5" r="1.6" /><circle cx="11.5" cy="5" r="1.6" /><path d="M4.5 5.1v5.8M11.5 6.6c0 3-3.5 1.9-3.5 4.3" /></svg>;
}
function FileIcon() {
  return <svg {...ICON}><path d="M4 2.2h5l3 3V13a.8.8 0 0 1-.8.8H4.8A.8.8 0 0 1 4 13z" /><path d="M9 2.2V5.2h3" /></svg>;
}

export interface TerminalPaneCardProps {
  surfaceId: string;
  rect: PaneRect;
  onClose?: () => void;
  onToggleZoom?: () => void;
  zoomed?: boolean;
}

export function TerminalPaneCard({ surfaceId, rect, onClose, onToggleZoom, zoomed }: TerminalPaneCardProps) {
  const [screen, setScreen] = useState('');
  const [loaded, setLoaded] = useState(false);
  const bodyRef = useRef<HTMLDivElement>(null);

  // 라이브 화면 — peek_ansi(색 포함) ~1s 폴링. zoom 이면 더 많은 줄.
  useEffect(() => {
    let stop = false;
    const lines = zoomed ? 200 : 80;
    const tick = async () => {
      const s = await fetchPeek(surfaceId, lines, true);
      if (!stop) { setScreen(s); setLoaded(true); }
    };
    void tick();
    const iv = setInterval(tick, 1000);
    return () => { stop = true; clearInterval(iv); };
  }, [surfaceId, zoomed]);

  // 새 출력 시 바닥으로(터미널처럼 최신이 아래).
  useEffect(() => {
    const el = bodyRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [screen]);

  const cwd = homeRelative(rect.cwd);
  const files = rect.files ?? 0;
  const ins = rect.insertions ?? 0;
  const del = rect.deletions ?? 0;
  const hasDiff = files > 0 || ins > 0 || del > 0;

  const btn: React.CSSProperties = {
    width: 22, height: 22, borderRadius: 6, border: '1px solid #2c333d', cursor: 'pointer',
    background: '#262c36', color: '#9aa4b2', display: 'inline-flex', alignItems: 'center',
    justifyContent: 'center', flexShrink: 0, padding: 0,
  };

  return (
    <div style={{ width: '100%', height: '100%', display: 'flex', flexDirection: 'column', background: '#1b1f27', overflow: 'hidden' }}>
      {/* Warp 상태바 — cwd · branch · diff + zoom/close */}
      <div style={{ flexShrink: 0, display: 'flex', alignItems: 'center', gap: 12, padding: '6px 9px', background: '#21262f', borderBottom: '1px solid #2c333d', fontFamily: 'var(--cth-font-mono, monospace)', fontSize: 11.5, minWidth: 0 }}>
        <span title={rect.cwd || surfaceId} style={{ display: 'inline-flex', alignItems: 'center', gap: 5, color: '#dfe4ec', minWidth: 0, flexShrink: 1, overflow: 'hidden' }}>
          <span style={{ color: '#e8c07d', flexShrink: 0, display: 'inline-flex' }}><FolderIcon /></span>
          <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{cwd || surfaceId}</span>
        </span>
        {rect.branch && (
          <span style={{ display: 'inline-flex', alignItems: 'center', gap: 5, color: '#aeb6c2', flexShrink: 0 }}>
            <span style={{ color: '#e8c07d', display: 'inline-flex' }}><BranchIcon /></span>
            <span style={{ fontWeight: 600 }}>{rect.branch}</span>
          </span>
        )}
        {hasDiff && (
          <span style={{ display: 'inline-flex', alignItems: 'center', gap: 6, color: '#aeb6c2', flexShrink: 0 }}>
            <span style={{ color: '#9aa4b2', display: 'inline-flex' }}><FileIcon /></span>
            <span>{files}</span>
            <span style={{ opacity: 0.45 }}>·</span>
            {ins > 0 && <span style={{ color: '#7ec98f' }}>+{ins}</span>}
            {del > 0 && <span style={{ color: '#e06c75' }}>-{del}</span>}
          </span>
        )}
        <div style={{ flex: 1 }} />
        {onToggleZoom && (
          <button onClick={onToggleZoom} title={zoomed ? '전체화면 해제' : '임시 전체화면'} style={btn}>
            <svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round">
              {zoomed ? <path d="M9 3h4v4M13 3l-4 4M7 13H3V9M3 13l4-4" /> : <path d="M3 7V3h4M13 9v4H9M3 3l4 4M13 13l-4-4" />}
            </svg>
          </button>
        )}
        {onClose && (
          <button onClick={onClose} title="포커스" style={{ ...btn, fontFamily: 'var(--cth-font-ui)', fontSize: 14, lineHeight: 1 }}>×</button>
        )}
      </div>
      {/* 라이브 터미널 화면 */}
      <div ref={bodyRef} style={{ flex: 1, overflow: 'auto', padding: '8px 10px', background: '#1b1f27' }}>
        {screen.trim() ? (
          <pre style={{ margin: 0, fontFamily: 'var(--cth-font-mono, monospace)', fontSize: 11, lineHeight: 1.4, whiteSpace: 'pre', color: '#c8cdd6' }}><AnsiText text={screen} /></pre>
        ) : (
          <div style={{ color: '#5a6472', fontFamily: 'var(--cth-font-mono, monospace)', fontSize: 11 }}>
            {loaded ? '(빈 화면)' : '화면을 불러오는 중…'}
          </div>
        )}
      </div>
    </div>
  );
}
