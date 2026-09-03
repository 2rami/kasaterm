import { useEffect, useMemo, useRef, useState } from 'react';
import { fetchBlocks, fetchPeek, sendToPane, sendToInbox, listDir, openGitPanel, type PaneBlock, type PaneRect } from '../lib/mcp';
import { AnsiText } from './AnsiText';

// 공백·특수문자 든 경로를 cd 인자로 안전하게 — 셸 single-quote 이스케이프.
function shQuote(p: string): string {
  return /[^A-Za-z0-9_/.~-]/.test(p) ? `'${p.replace(/'/g, `'\\''`)}'` : p;
}

// claude 가 아닌 plain 터미널 pane 타일을 Warp 처럼 "명령 블록 스택"으로 렌더한다.
// 블록은 백엔드가 OSC 133 C/D 셸 마크로 끊어 /blocks 로 제공(command·output·exit·
// duration). 색은 SCHALE clean-blue 토큰(라이트/다크 자동). vim 등 alt-screen TUI 는
// 블록 모델이 깨지므로 라이브 peek 화면으로 폴백한다.

function homeRelative(p?: string): string {
  if (!p) return '';
  const m = p.match(/^\/(?:Users|home)\/[^/]+(\/.*)?$/);
  return m ? '~' + (m[1] ?? '') : p;
}

// ms → "12.3s" / "1m 23s" / "1h 5m"
function formatDuration(ms: number): string {
  if (ms < 0) return '0s';
  const s = Math.floor(ms / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  if (h === 0 && m === 0) return `${(ms / 1000).toFixed(1)}s`;
  if (h === 0) return sec > 0 ? `${m}m ${sec}s` : `${m}m`;
  return m > 0 ? `${h}h ${m}m` : `${h}h`;
}

// epoch ms → "just now" / "3m ago" / "2h ago" / "4d ago"
function relativeTime(ms: number, now: number): string {
  const diff = now - ms;
  if (diff < 60_000) return 'just now';
  const min = Math.floor(diff / 60_000);
  if (min < 60) return `${min}m ago`;
  const hr = Math.floor(min / 60);
  if (hr < 24) return `${hr}h ago`;
  return `${Math.floor(hr / 24)}d ago`;
}

const ICON = { width: 13, height: 13, viewBox: '0 0 16 16', fill: 'none' as const, stroke: 'currentColor', strokeWidth: 1.4, strokeLinecap: 'round' as const, strokeLinejoin: 'round' as const };
function FolderIcon() { return <svg {...ICON}><path d="M2 4.2a1 1 0 0 1 1-1h3l1.4 1.5H13a1 1 0 0 1 1 1V12a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1z" /></svg>; }
function BranchIcon() { return <svg {...ICON}><circle cx="4.5" cy="3.5" r="1.6" /><circle cx="4.5" cy="12.5" r="1.6" /><circle cx="11.5" cy="5" r="1.6" /><path d="M4.5 5.1v5.8M11.5 6.6c0 3-3.5 1.9-3.5 4.3" /></svg>; }
function FileIcon() { return <svg {...ICON}><path d="M4 2.2h5l3 3V13a.8.8 0 0 1-.8.8H4.8A.8.8 0 0 1 4 13z" /><path d="M9 2.2V5.2h3" /></svg>; }
function CopyIcon() { return <svg {...ICON}><rect x="5" y="5" width="8" height="9" rx="1" /><path d="M11 5V3.5a1 1 0 0 0-1-1H3.5a1 1 0 0 0-1 1V11a1 1 0 0 0 1 1H5" /></svg>; }
function AttachIcon() { return <svg {...ICON}><path d="M8 11.5V3.5M8 3.5 5 6.5M8 3.5l3 3M3.5 12.5h9" /></svg>; }
function UpIcon() { return <svg {...ICON}><path d="M8 13V4M8 4 4.5 7.5M8 4l3.5 3.5" /></svg>; }

export interface TerminalBlockCardProps {
  surfaceId: string;
  rect: PaneRect;
  onClose?: () => void;
  onToggleZoom?: () => void;
  zoomed?: boolean;
  /** 활성 claude pane(있으면) — 블록 출력을 "에이전트에 첨부"할 대상. 없으면 클립보드. */
  activeAgentId?: string;
}

export function TerminalBlockCard({ surfaceId, rect, onClose, onToggleZoom, zoomed, activeAgentId }: TerminalBlockCardProps) {
  const [blocks, setBlocks] = useState<PaneBlock[]>([]);
  const [peek, setPeek] = useState('');
  const [showHistory, setShowHistory] = useState(false);
  const [histSel, setHistSel] = useState(0);
  const [input, setInput] = useState('');
  const [now, setNow] = useState(() => Date.now());
  const [hoverId, setHoverId] = useState<number | null>(null);
  const [copiedId, setCopiedId] = useState<number | null>(null);
  // 디렉토리 자동완성 팝업(Tab/cwd칩) — browsePath 로컬 추적해 cd 후 즉시 드릴다운.
  const [dirOpen, setDirOpen] = useState(false);
  const [browsePath, setBrowsePath] = useState('');
  const [dirList, setDirList] = useState<{ dirs: string[]; parent: string | null }>({ dirs: [], parent: null });
  const [dirFilter, setDirFilter] = useState('');
  const [dirSel, setDirSel] = useState(0);
  // 명령 history 커서: -1=현재 입력, 0..=최신부터의 인덱스.
  const [histIdx, setHistIdx] = useState(-1);
  const bodyRef = useRef<HTMLDivElement>(null);
  const blockRefs = useRef<Record<number, HTMLDivElement | null>>({});
  const inputRef = useRef<HTMLTextAreaElement>(null);

  // 블록 ~1s 폴링
  useEffect(() => {
    let stop = false;
    const lim = zoomed ? 80 : 40;
    const tick = async () => {
      const b = await fetchBlocks(surfaceId, lim);
      if (!stop) setBlocks(b);
    };
    void tick();
    const iv = setInterval(tick, 1000);
    return () => { stop = true; clearInterval(iv); };
  }, [surfaceId, zoomed]);

  // 상대시간 표시 갱신용 느린 틱
  useEffect(() => {
    const iv = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(iv);
  }, []);

  const last = blocks[blocks.length - 1];
  const tuiMode = !!last?.is_tui && last.exit_code == null;

  // vim/htop 등 alt-screen TUI 실행 중이면 블록 대신 라이브 화면 폴링
  useEffect(() => {
    if (!tuiMode) { setPeek(''); return; }
    let stop = false;
    const tick = async () => {
      const s = await fetchPeek(surfaceId, zoomed ? 200 : 80, true);
      if (!stop) setPeek(s);
    };
    void tick();
    const iv = setInterval(tick, 1000);
    return () => { stop = true; clearInterval(iv); };
  }, [surfaceId, tuiMode, zoomed]);

  // 새 블록/출력 시 바닥(HISTORY 보는 중엔 유지)
  useEffect(() => {
    const el = bodyRef.current;
    if (el && !showHistory && !tuiMode) el.scrollTop = el.scrollHeight;
  }, [blocks, showHistory, tuiMode]);

  const cwd = homeRelative(rect.cwd);
  const files = rect.files ?? 0, ins = rect.insertions ?? 0, del = rect.deletions ?? 0;
  const hasDiff = files > 0 || ins > 0 || del > 0;

  // 명령 history — blocks 의 command 를 최신순·중복제거(최신=[0]).
  const history = useMemo(() => {
    const seen = new Set<string>();
    const out: string[] = [];
    for (let i = blocks.length - 1; i >= 0; i--) {
      const c = blocks[i].command?.trim();
      if (c && !seen.has(c)) { seen.add(c); out.push(c); }
    }
    return out;
  }, [blocks]);

  const autoGrow = (el: HTMLTextAreaElement | null) => {
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = `${Math.min(el.scrollHeight, 132)}px`;
  };

  const submitInput = async () => {
    const t = input.trim();
    if (!t) return;
    setInput('');
    setHistIdx(-1);
    requestAnimationFrame(() => autoGrow(inputRef.current));
    await sendToPane(surfaceId, t);
  };

  // ── 디렉토리 자동완성 ──────────────────────────────────────────────
  const loadDir = async (path: string) => {
    const d = await listDir(path);
    setBrowsePath(d.path || path);
    setDirList({ dirs: d.dirs, parent: d.parent });
    setDirFilter('');
    setDirSel(0);
  };
  const openDirPopup = () => { setDirOpen(true); void loadDir(rect.cwd || ''); };
  const cdInto = async (abs: string) => {
    await sendToPane(surfaceId, `cd ${shQuote(abs)}`);
    await loadDir(abs); // 팝업 유지 → 연속 드릴다운
  };
  const dirFiltered = dirFilter
    ? dirList.dirs.filter((d) => d.toLowerCase().includes(dirFilter.toLowerCase()))
    : dirList.dirs;
  // 항목: [상위(있으면), ...필터된 디렉토리]
  const dirItems: Array<{ parent: true } | { parent: false; name: string }> = [
    ...(dirList.parent ? [{ parent: true as const }] : []),
    ...dirFiltered.map((name) => ({ parent: false as const, name })),
  ];
  const activateDir = (i: number) => {
    const it = dirItems[i];
    if (!it) return;
    if (it.parent) void cdInto(dirList.parent ?? browsePath);
    else void cdInto(browsePath.replace(/\/$/, '') + '/' + it.name);
  };

  const copyOutput = (b: PaneBlock) => {
    void navigator.clipboard?.writeText(`$ ${b.command}\n${b.output}`);
    setCopiedId(b.id);
    setTimeout(() => setCopiedId((c) => (c === b.id ? null : c)), 1200);
  };

  const attachToAgent = (b: PaneBlock) => {
    const text = `\`\`\`\n$ ${b.command}\n${b.output}\n\`\`\``;
    if (activeAgentId) void sendToInbox(activeAgentId, text);
    else void navigator.clipboard?.writeText(text);
  };

  const scrollToBlock = (id: number) => {
    setShowHistory(false);
    requestAnimationFrame(() => blockRefs.current[id]?.scrollIntoView({ block: 'center' }));
  };

  const btn: React.CSSProperties = {
    width: 22, height: 22, borderRadius: 6, border: '1px solid var(--cth-cream-200)', cursor: 'pointer',
    background: 'var(--cth-cream-100)', color: 'var(--cth-ink-500)', display: 'inline-flex',
    alignItems: 'center', justifyContent: 'center', flexShrink: 0, padding: 0,
  };
  const chip: React.CSSProperties = {
    display: 'inline-flex', alignItems: 'center', gap: 5, flexShrink: 0,
    fontFamily: 'var(--cth-font-mono)', fontSize: 11.5,
  };

  return (
    // 흰 카드 없이 하늘색 배경 위에 직접 — 멀티뷰 배경이 그대로 비친다.
    <div style={{ width: '100%', height: '100%', display: 'flex', flexDirection: 'column', background: 'transparent', overflow: 'hidden' }}>
      {/* 상단바 — cwd · branch · diff + HISTORY/zoom/close */}
      <div style={{ flexShrink: 0, display: 'flex', alignItems: 'center', gap: 12, padding: '6px 9px', background: 'transparent', borderBottom: '1px solid var(--cth-ink-100)', minWidth: 0 }}>
        <span title={rect.cwd || surfaceId} style={{ ...chip, color: 'var(--cth-ink-700)', minWidth: 0, flexShrink: 1, overflow: 'hidden' }}>
          <span style={{ color: 'var(--cth-sky)', flexShrink: 0, display: 'inline-flex' }}><FolderIcon /></span>
          <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{cwd || surfaceId}</span>
        </span>
        {rect.branch && (
          <span style={{ ...chip, color: 'var(--cth-ink-500)' }}>
            <span style={{ color: 'var(--cth-sky)', display: 'inline-flex' }}><BranchIcon /></span>
            <span style={{ fontWeight: 600 }}>{rect.branch}</span>
          </span>
        )}
        {hasDiff && (
          <span style={{ ...chip, gap: 6, color: 'var(--cth-ink-500)' }}>
            <span style={{ color: 'var(--cth-ink-300)', display: 'inline-flex' }}><FileIcon /></span>
            <span>{files}</span>
            {ins > 0 && <span style={{ color: 'var(--cth-mint)' }}>+{ins}</span>}
            {del > 0 && <span style={{ color: 'var(--cth-coral-text)' }}>-{del}</span>}
          </span>
        )}
        <div style={{ flex: 1 }} />
        <button onClick={() => { setShowHistory((v) => !v); setHistSel(Math.max(0, blocks.length - 1)); }}
          title="HISTORY" style={{ ...btn, width: 'auto', padding: '0 8px', fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700, letterSpacing: 0.4, color: showHistory ? 'var(--cth-sky)' : 'var(--cth-ink-500)', borderColor: showHistory ? 'var(--cth-sky)' : 'var(--cth-cream-200)' }}>
          HISTORY
        </button>
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

      {/* 본문 — TUI 모드면 라이브 화면, 아니면 블록 스택 / HISTORY */}
      {showHistory ? (
        <HistoryPanel blocks={blocks} now={now} sel={histSel} onSel={setHistSel} onPick={scrollToBlock} />
      ) : tuiMode ? (
        <div style={{ flex: 1, overflow: 'auto', padding: '8px 10px', background: 'transparent' }}>
          <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-300)', marginBottom: 6 }}>
            ⛶ 전체화면 앱 실행 중 — 라이브 화면
          </div>
          <pre style={{ margin: 0, fontFamily: 'var(--cth-font-mono)', fontSize: 11, lineHeight: 1.4, whiteSpace: 'pre', color: 'var(--cth-ink-700)' }}><AnsiText text={peek} /></pre>
        </div>
      ) : (
        <div ref={bodyRef} style={{ flex: 1, overflow: 'auto', padding: '6px 0', background: 'transparent' }}>
          {blocks.length === 0 ? (
            <div style={{ color: 'var(--cth-ink-300)', fontFamily: 'var(--cth-font-mono)', fontSize: 11, padding: '8px 12px' }}>
              명령을 실행하면 블록이 쌓여요.
            </div>
          ) : blocks.map((b) => {
            const running = b.exit_code == null && b.duration_ms == null;
            const ok = b.exit_code === 0;
            const accent = running ? 'var(--cth-status-working)' : ok ? 'var(--cth-status-success)' : 'var(--cth-status-blocked)';
            const hovered = hoverId === b.id;
            return (
              <div key={b.id} ref={(el) => { blockRefs.current[b.id] = el; }}
                onMouseEnter={() => setHoverId(b.id)} onMouseLeave={() => setHoverId((h) => (h === b.id ? null : h))}
                style={{ borderLeft: `3px solid ${accent}`, margin: '2px 8px', padding: '5px 9px', borderRadius: '0 6px 6px 0', background: hovered ? 'var(--cth-cream-100)' : 'transparent' }}>
                {/* 명령 줄 + 상태/소요시간 + 호버 액션 */}
                <div style={{ display: 'flex', alignItems: 'center', gap: 8, minWidth: 0 }}>
                  <span style={{ fontFamily: 'var(--cth-font-mono)', fontSize: 12, fontWeight: 700, color: 'var(--cth-ink-700)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', flexShrink: 1, minWidth: 0 }}>
                    {b.command || '(명령)'}
                  </span>
                  <div style={{ flex: 1 }} />
                  {running ? (
                    <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 10.5, fontWeight: 600, color: 'var(--cth-status-working)', display: 'inline-flex', alignItems: 'center', gap: 4, flexShrink: 0 }}>
                      <span style={{ width: 6, height: 6, borderRadius: '50%', background: 'var(--cth-status-working)' }} />실행 중
                    </span>
                  ) : (
                    <>
                      {!ok && b.exit_code != null && (
                        <span style={{ fontFamily: 'var(--cth-font-mono)', fontSize: 10.5, fontWeight: 600, color: 'var(--cth-coral-text)', flexShrink: 0 }}>exit {b.exit_code}</span>
                      )}
                      {b.duration_ms != null && (
                        <span style={{ fontFamily: 'var(--cth-font-mono)', fontSize: 10.5, color: 'var(--cth-ink-300)', flexShrink: 0 }}>{formatDuration(b.duration_ms)}</span>
                      )}
                    </>
                  )}
                  {hovered && (
                    <span style={{ display: 'inline-flex', gap: 4, flexShrink: 0 }}>
                      <button onClick={() => copyOutput(b)} title="출력 복사" style={{ ...btn, width: 20, height: 20, color: copiedId === b.id ? 'var(--cth-mint)' : 'var(--cth-ink-500)' }}><CopyIcon /></button>
                      <button onClick={() => attachToAgent(b)} title={activeAgentId ? '에이전트에 첨부' : '출력 복사(에이전트 없음)'} style={{ ...btn, width: 20, height: 20 }}><AttachIcon /></button>
                    </span>
                  )}
                </div>
                {/* 출력 */}
                {b.is_tui ? (
                  <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 10.5, color: 'var(--cth-ink-300)', marginTop: 3 }}>⛶ 전체화면 앱(vim/htop 등) — 라이브 화면은 포커스해서 확인</div>
                ) : b.output.trim() ? (
                  <pre style={{ margin: '3px 0 0', fontFamily: 'var(--cth-font-mono)', fontSize: 11, lineHeight: 1.4, whiteSpace: 'pre-wrap', wordBreak: 'break-all', color: 'var(--cth-ink-700)', maxHeight: zoomed ? undefined : 220, overflow: 'auto' }}><AnsiText text={b.output} /></pre>
                ) : null}
              </div>
            );
          })}
        </div>
      )}

      {/* 하단 입력 영역 — Warp 식: 클릭 가능한 칩 줄 + 박스 없는 넓은 입력 + 힌트 */}
      <div style={{ flexShrink: 0, position: 'relative', borderTop: '1px solid var(--cth-ink-100)' }}>
        {dirOpen && (
          <DirPopup browsePath={browsePath} items={dirItems} filter={dirFilter} setFilter={setDirFilter}
            sel={dirSel} setSel={setDirSel} onActivate={activateDir} onClose={() => setDirOpen(false)} />
        )}
        {/* 칩 줄 — cwd→디렉토리 이동, branch/diff→git 패널 */}
        <div style={{ display: 'flex', alignItems: 'center', gap: 10, padding: '7px 11px 3px', minWidth: 0 }}>
          <button onClick={openDirPopup} title="디렉토리 이동 (Tab)"
            style={{ ...chip, background: 'transparent', border: 'none', cursor: 'pointer', padding: '2px 0', color: 'var(--cth-ink-500)', maxWidth: '46%', overflow: 'hidden' }}>
            <span style={{ color: 'var(--cth-sky)', flexShrink: 0, display: 'inline-flex' }}><FolderIcon /></span>
            <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{cwd || surfaceId}</span>
          </button>
          {rect.branch && (
            <button onClick={() => void openGitPanel()} title="git 패널 열기"
              style={{ ...chip, background: 'transparent', border: 'none', cursor: 'pointer', padding: '2px 0', color: 'var(--cth-ink-500)' }}>
              <span style={{ color: 'var(--cth-sky)', display: 'inline-flex' }}><BranchIcon /></span>
              <span style={{ fontWeight: 600 }}>{rect.branch}</span>
            </button>
          )}
          {hasDiff && (
            <button onClick={() => void openGitPanel()} title="git 패널 열기"
              style={{ ...chip, gap: 6, background: 'transparent', border: 'none', cursor: 'pointer', padding: '2px 0', color: 'var(--cth-ink-500)' }}>
              <span style={{ color: 'var(--cth-ink-300)', display: 'inline-flex' }}><FileIcon /></span>
              <span>{files}</span>
              {ins > 0 && <span style={{ color: 'var(--cth-mint)' }}>+{ins}</span>}
              {del > 0 && <span style={{ color: 'var(--cth-coral-text)' }}>-{del}</span>}
            </button>
          )}
        </div>
        {/* 입력 줄 — 박스/테두리 없이 배경 위에 직접, 커서만(caret sky). 멀티라인 자동 높이. */}
        <div style={{ display: 'flex', alignItems: 'flex-start', gap: 8, padding: '0 11px 2px' }}>
          <span style={{ color: 'var(--cth-sky)', fontFamily: 'var(--cth-font-mono)', fontSize: 13, fontWeight: 700, lineHeight: 1.65, flexShrink: 0, userSelect: 'none' }}>❯</span>
          <textarea ref={inputRef} value={input} rows={1}
            onChange={(e) => { setInput(e.target.value); setHistIdx(-1); autoGrow(e.target); }}
            onKeyDown={(e) => {
              if (e.key === 'Tab') { e.preventDefault(); openDirPopup(); return; }
              if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); void submitInput(); return; }
              const el = e.currentTarget;
              if (e.key === 'ArrowUp' && el.selectionStart === 0 && el.selectionEnd === 0) {
                if (histIdx < history.length - 1) {
                  e.preventDefault();
                  const ni = histIdx + 1; setHistIdx(ni); setInput(history[ni] ?? '');
                  requestAnimationFrame(() => autoGrow(inputRef.current));
                }
              } else if (e.key === 'ArrowDown' && el.selectionStart === el.value.length) {
                if (histIdx > 0) {
                  e.preventDefault();
                  const ni = histIdx - 1; setHistIdx(ni); setInput(history[ni] ?? '');
                  requestAnimationFrame(() => autoGrow(inputRef.current));
                } else if (histIdx === 0) {
                  e.preventDefault();
                  setHistIdx(-1); setInput('');
                  requestAnimationFrame(() => autoGrow(inputRef.current));
                }
              }
            }}
            placeholder="명령을 입력하세요  ·  예: cargo build, git status"
            style={{ flex: 1, minWidth: 0, resize: 'none', background: 'transparent', border: 'none', outline: 'none', fontFamily: 'var(--cth-font-mono)', fontSize: 13, lineHeight: 1.65, color: 'var(--cth-ink-700)', caretColor: 'var(--cth-sky)', padding: 0, maxHeight: 132, overflow: 'auto' }} />
        </div>
        {/* 힌트 줄 */}
        <div style={{ padding: '0 11px 7px 30px', fontFamily: 'var(--cth-font-ui)', fontSize: 10, color: 'var(--cth-ink-300)', display: 'flex', gap: 13, flexWrap: 'wrap' }}>
          <span><b style={{ fontFamily: 'var(--cth-font-mono)', color: 'var(--cth-ink-500)' }}>⏎</b> 실행</span>
          <span><b style={{ fontFamily: 'var(--cth-font-mono)', color: 'var(--cth-ink-500)' }}>⇧⏎</b> 줄바꿈</span>
          <span><b style={{ fontFamily: 'var(--cth-font-mono)', color: 'var(--cth-ink-500)' }}>⇥</b> 디렉토리</span>
          <span><b style={{ fontFamily: 'var(--cth-font-mono)', color: 'var(--cth-ink-500)' }}>↑↓</b> 기록</span>
        </div>
      </div>
    </div>
  );
}

// HISTORY 패널 — 블록 command + 상대시간 리스트. ↑↓ 네비, Enter/클릭 시 해당 블록으로.
function HistoryPanel({ blocks, now, sel, onSel, onPick }: {
  blocks: PaneBlock[]; now: number; sel: number; onSel: (i: number) => void; onPick: (id: number) => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => { ref.current?.focus(); }, []);
  return (
    <div ref={ref} tabIndex={0}
      onKeyDown={(e) => {
        if (e.key === 'ArrowDown') { e.preventDefault(); onSel(Math.min(blocks.length - 1, sel + 1)); }
        else if (e.key === 'ArrowUp') { e.preventDefault(); onSel(Math.max(0, sel - 1)); }
        else if (e.key === 'Enter' && blocks[sel]) { e.preventDefault(); onPick(blocks[sel].id); }
      }}
      style={{ flex: 1, overflow: 'auto', padding: '4px 0', background: 'transparent', outline: 'none' }}>
      {blocks.length === 0 ? (
        <div style={{ color: 'var(--cth-ink-300)', fontFamily: 'var(--cth-font-mono)', fontSize: 11, padding: '8px 12px' }}>기록이 없어요.</div>
      ) : [...blocks].reverse().map((b) => {
        const idx = blocks.indexOf(b);
        const selected = idx === sel;
        const ok = b.exit_code === 0;
        const dot = b.exit_code == null && b.duration_ms == null ? 'var(--cth-status-working)' : ok ? 'var(--cth-status-success)' : 'var(--cth-status-blocked)';
        return (
          <div key={b.id} onClick={() => { onSel(idx); onPick(b.id); }} onMouseEnter={() => onSel(idx)}
            style={{ display: 'flex', alignItems: 'center', gap: 8, padding: '5px 12px', cursor: 'pointer', background: selected ? 'var(--cth-cream-100)' : 'transparent' }}>
            <span style={{ width: 6, height: 6, borderRadius: '50%', background: dot, flexShrink: 0 }} />
            <span style={{ fontFamily: 'var(--cth-font-mono)', fontSize: 11.5, color: 'var(--cth-ink-700)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', flex: 1 }}>{b.command || '(명령)'}</span>
            <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 10, color: 'var(--cth-ink-300)', flexShrink: 0 }}>{relativeTime(b.started_ms, now)}</span>
          </div>
        );
      })}
    </div>
  );
}

// 디렉토리 자동완성 팝업 — Tab/cwd칩으로 열림. 검색 필터 + ↑↓ 네비 + Enter/클릭 = cd.
// 입력 위(bottom:100%)에 floating, 바깥 클릭 시 닫힘. 선택 강조는 SCHALE sky.
function DirPopup({ browsePath, items, filter, setFilter, sel, setSel, onActivate, onClose }: {
  browsePath: string;
  items: Array<{ parent: true } | { parent: false; name: string }>;
  filter: string; setFilter: (s: string) => void;
  sel: number; setSel: (i: number) => void;
  onActivate: (i: number) => void; onClose: () => void;
}) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => { ref.current?.focus(); }, []);
  const home = homeRelative(browsePath);
  return (
    <>
      <div onClick={onClose} style={{ position: 'fixed', inset: 0, zIndex: 40 }} />
      <div style={{
        position: 'absolute', left: 8, right: 8, bottom: '100%', marginBottom: 6, zIndex: 41,
        background: 'var(--cth-cream-50)', border: '1px solid var(--cth-ink-100)', borderRadius: 10,
        boxShadow: '0 8px 28px rgba(20,40,80,0.18)', overflow: 'hidden', maxHeight: 304, display: 'flex', flexDirection: 'column',
      }}>
        <input ref={ref} value={filter}
          onChange={(e) => { setFilter(e.target.value); setSel(0); }}
          onKeyDown={(e) => {
            if (e.key === 'ArrowDown') { e.preventDefault(); setSel(Math.min(items.length - 1, sel + 1)); }
            else if (e.key === 'ArrowUp') { e.preventDefault(); setSel(Math.max(0, sel - 1)); }
            else if (e.key === 'Enter') { e.preventDefault(); onActivate(sel); }
            else if (e.key === 'Escape' || e.key === 'Tab') { e.preventDefault(); onClose(); }
          }}
          placeholder="디렉토리 검색…"
          style={{ flexShrink: 0, background: 'transparent', border: 'none', borderBottom: '1px solid var(--cth-ink-100)', outline: 'none', padding: '9px 12px', fontFamily: 'var(--cth-font-mono)', fontSize: 12.5, color: 'var(--cth-ink-700)', caretColor: 'var(--cth-sky)' }} />
        <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 10, color: 'var(--cth-ink-300)', padding: '5px 12px 2px', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', flexShrink: 0 }}>{home}</div>
        <div style={{ overflow: 'auto', padding: '2px 0 6px' }}>
          {items.length === 0 ? (
            <div style={{ padding: '8px 12px', fontFamily: 'var(--cth-font-mono)', fontSize: 11.5, color: 'var(--cth-ink-300)' }}>하위 디렉토리가 없어요.</div>
          ) : items.map((it, i) => {
            const on = i === sel;
            return (
              <div key={it.parent ? '..' : it.name} onMouseEnter={() => setSel(i)} onClick={() => onActivate(i)}
                style={{ display: 'flex', alignItems: 'center', gap: 9, padding: '7px 12px', cursor: 'pointer', background: on ? 'var(--cth-sky)' : 'transparent', color: on ? '#fff' : 'var(--cth-ink-700)' }}>
                <span style={{ display: 'inline-flex', color: on ? '#fff' : 'var(--cth-sky)', flexShrink: 0 }}>
                  {it.parent ? <UpIcon /> : <FolderIcon />}
                </span>
                <span style={{ fontFamily: 'var(--cth-font-mono)', fontSize: 12.5, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                  {it.parent ? '.. (상위 폴더)' : it.name}
                </span>
              </div>
            );
          })}
        </div>
      </div>
    </>
  );
}
