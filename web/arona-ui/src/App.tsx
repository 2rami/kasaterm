import { useEffect, useRef, useState } from 'react';
import { useStore, type Agent } from './store';
import { ClassroomView } from './components/ClassroomView';
import { CommandCenter } from './components/CommandCenter';
import { TitleBar } from './components/TitleBar';
import { RoomMap } from './components/RoomMap';
import { startBoardPolling, focusPane, fetchClaudeUsage, fetchSessions, fetchBackgroundAgents, switchSession, newRoom, closeRoom, fetchLayout, type ClaudeUsage, type SessionsInfo, type RecentSession, type PaneRect, type BackgroundAgent } from './lib/mcp';
import { TerminalPeekPanel } from './components/TerminalPeekPanel';
import { TerminalBlockCard } from './components/TerminalBlockCard';
import { assignSprites } from './lib/sprites';
import { accentByName, hex } from './design/tokens';

type ViewMode = 'terminal' | 'classroom';
// 중앙 멀티뷰 한 칸 = 살아있는 학생(surface id) 또는 오프라인 과거 세션(offline=true).
// transferred = ←← detach(또는 저장 버튼)로 방금 background 로 넘어간 세션 — 헤더에
// "오프라인" 대신 "백그라운드로 넘어감" 배지를 띄운다.
// daemonShort = background(claude agents) 세션의 short id(8자). 있으면 이어가기 명령을
// `claude attach <short>` 로 — background 세션은 `--resume` 불가("use claude agents to attach").
type PeekItem = { id: string; title: string; offline?: boolean; cwd?: string; transferred?: boolean; daemonShort?: string };

// dev 디자인 검증용 목 학생(URL ?mock=1). board 비어도 풀 화면을 본다.
const MOCK_AGENTS: Agent[] = [
  { id: '%1', name: '아로나', character: '아로나', accent: 'sky', status: 'idle', project: 'tmuxify', progress: 2, contextTokens: 30000, tokensIn: 24000, tokensOut: 6000, costUsd: 0.18, contextLimit: 200000, model: 'claude-opus-4-8', cwd: '/Users/kasa/Desktop/momewomo/tmuxify', branch: 'main', isGod: true, lastReply: '선생님, 오늘 의뢰 정리했어요!' },
  { id: '%2', name: '모모이', character: '모모이', accent: 'coral', status: 'working', currentTool: 'Bash', project: 'API 장애 분석', action: 'log_01.txt 원인 추적 중', progress: 5, contextTokens: 90000, tokensIn: 72000, tokensOut: 18000, costUsd: 0.42, subagents: ['로그 패턴 분석', '메트릭 수집'], contextLimit: 200000 },
  { id: '%3', name: '유즈', character: '유즈', accent: 'lemon', status: 'working', currentTool: 'Edit', project: '자동화 스크립트', action: '빌드 파이프라인 작성', progress: 4, contextTokens: 64000, tokensIn: 50000, tokensOut: 14000, costUsd: 0.31 },
  { id: '%4', name: '아리스', character: '아리스', accent: 'lilac', status: 'waiting', project: '일일 보고서', progress: 3, contextTokens: 45000, tokensIn: 38000, tokensOut: 7000, costUsd: 0.15, lastReply: '이 방향이 맞을까요?' },
  { id: '%5', name: '미도리', character: '미도리', accent: 'mint', status: 'idle', project: '시스템 테스트', progress: 1, contextTokens: 12000, tokensIn: 10000, tokensOut: 2000, costUsd: 0.05 },
];

// 웹뷰는 항상 SCHALE OS 교실 — solo/god 모드 마커 폐기(잔재 정리). ready 로 초기 로딩만 게이트.
export function App() {
  const agents = useStore((s) => s.agents);
  const backgroundAgents = useStore((s) => s.backgroundAgents);
  const [ready, setReady] = useState(false);
  // 기본 뷰 = 터미널 pane 그리드(세션 뷰어). 교실(캐릭터)·카드는 토글로.
  const [view, setView] = useState<ViewMode>('terminal');
  // 중앙 멀티뷰 = 터미널 layout 미러(거노: 터미널이랑 pane 위치 동기화). fetchLayout 의
  // % 배치를 그대로 absolute 로 그린다. activeId = 마지막 포커스 학생(우측·교실 강조).
  // offlinePeek = 과거 세션 단독 보기(layout 과 별개, 읽기 전용).
  const [layoutRects, setLayoutRects] = useState<PaneRect[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  // 멀티뷰 타일 임시 전체화면 — 제목 더블클릭 토글(터미널 toggle_pane_zoom 의 arona 판).
  // 그 surface 하나만 풀커버로, 나머지는 가린다. 방 전환·layout 에서 사라지면 자동 해제.
  const [zoomedSurface, setZoomedSurface] = useState<string | null>(null);
  const [offlinePeek, setOfflinePeek] = useState<PeekItem | null>(null);
  // ←← detach(또는 저장 버튼)로 "지금 보던 세션이 background daemon 으로 넘어갔다"를 감지해
  // 그 후신 대화로 자동 전환하기 위한 상태. handledDetach = 이미 전환한 sessionId(사용자가
  // 닫은 뒤 재강탈 방지), pendingDetach = 저장 버튼을 누른 surface(그 부모를 가진 bg 가 뜨면
  // 전환), toast = 우하단 알림 pill.
  const handledDetach = useRef<Set<string>>(new Set());
  // 폴링 tick 사이 background sessionId 집합 — "방금 새로 등장한" 세션 감지용(터미널 ←← detach).
  const prevBgIds = useRef<Set<string> | null>(null);
  const [pendingDetach, setPendingDetach] = useState<string | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  useEffect(() => {
    if (!toast) return;
    const t = window.setTimeout(() => setToast(null), 3200);
    return () => window.clearTimeout(t);
  }, [toast]);
  // wry webview(WKWebView)는 브라우저와 달리 ⌘R/F5 기본 새로고침이 없다(거노: webview도
  // 새로고침되게). dev 서버(5173) 브라우저 기본과 겹쳐도 무해.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'F5' || ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'r')) {
        e.preventDefault();
        location.reload();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);
  // 학생 대화를 열면 = 그 학생의 현재 대기('확인 필요')를 '확인'한 것 → 코랄 표시 해제.
  // 학생 클릭 = 터미널에서 그 pane 포커스(layout 미러라 위치는 터미널이 결정 — 거노: pane
  // 위치 동기화). 우측·교실 강조도 그 학생으로. 과거 세션 단독 보기 중이면 라이브로 복귀.
  const openStudent = (id: string, _title: string) => {
    void focusPane(id);
    setActiveId(id);
    setOfflinePeek(null);
    useStore.getState().ackStudent(id);
  };
  // 최근 세션 클릭 → 바로 resume 하지 않고, 그 세션의 대화를 오프라인(읽기 전용) 뷰어로
  // 메인에 단독 띄운다(layout 미러와 별개). 이어가기는 뷰어 하단 '현재 터미널에 입력'.
  const openOfflineSession = (s: RecentSession) => {
    setOfflinePeek({ id: s.id, title: s.label, offline: true, cwd: s.cwd });
    setView('terminal');
  };
  // 보드의 background daemon 세션 클릭 → 그 session_id 의 transcript 를 중앙에 읽기 전용으로
  // 띄운다(offlinePeek). resume(pane spawn) 과 별개라 daemon 세션을 안전하게 들여다보기.
  const openBackgroundSession = (a: BackgroundAgent, transferred = false) => {
    setOfflinePeek({ id: a.sessionId, title: a.name || a.id, offline: true, cwd: a.cwd, transferred, daemonShort: a.kind === 'background' ? a.id : undefined });
    setView('terminal');
  };
  // 저장 버튼 = 그 surface 를 background 로 detach 요청. 부모=그 surface 인 background 가
  // 뜨면 아래 감지 useEffect 가 자동 전환한다. 여기선 토스트 + 그 surface 를 pendingDetach 로
  // 표시(6초 후 자동 해제 — 매칭 실패 시 활성 pane 오작동 방지).
  const handleSaved = (surface: string) => {
    setPendingDetach(surface);
    setToast('백그라운드로 저장했어요 · 넘어간 대화로 이어가는 중…');
    window.setTimeout(() => setPendingDetach((s) => (s === surface ? null : s)), 6000);
  };
  // 서브에이전트 드릴인 — 학생 메타칸의 ↳ 칩 클릭 → 그 서브에이전트 대화를 부모 옆 별도
  // 타일로(거노: "따로 볼수있게"). layout 미러엔 없는 가상 타일이라 별도 배열로 관리.
  // 같은 agentId 다시 누르면 토글로 닫힌다.
  const [subPeeks, setSubPeeks] = useState<{ parentSurface: string; agentId: string; agentType: string; label: string }[]>([]);
  const openSubagent = (parentSurface: string, agentId: string, agentType: string, label: string) => {
    setSubPeeks((prev) => prev.some((p) => p.agentId === agentId)
      ? prev.filter((p) => p.agentId !== agentId)
      : [...prev, { parentSurface, agentId, agentType, label }]);
    setView('terminal');
  };
  // 방 = kasaterm 윈도우(거노). GET /sessions 폴링 → 좌측 방 네비. 클릭하면 그 윈도우로.
  const [sessions, setSessions] = useState<SessionsInfo>({ count: 0, active: 0, labels: [], saved: [] });
  useEffect(() => {
    let stop = false;
    const tick = async () => { const s = await fetchSessions(); if (!stop) setSessions(s); };
    void tick();
    const iv = setInterval(tick, 1500);
    return () => { stop = true; clearInterval(iv); };
  }, []);
  const [usage, setUsage] = useState<ClaudeUsage | null>(null); // claude oauth 사용량(5h/주간)
  // 라이트/다크 테마 — 태양 버튼 토글, localStorage 영속(거노). data-theme 로 토큰 재매핑.
  const [theme, setTheme] = useState<'light' | 'dark'>(() => (localStorage.getItem('schale-theme') === 'dark' ? 'dark' : 'light'));
  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    localStorage.setItem('schale-theme', theme);
  }, [theme]);
  // 좌(방/학생)·우(업무/소스/스케줄) 패널 — 상단 아이콘 클릭 시 팝오버 오버레이로 펼침(거노:
  // 상단 아이콘+팝오버, 평소엔 멀티뷰만 꽉 차게). 기본 닫힘, 멀티뷰 공간을 안 뺏는다.
  const [leftOpen, setLeftOpen] = useState(false);
  const [rightOpen, setRightOpen] = useState(false);
  // 집중 모드 — 타이틀바·헤더·좌우 패널·풋터까지 전부 접고 중앙(멀티뷰/대화)만 꽉 채운다
  // (거노: 평소에도 다 닫을 수 있게). 헤더 버튼·⌘\ 토글, 우상단 떠있는 버튼·Esc 로 복귀.
  const [focusMode, setFocusMode] = useState(() => localStorage.getItem('schale-focus') === '1');
  useEffect(() => { localStorage.setItem('schale-focus', focusMode ? '1' : '0'); }, [focusMode]);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === '\\') { e.preventDefault(); setFocusMode((v) => !v); }
      else if (e.key === 'Escape') { setLeftOpen(false); setRightOpen(false); setFocusMode((v) => (v ? false : v)); }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);
  // claude oauth 사용량(5시간/주간 한도·리셋)을 1분마다 폴링 → TitleBar 게이지.
  // 실패(토큰만료·429·네트워크)면 null 이 오는데, 그때 칩을 지우면 1분마다 깜빡인다(거노).
  // 마지막 성공값을 유지하고, 실패는 그냥 무시 — 다음 성공 틱이 갱신.
  useEffect(() => {
    let stop = false;
    const tick = async () => { const u = await fetchClaudeUsage(); if (!stop) setUsage((prev) => u ?? prev); };
    void tick();
    const iv = setInterval(tick, 60_000);
    return () => { stop = true; clearInterval(iv); };
  }, []);

  const forceMock = new URLSearchParams(location.search).get('mock') === '1';
  useEffect(() => {
    // solo/god 모드 마커 폐기(잔재 정리) — 웹뷰는 항상 SCHALE OS 교실. 초기 1회 ready 만.
    setReady(true);
  }, []);
  useEffect(() => {
    if (forceMock) { useStore.getState().setAgents(MOCK_AGENTS); return; }
    if (ready) return startBoardPolling(1000);
  }, [ready]);
  // claude agents(pane 밖 background 학생) 폴링 → 교실에 별도 표시. claude 프로세스
  // spawn 비용이 있어 3초로 느슨하게(background 세션은 자주 안 바뀜).
  useEffect(() => {
    if (forceMock || !ready) return;
    let stop = false;
    const tick = async () => { const a = await fetchBackgroundAgents(); if (!stop) useStore.getState().setBackgroundAgents(a); };
    void tick();
    const iv = setInterval(tick, 3000);
    return () => { stop = true; clearInterval(iv); };
  }, [ready]);

  // 터미널 뷰 기본(A안: 중앙 단일 대화) — 선택된 학생이 없으면 god(아로나)·첫 학생을
  // 자동으로 띄워 중앙이 비지 않게. peek 있으면 그대로 둔다(거노).
  useEffect(() => {
    if (activeId || !agents.length) return;
    const first = [...agents].sort((a, b) => Number(b.isGod) - Number(a.isGod))[0];
    if (first) setActiveId(first.id);
  }, [agents, activeId]);
  // foreground 학생이 없고(claude pane 이 안 떠 있음) background daemon 세션만 있으면 — 첫
  // background 를 중앙에 자동 표시(offlinePeek 버블). 터미널이 꺼져도/claude 가 안 떠도 웹뷰가
  // "아로나 오는 중…" 빈 교실 스피너 대신 daemon claude 대화를 바로 보여준다(거노 핵심).
  useEffect(() => {
    if (activeId || offlinePeek || agents.length > 0 || backgroundAgents.length === 0) return;
    // 활성(working) background 를 우선 — 목록 첫 거(옛 done 세션)가 아니라 지금 도는 대화를
    // 보여줘야 한다(거노: 대화 안 맞던 버그). working 없으면 background, 그것도 없으면 첫 거.
    const bg = backgroundAgents.find((a) => a.kind === 'background' && (a.state === 'working' || a.status === 'working'))
      ?? backgroundAgents.find((a) => a.kind === 'background')
      ?? backgroundAgents[0];
    if (bg) openBackgroundSession(bg);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [agents.length, backgroundAgents, activeId, offlinePeek]);
  // "지금 보던 세션이 background daemon 으로 넘어감" 감지 → 후신 자동 전환 + 넘어감 배지.
  // 여기선 **명시적 신호만** 처리한다: (1) '저장' 버튼을 누른 surface(pendingDetach)의 후신,
  // (2) 수동으로 보던 과거 offline 세션(viewedSid)이 background 로 승격. activeId(로드 직후
  // 자동 선택되는 pane)로는 매칭하지 않는다 — 저장 안 했는데 그 pane 의 과거 fork 세션으로
  // 튕기던 버그(handledDetach 는 리로드마다 리셋돼 로드 직후 강탈을 못 막음) 방지. 터미널에서
  // 직접 ←← 한 경우는 아래 "새로 등장" effect 가 부모 pane 생존으로 판정해 커버한다.
  useEffect(() => {
    const viewedSid = offlinePeek && !offlinePeek.transferred ? offlinePeek.id : null;
    if (!pendingDetach && !viewedSid) return;
    const hit = backgroundAgents.find((a) =>
      a.kind === 'background'
      && !handledDetach.current.has(a.sessionId)
      && offlinePeek?.id !== a.sessionId
      && (
        (!!pendingDetach && a.parentSurface === pendingDetach)
        || (!!viewedSid && (a.parentSessionId === viewedSid || a.sessionId === viewedSid))
      ));
    if (hit) {
      handledDetach.current.add(hit.sessionId);
      setPendingDetach(null);
      openBackgroundSession(hit, true);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [backgroundAgents, offlinePeek, pendingDetach]);
  // 터미널에서 직접 ←← 로 detach 한 경우 — 저장버튼도, 웹뷰에서 선택한 activeId 매칭도 안
  // 걸린다(웹뷰가 그 pane 을 활성 선택 안 했을 수 있음). 그래서 폴링에서 "직전 tick 엔 없던,
  // 방금 새로 등장한 working background" 를 감지해 그 대화로 전환한다. 단 fetchBackgroundAgents
  // 는 --all(다른 방·위임 세션 포함)이라, **부모 pane 이 지금 이 창에 살아있는**(parentSurface
  // 가 라이브 agents 에 있는) 세션만 잡는다 — 무관한 다른 방 세션이 중앙뷰를 강탈하지 않게.
  // 첫 tick(prevBgIds=null)은 기준선만 세우고, 수동 열람 중(offlinePeek non-transferred)엔 안 뺏는다.
  useEffect(() => {
    const cur = new Set(backgroundAgents.filter((a) => a.kind === 'background').map((a) => a.sessionId));
    const prev = prevBgIds.current;
    prevBgIds.current = cur;
    if (prev === null) return;
    if (offlinePeek && !offlinePeek.transferred) return;
    const fresh = backgroundAgents.find((a) =>
      a.kind === 'background'
      && !prev.has(a.sessionId)
      && !handledDetach.current.has(a.sessionId)
      && (a.state === 'working' || a.status === 'working')
      && !!a.parentSurface
      && agents.some((ag) => ag.id === a.parentSurface));
    if (fresh) {
      handledDetach.current.add(fresh.sessionId);
      setPendingDetach(null);
      openBackgroundSession(fresh, true);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [backgroundAgents, offlinePeek, agents]);
  // 터미널 layout(% 배치) 폴링 → 중앙 멀티뷰가 터미널 split 을 그대로 미러(거노: pane 위치
  // 동기화). 과거 세션 단독 보기 중이거나 터미널 뷰가 아니면 폴링 불필요.
  useEffect(() => {
    // offlinePeek(bg 대화) 중에도 layout 은 계속 폴링한다 — fg pane 이 살아있으면 bg 를 왼쪽
    // 타일로 두고 그 옆에 fg 그리드를 나란히 그려야 하고, fg 가 없으면 fetchLayout 이 빈 배열이라
    // 자연히 bg 단독 전체가 된다(렌더의 layoutRects.length===0 분기).
    if (forceMock || !ready || view !== 'terminal') return;
    let stop = false;
    const tick = async () => { const r = await fetchLayout(); if (!stop) setLayoutRects(r); };
    void tick();
    const iv = setInterval(tick, 1500);
    return () => { stop = true; clearInterval(iv); };
  }, [ready, view]);
  // 줌된 pane 이 split/close 로 layout 에서 사라지면 전체화면 자동 해제(유령 줌 방지).
  useEffect(() => {
    if (zoomedSurface && !layoutRects.some((r) => r.surface_id === zoomedSurface)) setZoomedSurface(null);
  }, [layoutRects, zoomedSurface]);

  if (!ready) {
    return <div style={{ padding: 24, color: 'var(--cth-ink-500)' }}>로딩…</div>;
  }

  // god 먼저 정렬 + 작업명 학생에게 게임개발부 외형(spriteChar) 폴백 배정(교실 스프라이트용).
  const sorted = assignSprites([...agents].sort((a, b) => Number(b.isGod) - Number(a.isGod)));
  // 방 = kasaterm 윈도우. board(collab_board)는 활성 윈도우의 panes 만 주므로 보이는
  // 학생이 곧 그 방 학생 — 클라이언트 cwd 필터 없이 그대로 그린다(거노).
  const shown = sorted;
  // 배경 = 기본 교실바닥 하나로(거노: 평면도/방별맵 실험 접고 처음꺼 하나만).
  const roomBg = 'classroom-floor.png';

  // 우측 CommandCenter·교실 강조에 넘길 active 학생(과거 세션 보기 중이면 그걸 우선).
  const activeSelected: PeekItem | null = offlinePeek
    ?? (activeId ? { id: activeId, title: sorted.find((x) => x.id === activeId)?.name ?? activeId } : null);

  // 방(윈도우) 전환 — POST /session-switch?idx → 그 터미널 윈도우로(거노: "gui에서
  // 방바꾸면 터미널 윈도우 바뀌게"). board 폴링이 그 윈도우 학생으로 따라온다.
  const selectRoom = (idx: number) => {
    void switchSession(idx);
    setSessions((s) => ({ ...s, active: idx })); // 폴링 전 즉시 하이라이트
    // 방 바뀌면 active·과거세션 초기화 — layout 폴링이 새 방 pane 으로 미러를 갱신(거노)
    setActiveId(null);
    setOfflinePeek(null);
    setLayoutRects([]);
    setSubPeeks([]);
    setZoomedSurface(null);
  };

  return (
    // SCHALE OS 전체 셸: 세로 100% + 가로 100%
    <div style={{ height: '100%', display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>

      {/* 슬림 아이콘 바 — 집중 모드면 숨김. 좌=방, 우=업무·교실토글·집중(거노: 종·메일·터미널/교실탭 제거). */}
      {!focusMode && (<TitleBar
        leftBadge={agents.filter((a) => a.status === 'waiting' || a.status === 'blocked').length}
        usage={usage}
        theme={theme}
        onToggleTheme={() => setTheme((t) => (t === 'dark' ? 'light' : 'dark'))}
        onToggleLeft={() => { setLeftOpen((v) => !v); setRightOpen(false); }}
        onToggleRight={() => { setRightOpen((v) => !v); setLeftOpen(false); }}
        leftOpen={leftOpen}
        rightOpen={rightOpen}
        classroom={view === 'classroom'}
        onToggleClassroom={() => setView((v) => (v === 'classroom' ? 'terminal' : 'classroom'))}
        onFocus={() => setFocusMode(true)}
      />)}

      {/* 바디: 멀티뷰가 100% 점유 + 좌·우 패널은 팝오버 오버레이(거노: 패널이 공간 안 뺏게).
          relative = 팝오버 absolute 기준. */}
      <div style={{ flex: 1, display: 'flex', overflow: 'hidden', position: 'relative' }}>

        {/* 메인 컬럼 — 멀티뷰 */}
        <div style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>

          {/* 중앙: 터미널 pane 그리드(기본 뷰어) / 교실(캐릭터) / 카드 */}
          <div style={{ flex: 1, overflow: view === 'terminal' ? 'hidden' : 'auto', padding: view === 'terminal' ? 0 : view === 'classroom' ? 8 : 'var(--cth-space-4)' }}>
            {view === 'terminal' ? (
              offlinePeek && layoutRects.length === 0 ? (
                // fg pane 이 하나도 없음(터미널 꺼짐/claude 안 뜸) → bg 대화 단독 전체.
                <TerminalPeekPanel
                  surfaceId=""
                  title={offlinePeek.title}
                  onClose={() => setOfflinePeek(null)}
                  embedded
                  session={{ id: offlinePeek.id, cwd: offlinePeek.cwd ?? '', label: offlinePeek.title, transferred: offlinePeek.transferred, daemonShort: offlinePeek.daemonShort }}
                />
              ) : (
                // bg peek(좌, 있으면) + layout 미러(중, flex) + 서브에이전트 드릴인(우) — fg pane
                // 이 살아있으면 bg 대화가 전체를 덮지 않고 왼쪽 타일로 나란히(거노: 왼 bg·오 fg).
                <div style={{ height: '100%', display: 'flex' }}>
                  {offlinePeek && (
                    <div style={{ width: 360, flexShrink: 0, display: 'flex', flexDirection: 'column', borderRight: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-100)' }}>
                      <TerminalPeekPanel
                        surfaceId=""
                        title={offlinePeek.title}
                        onClose={() => setOfflinePeek(null)}
                        embedded
                        session={{ id: offlinePeek.id, cwd: offlinePeek.cwd ?? '', label: offlinePeek.title, transferred: offlinePeek.transferred, daemonShort: offlinePeek.daemonShort }}
                      />
                    </div>
                  )}
                  <div style={{ flex: 1, minWidth: 0, position: 'relative' }}>
                    {layoutRects.length ? (
                      <div style={{ position: 'relative', height: '100%', background: 'var(--cth-cream-100)' }}>
                        {layoutRects.map((r) => {
                          const a = agents.find((x) => x.id === r.surface_id);
                          const isZoom = zoomedSurface === r.surface_id;
                          const hidden = zoomedSurface != null && !isZoom; // 다른 타일이 줌이면 가린다
                          const active = activeId === r.surface_id;
                          // 학생별 accent 색 테두리 — 늘 얇게 둘러 구별하고, active/zoom 은 굵게 강조.
                          const accentHex = a?.accent ? hex(accentByName[a.accent]) : 'var(--cth-cream-200)';
                          return (
                            <div key={r.surface_id} onMouseDownCapture={() => setActiveId(r.surface_id)}
                              style={isZoom
                                ? { position: 'absolute', left: 0, top: 0, width: '100%', height: '100%', padding: 1, boxSizing: 'border-box', zIndex: 10, outline: `2.5px solid ${accentHex}`, outlineOffset: -2 }
                                : { position: 'absolute', left: `${r.x}%`, top: `${r.y}%`, width: `${r.w}%`, height: `${r.h}%`, padding: 1, boxSizing: 'border-box', display: hidden ? 'none' : undefined, outline: `${active ? 2.5 : 1.5}px solid ${accentHex}`, outlineOffset: -2 }}>
                              {a ? (
                                <TerminalPeekPanel
                                  surfaceId={r.surface_id}
                                  title={a.name ?? r.surface_id}
                                  onClose={() => void focusPane(r.surface_id)}
                                  onOpenSubagent={openSubagent}
                                  onToggleZoom={() => setZoomedSurface((z) => (z === r.surface_id ? null : r.surface_id))}
                                  zoomed={isZoom}
                                  embedded
                                />
                              ) : (
                                /* claude 가 아닌 plain 터미널 — board 행이 없어 빈창이던 걸 Warp 명령블록 스택으로. */
                                <TerminalBlockCard
                                  surfaceId={r.surface_id}
                                  rect={r}
                                  onClose={() => void focusPane(r.surface_id)}
                                  onToggleZoom={() => setZoomedSurface((z) => (z === r.surface_id ? null : r.surface_id))}
                                  zoomed={isZoom}
                                  activeAgentId={agents[0]?.id}
                                />
                              )}
                            </div>
                          );
                        })}
                      </div>
                    ) : (
                      <div style={{ height: '100%', display: 'flex', flexDirection: 'column', alignItems: 'center', justifyContent: 'center', gap: 12, padding: 24, textAlign: 'center', color: 'var(--cth-ink-300)', fontFamily: 'var(--cth-font-ui)' }}>
                        <svg width="40" height="40" viewBox="0 0 24 24" fill="none" stroke="var(--cth-ink-300)" strokeWidth={1.4} strokeLinecap="round" strokeLinejoin="round" style={{ opacity: 0.7 }}>
                          <path d="M21 11.5a8.38 8.38 0 0 1-.9 3.8 8.5 8.5 0 0 1-7.6 4.7 8.38 8.38 0 0 1-3.8-.9L3 21l1.9-5.7a8.38 8.38 0 0 1-.9-3.8 8.5 8.5 0 0 1 4.7-7.6 8.38 8.38 0 0 1 3.8-.9h.5a8.48 8.48 0 0 1 8 8v.5z" />
                        </svg>
                        <div style={{ fontSize: 14, fontWeight: 600, color: 'var(--cth-ink-500)' }}>아직 열린 대화가 없어요</div>
                        <div style={{ fontSize: 12, lineHeight: 1.7 }}>터미널에서 <span style={{ fontFamily: 'var(--cth-font-mono)', color: 'var(--cth-ink-500)' }}>claude</span> 를 켜면<br />여기에 대화가 떠요</div>
                      </div>
                    )}
                  </div>
                  {subPeeks.length > 0 && (
                    <div style={{ width: 360, flexShrink: 0, display: 'flex', flexDirection: 'column', borderLeft: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-100)' }}>
                      {/* 서브에이전트 뷰 열 — 부모(학생) 세션에 속한 영역임을 헤더로 못박는다(거노:
                          다른 pane 과 헷갈리지 않게). 각 타일은 부모 이름 prefix + 스카이 좌측 띠로 묶음. */}
                      <div style={{ flexShrink: 0, padding: '6px 12px', fontFamily: 'var(--cth-font-display)', fontSize: 10, fontWeight: 700, color: 'var(--cth-sky)', borderBottom: '1px solid var(--cth-cream-200)', display: 'flex', alignItems: 'center', gap: 6 }}>
                        <svg width="12" height="12" viewBox="0 0 16 16"><path d="M4 3v6a3 3 0 0 0 3 3h6" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" /></svg>
                        서브에이전트 뷰
                      </div>
                      {subPeeks.map((p) => {
                        const parentName = agents.find((a) => a.id === p.parentSurface)?.name ?? p.parentSurface;
                        return (
                          <div key={p.agentId} style={{ flex: 1, minHeight: 0, borderBottom: '1px solid var(--cth-cream-200)', borderLeft: '3px solid var(--cth-sky)', boxSizing: 'border-box' }}>
                            <TerminalPeekPanel
                              surfaceId=""
                              title={`${parentName} ↳ ${p.label}`}
                              embedded
                              subagent={p}
                              onClose={() => setSubPeeks((prev) => prev.filter((x) => x.agentId !== p.agentId))}
                            />
                          </div>
                        );
                      })}
                    </div>
                  )}
                </div>
              )
            ) : (
              <ClassroomView agents={shown} background={roomBg} onSelect={openStudent} selectedId={activeId ?? undefined} />
            )}
          </div>
        </div>

        {/* 좌 패널 팝오버 — 방·학생. 딤 클릭/Esc 닫힘. 선택 시 자동 닫힘(거노: 상단 아이콘+팝오버). */}
        {!focusMode && leftOpen && (
          <>
            <div onClick={() => setLeftOpen(false)} style={{ position: 'absolute', inset: 0, zIndex: 30, background: 'rgba(21,41,74,0.16)' }} />
            <div style={{ position: 'absolute', left: 0, top: 0, bottom: 0, zIndex: 31, display: 'flex', boxShadow: '3px 0 16px rgba(21,41,74,0.20)' }}>
              <RoomMap
                sessions={sessions}
                onSwitch={(i) => { selectRoom(i); setLeftOpen(false); }}
                agents={shown}
                selectedId={activeId ?? undefined}
                onSelectStudent={(id, t) => { openStudent(id, t); setLeftOpen(false); }}
                onNewRoom={(god) => { void newRoom(god); }}
                onCloseRoom={(i) => { void closeRoom(i); }}
                onOpenSession={(s) => { openOfflineSession(s); setLeftOpen(false); }}
                onCollapse={() => setLeftOpen(false)}
              />
            </div>
          </>
        )}

        {/* 우 패널 팝오버 — 업무·소스 컨트롤·스케줄. 학생 픽은 패널 유지(계속 보게). */}
        {!focusMode && rightOpen && (
          <>
            <div onClick={() => setRightOpen(false)} style={{ position: 'absolute', inset: 0, zIndex: 30, background: 'rgba(21,41,74,0.16)' }} />
            <div style={{ position: 'absolute', right: 0, top: 0, bottom: 0, zIndex: 31, width: 340, display: 'flex', minHeight: 0, boxShadow: '-3px 0 16px rgba(21,41,74,0.20)' }}>
              <CommandCenter selected={activeSelected} onClearDialog={() => setOfflinePeek(null)} onPickStudent={openStudent} onOpenBackground={openBackgroundSession} onSaved={handleSaved} openGitTab={0} onCollapse={() => setRightOpen(false)} />
            </div>
          </>
        )}
      </div>

      {/* 집중 모드 복귀 — 패널 다 숨겼을 때만 우상단 떠있는 버튼(거노). Esc·⌘\ 로도 해제. */}
      {focusMode && (
        <button onClick={() => setFocusMode(false)} title="패널 보이기 (Esc · ⌘\)"
          style={{ position: 'fixed', top: 8, right: 8, zIndex: 50, width: 30, height: 30, borderRadius: 9, cursor: 'pointer', border: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-50)', color: 'var(--cth-ink-500)', display: 'inline-flex', alignItems: 'center', justifyContent: 'center', boxShadow: '0 2px 8px rgba(21,41,74,0.16)' }}>
          <svg width="16" height="16" viewBox="0 0 16 16"><path d="M2.5 6V2.5H6M14 6V2.5h-3.5M2.5 10v3.5H6M14 10v3.5h-3.5" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" /></svg>
        </button>
      )}

      {/* 넘어감/저장 알림 — 우하단 pill, 3.2초 자동 소멸(위 useEffect). */}
      {toast && (
        <div style={{ position: 'fixed', right: 16, bottom: 16, zIndex: 60, display: 'inline-flex', alignItems: 'center', gap: 8, maxWidth: 360, padding: '10px 14px', borderRadius: 12, background: 'var(--cth-ink-900)', color: 'var(--cth-cream-50)', fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 600, boxShadow: '0 6px 20px rgba(21,41,74,0.28)' }}>
          <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" style={{ flexShrink: 0, color: '#F0B45A' }}><path d="M9 2 3.5 9H8l-1 5 5.5-7H8l1-5Z" /></svg>
          <span style={{ minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis' }}>{toast}</span>
        </div>
      )}

    </div>
  );
}
