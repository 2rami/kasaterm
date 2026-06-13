import { useEffect, useRef, useState } from 'react';
import { fetchTranscript, fetchPeek, fetchSentImages, imageFileUrl, openFile, sendToPane, closeAgent, type Turn } from '@/lib/mcp';
import { SpritePortrait } from './SpritePortrait';
import { Markdown } from './Markdown';
import { useStore } from '@/store';

// claude TUI 화면(peek)에서 대화를 파싱 — transcript jsonl 이 비어있을 때 fallback
// (일부 claude 는 세션 중 jsonl 을 라이브로 안 써, 우리가 PTY 를 소유하니 화면에서
// 직접 읽는다). ❯=선생님 프롬프트, ⏺=학생 답변. 하단 ─── 아래(입력창·상태바)와
// 툴콜(⏺ Bash(…))·박스·상태줄은 버린다. best-effort — 명확한 턴만 추출.
// 화면 텍스트에서 이미지 경로 추출 — 코덱스/raw SendUserFile 은 진짜 툴 이벤트가
// 아니라 `<parameter name="files">["…png"]` 와 `› [image] ~/…png (…)` 로 화면에
// 텍스트로 찍혀 훅이 못 잡는다(거노 실측). 그래서 화면을 직접 파싱한다. 절대(/)·홈(~)
// 경로 둘 다, basename 으로 dedupe(절대경로 우선).
function parseImagePaths(screen: string): string[] {
  const re = /[~/][^\s"'\[\]()]+\.(?:png|jpe?g|gif|webp|bmp|tiff?)/gi;
  const found = screen.match(re) ?? [];
  const byBase = new Map<string, string>();
  for (const p of found) {
    const base = p.split('/').pop() ?? p;
    const cur = byBase.get(base);
    if (!cur || (p.startsWith('/') && !cur.startsWith('/'))) byBase.set(base, p);
  }
  return [...byBase.values()];
}

function parsePtyConversation(screen: string): Turn[] {
  const lines = screen.split('\n');
  // 하단 입력박스(─── / ❯ <라이브 타이핑> / ───)를 대화에서 제외 — 안 그러면
  // 전송 전 입력 중인 글자가 노란 말풍선으로 떴다(거노 실측). 입력박스는 마지막
  // 두 divider 가 가깝게(≤4줄) 붙은 구간이라, 그 위 divider 부터 잘라낸다.
  const dividers: number[] = [];
  for (let i = 0; i < lines.length; i++) {
    if (/^[─—-]{10,}\s*$/.test(lines[i].trim())) dividers.push(i);
  }
  let end = lines.length;
  if (dividers.length >= 2 && dividers[dividers.length - 1] - dividers[dividers.length - 2] <= 4) {
    end = dividers[dividers.length - 2];
  } else if (dividers.length >= 1) {
    end = dividers[dividers.length - 1];
  }
  const skip = (l: string) =>
    /^[│╰╭├┤┬┴┼╮╯]/.test(l) || /┃/.test(l) ||
    /^(⏵⏵|⎿|⚠|▎|✻|╰|╭)/.test(l) || /^[─—-]{5,}/.test(l) || /\d+\s*tokens?\s*$/.test(l);
  const turns: Turn[] = [];
  let cur: Turn | null = null;
  for (const raw of lines.slice(0, end)) {
    const line = raw.replace(/\s+$/, '');
    if (!line.trim()) continue;
    const u = line.match(/^[❯>]\s+(.+)$/);
    const a = line.match(/^⏺\s+(.+)$/);
    if (u) {
      cur = { role: 'user', text: u[1].trim() };
      turns.push(cur);
    } else if (a) {
      const body = a[1].trim();
      if (/^[A-Z]\w*\(/.test(body)) { cur = null; continue; } // 툴콜 — 대화 아님
      cur = { role: 'assistant', text: body };
      turns.push(cur);
    } else if (cur && /^\s{2,}\S/.test(raw) && !skip(line.trim())) {
      cur.text += '\n' + line.trim(); // 들여쓴 연속줄 이어붙임
    } else if (skip(line.trim())) {
      cur = null;
    }
  }
  return turns;
}

// claude 인터랙티브 선택 메뉴(/model · AskUserQuestion 등)를 화면에서 추출 →
// 채팅창에 선택지 카드로 띄운다(거노: 터미널 메뉴를 UI 로). "❯ 1. label" 패턴,
// 2개 이상이어야 메뉴로 인정. label 우측 정렬 설명(2+ 공백 뒤)은 버린다.
function parsePromptMenu(screen: string): { title: string; options: { idx: number; label: string; cur: boolean }[] } | null {
  const lines = screen.split('\n');
  const opts: { idx: number; label: string; cur: boolean; line: number }[] = [];
  for (let i = 0; i < lines.length; i++) {
    const m = lines[i].match(/^\s*([❯>●]?)\s*(\d+)\.\s+(.+?)\s*$/);
    if (m) {
      opts.push({ idx: parseInt(m[2], 10), label: m[3].replace(/\s{2,}.*$/, '').trim(), cur: /[❯>]/.test(m[1] ?? ''), line: i });
    }
  }
  if (opts.length < 2) return null;
  const first = opts[0].line;
  let title = '';
  for (let i = first - 1; i >= 0 && i >= first - 6; i--) {
    const t = lines[i].trim();
    if (!t || /^[─—\-│╭╰╮╯>❯●]/.test(t)) continue;
    title = t.length > 60 ? t.slice(0, 59) + '…' : t;
    break;
  }
  return { title, options: opts.map((o) => ({ idx: o.idx, label: o.label, cur: o.cur })) };
}

// board.model 은 상태바 파싱 표시명("Opus 4.8 (1M context)") 우선 — claude- id 면 포맷,
// 아니면(이미 표시명) 그대로. 1M context 변형이 그대로 보인다.
const shortModel = (m?: string) =>
  !m ? '' : !m.startsWith('claude-') ? m
    : m.replace('claude-', '').replace(/-(\d+)-(\d+)$/, ' $1.$2').replace(/^./, (c) => c.toUpperCase());
const shortCwd = (p?: string) => (!p ? '' : p.split('/').filter(Boolean).slice(-2).join('/'));

function MetaChip({ label, dim, onClick }: { label: string; dim?: boolean; onClick?: () => void }) {
  return (
    <span
      onClick={onClick}
      title={onClick ? '클릭해서 변경' : undefined}
      style={{
        fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: 600,
        padding: '2px 8px', borderRadius: 6,
        background: dim ? 'transparent' : 'var(--cth-cream-100)',
        color: dim ? 'var(--cth-ink-300)' : 'var(--cth-ink-700)',
        border: dim ? 'none' : '1px solid var(--cth-cream-200)',
        cursor: onClick ? 'pointer' : undefined,
      }}>{label}</span>
  );
}

export interface TerminalPeekPanelProps {
  surfaceId: string;
  title: string;
  onClose: () => void;
  /** CommandCenter '학생별 대화' 탭에 내장될 때 — 폭을 부모에 맞추고 좌측 보더 제거. */
  embedded?: boolean;
}

// 학생 대화 패널 — 메신저처럼 대화만(선생님 프롬프트 오른쪽·학생 답변 왼쪽).
// 화면(raw 터미널)은 '터미널 보기'로 보면 되므로 여기엔 두지 않는다.
export function TerminalPeekPanel({ surfaceId, title, onClose, embedded }: TerminalPeekPanelProps) {
  const agent = useStore((s) => s.agents.find((a) => a.id === surfaceId));
  const [turns, setTurns] = useState<Turn[]>([]);
  const [menu, setMenu] = useState<{ title: string; options: { idx: number; label: string; cur: boolean }[] } | null>(null);
  const [images, setImages] = useState<string[]>([]); // SendUserFile 로 보낸 이미지
  const [loaded, setLoaded] = useState(false); // 첫 폴 완료 — 빈 상태 문구 분기
  const [input, setInput] = useState('');
  const [sending, setSending] = useState(false);
  const [flash, setFlash] = useState<'ok' | 'err' | null>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const atBottomRef = useRef(true); // 사용자가 위로 스크롤했으면 자동 하단고정 멈춤

  // 대화 내역: transcript jsonl 우선(정상 학생 — 깔끔). 비어있으면 PTY 화면(peek)
  // 에서 파싱(claude 가 jsonl 라이브 기록 안 해도 화면엔 항상 있음). 학생 바뀌면 초기화.
  useEffect(() => {
    let stopped = false;
    setLoaded(false);
    setTurns([]);
    setMenu(null);
    setImages([]);
    setInput('');
    const tick = async () => {
      const [ts, screen, imgs] = await Promise.all([
        fetchTranscript(surfaceId, 30),
        fetchPeek(surfaceId, 60),
        fetchSentImages(surfaceId, 12),
      ]);
      if (stopped) return;
      setTurns(ts.length ? ts : parsePtyConversation(screen));
      setMenu(parsePromptMenu(screen));
      // 훅 기록(진짜 SendUserFile) + 화면 파싱(코덱스/raw) 병합, basename dedupe.
      const byBase = new Map<string, string>();
      for (const p of [...imgs, ...parseImagePaths(screen)]) {
        const base = p.split('/').pop() ?? p;
        const cur = byBase.get(base);
        if (!cur || (p.startsWith('/') && !cur.startsWith('/'))) byBase.set(base, p);
      }
      setImages([...byBase.values()]);
      setLoaded(true);
    };
    void tick();
    const iv = setInterval(tick, 1500);
    return () => { stopped = true; clearInterval(iv); };
  }, [surfaceId]);

  // 새 내용 도착 시, 사용자가 하단에 있을 때만 따라내린다(위로 스크롤 중이면 안 건드림 —
  // 거노: 스크롤 올리면 자꾸 내려가던 버그).
  useEffect(() => {
    const el = bodyRef.current;
    if (el && atBottomRef.current) el.scrollTop = el.scrollHeight;
  }, [turns, images]);

  const onBodyScroll = () => {
    const el = bodyRef.current;
    if (el) atBottomRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
  };

  // 실시간 미러: 웹 입력을 칠 때마다 터미널 PTY 라인과 동기화한다(거노 요청 —
  // "웹에 치면 터미널에 실시간으로"). Ctrl-U(\x15)로 줄을 비우고 현재 입력 전체를
  // 재전송(submit=false). 매 글자 전체 재전송이라 백스페이스·편집·IME 까지 self-heal.
  // \x15 가 claude TUI(Ink)에서 줄을 비우는 건 submit 경로에서 검증됨.
  const mirror = (next: string) => {
    setInput(next);
    void sendToPane(surfaceId, '\x15' + next, false);
  };

  // 제출: 라인은 이미 라이브로 쳐져 있으니 CR 만 보낸다(handler 가 \r 을 140ms 지연
  // 전송해 Ink 가 paste/입력 처리를 끝낸 뒤 Enter 로 먹는다).
  const submit = async () => {
    if (sending) return;
    setSending(true);
    const ok = await sendToPane(surfaceId, '\r', false);
    setSending(false);
    setFlash(ok ? 'ok' : 'err');
    setTimeout(() => setFlash(null), 1200);
    if (ok) setInput('');
    inputRef.current?.focus();
  };

  const onKey = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter') { e.preventDefault(); void submit(); }
    if (e.key === 'c' && e.ctrlKey) { e.preventDefault(); setInput(''); void sendToPane(surfaceId, '\x03', false); }
  };

  // 인터랙티브 메뉴 선택 → 숫자 단축키 전송(claude 가 즉시 선택). 다음 폴에서 갱신.
  const pickMenu = async (idx: number) => {
    setMenu(null);
    await sendToPane(surfaceId, String(idx), false);
  };

  // 학생 종료 — pane kill(close_surface). 되돌릴 수 없어 확인 후.
  const onKill = async () => {
    if (!window.confirm(`${title} 학생을 종료할까요? (pane 닫힘)`)) return;
    await closeAgent(surfaceId);
    onClose();
  };

  return (
    <div style={{
      width: embedded ? '100%' : 340, flex: embedded ? 1 : undefined,
      flexShrink: 0, height: '100%',
      display: 'flex', flexDirection: 'column',
      background: 'var(--cth-cream-50)',
      borderLeft: embedded ? 'none' : '1px solid var(--cth-cream-200)',
      overflow: 'hidden'
    }}>
      {/* 헤더: 캐릭터명 + 닫기 */}
      <div style={{
        display: 'flex', alignItems: 'center', gap: 10,
        padding: '10px 12px',
        background: 'var(--cth-cream-50)',
        borderBottom: '1px solid var(--cth-cream-200)'
      }}>
        <span style={{ fontFamily: 'var(--cth-font-display)', fontSize: 15, fontWeight: 700, color: 'var(--cth-ink-900)' }}>
          {title} <span style={{ color: 'var(--cth-ink-300)', fontWeight: 400, fontSize: 13 }}>{surfaceId}</span>
        </span>
        <div style={{ flex: 1 }} />
        <button
          onClick={() => void onKill()}
          title="학생 종료 (pane 닫기)"
          style={{
            height: 28, padding: '0 10px', borderRadius: 8, border: 'none', cursor: 'pointer',
            background: 'var(--cth-coral)', color: '#fff',
            fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 600,
            display: 'inline-flex', alignItems: 'center'
          }}
        >종료</button>
        <button
          onClick={onClose}
          title="대화 닫기"
          style={{
            width: 28, height: 28, borderRadius: 8, border: 'none', cursor: 'pointer',
            background: 'var(--cth-cream-100)', color: 'var(--cth-ink-500)',
            fontFamily: 'var(--cth-font-ui)', fontSize: 16, lineHeight: 1,
            display: 'inline-flex', alignItems: 'center', justifyContent: 'center'
          }}
        >×</button>
      </div>

      {/* 학생 메타 — 모델·브랜치·컨텍스트%·경로(클로드 실제 지표) */}
      {agent && (agent.model || agent.branch || agent.cwd) && (
        <div style={{
          display: 'flex', flexWrap: 'wrap', gap: 6, alignItems: 'center',
          padding: '6px 12px', borderBottom: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-50)',
        }}>
          {agent.model && <MetaChip label={shortModel(agent.model)} onClick={() => void sendToPane(surfaceId, '/model', true)} />}
          {agent.branch && <MetaChip label={`⎇ ${agent.branch}`} />}
          {/* 컨텍스트 % — 상태바 파싱(contextPct) 우선, 없으면 토큰/한도 계산 폴백 */}
          {agent.contextPct != null && agent.contextPct > 0 ? (
            <MetaChip label={`컨텍스트 ${agent.contextPct}%`} />
          ) : agent.contextTokens != null && agent.contextLimit ? (
            <MetaChip label={`컨텍스트 ${Math.round((agent.contextTokens / agent.contextLimit) * 100)}%`} />
          ) : null}
          {agent.cwd && <MetaChip label={shortCwd(agent.cwd)} dim />}
        </div>
      )}

      {/* 대화(채팅 버블) + 보낸 이미지 */}
      <div ref={bodyRef} onScroll={onBodyScroll} style={{ flex: 1, overflow: 'auto', padding: '14px 16px', background: 'var(--cth-cream-100)' }}>
        {turns.length === 0 && images.length === 0 ? (
          <div style={{ color: 'var(--cth-ink-300)', fontFamily: 'var(--cth-font-ui)', fontSize: 13, textAlign: 'center', marginTop: 40 }}>
            {loaded ? '아직 대화가 없어요' : '대화를 불러오는 중…'}
          </div>
        ) : (
          <>
            {turns.map((t, i) => {
              const mine = t.role === 'user';
              // 메신저: 선생님(user)=우측 카톡 노랑, 학생(assistant)=좌측 아바타+흰 말풍선.
              return (
                <div key={i} style={{ display: 'flex', justifyContent: mine ? 'flex-end' : 'flex-start', marginBottom: 10, gap: 8, alignItems: 'flex-end' }}>
                  {!mine && (
                    <div style={{ width: 34, height: 34, borderRadius: 11, overflow: 'hidden', flexShrink: 0, background: 'var(--cth-cream-100)', border: '1px solid var(--cth-cream-200)', display: 'flex', alignItems: 'flex-end', justifyContent: 'center' }}>
                      <SpritePortrait character={title} scale={1.5} />
                    </div>
                  )}
                  <div style={{
                    maxWidth: '72%', padding: '8px 12px',
                    borderRadius: 14,
                    borderTopLeftRadius: mine ? 14 : 4,
                    borderTopRightRadius: mine ? 4 : 14,
                    background: mine ? '#FEE500' : '#fff',
                    color: mine ? '#3A2E00' : 'var(--cth-ink-900)',
                    border: mine ? 'none' : '1px solid var(--cth-cream-200)',
                    boxShadow: '0 1px 3px rgba(21, 41, 74, 0.08)',
                    fontFamily: 'var(--cth-font-ui)', fontSize: 13, lineHeight: 1.55,
                    wordBreak: 'break-word'
                  }}><Markdown text={t.text} /></div>
                </div>
              );
            })}

            {/* 학생이 SendUserFile 로 보낸 이미지 — 좌측(학생) 이미지 버블. 클릭=원본. */}
            {images.map((path, i) => (
              <div key={`img-${path}-${i}`} style={{ display: 'flex', justifyContent: 'flex-start', marginBottom: 10, gap: 8, alignItems: 'flex-end' }}>
                <div style={{ width: 34, height: 34, borderRadius: 11, overflow: 'hidden', flexShrink: 0, background: 'var(--cth-cream-100)', border: '1px solid var(--cth-cream-200)', display: 'flex', alignItems: 'flex-end', justifyContent: 'center' }}>
                  <SpritePortrait character={title} scale={1.5} />
                </div>
                <button onClick={() => void openFile(path)} title={`${path}\n클릭 = OS 기본 뷰어로 열기`} style={{
                  maxWidth: '74%', padding: 4, borderRadius: 14, borderTopLeftRadius: 4, cursor: 'pointer',
                  background: '#fff', border: '1px solid var(--cth-cream-200)',
                  boxShadow: '0 1px 3px rgba(21, 41, 74, 0.08)', display: 'block',
                }}>
                  <img src={imageFileUrl(path)} alt={path.split('/').pop() ?? ''} style={{
                    display: 'block', maxWidth: '100%', maxHeight: 240, borderRadius: 10, objectFit: 'contain',
                  }} />
                </button>
              </div>
            ))}
          </>
        )}
      </div>

      {/* 인터랙티브 메뉴(/model·AskQuestion) — 화면 파싱 → 선택지 카드. 클릭 전송. */}
      {menu && (
        <div style={{ padding: '10px 14px', borderTop: '1px solid var(--cth-cream-200)', background: 'var(--cth-sky-light)', maxHeight: 220, overflowY: 'auto' }}>
          {menu.title && <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 700, color: 'var(--cth-ink-900)', marginBottom: 8 }}>{menu.title}</div>}
          <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
            {menu.options.map((o) => (
              <button key={o.idx} onClick={() => void pickMenu(o.idx)} style={{
                textAlign: 'left', padding: '8px 12px', borderRadius: 9, cursor: 'pointer',
                border: o.cur ? '2px solid var(--cth-sky)' : '1px solid var(--cth-cream-200)',
                background: '#fff', fontFamily: 'var(--cth-font-ui)', fontSize: 13, color: 'var(--cth-ink-900)',
                display: 'flex', alignItems: 'center', gap: 8,
              }}>
                <span style={{ fontWeight: 800, color: 'var(--cth-sky)', minWidth: 14 }}>{o.idx}</span>
                <span style={{ flex: 1 }}>{o.label}</span>
                {o.cur && <span style={{ fontSize: 10, color: 'var(--cth-sky)', fontWeight: 700 }}>현재</span>}
              </button>
            ))}
          </div>
        </div>
      )}

      {/* 입력창 — 학생에게 직접 전송(양방향) */}
      <div style={{
        display: 'flex', alignItems: 'center', gap: 8,
        padding: '8px 12px',
        background: 'var(--cth-cream-50)',
        borderTop: '1px solid var(--cth-cream-200)'
      }}>
        <span style={{
          fontFamily: 'var(--cth-font-ui)', fontSize: 13, fontWeight: 700,
          color: flash === 'ok' ? 'var(--cth-mint)' : flash === 'err' ? 'var(--cth-coral)' : 'var(--cth-sky)',
          flexShrink: 0
        }}>{flash === 'err' ? '!' : '›'}</span>
        <input
          ref={inputRef}
          value={input}
          onChange={(e) => mirror(e.target.value)}
          onKeyDown={onKey}
          disabled={sending}
          placeholder="학생에게 지시 — 치는 대로 터미널에 실시간 · Enter 전송 · Ctrl+C 인터럽트"
          style={{
            flex: 1,
            fontFamily: 'var(--cth-font-ui)', fontSize: 13,
            background: '#fff', border: '1px solid var(--cth-cream-200)', borderRadius: 9,
            padding: '7px 11px', outline: 'none',
            color: 'var(--cth-ink-900)', opacity: sending ? 0.5 : 1
          }}
        />
        <button
          onClick={() => void submit()}
          disabled={sending}
          style={{
            fontFamily: 'var(--cth-font-ui)', fontSize: 13, fontWeight: 600,
            padding: '7px 14px', border: 'none', borderRadius: 9,
            cursor: sending ? 'not-allowed' : 'pointer',
            background: 'linear-gradient(180deg, #6BB0F0, #4A90E2)', color: '#fff',
            boxShadow: 'inset 0 1px 0 rgba(255,255,255,0.5)',
            opacity: sending ? 0.4 : 1
          }}
        >전송</button>
      </div>
    </div>
  );
}
