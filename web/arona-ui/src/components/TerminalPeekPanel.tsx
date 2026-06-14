import { useEffect, useRef, useState } from 'react';
import { fetchTranscript, fetchSentImages, imageFileUrl, openFile, sendToPane, closeAgent, type Turn } from '@/lib/mcp';
import { SpritePortrait } from './SpritePortrait';
import { Markdown } from './Markdown';
import { useStore } from '@/store';

// 대화는 transcript jsonl(깨끗한 구조화 데이터)만 쓴다 — 거노: "peek로 가져오게
// 하는건 없게해". 화면(peek) 파싱은 TUI 찌꺼기(Jump-to-bottom 오버레이·라이브 입력
// 줄·번호목록을 메뉴로 오인·스크롤 따라 끊김)가 새서 폐기했다. 인터랙티브 메뉴
// (AskUserQuestion)는 추후 transcript의 tool_use에서 뽑아 다시 붙인다(peek 아님).

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

  // 대화 내역: transcript jsonl 만(거노: peek 폐기). claude 가 턴 완료 시 jsonl 에
  // 쓰므로 응답 중엔 약간 지연될 수 있으나, 화면 찌꺼기 없이 깨끗하다. 학생 바뀌면 초기화.
  useEffect(() => {
    let stopped = false;
    setLoaded(false);
    setTurns([]);
    setMenu(null);
    setImages([]);
    setInput('');
    const tick = async () => {
      const [ts, imgs] = await Promise.all([
        fetchTranscript(surfaceId, 30),
        fetchSentImages(surfaceId, 12),
      ]);
      if (stopped) return;
      setTurns(ts);
      setImages(imgs);
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

  // 제출: 미러로 이미 친 라인을 submit_payload(\x15 클리어 + 괄호붙여넣기 + CR)로 한
  // 번에 보낸다(submit=true). 서버가 이때만 messages.jsonl 에 깨끗한 텍스트로 1회 영속
  // — 미러 partial(submit=false)은 기록 안 돼 모모톡에 한 자씩 쌓이던 버그가 사라진다.
  // 빈 줄(프롬프트 확인용 Enter)은 영속 없이 bare CR.
  const submit = async () => {
    if (sending) return;
    const text = input;
    setSending(true);
    const ok = text
      ? await sendToPane(surfaceId, text, true)
      : await sendToPane(surfaceId, '\r', false);
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
        {turns.length === 0 && images.length === 0 && agent?.status !== 'working' && agent?.status !== 'thinking' ? (
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

            {/* 로딩 인디케이터 — 학생이 working/thinking 이면 타이핑 점(거노: 채팅창에서
                로딩중인지 모름). transcript 는 턴 완료 시 갱신이라 그 사이 공백을 메운다. */}
            {(agent?.status === 'working' || agent?.status === 'thinking') && (
              <div style={{ display: 'flex', justifyContent: 'flex-start', marginBottom: 10, gap: 8, alignItems: 'flex-end' }}>
                <div style={{ width: 34, height: 34, borderRadius: 11, overflow: 'hidden', flexShrink: 0, background: 'var(--cth-cream-100)', border: '1px solid var(--cth-cream-200)', display: 'flex', alignItems: 'flex-end', justifyContent: 'center' }}>
                  <SpritePortrait character={title} scale={1.5} />
                </div>
                <div style={{
                  padding: '10px 14px', borderRadius: 14, borderTopLeftRadius: 4,
                  background: '#fff', border: '1px solid var(--cth-cream-200)',
                  boxShadow: '0 1px 3px rgba(21, 41, 74, 0.08)',
                  display: 'inline-flex', alignItems: 'center', gap: 7,
                  fontFamily: 'var(--cth-font-ui)', fontSize: 12, color: 'var(--cth-ink-500)',
                }}>
                  <span style={{ display: 'inline-flex', gap: 3 }}>
                    {[0, 1, 2].map((d) => (
                      <span key={d} style={{
                        width: 6, height: 6, borderRadius: 999, background: 'var(--cth-sky)',
                        animation: 'cth-pulse 1s ease-in-out infinite', animationDelay: `${d * 0.15}s`,
                      }} />
                    ))}
                  </span>
                  {agent?.status === 'thinking' ? '생각 중…' : (agent?.currentTool || '작업 중…')}
                </div>
              </div>
            )}
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
