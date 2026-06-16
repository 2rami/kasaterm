import { useEffect, useRef, useState } from 'react';
import { fetchConversation, fetchTranscript, fetchPeek, fetchSentImages, imageFileUrl, openFile, sendToPane, closeAgent, fetchSlashCommands, type Turn } from '@/lib/mcp';
import { SpritePortrait } from './SpritePortrait';
import { Markdown } from './Markdown';
import { useStore } from '@/store';

// 대화 소스 = 캡처 프록시(/conversation)만. claude API 호출을 가로채 messages[] 를
// 깨끗하게 캡처(ccglass 방식) — peek(화면 스크래핑) 폐기(거노). 프록시 안 탄 pane 만
// transcript jsonl 폴백. 인터랙티브 메뉴(/model·AskUserQuestion)는 화면에만 떠서
// peek 와 함께 빠졌다 — 추후 필요하면 프록시 캡처에서 복원.

// board.model 은 상태바 파싱 표시명("Opus 4.8 (1M context)") 우선 — claude- id 면 포맷,
// 아니면(이미 표시명) 그대로. 1M context 변형이 그대로 보인다.
const shortModel = (m?: string) =>
  !m ? '' : !m.startsWith('claude-') ? m
    : m.replace('claude-', '').replace(/-(\d+)-(\d+)$/, ' $1.$2').replace(/^./, (c) => c.toUpperCase());
const shortCwd = (p?: string) => (!p ? '' : p.split('/').filter(Boolean).slice(-2).join('/'));

// 슬래시 자동완성 — claude-code 가 보여주는 내장 명령 전부(거노). 입력 '/co' →
// /compact·/context·/cost·/config 필터. label=명령, desc=한 줄 설명(드롭다운 표시).
const SLASH_COMMANDS: { cmd: string; desc: string }[] = [
  // 세션
  { cmd: '/help', desc: '도움말·명령 목록' },
  { cmd: '/clear', desc: '새 대화 시작' },
  { cmd: '/resume', desc: '이전 대화 복구·세션 선택' },
  { cmd: '/branch', desc: '현 지점에서 대화 분기' },
  { cmd: '/fork', desc: '백그라운드 서브에이전트로 분기 실행' },
  { cmd: '/teleport', desc: '웹 세션을 터미널로 가져오기' },
  { cmd: '/rename', desc: '세션 이름 설정' },
  { cmd: '/recap', desc: '현 세션 1줄 요약' },
  // 컨텍스트·메모리
  { cmd: '/context', desc: '컨텍스트 사용량 시각화' },
  { cmd: '/compact', desc: '대화 요약해 컨텍스트 확보' },
  { cmd: '/memory', desc: 'CLAUDE.md 메모리 편집' },
  { cmd: '/btw', desc: '히스토리 없이 빠른 질문' },
  // 모델·설정
  { cmd: '/model', desc: 'AI 모델 전환' },
  { cmd: '/effort', desc: 'effort level 설정' },
  { cmd: '/fast', desc: '빠른 모드 토글' },
  { cmd: '/config', desc: '설정 인터페이스' },
  { cmd: '/theme', desc: '색 테마 변경' },
  { cmd: '/keybindings', desc: '키 단축키 편집' },
  { cmd: '/advisor', desc: 'advisor(2차 모델 조언) 토글' },
  { cmd: '/tui', desc: '터미널 UI 렌더러 설정' },
  { cmd: '/voice', desc: '음성 받아쓰기 토글' },
  // 코드 작업
  { cmd: '/diff', desc: '미커밋 변경·per-turn diff' },
  { cmd: '/code-review', desc: 'diff 검토(버그·개선점)' },
  { cmd: '/simplify', desc: '코드 정리·개선' },
  { cmd: '/review', desc: 'PR 로컬 검토' },
  { cmd: '/security-review', desc: '보안 취약점 분석' },
  { cmd: '/rewind', desc: '이전 지점으로 복구' },
  { cmd: '/plan', desc: 'plan mode 진입' },
  { cmd: '/ultraplan', desc: '클라우드 plan 세션' },
  // 실행·검증
  { cmd: '/run', desc: '앱 실행 후 변경 확인' },
  { cmd: '/verify', desc: '변경 실제 동작 검증' },
  // 병렬 작업
  { cmd: '/agents', desc: '서브에이전트 매니저' },
  { cmd: '/background', desc: '세션을 백그라운드로 detach' },
  { cmd: '/tasks', desc: '백그라운드 작업 목록' },
  { cmd: '/batch', desc: '대규모 변경 병렬 실행' },
  { cmd: '/goal', desc: '조건 충족까지 자동 진행' },
  { cmd: '/loop', desc: '프롬프트 반복 실행' },
  { cmd: '/schedule', desc: '클라우드 routine 생성' },
  // 협업·배포
  { cmd: '/remote-control', desc: 'claude.ai 원격 제어' },
  { cmd: '/desktop', desc: '데스크탑 앱에서 계속' },
  { cmd: '/autofix-pr', desc: 'PR CI 실패 자동 수정' },
  { cmd: '/workflows', desc: '워크플로우 진행 보기' },
  // 권한·디렉터리
  { cmd: '/permissions', desc: '도구 권한 규칙 관리' },
  { cmd: '/cd', desc: 'working directory 이동' },
  { cmd: '/add-dir', desc: 'working directory 추가' },
  // 데이터·문서
  { cmd: '/copy', desc: '마지막 응답 복사' },
  { cmd: '/export', desc: '대화 내보내기' },
  { cmd: '/feedback', desc: '버그 리포트·피드백' },
  // MCP·통합
  { cmd: '/mcp', desc: 'MCP 서버 관리·인증' },
  { cmd: '/chrome', desc: 'Chrome의 Claude 설정' },
  { cmd: '/install-github-app', desc: 'GitHub Actions 앱 설정' },
  { cmd: '/hooks', desc: 'tool event 훅 설정' },
  // 정보·진단
  { cmd: '/status', desc: '버전·모델·계정 상태' },
  { cmd: '/usage', desc: '비용·plan 사용량·통계' },
  { cmd: '/cost', desc: '비용·사용량(=/usage)' },
  { cmd: '/doctor', desc: '설치·설정 진단' },
  { cmd: '/debug', desc: '디버그 로깅·문제 분석' },
  { cmd: '/insights', desc: '세션 분석 리포트' },
  { cmd: '/changelog', desc: '변경사항 버전 선택' },
  // 프로젝트·초기화
  { cmd: '/init', desc: 'CLAUDE.md 프로젝트 초기화' },
  { cmd: '/skills', desc: '사용 가능 skills 목록' },
  { cmd: '/plugin', desc: 'plugins 관리' },
  { cmd: '/ide', desc: 'IDE 통합 관리' },
  { cmd: '/terminal-setup', desc: '터미널 keybinding 설정' },
  { cmd: '/statusline', desc: 'status line 설정' },
  // 계정·플랫폼
  { cmd: '/login', desc: 'Anthropic 로그인' },
  { cmd: '/logout', desc: 'Anthropic 로그아웃' },
  { cmd: '/upgrade', desc: 'plan 업그레이드' },
  { cmd: '/privacy-settings', desc: '개인정보 설정' },
  { cmd: '/mobile', desc: '모바일 앱 QR' },
];

// /context 터미널 화면에서 의미있는 라인만 추출(거노: GUI 새로 만들지 말고 터미널에 보이는
// 거 잘 정리해서). 실측 포맷: "⛁ Custom agents: 602 tokens (0.1%)"·"⛶ Free space: 835.2k
// (83.5%)" 분해 라인 + "MCP tools · /mcp" / "└ 227 tools · 97.4k tokens" 상세. 색블록 그리드·
// 트리 글리프·프롬프트는 걷어내고, 토큰/퍼센트/"· /명령"/free space 라인만. 헤더가 peek 위에서
// 잘려도 동작하게 헤더 비의존. 중복(그리드 옆 라벨 반복) 제거.
function extractContext(screen: string): string | null {
  const lines = screen.split('\n');
  const strip = (s: string) => s.replace(/\x1b\[[0-9;]*m/g, ''); // ANSI 제거(판정·trim 용)
  const out: string[] = [];
  let started = false;
  for (const raw of lines) {
    // 박스·트리·프롬프트 글리프만 제거 — ⛁⛶ 동전 그리드·ANSI 색은 유지(AnsiText 가 색 렌더,
    // 거노: 동전 색까지 똑같이). 정렬 공백 보존(trailing 만 정리, pre 표시).
    // claude 트리 글리프 ⎿⎾⏋⏌(U+23BE~) 도 제거 — "⎿ Context Usage" 의 ⎿ 때문에 헤더 매칭이
    // 실패해 ctxView 가 안 떴다(거노: /context 안 떠). 박스·트리·프롬프트 글리프 일괄.
    const t = raw.replace(/[│┃╭╮╰╯─━┌┐└┘├┤┬┴┼❯●◐◓◑◒⎿⎾⏋⏌⎽⎼]/g, '').replace(/\s+$/, '');
    const plain = strip(t);
    // "Context Usage" 가 줄 시작일 때만 시작 — 슬래시 자동완성 설명 "Visualize current context
    // usage"(문장 중간)를 오캡처하던 것 차단(거노: /context 이상해).
    if (/^\s*context usage\b/i.test(plain)) started = true;
    if (!started) continue;
    if (/esc to|jump to|bypass permissions|\/rc active|setup issue|\/doctor|^\s*\d+ tokens\s*$/i.test(plain)) continue;
    out.push(t);
  }
  while (out.length && !strip(out[0]).trim()) out.shift();
  while (out.length && !strip(out[out.length - 1]).trim()) out.pop();
  const cleaned = out.filter((l, i) => strip(l).trim() !== '' || (i > 0 && strip(out[i - 1]).trim() !== ''));
  return cleaned.length >= 2 ? cleaned.join('\n') : null;
}

// 슬래시 명령 결과(## Context Usage 등)·시스템 주입([Request interrupted], <command-*>,
// Caveat, local-command)은 claude 가 user 메시지로 넣어 대화창에 선생님 발신(노란 버블)로
// 샌다(거노: 내가 안 보낸 스크립트가 뜸). 진짜 사용자 발화가 아니므로 숨긴다.
function isSystemInjection(t: Turn): boolean {
  if (t.role !== 'user') return false;
  return /\[Request interrupted|<command-(name|args|message)>|<local-command|^\s*##\s*Context Usage|^\s*Caveat:\s/i.test(t.text);
}

// ANSI 색 — /context peek_ansi 의 SGR(\x1b[…m)을 색 span 으로(거노: 동전 색까지 똑같이).
const ANSI16 = ['#3b4252', '#bf616a', '#a3be8c', '#ebcb8b', '#5e81ac', '#b48ead', '#88c0d0', '#e5e9f0', '#4c566a', '#d08770', '#a3be8c', '#ebcb8b', '#81a1c1', '#b48ead', '#8fbcbb', '#eceff4'];
function ansi256(n: number): string {
  if (n < 16) return ANSI16[n] ?? '#ccc';
  if (n < 232) { const i = n - 16, r = Math.floor(i / 36), g = Math.floor((i % 36) / 6), b = i % 6; const v = (c: number) => (c ? 55 + c * 40 : 0); return `rgb(${v(r)},${v(g)},${v(b)})`; }
  const v = 8 + (n - 232) * 10; return `rgb(${v},${v},${v})`;
}
function AnsiText({ text }: { text: string }) {
  const nodes: React.ReactNode[] = [];
  let color: string | undefined;
  let key = 0;
  const re = /\x1b\[([0-9;]*)m/g;
  let last = 0;
  let m: RegExpExecArray | null;
  const seg = (t: string, c?: string) => { if (t) nodes.push(<span key={key++} style={c ? { color: c } : undefined}>{t}</span>); };
  while ((m = re.exec(text))) {
    seg(text.slice(last, m.index), color);
    const codes = m[1].split(';').filter((s) => s !== '').map(Number);
    for (let i = 0; i < codes.length; i++) {
      const c = codes[i];
      if (c === 0 || c === 39) color = undefined;
      else if (c >= 30 && c <= 37) color = ANSI16[c - 30];
      else if (c >= 90 && c <= 97) color = ANSI16[c - 90 + 8];
      else if (c === 38) {
        if (codes[i + 1] === 2) { color = `rgb(${codes[i + 2]},${codes[i + 3]},${codes[i + 4]})`; i += 4; }
        else if (codes[i + 1] === 5) { color = ansi256(codes[i + 2]); i += 2; }
      }
    }
    last = re.lastIndex;
  }
  seg(text.slice(last), color);
  return <>{nodes}</>;
}

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
// claude 인터랙티브 선택 메뉴(/model 등 API 안 타는 것)를 화면에서 추출 → 선택지 카드.
// AskUserQuestion 은 캡처 프록시 tool_use 로 정확히 잡으니(거노: 추정 금지) 이 화면 파싱은
// 그 외(/model·권한 프롬프트) 폴백 전용. **❯ 커서가 실제로 찍힌 줄이 있어야만** 메뉴로
// 인정한다 — 일반 출력의 "1. 2. 3." 번호목록·todo·코드엔 ❯ 가 없어 false-positive(거노:
// askquestion 아닌데 선택지 뜸)를 막는다. '>' 는 셸 프롬프트·인용에 흔해 커서에서 뺀다.
function parsePromptMenu(screen: string): { title: string; options: { idx: number; label: string; cur: boolean }[] } | null {
  const lines = screen.split('\n');
  const opts: { idx: number; label: string; cur: boolean; line: number }[] = [];
  for (let i = 0; i < lines.length; i++) {
    const m = lines[i].match(/^\s*([❯●]?)\s*(\d+)\.\s+(.+?)\s*$/);
    if (m) {
      opts.push({ idx: parseInt(m[2], 10), label: m[3].replace(/\s{2,}.*$/, '').trim(), cur: /[❯●]/.test(m[1] ?? ''), line: i });
    }
  }
  if (opts.length < 2) return null;
  // ❯ 커서가 어느 옵션에도 없으면 claude 인터랙티브 메뉴가 아니다(그냥 번호 나열). reject.
  if (!opts.some((o) => o.cur)) return null;
  // 옵션 줄들이 흩어져 있으면(중간에 멀리 떨어진 숫자행) 메뉴가 아니다 — 연속 블록만.
  if (opts[opts.length - 1].line - opts[0].line > opts.length + 2) return null;
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

// claude 라이브 작업 표시(footer)를 화면에서 추출 — board status 는 idle/working 2값뿐이라
// "무슨 작업·몇 초"를 모른다(거노: "✻ Twisting… (23s)" 같은 진행 표시도 GUI 에). **진행형만**
// 잡는다: "✻ Churned for 42s" 같은 완료형은 작업이 *끝난* 표시라 로딩 점으로 띄우면 "다 됐는데
// 생각 중처럼 보인다"(거노). 별기호+verb…+(경과초) 신호로만 판정해 한글 verb 도 커버.
function parseSpinner(screen: string): string | null {
  const lines = screen.split('\n');
  for (let i = lines.length - 1; i >= 0 && i >= lines.length - 14; i--) {
    const t = lines[i].trim();
    // 진행형: 별 + verb… + (경과시간). 완료형("for Ns")은 의도적으로 무시.
    const m = t.match(/[✠-❏][^\S\n]*(.+?…)\s*\(((?:\d+m\s*)?\d+s)\b/);
    if (m) return `${m[1]} · ${m[2]}`;
  }
  return null;
}

// effort level — claude 상태바 "high · /effort" 파싱(거노: effort 도 모델 옆에). 없으면 null.
function parseEffort(screen: string): string | null {
  const m = screen.match(/\b(low|medium|high|xhigh|max)\b\s*·\s*\/effort/i);
  return m ? m[1].toLowerCase() : null;
}

// /effort 슬라이더 옵션 — ←/→ 6단계. ultracode 는 max 우측(xhigh+workflows).
const EFFORT_OPTS = ['low', 'medium', 'high', 'xhigh', 'max', 'ultracode'];

// /effort 슬라이더 감지 + 현재 위치 — "Effort"+"Faster"/"Smarter" 화면(numbered 아닌 슬라이더).
// ▲ 마커 컬럼을 옵션 라인 컬럼과 매칭해 현재 인덱스(0-based)를 구한다. 아니면 null.
function parseEffortMenu(screen: string): number | null {
  if (!/\bEffort\b/.test(screen) || !/Faster/.test(screen) || !/Smarter/.test(screen)) return null;
  const lines = screen.split('\n');
  const optLine = lines.find((l) => /\blow\b/.test(l) && /\bmedium\b/.test(l) && /\bhigh\b/.test(l));
  if (!optLine) return null;
  const arrowLine = lines.find((l) => l.includes('▲'));
  let current = 2; // 슬라이더 기본 high
  if (arrowLine) {
    const col = arrowLine.indexOf('▲');
    let best = 2, bestDist = Infinity;
    ['low', 'medium', 'high', 'xhigh', 'max'].forEach((o, i) => {
      const idx = optLine.indexOf(o);
      if (idx < 0) return;
      const d = Math.abs(idx + o.length / 2 - col);
      if (d < bestDist) { bestDist = d; best = i; }
    });
    current = best;
  }
  // ultracode 표시줄이 강조(현재)면 5.
  if (/ultracode/i.test(screen) && /xhigh\s*\+\s*workflows/i.test(screen)) {
    // ultracode 선택 여부는 ▲ 가 max 우측을 넘어갔는지로 — 별도 신호 없으면 슬라이더 값 유지.
  }
  return current;
}

// 화면(raw 터미널)은 '터미널 보기'로 보면 되므로 여기엔 두지 않는다.
export function TerminalPeekPanel({ surfaceId, title, onClose, embedded }: TerminalPeekPanelProps) {
  const agent = useStore((s) => s.agents.find((a) => a.id === surfaceId));
  const [turns, setTurns] = useState<Turn[]>([]);
  const [menu, setMenu] = useState<{ title: string; options: { idx: number; label: string; cur: boolean; description?: string }[]; multi?: boolean } | null>(null);
  const [checked, setChecked] = useState<Set<number>>(new Set()); // multiSelect 체크된 인덱스
  const [images, setImages] = useState<string[]>([]); // SendUserFile 로 보낸 이미지
  const [loaded, setLoaded] = useState(false); // 첫 폴 완료 — 빈 상태 문구 분기
  const [spinner, setSpinner] = useState<string | null>(null); // claude 라이브 작업 표시(verb·경과초)
  const [effort, setEffort] = useState<string | null>(null); // effort level(상태바 파싱) — 모델 옆 칩
  const [effortMenu, setEffortMenu] = useState<number | null>(null); // /effort 슬라이더 현재 idx(뜬 동안)
  const [input, setInput] = useState('');
  const [sending, setSending] = useState(false);
  const [flash, setFlash] = useState<'ok' | 'err' | null>(null);
  // window.confirm 은 wry webview(macOS)에서 무반응 — 자체 확인 모달로 종료·compact 확인(거노).
  const [confirm, setConfirm] = useState<{ msg: string; sub?: string; danger?: boolean; yes: string; onYes: () => void } | null>(null);
  // 슬래시 자동완성 — 입력이 '/' 로 시작하면 claude-code 명령 드롭다운(↑↓ 선택·Tab/Enter 완성).
  const [slashIdx, setSlashIdx] = useState(0);
  const [navIdx, setNavIdx] = useState(0); // 선택지 카드 키보드 네비(↑↓) 하이라이트
  // 동적 슬래시(스킬·커스텀·플러그인) — 디스크 스캔. 정적 내장 목록과 병합(거노: 스킬 다).
  const [dynamicSlash, setDynamicSlash] = useState<{ cmd: string; desc: string }[]>([]);
  useEffect(() => { void fetchSlashCommands().then(setDynamicSlash); }, []);
  // /context 출력 정리본 — GUI 모달 새로 만들지 말고(거노) 터미널 /context 화면(peek)을
  // 정리해 대화창 안에 보여준다. null=비활성, 그 외=정리된 그리드 텍스트.
  const [ctxView, setCtxView] = useState<string | null>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const atBottomRef = useRef(true); // 사용자가 위로 스크롤했으면 자동 하단고정 멈춤
  // ESC/Enter 로 메뉴를 닫은 직후 ~700ms 는 화면 재파싱으로 카드를 다시 띄우지 않는다
  // (거노: GUI 에서 esc 눌러도 안 멈춤 — tick 이 stale 화면에서 메뉴를 재감지해 재등장).
  const menuSuppressRef = useRef(0);

  // 대화 내역: transcript jsonl 우선(깨끗), 비었으면 PTY 화면(peek) 폴백 — 인터랙티브
  // claude 가 jsonl 을 라이브로 안 써 진행 중엔 transcript 가 빈다(claude-code-guide
  // 확인). jsonl 이 flush 되면 자동으로 transcript 우선. 학생 바뀌면 초기화.
  useEffect(() => {
    let stopped = false;
    setLoaded(false);
    setTurns([]);
    setMenu(null);
    setSpinner(null);
    setImages([]);
    setInput('');
    const tick = async () => {
      const [conv, ts, imgs, screen] = await Promise.all([
        fetchConversation(surfaceId),
        fetchTranscript(surfaceId, 30),
        fetchSentImages(surfaceId, 12),
        fetchPeek(surfaceId, 60),
      ]);
      if (stopped) return;
      // 캡처 프록시(깨끗·라이브, ccglass 방식) 우선, 안 탄 pane 만 transcript jsonl 폴백.
      // 슬래시 결과·시스템 주입 user turn 은 선생님 발신으로 새므로 제거(거노).
      let next: Turn[] = (conv.turns.length ? conv.turns : ts).filter((t) => !isSystemInjection(t));
      // 진행 중 응답 — 프록시가 SSE 로 라이브 캡처한 어시스턴트 텍스트를 마지막 버블로.
      if (conv.streaming.trim()) next = [...next, { role: 'assistant', text: conv.streaming }];
      setTurns(next);
      // 인터랙티브 선택지 — AskUserQuestion 은 캡처 프록시 tool_use 로 질문/선택지가 정확히
      // 잡힌다(거노: peek 추정 금지). 그게 있으면 그걸 쓰고, 없을 때만 화면 메뉴(/model 등
      // API 안 타는 것)를 peek 폴백으로 파싱한다.
      const aq = conv.tool_uses?.find((t) => t.name === 'AskUserQuestion' && t.input.questions?.length);
      // ESC/Enter 직후 suppress 창 동안은 메뉴 재감지 보류(닫은 카드가 stale 화면으로 재등장 방지).
      const suppressed = Date.now() < menuSuppressRef.current;
      if (aq) {
        const q = aq.input.questions![0];
        setMenu({
          title: q.question,
          options: q.options.map((o, i) => ({ idx: i + 1, label: o.label, cur: false, description: o.description })),
          multi: !!q.multiSelect,
        });
      } else if (!suppressed) {
        setMenu(parsePromptMenu(screen));
      }
      // claude 라이브 작업 표시(verb·경과초)도 화면에만 — 로딩 인디케이터에 실값으로.
      setSpinner(parseSpinner(screen));
      setEffort(parseEffort(screen));
      if (!suppressed) setEffortMenu(parseEffortMenu(screen)); // /effort 슬라이더 뜨면 GUI 카드로
      // /context 출력이 화면에 보이면 자동으로 컨텍스트 패널 활성(거노: 어디서 /context 보내든
      // 떠야 함 — 모모톡/학생별/터미널 무관). null 일 때만 켜고, 색 갱신은 peek_ansi 폴링이 담당.
      const ctxAuto = extractContext(screen);
      if (ctxAuto) setCtxView((prev) => prev ?? ctxAuto);
      setImages(imgs); // 훅 기록(진짜 SendUserFile)만 — 화면 파싱 폐기
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

  // 새 질문(title 변경) 시 체크·네비 초기화(메뉴 열릴 때 터미널 커서는 ❯1=navIdx 0).
  useEffect(() => { setChecked(new Set()); setNavIdx(0); }, [menu?.title]);

  // /context 활성 시 peek_ansi(색 포함) 폴링 → 동전 색까지 대화창에(거노: peek 로 색까지).
  const ctxOpen = ctxView != null;
  useEffect(() => {
    if (!ctxOpen) return;
    let stop = false;
    const poll = async () => {
      // peek_ansi(색) 우선, 헤더 못 잡으면 일반 peek 폴백 — 색은 없어도 "불러오는 중" 멈춤 방지(거노).
      let ctx = extractContext(await fetchPeek(surfaceId, 80, true));
      if (!ctx) ctx = extractContext(await fetchPeek(surfaceId, 80, false));
      if (!stop && ctx) setCtxView(ctx);
    };
    void poll();
    const iv = setInterval(poll, 1500);
    return () => { stop = true; clearInterval(iv); };
  }, [ctxOpen, surfaceId]);

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
    setSlashIdx(0);
    void sendToPane(surfaceId, '\x15' + next, false);
  };

  // 제출: 미러로 이미 친 라인을 submit_payload(\x15 클리어 + 괄호붙여넣기 + CR)로 한
  // 번에 보낸다(submit=true). 서버가 이때만 messages.jsonl 에 깨끗한 텍스트로 1회 영속
  // — 미러 partial(submit=false)은 기록 안 돼 모모톡에 한 자씩 쌓이던 버그가 사라진다.
  // 빈 줄(프롬프트 확인용 Enter)은 영속 없이 bare CR.
  const submit = async () => {
    if (sending) return;
    const text = input;
    // GUI 가 네이티브로 처리하는 인터랙티브 명령 — 터미널에 보내면 그리드가 대화창에 안
    // 잡히고 다음 프롬프트에서 깨져 보였다(거노). 미러 라인(\x15)을 비우고 GUI 패널로.
    if (text.trim() === '/context') {
      setInput('');
      void sendToPane(surfaceId, '/context', true, false); // 터미널에 /context → peek 정리해 대화창에
      setCtxView('컨텍스트 불러오는 중…');
      return;
    }
    setSending(true);
    const ok = text
      ? await sendToPane(surfaceId, text, true, false) // 학생별 대화 — 모모톡에 안 남김
      : await sendToPane(surfaceId, '\r', false);
    setSending(false);
    setFlash(ok ? 'ok' : 'err');
    setTimeout(() => setFlash(null), 1200);
    if (ok) setInput('');
    inputRef.current?.focus();
  };

  const onKey = (e: React.KeyboardEvent<HTMLInputElement>) => {
    // 슬래시 드롭다운 열림 — ↑↓ 후보 이동, Tab/Enter 완성, Esc 닫기(우선 처리).
    if (slashOpen) {
      if (e.key === 'ArrowDown') { e.preventDefault(); setSlashIdx((i) => Math.min(slashMatches.length - 1, i + 1)); return; }
      if (e.key === 'ArrowUp') { e.preventDefault(); setSlashIdx((i) => Math.max(0, i - 1)); return; }
      if (e.key === 'Tab' || e.key === 'Enter') {
        e.preventDefault();
        const pick = slashMatches[Math.min(slashIdx, slashMatches.length - 1)];
        if (pick) mirror(pick.cmd + ' '); // 완성 후 인자 입력 or Enter 한 번 더로 제출
        return;
      }
      if (e.key === 'Escape') { e.preventDefault(); setInput(''); void sendToPane(surfaceId, '\x15', false); return; }
    }
    if (e.key === 'Enter') { e.preventDefault(); void submit(); }
    if (e.key === 'c' && e.ctrlKey) { e.preventDefault(); setInput(''); void sendToPane(surfaceId, '\x03', false); }
  };

  // 인터랙티브 메뉴 선택 → 숫자 단축키 전송(claude 가 즉시 선택). 다음 폴에서 갱신.
  const pickMenu = async (oi: number) => {
    setMenu(null);
    // 단일 선택 — navIdx(=터미널 커서)에서 oi(0-based)로 ↑↓ 이동 후 Enter. peek 안 쓰고
    // GUI 커서와 터미널 커서를 같이 움직여 일치(거노: 완전 연동).
    const delta = oi - navIdx;
    const move = delta > 0 ? '\x1b[B'.repeat(delta) : '\x1b[A'.repeat(-delta);
    await sendToPane(surfaceId, move + '\r', false);
  };

  // multiSelect 체크 토글 — peek 안 쓰고(거노) GUI navIdx 를 터미널 커서와 동일하게 유지:
  // 클릭한 항목(oi, 0-based)으로 navIdx 만큼 이동 + Space. GUI 와 터미널이 항상 같이 움직여 일치.
  const toggleCheck = (oi: number) => {
    const delta = oi - navIdx;
    const move = delta > 0 ? '\x1b[B'.repeat(delta) : '\x1b[A'.repeat(-delta);
    setNavIdx(oi);
    void sendToPane(surfaceId, move + ' ', false); // 이동 + Space(토글)
    setChecked((s) => { const n = new Set(s); if (n.has(oi)) n.delete(oi); else n.add(oi); return n; });
  };

  // multiSelect 제출 — 체크는 toggleCheck 가 이미 터미널에 미러함. Submit 영역으로 Tab + Enter.
  const submitMulti = async () => {
    setMenu(null);
    setChecked(new Set());
    await sendToPane(surfaceId, '\t\r', false);
  };

  // /effort 슬라이더 선택 — 현재(effortMenu)에서 target 으로 ←/→ 이동 후 Enter(거노: effort 연동).
  const pickEffort = (target: number) => {
    const cur = effortMenu ?? 2;
    const delta = target - cur;
    const move = delta > 0 ? '\x1b[C'.repeat(delta) : '\x1b[D'.repeat(-delta);
    setEffortMenu(null);
    void sendToPane(surfaceId, move + '\r', false);
  };

  // 선택지 카드 키보드 조작(거노: 방향키·엔터·esc 안 됨) — ↑↓ 네비, Space 다중 토글,
  // Enter 선택/제출, Esc 취소. 마우스 클릭과 병행. 카드 뜬 동안만 활성.
  useEffect(() => {
    if (!menu) return;
    const onKey = (e: KeyboardEvent) => {
      const opts = menu.options;
      // ↑↓: GUI navIdx 와 터미널 커서를 동시에 이동. 끝(0/max)에 닿으면 터미널 키도 안 보낸다
      // — GUI 는 clamp 인데 터미널만 계속 내려가 둘이 어긋나던 것(거노: 꾹 누르면 다른 거 선택).
      if (e.key === 'ArrowDown') { e.preventDefault(); if (navIdx < opts.length - 1) { setNavIdx(navIdx + 1); void sendToPane(surfaceId, '\x1b[B', false); } }
      else if (e.key === 'ArrowUp') { e.preventDefault(); if (navIdx > 0) { setNavIdx(navIdx - 1); void sendToPane(surfaceId, '\x1b[A', false); } }
      else if (e.key === ' ' && menu.multi) { e.preventDefault(); void sendToPane(surfaceId, ' ', false); setChecked((s) => { const n = new Set(s); if (n.has(navIdx)) n.delete(navIdx); else n.add(navIdx); return n; }); }
      else if (e.key === 'Enter') {
        e.preventDefault();
        menuSuppressRef.current = Date.now() + 700;
        if (menu.multi) void submitMulti();
        else void pickMenu(navIdx);
      } else if (e.key === 'Escape') {
        e.preventDefault();
        menuSuppressRef.current = Date.now() + 700; // 닫은 뒤 재감지 보류
        setMenu(null); setChecked(new Set());
        void sendToPane(surfaceId, '\x1b', false); // 터미널 메뉴도 취소
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [menu, navIdx, checked, surfaceId]);

  // /effort 슬라이더 키보드 — ←/→ 이동(터미널 동시), Enter 확정, Esc 취소(거노: 방향키 연동).
  useEffect(() => {
    if (effortMenu == null) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'ArrowRight') { e.preventDefault(); setEffortMenu((i) => Math.min(EFFORT_OPTS.length - 1, (i ?? 2) + 1)); void sendToPane(surfaceId, '\x1b[C', false); }
      else if (e.key === 'ArrowLeft') { e.preventDefault(); setEffortMenu((i) => Math.max(0, (i ?? 2) - 1)); void sendToPane(surfaceId, '\x1b[D', false); }
      else if (e.key === 'Enter') { e.preventDefault(); menuSuppressRef.current = Date.now() + 700; setEffortMenu(null); void sendToPane(surfaceId, '\r', false); }
      else if (e.key === 'Escape') { e.preventDefault(); menuSuppressRef.current = Date.now() + 700; setEffortMenu(null); void sendToPane(surfaceId, '\x1b', false); }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [effortMenu, surfaceId]);

  // 학생 종료 — pane kill(close_surface). window.confirm 은 wry webview 에서 무반응이라
  // (거노: ba gui 종료버튼 눌러도 반응없음) 자체 확인 모달로.
  const onKill = () => {
    setConfirm({
      msg: `${title} 학생을 종료할까요?`,
      sub: 'pane 이 닫히고 되돌릴 수 없어요.',
      danger: true, yes: '종료',
      onYes: async () => { await closeAgent(surfaceId); onClose(); },
    });
  };

  // 슬래시 자동완성 후보 — 입력이 '/' 로 시작하고 아직 인자(공백) 전이면 매칭. 첫 후보로 reset.
  const slashQuery = input.startsWith('/') && !input.includes(' ') ? input.toLowerCase() : null;
  const allSlash = [...SLASH_COMMANDS, ...dynamicSlash.filter((d) => !SLASH_COMMANDS.some((s) => s.cmd === d.cmd))];
  const slashMatches = slashQuery ? allSlash.filter((c) => c.cmd.toLowerCase().startsWith(slashQuery)) : [];
  const slashOpen = slashMatches.length > 0 && slashQuery !== null && slashQuery !== slashMatches[0]?.cmd;

  return (
    <div style={{
      width: embedded ? '100%' : 340, flex: embedded ? 1 : undefined,
      flexShrink: 0, height: '100%', position: 'relative', // 확인 모달 기준
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
          {agent.model && <MetaChip label={shortModel(agent.model)} onClick={() => void sendToPane(surfaceId, '/model', true, false)} />}
          {/* effort — 모델 옆(거노). 항상 표시(상태바 값 있으면 같이), 클릭 → /effort 메뉴
              카드(방향키 선택). 입력창에 "/effort max" 타이핑(슬래시 자동완성)도 가능. */}
          {agent.model && <MetaChip label={effort ? `effort: ${effort}` : 'effort'} onClick={() => void sendToPane(surfaceId, '/effort', true, false)} />}
          {/* 브랜치 칩 클릭 → 미커밋 변경사항(/diff) 확인(거노: 브랜치 버튼). */}
          {agent.branch && <MetaChip label={`⎇ ${agent.branch}`} onClick={() => setConfirm({
            msg: '변경사항을 볼까요?',
            sub: `${agent.branch} 브랜치의 미커밋 변경(/diff)을 학생에게 띄워요.`,
            yes: '변경 보기',
            onYes: () => { void sendToPane(surfaceId, '/diff', true, false); },
          })} />}
          {/* 컨텍스트 % 칩 클릭 → /compact 할지 확인(거노: 컨텍스트 버튼 누르면 컴팩트 물어보는 UX).
              상태바 파싱(contextPct) 우선, 없으면 토큰/한도 계산 폴백. */}
          {(() => {
            const pct = agent.contextPct != null && agent.contextPct > 0 ? agent.contextPct
              : agent.contextTokens != null && agent.contextLimit ? Math.round((agent.contextTokens / agent.contextLimit) * 100)
              : null;
            if (pct == null) return null;
            return <MetaChip label={`컨텍스트 ${pct}%`} onClick={() => { void sendToPane(surfaceId, '/context', true, false); setCtxView('컨텍스트 불러오는 중…'); }} />;
          })()}
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
                    background: mine ? '#FEE500' : 'var(--cth-cream-50)',
                    color: mine ? '#3A2E00' : 'var(--cth-ink-900)',
                    border: mine ? 'none' : '1px solid var(--cth-cream-200)',
                    boxShadow: '0 1px 3px rgba(21, 41, 74, 0.08)',
                    fontFamily: 'var(--cth-font-ui)', fontSize: 13, lineHeight: 1.55,
                    wordBreak: 'break-word'
                  }}>
                    {t.text && <Markdown text={t.text} />}
                    {t.images?.map((p, j) => (
                      <button key={j} onClick={() => void openFile(p)} title="클릭 = 원본 보기" style={{ display: 'block', padding: 0, border: 'none', background: 'none', cursor: 'pointer', marginTop: t.text ? 6 : 0 }}>
                        <img src={imageFileUrl(p)} alt="" style={{ maxWidth: '100%', maxHeight: 240, borderRadius: 8, display: 'block' }} />
                      </button>
                    ))}
                  </div>
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
                  background: 'var(--cth-cream-50)', border: '1px solid var(--cth-cream-200)',
                  boxShadow: '0 1px 3px rgba(21, 41, 74, 0.08)', display: 'block',
                }}>
                  <img src={imageFileUrl(path)} alt={path.split('/').pop() ?? ''} style={{
                    display: 'block', maxWidth: '100%', maxHeight: 240, borderRadius: 10, objectFit: 'contain',
                  }} />
                </button>
              </div>
            ))}

            {/* /context 결과 — 별도 패널(x 안 닫히던) 대신 채팅 버블로(거노: 채팅창 안에 입력되게).
                선생님 "/context" 친 기록(우측) + 아로나 컨텍스트 결과(좌측, 색 그리드). */}
            {ctxView && (
              <>
                <div style={{ display: 'flex', justifyContent: 'flex-end', marginBottom: 10 }}>
                  <div style={{ maxWidth: '72%', padding: '8px 12px', borderRadius: 14, borderTopRightRadius: 4, background: '#FEE500', color: '#3A2E00', fontFamily: 'var(--cth-font-ui)', fontSize: 13, fontWeight: 600 }}>/context</div>
                </div>
                <div style={{ display: 'flex', justifyContent: 'flex-start', marginBottom: 10, gap: 8, alignItems: 'flex-end' }}>
                  <div style={{ width: 34, height: 34, borderRadius: 11, overflow: 'hidden', flexShrink: 0, background: 'var(--cth-cream-100)', border: '1px solid var(--cth-cream-200)', display: 'flex', alignItems: 'flex-end', justifyContent: 'center' }}>
                    <SpritePortrait character={title} scale={1.5} />
                  </div>
                  <div style={{ maxWidth: '85%', padding: '8px 12px', borderRadius: 14, borderTopLeftRadius: 4, background: 'var(--cth-cream-50)', border: '1px solid var(--cth-cream-200)', boxShadow: '0 1px 3px rgba(21,41,74,0.08)', overflowX: 'auto' }}>
                    <pre style={{ fontFamily: 'var(--cth-font-mono)', fontSize: 10, lineHeight: 1.35, whiteSpace: 'pre', margin: 0, color: 'var(--cth-ink-700)' }}><AnsiText text={ctxView} /></pre>
                  </div>
                </div>
              </>
            )}

            {/* 로딩 인디케이터 — 학생이 working/thinking 이면 타이핑 점(거노: 채팅창에서
                로딩중인지 모름). transcript 는 턴 완료 시 갱신이라 그 사이 공백을 메운다.
                단 마지막 버블이 이미 학생 답변(완료·streaming)이면 숨긴다 — claude 가 답변
                직후 State Classifier nudge 를 도는 동안 status 가 잠깐 thinking 으로 남아
                "생각 중" 이 답변 아래 계속 떴다(거노). 마지막이 선생님 발화일 때만 표시. */}
            {(spinner || ((agent?.status === 'working' || agent?.status === 'thinking') &&
              turns.length > 0 && turns[turns.length - 1]?.role !== 'assistant')) && (
              <div style={{ display: 'flex', justifyContent: 'flex-start', marginBottom: 10, gap: 8, alignItems: 'flex-end' }}>
                <div style={{ width: 34, height: 34, borderRadius: 11, overflow: 'hidden', flexShrink: 0, background: 'var(--cth-cream-100)', border: '1px solid var(--cth-cream-200)', display: 'flex', alignItems: 'flex-end', justifyContent: 'center' }}>
                  <SpritePortrait character={title} scale={1.5} />
                </div>
                <div style={{
                  padding: '10px 14px', borderRadius: 14, borderTopLeftRadius: 4,
                  background: 'var(--cth-cream-50)', border: '1px solid var(--cth-cream-200)',
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
                  {spinner || (agent?.status === 'thinking' ? '생각 중…' : (agent?.currentTool || '작업 중…'))}
                </div>
              </div>
            )}
          </>
        )}
      </div>

      {/* 인터랙티브 메뉴(/model·AskUserQuestion) — 선택지 카드. 단일=클릭 즉시 선택,
          multiSelect=체크박스 여러개 토글 후 제출(거노: 중복선택 GUI). */}
      {menu && (
        <div style={{ padding: '10px 14px', borderTop: '1px solid var(--cth-cream-200)', background: 'var(--cth-sky-light)', maxHeight: 260, overflowY: 'auto' }}>
          {menu.title && <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 700, color: 'var(--cth-ink-900)', marginBottom: menu.multi ? 4 : 8 }}>{menu.title}</div>}
          {menu.multi && <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-500)', marginBottom: 8 }}>여러 개 선택 가능 — 체크하고 제출</div>}
          <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
            {menu.options.map((o, oi) => {
              const on = menu.multi ? checked.has(oi) : oi === navIdx;
              const isNav = oi === navIdx;
              return (
                <button key={o.idx} onClick={() => (menu.multi ? toggleCheck(oi) : void pickMenu(oi))} style={{
                  textAlign: 'left', padding: '8px 12px', borderRadius: 9, cursor: 'pointer',
                  border: on ? '2px solid var(--cth-sky)' : isNav ? '2px solid var(--cth-sky-light)' : '1px solid var(--cth-cream-200)',
                  background: isNav ? 'var(--cth-paper-100)' : 'var(--cth-cream-50)', fontFamily: 'var(--cth-font-ui)', fontSize: 13, color: 'var(--cth-ink-900)',
                  display: 'flex', alignItems: 'flex-start', gap: 8,
                }}>
                  {menu.multi ? (
                    <span style={{
                      width: 18, height: 18, borderRadius: 4, flexShrink: 0, marginTop: 1,
                      border: on ? 'none' : '1.5px solid var(--cth-ink-300)',
                      background: on ? 'var(--cth-sky)' : 'transparent',
                      display: 'flex', alignItems: 'center', justifyContent: 'center',
                    }}>
                      {on && <svg width="12" height="12" viewBox="0 0 16 16"><path d="M3 8.5l3 3 7-7" stroke="#fff" strokeWidth="2.4" fill="none" strokeLinecap="round" strokeLinejoin="round" /></svg>}
                    </span>
                  ) : (
                    <span style={{ fontWeight: 800, color: 'var(--cth-sky)', minWidth: 14 }}>{o.idx}</span>
                  )}
                  <span style={{ flex: 1 }}>
                    <span style={{ fontWeight: 600 }}>{o.label}</span>
                    {o.description && <span style={{ display: 'block', fontSize: 11, color: 'var(--cth-ink-300)', marginTop: 2, lineHeight: 1.4 }}>{o.description}</span>}
                  </span>
                  {!menu.multi && o.cur && <span style={{ fontSize: 10, color: 'var(--cth-sky)', fontWeight: 700 }}>현재</span>}
                </button>
              );
            })}
          </div>
          {menu.multi && (
            <button
              onClick={() => void submitMulti()}
              disabled={checked.size === 0}
              style={{
                marginTop: 9, width: '100%', padding: '9px', border: 'none', borderRadius: 9,
                cursor: checked.size === 0 ? 'not-allowed' : 'pointer',
                background: checked.size > 0 ? 'var(--cth-sky)' : 'var(--cth-cream-200)',
                color: checked.size > 0 ? '#fff' : 'var(--cth-ink-300)',
                fontFamily: 'var(--cth-font-ui)', fontSize: 13, fontWeight: 700,
              }}
            >제출{checked.size > 0 ? ` (${checked.size}개)` : ''}</button>
          )}
        </div>
      )}

      {/* /effort 슬라이더 — 터미널 슬라이더(←/→)를 GUI 카드로(거노: effort 연동). 현재 강조,
          클릭/←→ → 터미널 ←/→ + Enter. */}
      {effortMenu != null && (
        <div style={{ padding: '10px 14px', borderTop: '1px solid var(--cth-cream-200)', background: 'var(--cth-sky-light)' }}>
          <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 700, color: 'var(--cth-ink-900)', marginBottom: 8 }}>
            Effort <span style={{ fontWeight: 400, color: 'var(--cth-ink-500)', fontSize: 11 }}>· 클릭 또는 ←/→ 후 Enter</span>
          </div>
          <div style={{ display: 'flex', flexWrap: 'wrap', gap: 6 }}>
            {EFFORT_OPTS.map((o, i) => (
              <button key={o} onClick={() => pickEffort(i)} style={{
                padding: '6px 12px', borderRadius: 8, cursor: 'pointer',
                border: i === effortMenu ? '2px solid var(--cth-sky)' : '1px solid var(--cth-cream-200)',
                background: i === effortMenu ? 'var(--cth-sky)' : 'var(--cth-cream-50)',
                color: i === effortMenu ? '#fff' : 'var(--cth-ink-900)',
                fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: i === effortMenu ? 700 : 500,
              }}>{o}</button>
            ))}
          </div>
        </div>
      )}

      {/* 입력창 + 슬래시 자동완성 드롭다운 — '/' 치면 claude 명령 후보(거노). */}
      <div style={{ position: 'relative' }}>
      {slashOpen && (
        <div style={{
          position: 'absolute', left: 12, right: 12, bottom: '100%', marginBottom: 6,
          background: 'var(--cth-cream-50)', border: '1px solid var(--cth-cream-200)', borderRadius: 10,
          boxShadow: '0 4px 16px rgba(21,41,74,0.16)', overflow: 'hidden auto', maxHeight: 240, zIndex: 20,
        }}>
          {slashMatches.map((c, i) => (
            <button
              key={c.cmd}
              onMouseDown={(e) => { e.preventDefault(); mirror(c.cmd + ' '); inputRef.current?.focus(); }}
              onMouseEnter={() => setSlashIdx(i)}
              style={{
                display: 'flex', alignItems: 'baseline', gap: 8, width: '100%', textAlign: 'left',
                padding: '7px 12px', border: 'none', cursor: 'pointer',
                background: i === slashIdx ? 'var(--cth-sky-light)' : 'transparent',
              }}
            >
              <span style={{ fontFamily: 'var(--cth-font-mono)', fontSize: 12, fontWeight: 700, color: 'var(--cth-sky)', flexShrink: 0 }}>{c.cmd}</span>
              <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-500)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{c.desc}</span>
            </button>
          ))}
        </div>
      )}
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
            background: 'var(--cth-cream-50)', border: '1px solid var(--cth-cream-200)', borderRadius: 9,
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

      {/* 확인 모달 — window.confirm 대체(wry webview 무반응). 종료·compact·diff 공통. */}
      {confirm && (
        <div
          onClick={() => setConfirm(null)}
          style={{ position: 'absolute', inset: 0, zIndex: 50, display: 'flex', alignItems: 'center', justifyContent: 'center', background: 'rgba(21,41,74,0.32)', padding: 20 }}
        >
          <div onClick={(e) => e.stopPropagation()} style={{ background: 'var(--cth-cream-50)', borderRadius: 14, padding: '18px 20px', width: '100%', maxWidth: 300, boxShadow: '0 12px 32px rgba(21,41,74,0.32)' }}>
            <div style={{ fontFamily: 'var(--cth-font-display)', fontSize: 15, fontWeight: 700, color: 'var(--cth-ink-900)', marginBottom: confirm.sub ? 6 : 16 }}>{confirm.msg}</div>
            {confirm.sub && <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 12, color: 'var(--cth-ink-500)', lineHeight: 1.5, marginBottom: 16 }}>{confirm.sub}</div>}
            <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
              <button onClick={() => setConfirm(null)} style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 13, fontWeight: 600, padding: '7px 14px', border: '1px solid var(--cth-cream-200)', borderRadius: 9, cursor: 'pointer', background: 'var(--cth-cream-50)', color: 'var(--cth-ink-500)' }}>취소</button>
              <button onClick={() => { const c = confirm; setConfirm(null); c.onYes(); }} style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 13, fontWeight: 700, padding: '7px 14px', border: 'none', borderRadius: 9, cursor: 'pointer', background: confirm.danger ? 'var(--cth-coral)' : 'var(--cth-sky)', color: '#fff' }}>{confirm.yes}</button>
            </div>
          </div>
        </div>
      )}

    </div>
  );
}
