import { type CSSProperties, useEffect, useMemo, useRef, useState } from 'react';
import { fetchConversation, fetchTranscriptRaw, fetchSessionTranscriptRaw, fetchPeek, fetchSentImages, imageFileUrl, openFile, sendToPane, pasteToActiveTerminal, revealTerminal, closeAgent, fetchSlashCommands, pasteImageToPane, swapCharacter, type Turn } from '@/lib/mcp';
import { SpritePortrait } from './SpritePortrait';
import { CharacterPicker } from './CharacterPicker';
import { Markdown } from './Markdown';
import { ToolUseCard } from './tool-use-card';
import { ThinkingBlock } from './thinking-block';
import { buildToolMap, type ToolMap } from '@/lib/build-tool-map';
import type { SessionEvent } from '@/lib/types';
import { AnsiText } from './AnsiText';
import { useStore } from '@/store';

// 대화 본문 = transcript jsonl(/transcript-raw, raw SessionEvent[]). ccsv 파서·per-tool
// 렌더를 이식해 Bash/Edit/Read 도구 호출이 카톡 버블 사이에 카드로 인터리브된다(거노:
// 데스크탑 앱처럼 보기좋게). 캡처 프록시(/conversation)는 AskUserQuestion 선택지·라이브
// streaming·effort/model 표시의 보너스 소스로만 — 프록시 꺼지면 그 부분만 비활성(본문은
// jsonl 로 멀쩡). /model·권한 프롬프트는 화면(peek) 폴백.

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

// 시스템 주입 잔여([Request interrupted], Caveat, ## Context Usage)는 진짜 발화가 아니라
// 숨긴다. <command-*>·<local-command-stdout> 는 더는 통째로 숨기지 않고 parseSlashCommand 가
// 슬래시 카드/로컬출력 버블로 승격한다(아래 eventsToItems). 여기엔 그 외 케이스만 남긴다.
function isSystemInjectionText(role: string, text: string): boolean {
  if (role !== 'user') return false;
  return /\[Request interrupted|^\s*##\s*Context Usage|^\s*Caveat:\s/i.test(text);
}

// ccsv parseUserMessage 발췌 — user 텍스트의 <command-name>/<command-args>/<command-message>
// 또는 <local-command-stdout> 태그를 파싱. command 면 슬래시 명령 카드(우측, green),
// local-command 면 로컬 출력 버블(좌측). 태그 없으면 null(일반 텍스트로 처리).
type ParsedUserMessage =
  | { kind: 'command'; commandName: string; commandArgs?: string; commandMessage?: string }
  | { kind: 'local-command'; stdout: string };
function parseSlashCommand(content: string): ParsedUserMessage | null {
  const tag = (name: string) => {
    const m = content.match(new RegExp(`<${name}>([\\s\\S]*?)</${name}>`));
    return m ? m[1] : undefined;
  };
  const commandName = tag('command-name');
  if (commandName !== undefined) {
    return { kind: 'command', commandName: commandName.trim(), commandArgs: tag('command-args'), commandMessage: tag('command-message') };
  }
  const stdout = tag('local-command-stdout');
  if (stdout !== undefined) return { kind: 'local-command', stdout };
  return null;
}

// ccsv formatDuration — ms → "12.3s" / "1m 23s" / "1h 5m". 푸터엔 초만 쓰지만 분/시 케이스 보존.
function formatDuration(durationMs: number): string {
  if (durationMs < 0) return '0s';
  const totalSeconds = Math.floor(durationMs / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  if (hours === 0 && minutes === 0) return `${(durationMs / 1000).toFixed(1)}s`;
  if (hours === 0) return seconds > 0 ? `${minutes}m ${seconds}s` : `${minutes}m`;
  return minutes > 0 ? `${hours}h ${minutes}m` : `${hours}h`;
}

// ccsv ConversationList turnDurationMap 이식 — 각 턴(실유저 메시지 = sidechain 아닌 user 중
// content[0] 가 tool_result 가 아닌 것)에서 다음 실유저 직전까지 마지막 assistant 의 uuid →
// (해당 턴 시작 ts ~ 그 assistant ts) 소요 ms 를 매핑. 그 assistant 버블 아래 시계 푸터로 표시.
function turnDurations(events: SessionEvent[]): Map<string, number> {
  const map = new Map<string, number>();
  const isRealUser = (ev: SessionEvent): boolean => {
    if (ev.type !== 'user' || (ev as { isSidechain?: boolean }).isSidechain) return false;
    const content = (ev as { message?: { content?: unknown } }).message?.content;
    if (Array.isArray(content)) {
      const first = content[0] as { type?: string } | undefined;
      if (first && typeof first === 'object' && first.type === 'tool_result') return false;
    }
    return true;
  };
  const starts: number[] = [];
  for (let i = 0; i < events.length; i++) if (isRealUser(events[i])) starts.push(i);
  for (let t = 0; t < starts.length; t++) {
    const startIdx = starts[t];
    const endIdx = starts[t + 1] ?? events.length;
    const startTs = (events[startIdx] as { timestamp?: string }).timestamp;
    let lastAsst: SessionEvent | undefined;
    for (let i = startIdx + 1; i < endIdx; i++) {
      const ev = events[i];
      if (ev.type === 'assistant' && !(ev as { isSidechain?: boolean }).isSidechain) lastAsst = ev;
    }
    const endTs = (lastAsst as { timestamp?: string } | undefined)?.timestamp;
    const uuid = (lastAsst as { uuid?: string } | undefined)?.uuid;
    if (startTs && endTs && uuid) {
      const dur = Date.parse(endTs) - Date.parse(startTs);
      if (!Number.isNaN(dur) && dur >= 0) map.set(uuid, dur);
    }
  }
  return map;
}

// 턴별 assistant 의 출력 토큰(message.usage.output_tokens) — 완료 응답 버블 푸터 "↓N".
// transcript usage 라 정확(ccsv 식). 진행 중 라이브 토큰은 캡처 프록시 spinner 가 따로 담당.
function turnTokens(events: SessionEvent[]): Map<string, number> {
  const map = new Map<string, number>();
  for (const ev of events) {
    if (ev.type !== 'assistant' || (ev as { isSidechain?: boolean }).isSidechain) continue;
    const uuid = (ev as { uuid?: string }).uuid;
    const usage = (ev as { message?: { usage?: Record<string, unknown> } }).message?.usage;
    if (!uuid || !usage) continue;
    const out = typeof usage.output_tokens === 'number' ? usage.output_tokens : 0;
    if (out > 0) map.set(uuid, out);
  }
  return map;
}

// 토큰 수 → "5.5k" / "1.2M" 짧은 표기(터미널 상태바 ↓5.5k 와 동형).
function fmtTok(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}

// transcript user 이벤트(또는 permission-mode 이벤트)에 박힌 현재 권한 모드를 역순으로
// 찾는다. peek 화면 추정 없이 정확(거노: ccsv 방식). default(normal)는 칩 미표시 위해 null.
function latestPermissionMode(events: SessionEvent[]): string | null {
  for (let i = events.length - 1; i >= 0; i--) {
    const m = (events[i] as { permissionMode?: string }).permissionMode;
    if (m) return m;
  }
  return null;
}

// ccsv formatSystemMessage 발췌 — system 이벤트(api_error/compact_boundary)를 한 줄씩 평탄화한
// 텍스트로. 접이식 'System' 버블 본문에 들어간다.
function flattenSystem(ev: SessionEvent): string | null {
  const e = ev as {
    subtype?: string; level?: string;
    error?: { status?: number; requestID?: string | null; error?: { message?: string; error?: { message?: string } } };
    retryAttempt?: number; maxRetries?: number; retryInMs?: number;
    compactMetadata?: { trigger?: string; preTokens?: number };
  };
  const lines: string[] = [];
  if (e.subtype === 'api_error') {
    if (e.error?.status !== undefined) lines.push(`Status: ${e.error.status}`);
    if (e.error?.requestID) lines.push(`Request ID: ${e.error.requestID}`);
    const msg = e.error?.error?.error?.message ?? e.error?.error?.message ?? (e.error?.error ? JSON.stringify(e.error.error, null, 2) : null);
    if (msg) lines.push(`Error: ${msg}`);
    if (e.retryAttempt !== undefined) lines.push(`Retry: ${e.retryAttempt}/${e.maxRetries}`);
    if (e.retryInMs !== undefined) lines.push(`Retry In: ${(e.retryInMs / 1000).toFixed(2)}s`);
  } else if (e.subtype === 'compact_boundary') {
    if (e.compactMetadata?.trigger) lines.push(`Trigger: ${e.compactMetadata.trigger}`);
    if (e.compactMetadata?.preTokens !== undefined) lines.push(`Pre-Tokens: ${e.compactMetadata.preTokens}`);
  } else {
    return null;
  }
  return lines.length ? lines.join('\n') : null;
}

// jsonl SessionEvent[] → 카톡 렌더 아이템 평탄화. user/assistant content[] 를 순회:
// text→버블, thinking→ThinkingBlock, tool_use→ToolUseCard(페어된 tool_result 포함).
// AskUserQuestion 은 기존 선택지 menu 카드가 전담하므로 제외. tool_result 블록은
// buildToolMap 이 페어링해 카드 안에서 표시되니 별도 버블로 안 만든다.
type RenderItem =
  | { kind: 'bubble'; role: string; text: string; uuid?: string }
  | { kind: 'tool'; toolUse: { id?: string; name?: string; input?: unknown }; pair: ReturnType<ToolMap['get']> }
  | { kind: 'thinking'; text: string }
  | { kind: 'command'; commandName: string; commandArgs?: string; commandMessage?: string }
  | { kind: 'local-command'; stdout: string }
  | { kind: 'system'; text: string };

// 한 user 텍스트가 슬래시 명령/로컬 출력 태그를 품으면 카드/버블 아이템으로, 아니면 일반
// 버블로 푸시. 시스템 주입 잔여는 isSystemInjectionText 로 계속 숨긴다.
function pushUserText(items: RenderItem[], text: string): void {
  const parsed = parseSlashCommand(text);
  if (parsed?.kind === 'command') {
    items.push({ kind: 'command', commandName: parsed.commandName, commandArgs: parsed.commandArgs, commandMessage: parsed.commandMessage });
  } else if (parsed?.kind === 'local-command') {
    if (parsed.stdout.trim()) items.push({ kind: 'local-command', stdout: parsed.stdout });
  } else if (text.trim() && !isSystemInjectionText('user', text)) {
    items.push({ kind: 'bubble', role: 'user', text });
  }
}

function eventsToItems(events: SessionEvent[], toolMap: ToolMap): RenderItem[] {
  const items: RenderItem[] = [];
  for (const ev of events) {
    // 서브에이전트 분기(sidechain)는 메인 대화에 섞이는 노이즈라 제외.
    if ((ev as { isSidechain?: boolean }).isSidechain) continue;
    if (ev.type === 'system') {
      const text = flattenSystem(ev);
      if (text) items.push({ kind: 'system', text });
      continue;
    }
    if (ev.type !== 'user' && ev.type !== 'assistant') continue;
    const role = ev.type;
    const uuid = (ev as { uuid?: string }).uuid;
    const content = (ev as { message?: { content?: unknown } }).message?.content;
    if (typeof content === 'string') {
      if (role === 'user') pushUserText(items, content);
      else if (content.trim()) items.push({ kind: 'bubble', role, text: content, uuid });
    } else if (Array.isArray(content)) {
      for (const block of content) {
        if (!block || typeof block !== 'object') continue;
        const b = block as { type?: string; text?: string; thinking?: string; id?: string; name?: string; input?: unknown };
        if (b.type === 'text' && typeof b.text === 'string' && b.text.trim()) {
          if (role === 'user') pushUserText(items, b.text);
          else items.push({ kind: 'bubble', role, text: b.text, uuid });
        } else if (b.type === 'thinking' && typeof b.thinking === 'string' && b.thinking.trim()) {
          items.push({ kind: 'thinking', text: b.thinking });
        } else if (b.type === 'tool_use' && b.name !== 'AskUserQuestion') {
          items.push({ kind: 'tool', toolUse: { id: b.id, name: b.name, input: b.input }, pair: b.id ? toolMap.get(b.id) : undefined });
        }
      }
    }
  }
  return items;
}

// ANSI 색(SGR) 렌더는 ./AnsiText 모듈로 분리 — bash/read 카드 resultView 와 공용.

// 채팅방 맨위/맨아래 점프 버튼 공용 스타일(거노: 긴 대화 스크롤).
const SCROLL_BTN: CSSProperties = {
  width: 30, height: 30, borderRadius: 999, cursor: 'pointer',
  border: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-50)',
  color: 'var(--cth-ink-500)', boxShadow: '0 1px 4px rgba(21,41,74,0.12)',
  display: 'inline-flex', alignItems: 'center', justifyContent: 'center',
};

function MetaChip({ label, dim, onClick, tone, title }: { label: string; dim?: boolean; onClick?: () => void; tone?: 'danger'; title?: string }) {
  const danger = tone === 'danger';
  return (
    <span
      onClick={onClick}
      title={title ?? (onClick ? '클릭해서 변경' : undefined)}
      style={{
        fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: danger ? 700 : 600,
        padding: '2px 8px', borderRadius: 6,
        background: danger ? 'color-mix(in srgb, var(--cth-coral) 16%, #fff)' : dim ? 'transparent' : 'var(--cth-cream-100)',
        color: danger ? 'var(--cth-coral)' : dim ? 'var(--cth-ink-300)' : 'var(--cth-ink-700)',
        border: danger ? '1px solid var(--cth-coral)' : dim ? 'none' : '1px solid var(--cth-cream-200)',
        cursor: onClick ? 'pointer' : undefined,
      }}>{label}</span>
  );
}

// 권한 모드(transcript permissionMode) → 칩 라벨. default/normal 은 일반 상태라 미표시(null).
function modeLabel(m: string | null): string | null {
  switch (m) {
    case 'plan': return 'plan';
    case 'acceptEdits': return 'accept edits';
    case 'bypassPermissions': return 'bypass';
    default: return null;
  }
}

// system 이벤트(api_error/compact_boundary) — 좌측 작은 회색 접이식 'System' 버블.
// 클릭하면 평탄화된 상세(Status/Request ID/Error/Pre-Tokens…)가 펼쳐진다.
function SystemBubble({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  return (
    <div style={{ display: 'flex', justifyContent: 'flex-start', marginBottom: 10 }}>
      <div style={{ maxWidth: '85%', width: '100%' }}>
        <button
          onClick={() => setOpen((o) => !o)}
          style={{
            display: 'flex', alignItems: 'center', gap: 6, width: '100%', textAlign: 'left',
            padding: '6px 10px', borderRadius: 9, cursor: 'pointer',
            background: 'var(--cth-cream-100)', border: '1px solid var(--cth-cream-200)',
            fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: 600, color: 'var(--cth-ink-300)',
          }}>
          <svg width="11" height="11" viewBox="0 0 16 16" style={{ transform: open ? 'rotate(180deg)' : undefined, transition: 'transform .15s', flexShrink: 0 }}>
            <path d="M4 6l4 4 4-4" stroke="currentColor" strokeWidth="2" fill="none" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
          System
        </button>
        {open && (
          <pre style={{
            margin: '6px 0 0', padding: '8px 10px', borderRadius: 9, overflowX: 'auto',
            background: 'var(--cth-cream-50)', border: '1px solid var(--cth-cream-200)',
            fontFamily: 'var(--cth-font-mono)', fontSize: 10, lineHeight: 1.4, whiteSpace: 'pre-wrap',
            color: 'var(--cth-ink-500)',
          }}>{text}</pre>
        )}
      </div>
    </div>
  );
}

export interface TerminalPeekPanelProps {
  surfaceId: string;
  title: string;
  onClose: () => void;
  /** CommandCenter '학생별 대화' 탭에 내장될 때 — 폭을 부모에 맞추고 좌측 보더 제거. */
  embedded?: boolean;
  /** 오프라인(과거) 세션 읽기 전용 미리보기 — 라이브 pane 없이 uuid+cwd 로 jsonl 을 1회
   *  읽어 대화만 렌더. 입력창·라이브 폴링·학생 액션은 끄고, 하단에 '현재 터미널에 입력'
   *  이어가기 액션바를 띄운다. 있으면 surfaceId 는 빈 값('')으로 들어온다. */
  session?: { id: string; cwd: string; label: string };
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
export function TerminalPeekPanel({ surfaceId, title, onClose, embedded, session }: TerminalPeekPanelProps) {
  const offline = !!session;
  const agent = useStore((s) => s.agents.find((a) => a.id === surfaceId));
  // 아바타는 board(라이브) 캐릭터명 우선 — title(클릭 시점 고정)이 pane id('%3')로 깨졌을 때
  // 보강(거노: 프사 %). board 도 id 면 SpritePortrait 가 사람 실루엣으로 막는다.
  const avatarChar = agent?.character && !/^%?\d+$/.test(agent.character) ? agent.character : title;
  const [events, setEvents] = useState<SessionEvent[]>([]);
  const [streaming, setStreaming] = useState('');
  // 하이브리드 폴백: jsonl(events)이 아직 안 써진 진행 중 구간엔 캡처 프록시의 텍스트
  // 대화(conv.turns)를 라이브로 띄운다. jsonl 이 flush 되면 events 가 우선해 per-tool
  // 카드로 자동 승격. claude 가 jsonl 을 라이브로 안 써(2.1.x) 진행 중엔 프록시가 메운다.
  const [convTurns, setConvTurns] = useState<Turn[]>([]);
  const [menu, setMenu] = useState<{ title: string; options: { idx: number; label: string; cur: boolean; description?: string }[]; multi?: boolean } | null>(null);
  const [checked, setChecked] = useState<Set<number>>(new Set()); // multiSelect 체크된 인덱스
  const [images, setImages] = useState<string[]>([]); // SendUserFile 로 보낸 이미지
  const [loaded, setLoaded] = useState(false); // 첫 폴 완료 — 빈 상태 문구 분기
  const [spinner, setSpinner] = useState<string | null>(null); // claude 라이브 작업 표시(verb·경과초)
  const [effort, setEffort] = useState<string | null>(null); // effort level(상태바 파싱) — 모델 옆 칩
  const [convTokensOut, setConvTokensOut] = useState(0); // 진행 중 응답 누적 출력 토큰(프록시 SSE 라이브) — spinner ↓N
  const [mode, setMode] = useState<string | null>(null); // 권한 모드(transcript permissionMode) — 헤더 칩
  const [convModel, setConvModel] = useState(''); // 캡처 프록시가 요청에서 잡은 model(거노: 화면스크랩 대신 프록시 소스)
  const [dragOver, setDragOver] = useState(false); // 이미지 드래그 오버 — 점선 드롭존 오버레이
  const [pendingPreviews, setPendingPreviews] = useState<string[]>([]); // staged 이미지 미리보기 data URL(여러 개)
  const hasPending = pendingPreviews.length > 0;
  const [effortMenu, setEffortMenu] = useState<number | null>(null); // /effort 슬라이더 현재 idx(뜬 동안)
  const [input, setInput] = useState('');
  const [sending, setSending] = useState(false);
  const [flash, setFlash] = useState<'ok' | 'err' | null>(null);
  // window.confirm 은 wry webview(macOS)에서 무반응 — 자체 확인 모달로 종료·compact 확인(거노).
  const [confirm, setConfirm] = useState<{ msg: string; sub?: string; danger?: boolean; yes: string; onYes: () => void } | null>(null);
  const [charPicker, setCharPicker] = useState(false); // 캐릭터 변경 팝업(헤더 버튼)
  const [atTop, setAtTop] = useState(true); // 스크롤 맨위 — true 면 ↑ 버튼 숨김
  const [atBottom, setAtBottom] = useState(true); // 스크롤 맨아래 — true 면 ↓ 버튼 숨김
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
  // 마지막으로 화면에서 진행형 verb 를 본 시각(ms) — 폴링 사이 verb 가 잠깐 안 보여도
  // 4초간 직전 spinner 를 유지해 깜빡임(있었다 없었다)을 흡수. board status 폴링 노이즈에
  // 안 흔들리게 시간 기반(거노: 똑같이 깜빡임).
  const lastSpinnerRef = useRef(0);
  // ESC 로 닫은 AskUserQuestion 질문(title) — tool_use 가 응답 전까지 캡처 프록시에 남아
  // tick 마다 카드가 부활하던 것(거노: esc 눌러도 취소 안 됨). 같은 질문이면 다시 안 띄운다.
  const dismissedQRef = useRef<string | null>(null);
  // 카드에서 내가 고른 선택 — AskUserQuestion 답은 conv.turns 로 안 새므로(시스템 주입 필터)
  // 직접 user 버블로 남긴다(거노: 뭐 선택했는지 대화창에 떠야). surface 바뀌면 리셋.
  const [myChoices, setMyChoices] = useState<Turn[]>([]);
  useEffect(() => { setMyChoices([]); dismissedQRef.current = null; }, [surfaceId]);

  // 대화 내역: transcript jsonl 우선(깨끗), 비었으면 PTY 화면(peek) 폴백 — 인터랙티브
  // claude 가 jsonl 을 라이브로 안 써 진행 중엔 transcript 가 빈다(claude-code-guide
  // 확인). jsonl 이 flush 되면 자동으로 transcript 우선. 학생 바뀌면 초기화.
  useEffect(() => {
    let stopped = false;
    setLoaded(false);
    setEvents([]);
    setStreaming('');
    setConvTurns([]);
    setMenu(null);
    setSpinner(null);
    lastSpinnerRef.current = 0;
    setConvTokensOut(0);
    setMode(null);
    setImages([]);
    setInput('');
    // 오프라인(과거) 세션: jsonl 은 더 이상 안 변하니 1회만 읽고 폴링 안 한다. 라이브
    // 소스(conversation/peek/sent-images/menu/spinner)는 죽은 세션엔 없어 전부 skip.
    if (offline && session) {
      void fetchSessionTranscriptRaw(session.id, session.cwd).then((evts) => {
        if (stopped) return;
        setEvents(evts);
        setLoaded(true);
      });
      return () => { stopped = true; };
    }
    const tick = async () => {
      // 세션 경계: transcript 첫 이벤트 ts = 현재 세션 시작. 이전 세션 이미지(sent-images.jsonl
      // 방단위 누적) 잔류를 백엔드 since 로 컷(거노: 이전 pane 이미지가 새 대화에 남던 것).
      const evts = await fetchTranscriptRaw(surfaceId);
      const ts0 = (evts[0] as { timestamp?: string } | undefined)?.timestamp;
      const since = ts0 ? Date.parse(ts0) / 1000 : undefined;
      const [conv, imgs, screen] = await Promise.all([
        fetchConversation(surfaceId),
        // since(현재 세션 시작 ts) 없으면 백엔드가 sent-images.jsonl(방 누적)을 통째로 줘
        // 이전 세션 이미지가 새 대화에 떴다(거노: 아무것도 안 쳤는데 이미지들). 세션 경계가
        // 잡힐 때(transcript 첫 이벤트)까지 보류 — 새/조용한 학생에 잔류 이미지가 안 뜨게.
        since ? fetchSentImages(surfaceId, 12, since) : Promise.resolve<string[]>([]),
        fetchPeek(surfaceId, 60),
      ]);
      if (stopped) return;
      // 본문은 jsonl(raw SessionEvent[]) — text/tool_use/tool_result/thinking 전부 보존해
      // per-tool 카드로 렌더. 프록시 streaming(진행 중 어시스턴트 응답)은 jsonl 이 아직
      // 안 써진 구간을 메우는 라이브 보너스(프록시 꺼지면 빈 문자열).
      setEvents(evts);
      setStreaming(conv.streaming.trim() ? conv.streaming : '');
      setConvTurns(conv.turns); // 프록시 텍스트 대화 — jsonl 비었을 때 라이브 폴백
      // 인터랙티브 선택지 — AskUserQuestion 은 캡처 프록시 tool_use 로 질문/선택지가 정확히
      // 잡힌다(거노: peek 추정 금지). 그게 있으면 그걸 쓰고, 없을 때만 화면 메뉴(/model 등
      // API 안 타는 것)를 peek 폴백으로 파싱한다.
      const aq = conv.tool_uses?.find((t) => t.name === 'AskUserQuestion' && t.input.questions?.length);
      // ESC/Enter 직후 suppress 창 동안은 메뉴 재감지 보류(닫은 카드가 stale 화면으로 재등장 방지).
      const suppressed = Date.now() < menuSuppressRef.current;
      if (aq) {
        const q = aq.input.questions![0];
        // ESC 로 닫은 질문이면 카드 부활 금지(거노: esc 취소). 다른 질문이면 dismiss 해제 후 표시.
        if (q.question === dismissedQRef.current) {
          setMenu(null);
        } else {
          dismissedQRef.current = null;
          setMenu({
            title: q.question,
            options: q.options.map((o, i) => ({ idx: i + 1, label: o.label, cur: false, description: o.description })),
            multi: !!q.multiSelect,
          });
        }
      } else {
        dismissedQRef.current = null; // aq 사라짐 = 응답/취소됨 → 다음 질문 위해 해제
        if (!suppressed) setMenu(parsePromptMenu(screen));
      }
      // claude 라이브 작업 표시(verb·경과초)도 화면에만 — 로딩 인디케이터에 실값으로.
      // 화면에 진행형 verb 가 잠깐 안 보이는 프레임마다 null 로 깜빡이던 것(거노)을 막는다:
      // 4초 grace — verb 를 본 지 4초 안이면 직전값 유지, 넘으면(작업 끝남) 클리어.
      const sp = parseSpinner(screen);
      if (sp) lastSpinnerRef.current = Date.now();
      setSpinner((prev) => sp ?? (Date.now() - lastSpinnerRef.current < 4000 ? prev : null));
      setConvTokensOut(conv.tokens_out ?? 0); // 진행 중 응답 누적 출력 토큰(프록시 SSE 라이브)
      setMode(latestPermissionMode(evts)); // 권한 모드 — transcript permissionMode(peek 추정 안 함)
      setEffort(conv.effort || parseEffort(screen)); // 캡처 프록시 effort(output_config.effort) 우선, 폴백 화면파싱
      setConvModel(conv.model || ''); // 프록시가 요청에서 잡은 model — /model 전환 시 다음 요청에 반영(실시간 소스)
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
  }, [surfaceId, offline, session?.id, session?.cwd]);

  // 새 내용 도착 시, 사용자가 하단에 있을 때만 따라내린다(위로 스크롤 중이면 안 건드림 —
  // 거노: 스크롤 올리면 자꾸 내려가던 버그).
  useEffect(() => {
    const el = bodyRef.current;
    if (el && atBottomRef.current) el.scrollTop = el.scrollHeight;
  }, [events, streaming, images]);

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
    if (!el) return;
    atBottomRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
    setAtBottom(atBottomRef.current);
    setAtTop(el.scrollTop < 48);
  };

  // 실시간 미러: 웹 입력을 칠 때마다 터미널 PTY 라인과 동기화한다(거노 요청 —
  // "웹에 치면 터미널에 실시간으로"). Ctrl-U(\x15)로 줄을 비우고 현재 입력 전체를
  // 재전송(submit=false). 매 글자 전체 재전송이라 백스페이스·편집·IME 까지 self-heal.
  // \x15 가 claude TUI(Ink)에서 줄을 비우는 건 submit 경로에서 검증됨.
  const mirror = (next: string) => {
    setInput(next);
    setSlashIdx(0);
    // 이미지 staged 동안엔 \x15 미러를 끈다 — claude 입력에 붙은 [Image] 가 지워지면 안 됨.
    // 텍스트는 제출 때 한꺼번에 보낸다(거노: 첨부 중엔 라이브 미러 잠시 off).
    if (hasPending) return;
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
    let ok: boolean;
    if (hasPending) {
      // 이미지들은 drop 때 이미 claude 에 paste 됨(미러 off 라 보존). 텍스트가 있으면 그
      // [Image] 뒤에 append(미러 안 탔으니 \x15 없이) 후 bare CR 로 제출. 전송된 user
      // 메시지(텍스트+이미지)는 프록시가 캡처해 말풍선에 뜬다(거노).
      if (text.trim()) await sendToPane(surfaceId, text, false);
      await new Promise((r) => setTimeout(r, 80));
      ok = await sendToPane(surfaceId, '\r', false);
      setPendingPreviews([]);
    } else {
      ok = text
        ? await sendToPane(surfaceId, text, true, false) // 학생별 대화 — 모모톡에 안 남김
        : await sendToPane(surfaceId, '\r', false);
    }
    setSending(false);
    setFlash(ok ? 'ok' : 'err');
    setTimeout(() => setFlash(null), 1200);
    if (ok) {
      setInput('');
      // 작업 중 미리 보낸 메시지도 즉시 말풍선으로(거노) — 다음 폴에서 transcript 에 같은
      // 텍스트가 뜨면 pendingChoices 가 중복 제거한다.
      if (text.trim()) setMyChoices((p) => [...p, { role: 'user', text: text.trim() }]);
    }
    inputRef.current?.focus();
  };

  // 이미지 드롭(아로나 대화창 어디든) → 그 학생 claude 에 첨부. dataTransfer 의 첫
  // 이미지 파일을 raw 바이트로 POST /paste-image → kasaterm 이 시스템 클립보드+Ctrl+V 로
  // 그 pane 에 [Image] 칩 첨부(webview 라 경로가 없어 바이트 전송).
  const onDropImage = async (e: React.DragEvent) => {
    e.preventDefault();
    setDragOver(false);
    // WKWebView(wry) 가 떨군 File 은 .type 이 빈 문자열일 때가 많아 type 필터로 걸러지면
    // 첨부가 안 됐다(거노 실측). type 또는 확장자로 보고, 둘 다 애매하면 첫 파일.
    const files = Array.from(e.dataTransfer?.files ?? []);
    const file =
      files.find((f) => f.type.startsWith('image/') || /\.(png|jpe?g|gif|webp|bmp)$/i.test(f.name)) ??
      files[0];
    // 드롭한 이미지 전부 처리(여러 개 다 보이게 — 거노). 각각 claude 에 순차 paste 해
    // 터미널에 [Image #1][Image #2]… 로 다 뜨고, 입력창엔 썸네일이 줄지어 쌓인다.
    const list = files.filter((f) => f.type.startsWith('image/') || /\.(png|jpe?g|gif|webp|bmp)$/i.test(f.name));
    const imgs = list.length ? list : (file ? [file] : []);
    if (!imgs.length) return;
    for (const f of imgs) {
      const reader = new FileReader();
      reader.onload = () => setPendingPreviews((prev) => [...prev, typeof reader.result === 'string' ? reader.result : '']);
      reader.readAsDataURL(f);
      await pasteImageToPane(surfaceId, f); // drop 즉시 claude 에 append(터미널 표시)
      await new Promise((r) => setTimeout(r, 200)); // 클립보드 덮어쓰기 race 방지 — 순차 paste
    }
    inputRef.current?.focus();
  };
  // 드래그가 파일을 싣고 있을 때만 드롭존 표시(텍스트 드래그·셀렉션 무시).
  const dragHasFile = (e: React.DragEvent) =>
    Array.from(e.dataTransfer?.types ?? []).includes('Files');

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
    // 평소 ESC = claude 작업중지(인터럽트). 슬래시 드롭다운 아닐 때만. staged 이미지 있으면
    // 먼저 첨부 취소(\x15 로 입력 비움). 그 외엔 ESC(\x1b) 를 pane 에 보내 생성 중단(거노).
    if (e.key === 'Escape') {
      e.preventDefault();
      if (hasPending) { setPendingPreviews([]); void sendToPane(surfaceId, '\x15', false); return; }
      void sendToPane(surfaceId, '\x1b', false);
      return;
    }
    if (e.key === 'Enter') { e.preventDefault(); void submit(); }
    if (e.key === 'c' && e.ctrlKey) { e.preventDefault(); setInput(''); void sendToPane(surfaceId, '\x03', false); }
  };

  // 인터랙티브 메뉴 선택 → 숫자 단축키 전송(claude 가 즉시 선택). 다음 폴에서 갱신.
  const pickMenu = async (oi: number) => {
    const label = menu?.options[oi]?.label;
    if (label) setMyChoices((p) => [...p, { role: 'user', text: label }]); // 대화창에 내 선택 남김(거노)
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
    const labels = menu ? [...checked].sort((a, b) => a - b).map((i) => menu.options[i]?.label).filter(Boolean) : [];
    if (labels.length) setMyChoices((p) => [...p, { role: 'user', text: labels.join(', ') }]); // 내 선택 남김(거노)
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
        if (menu) dismissedQRef.current = menu.title; // aq 부활 방지(거노: esc 눌러도 취소 안 되던 것)
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

  // jsonl events → 카톡 렌더 아이템(text 버블 + per-tool 카드 + thinking). tool_use_id
  // 페어링(buildToolMap)으로 카드가 결과/diff/stats 까지 그린다.
  const toolMap = useMemo(() => buildToolMap(events), [events]);
  // 턴별 소요시간 — 마지막 assistant uuid → ms. 그 버블 아래 시계 푸터.
  const durationMap = useMemo(() => turnDurations(events), [events]);
  // 턴별 출력 토큰 — assistant uuid → output_tokens. 시계 푸터 옆 "↓N"(완료 응답·정확).
  const tokenMap = useMemo(() => turnTokens(events), [events]);
  const items = useMemo(() => {
    if (events.length) return eventsToItems(events, toolMap);
    // jsonl 이 아직 안 써진 진행 중 구간 — 캡처 프록시 텍스트 대화로 라이브 폴백.
    // conv.turns 는 프록시가 이미 strip_meta·is_main_conversation 으로 정제 → 재필터 안 함.
    return convTurns.map((t) => ({ kind: 'bubble', role: t.role, text: t.text }) as RenderItem);
  }, [events, toolMap, convTurns]);
  // optimistic 으로 남긴 내 발화(myChoices) 중 transcript(items)에 아직 안 잡힌 것만 — 작업
  // 중 미리 보낸 메시지를 즉시 띄우되(거노), 다음 폴에서 transcript 에 같은 텍스트가 뜨면
  // 중복 제거. 메뉴 선택(label)은 transcript 에 raw 키로 들어가 매칭 안 돼 계속 남는다(의도).
  const pendingChoices = useMemo(
    () => myChoices.filter((c) => !items.some((it) =>
      it.kind === 'bubble' && (it as { role: string }).role === c.role
        && (it as { text: string }).text.trim() === c.text.trim())),
    [myChoices, items],
  );
  // 로딩 점 판정용 — 마지막 말풍선이 선생님(user)이면 학생이 아직 답하기 전.
  const lastBubbleRole = useMemo(() => {
    for (let i = items.length - 1; i >= 0; i--) if (items[i].kind === 'bubble') return (items[i] as { role: string }).role;
    return undefined;
  }, [items]);

  return (
    <div
      onDragEnter={(e) => { if (dragHasFile(e)) { e.preventDefault(); setDragOver(true); } }}
      onDragOver={(e) => { if (dragHasFile(e)) { e.preventDefault(); setDragOver(true); } }}
      onDragLeave={(e) => { if (!e.currentTarget.contains(e.relatedTarget as Node)) setDragOver(false); }}
      onDrop={onDropImage}
      style={{
      width: embedded ? '100%' : 340, flex: embedded ? 1 : undefined,
      flexShrink: 0, height: '100%', position: 'relative', // 확인 모달 기준
      display: 'flex', flexDirection: 'column',
      background: 'var(--cth-cream-50)',
      borderLeft: embedded ? 'none' : '1px solid var(--cth-cream-200)',
      overflow: 'hidden'
    }}>
      {/* 이미지 드래그 드롭존 — 파일 드래그 중에만 점선 오버레이(거노: 점선 뜨고 거기 드롭). */}
      {dragOver && (
        <div style={{
          position: 'absolute', inset: 8, zIndex: 50, pointerEvents: 'none',
          border: '2px dashed var(--cth-sky)', borderRadius: 14,
          background: 'color-mix(in srgb, var(--cth-sky) 12%, var(--cth-cream-50))',
          display: 'flex', alignItems: 'center', justifyContent: 'center',
          fontFamily: 'var(--cth-font-ui)', fontSize: 14, fontWeight: 800, color: 'var(--cth-sky)',
        }}>이미지를 놓으면 첨부돼요</div>
      )}
      {/* 헤더: 캐릭터명 + 닫기 */}
      <div style={{
        display: 'flex', alignItems: 'center', gap: 10,
        padding: '10px 12px',
        background: 'var(--cth-cream-50)',
        borderBottom: '1px solid var(--cth-cream-200)'
      }}>
        <span style={{ fontFamily: 'var(--cth-font-display)', fontSize: 15, fontWeight: 700, color: 'var(--cth-ink-900)' }}>
          {title} {offline ? (
            <span style={{ marginLeft: 2, padding: '1px 7px', borderRadius: 6, background: 'var(--cth-cream-200)', color: 'var(--cth-ink-500)', fontWeight: 700, fontSize: 10 }}>오프라인 · 읽기 전용</span>
          ) : (
            <span style={{ color: 'var(--cth-ink-300)', fontWeight: 400, fontSize: 13 }}>{surfaceId}</span>
          )}
        </span>
        <div style={{ flex: 1 }} />
        {!offline && (<>
        <button
          onClick={() => setCharPicker(true)}
          title="캐릭터 변경 (대화 리셋)"
          style={{
            height: 28, padding: '0 10px', borderRadius: 8, border: '1px solid var(--cth-cream-200)', cursor: 'pointer',
            background: 'var(--cth-cream-100)', color: 'var(--cth-ink-700)',
            fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 600,
            display: 'inline-flex', alignItems: 'center'
          }}
        >캐릭터</button>
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
        </>)}
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

      {/* 대화(채팅 버블) + 보낸 이미지 — relative 래퍼로 감싸 맨위/맨아래 스크롤 버튼을 띄운다(거노). */}
      <div style={{ flex: 1, position: 'relative', display: 'flex', flexDirection: 'column', minHeight: 0 }}>
      <div ref={bodyRef} onScroll={onBodyScroll} style={{ flex: 1, overflow: 'auto', padding: '14px 16px', background: 'var(--cth-cream-100)' }}>
        {events.length === 0 && convTurns.length === 0 && !streaming.trim() && myChoices.length === 0 && images.length === 0 && agent?.status !== 'working' && agent?.status !== 'thinking' ? (
          <div style={{ color: 'var(--cth-ink-300)', fontFamily: 'var(--cth-font-ui)', fontSize: 13, textAlign: 'center', marginTop: 40 }}>
            {loaded ? '아직 대화가 없어요' : '대화를 불러오는 중…'}
          </div>
        ) : (
          <>
            {[
              ...items,
              ...(streaming.trim() ? [{ kind: 'bubble', role: 'assistant', text: streaming } as RenderItem] : []),
              ...pendingChoices.map((t) => ({ kind: 'bubble', role: t.role, text: t.text } as RenderItem)),
            ].map((it, i) => {
              // 도구 호출(Bash/Edit/Read…) — 학생(좌측)에 per-tool 카드로 인터리브.
              if (it.kind === 'tool') {
                return (
                  <div key={i} style={{ display: 'flex', justifyContent: 'flex-start', marginBottom: 10 }}>
                    <div style={{ maxWidth: '90%', width: '100%' }}>
                      <ToolUseCard toolUse={it.toolUse} pair={it.pair} />
                    </div>
                  </div>
                );
              }
              // 사고(thinking) 블록 — 좌측 접이식.
              if (it.kind === 'thinking') {
                return (
                  <div key={i} style={{ display: 'flex', justifyContent: 'flex-start', marginBottom: 10 }}>
                    <div style={{ maxWidth: '90%', width: '100%' }}>
                      <ThinkingBlock thinking={it.text} />
                    </div>
                  </div>
                );
              }
              // system 이벤트(api_error/compact_boundary) — 좌측 접이식 회색 'System' 버블.
              if (it.kind === 'system') return <SystemBubble key={i} text={it.text} />;
              // 슬래시 명령(<command-*>) — 우측(선생님측) green 'Claude Code Command' 카드.
              if (it.kind === 'command') {
                const hasArgs = !!it.commandArgs && it.commandArgs.trim() !== '';
                const hasMsg = !!it.commandMessage && it.commandMessage.trim() !== '';
                return (
                  <div key={i} style={{ display: 'flex', justifyContent: 'flex-end', marginBottom: 10 }}>
                    <div style={{
                      maxWidth: '85%', padding: '8px 12px', borderRadius: 14, borderTopRightRadius: 4,
                      background: 'color-mix(in srgb, var(--cth-mint) 14%, var(--cth-cream-50))',
                      border: '1px solid color-mix(in srgb, var(--cth-mint) 40%, var(--cth-cream-200))',
                      boxShadow: '0 1px 3px rgba(21, 41, 74, 0.08)',
                    }}>
                      <div style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
                        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="var(--cth-mint)" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" style={{ flexShrink: 0 }}>
                          <polyline points="4 17 10 11 4 5" /><line x1="12" y1="19" x2="20" y2="19" />
                        </svg>
                        <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 700, color: 'var(--cth-ink-700)' }}>Claude Code Command</span>
                        <span style={{ fontFamily: 'var(--cth-font-mono)', fontSize: 11, fontWeight: 700, padding: '1px 7px', borderRadius: 6, color: 'var(--cth-mint)', border: '1px solid color-mix(in srgb, var(--cth-mint) 45%, transparent)' }}>{it.commandName}</span>
                      </div>
                      {(hasArgs || hasMsg) && (
                        <div style={{ marginTop: 6, display: 'flex', flexDirection: 'column', gap: 6 }}>
                          {hasArgs && (
                            <div>
                              <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 600, color: 'var(--cth-ink-300)' }}>Arguments</span>
                              <pre style={{ margin: '2px 0 0', padding: '5px 8px', borderRadius: 6, background: 'var(--cth-cream-50)', border: '1px solid var(--cth-cream-200)', fontFamily: 'var(--cth-font-mono)', fontSize: 11, whiteSpace: 'pre-wrap', wordBreak: 'break-all', color: 'var(--cth-ink-700)' }}>{it.commandArgs}</pre>
                            </div>
                          )}
                          {hasMsg && (
                            <div>
                              <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 600, color: 'var(--cth-ink-300)' }}>Message</span>
                              <pre style={{ margin: '2px 0 0', padding: '5px 8px', borderRadius: 6, background: 'var(--cth-cream-50)', border: '1px solid var(--cth-cream-200)', fontFamily: 'var(--cth-font-mono)', fontSize: 11, whiteSpace: 'pre-wrap', wordBreak: 'break-all', color: 'var(--cth-ink-700)' }}>{it.commandMessage}</pre>
                            </div>
                          )}
                        </div>
                      )}
                    </div>
                  </div>
                );
              }
              // <local-command-stdout> — 좌측(학생측) 'Local Command' 출력 버블.
              if (it.kind === 'local-command') {
                return (
                  <div key={i} style={{ display: 'flex', justifyContent: 'flex-start', marginBottom: 10, gap: 8, alignItems: 'flex-end' }}>
                    <div style={{ width: 34, height: 34, borderRadius: 11, overflow: 'hidden', flexShrink: 0, background: 'var(--cth-cream-100)', border: '1px solid var(--cth-cream-200)', display: 'flex', alignItems: 'flex-end', justifyContent: 'center' }}>
                      <SpritePortrait character={avatarChar} scale={1.5} bust />
                    </div>
                    <div style={{ maxWidth: '80%', padding: '8px 12px', borderRadius: 14, borderTopLeftRadius: 4, background: 'var(--cth-cream-50)', border: '1px solid var(--cth-cream-200)', boxShadow: '0 1px 3px rgba(21, 41, 74, 0.08)', overflowX: 'auto' }}>
                      <div style={{ display: 'flex', alignItems: 'center', gap: 6, marginBottom: 4 }}>
                        <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="var(--cth-ink-300)" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" style={{ flexShrink: 0 }}>
                          <polyline points="4 17 10 11 4 5" /><line x1="12" y1="19" x2="20" y2="19" />
                        </svg>
                        <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: 700, color: 'var(--cth-ink-500)' }}>Local Command</span>
                      </div>
                      <pre style={{ margin: 0, fontFamily: 'var(--cth-font-mono)', fontSize: 11, lineHeight: 1.4, whiteSpace: 'pre-wrap', wordBreak: 'break-word', color: 'var(--cth-ink-700)' }}>{it.stdout}</pre>
                    </div>
                  </div>
                );
              }
              const mine = it.role === 'user';
              // 턴 소요시간 — assistant 버블이 그 턴의 마지막이면 시계 푸터(거노 데스크탑 앱풍).
              const durMs = !mine && it.uuid ? durationMap.get(it.uuid) : undefined;
              const tokOut = !mine && it.uuid ? tokenMap.get(it.uuid) : undefined; // 완료 응답 출력 토큰(transcript usage)
              // 메신저: 선생님(user)=우측 카톡 노랑, 학생(assistant)=좌측 아바타+흰 말풍선.
              return (
                <div key={i} style={{ display: 'flex', justifyContent: mine ? 'flex-end' : 'flex-start', marginBottom: 10, gap: 8, alignItems: 'flex-end' }}>
                  {!mine && (
                    <div style={{ width: 34, height: 34, borderRadius: 11, overflow: 'hidden', flexShrink: 0, background: 'var(--cth-cream-100)', border: '1px solid var(--cth-cream-200)', display: 'flex', alignItems: 'flex-end', justifyContent: 'center' }}>
                      <SpritePortrait character={avatarChar} scale={1.5} bust />
                    </div>
                  )}
                  <div style={{ maxWidth: '72%', display: 'flex', flexDirection: 'column', alignItems: 'flex-start' }}>
                    <div style={{
                      padding: '8px 12px',
                      borderRadius: 14,
                      borderTopLeftRadius: mine ? 14 : 4,
                      borderTopRightRadius: mine ? 4 : 14,
                      background: mine ? '#FEE500' : 'var(--cth-cream-50)',
                      color: mine ? '#3A2E00' : 'var(--cth-ink-900)',
                      border: mine ? 'none' : '1px solid var(--cth-cream-200)',
                      boxShadow: '0 1px 3px rgba(21, 41, 74, 0.08)',
                      fontFamily: 'var(--cth-font-ui)', fontSize: 13, lineHeight: 1.55,
                      wordBreak: 'break-word', maxWidth: '100%',
                    }}>
                      {it.text && (mine
                        ? <span style={{ whiteSpace: 'pre-wrap', wordBreak: 'break-word' }}>{it.text}</span>
                        : <Markdown text={it.text} />)}
                    </div>
                    {(durMs != null || tokOut != null) && (
                      <div style={{ display: 'flex', alignItems: 'center', gap: 6, marginTop: 4, paddingLeft: 4, fontFamily: 'var(--cth-font-ui)', fontSize: 10, color: 'var(--cth-ink-300)' }}>
                        {durMs != null && (
                          <span style={{ display: 'inline-flex', alignItems: 'center', gap: 4 }}>
                            <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" style={{ flexShrink: 0 }}>
                              <circle cx="12" cy="12" r="9" /><polyline points="12 7 12 12 15.5 14" />
                            </svg>
                            {formatDuration(durMs)}
                          </span>
                        )}
                        {tokOut != null && <span title={`출력 ${tokOut.toLocaleString()} 토큰`}>↓ {fmtTok(tokOut)}</span>}
                      </div>
                    )}
                  </div>
                </div>
              );
            })}

            {/* 학생이 SendUserFile 로 보낸 이미지 — 좌측(학생) 이미지 버블. 클릭=원본. */}
            {images.map((path, i) => (
              <div key={`img-${path}-${i}`} style={{ display: 'flex', justifyContent: 'flex-start', marginBottom: 10, gap: 8, alignItems: 'flex-end' }}>
                <div style={{ width: 34, height: 34, borderRadius: 11, overflow: 'hidden', flexShrink: 0, background: 'var(--cth-cream-100)', border: '1px solid var(--cth-cream-200)', display: 'flex', alignItems: 'flex-end', justifyContent: 'center' }}>
                  <SpritePortrait character={avatarChar} scale={1.5} bust />
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
                    <SpritePortrait character={avatarChar} scale={1.5} bust />
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
              !streaming.trim() && lastBubbleRole === 'user')) && (
              <div style={{ display: 'flex', justifyContent: 'flex-start', marginBottom: 10, gap: 8, alignItems: 'flex-end' }}>
                <div style={{ width: 34, height: 34, borderRadius: 11, overflow: 'hidden', flexShrink: 0, background: 'var(--cth-cream-100)', border: '1px solid var(--cth-cream-200)', display: 'flex', alignItems: 'flex-end', justifyContent: 'center' }}>
                  <SpritePortrait character={avatarChar} scale={1.5} bust />
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
                  {convTokensOut > 0 && (
                    <span style={{ color: 'var(--cth-ink-300)', fontVariantNumeric: 'tabular-nums' }} title={`출력 ${convTokensOut.toLocaleString()} 토큰`}>↓ {fmtTok(convTokensOut)}</span>
                  )}
                </div>
              </div>
            )}
          </>
        )}
      </div>
      {/* 맨 위·맨 아래 점프 — 긴 대화에서 빠르게(거노). 이미 끝이면 해당 버튼 숨김. */}
      <div style={{ position: 'absolute', right: 14, bottom: 12, display: 'flex', flexDirection: 'column', gap: 6, zIndex: 20 }}>
        {!atTop && (
          <button onClick={() => bodyRef.current?.scrollTo({ top: 0, behavior: 'smooth' })} title="맨 위로" style={SCROLL_BTN}>
            <svg width="15" height="15" viewBox="0 0 16 16" style={{ display: 'block' }}><path d="M8 12V4M4.5 7.5 8 4l3.5 3.5" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" /></svg>
          </button>
        )}
        {!atBottom && (
          <button onClick={() => { const el = bodyRef.current; if (el) el.scrollTo({ top: el.scrollHeight, behavior: 'smooth' }); }} title="맨 아래로" style={SCROLL_BTN}>
            <svg width="15" height="15" viewBox="0 0 16 16" style={{ display: 'block' }}><path d="M8 4v8M4.5 8.5 8 12l3.5-3.5" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" /></svg>
          </button>
        )}
      </div>
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

      {/* 학생 메타(하단) — 모델·effort·권한모드·브랜치·경로. 헤더 아래(상단)에서 입력창 위로
          내렸다(거노: 하단 통합). 컨텍스트%는 Footer '인연'으로 일원화해 여기선 뺐다. */}
      {!offline && agent && (convModel || agent.model || agent.branch || agent.cwd) && (
        <div style={{
          display: 'flex', flexWrap: 'wrap', gap: 6, alignItems: 'center',
          padding: '6px 12px', borderTop: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-50)',
        }}>
          {(convModel || agent.model) && <MetaChip label={shortModel(convModel || agent.model)} onClick={() => void sendToPane(surfaceId, '/model', true, false)} />}
          {(convModel || agent.model) && <MetaChip label={effort ? `effort: ${effort}` : 'effort'} onClick={() => void sendToPane(surfaceId, '/effort', true, false)} />}
          {modeLabel(mode) && <MetaChip label={modeLabel(mode)!} tone={mode === 'bypassPermissions' ? 'danger' : undefined} title="claude 권한 모드 (shift+tab 로 전환)" />}
          {agent.branch && <MetaChip label={`⎇ ${agent.branch}`} onClick={() => setConfirm({
            msg: '변경사항을 볼까요?',
            sub: `${agent.branch} 브랜치의 미커밋 변경(/diff)을 학생에게 띄워요.`,
            yes: '변경 보기',
            onYes: () => { void sendToPane(surfaceId, '/diff', true, false); },
          })} />}
          {agent.cwd && <MetaChip label={shortCwd(agent.cwd)} dim />}
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

      {/* 입력창 + 슬래시 자동완성 드롭다운 — '/' 치면 claude 명령 후보(거노). 오프라인
          세션은 입력창 대신 '현재 터미널에 입력' 이어가기 액션바를 띄운다(읽기 전용). */}
      {offline ? (
        <div style={{
          padding: '10px 12px', background: 'var(--cth-cream-50)',
          borderTop: '1px solid var(--cth-cream-200)', display: 'flex', flexDirection: 'column', gap: 8,
        }}>
          <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-500)' }}>
            이어가려면 터미널에 입력 후 직접 엔터로 실행하세요
          </div>
          <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
            <code style={{
              flex: 1, fontFamily: 'var(--cth-font-mono)', fontSize: 12, color: 'var(--cth-ink-700)',
              background: 'var(--cth-cream-100)', border: '1px solid var(--cth-cream-200)', borderRadius: 8,
              padding: '7px 10px', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap',
            }}>claude --resume {session!.id}</code>
            <button
              onClick={async () => {
                const ok = await pasteToActiveTerminal(`claude --resume ${session!.id}`, false);
                if (ok) { void revealTerminal(1); setFlash('ok'); } else { setFlash('err'); }
                setTimeout(() => setFlash(null), 2200);
              }}
              style={{
                flexShrink: 0, fontFamily: 'var(--cth-font-ui)', fontSize: 13, fontWeight: 600,
                padding: '7px 14px', border: 'none', borderRadius: 9, cursor: 'pointer',
                background: 'linear-gradient(180deg, #6BB0F0, #4A90E2)', color: '#fff',
                boxShadow: 'inset 0 1px 0 rgba(255,255,255,0.5)',
              }}
            >현재 터미널에 입력</button>
          </div>
          {flash === 'ok' && <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-mint)' }}>터미널에 입력했어요 — 엔터로 실행하세요</div>}
          {flash === 'err' && <div style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-coral)' }}>입력 실패 — 터미널 pane 을 확인하세요</div>}
        </div>
      ) : (
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
      {/* staged 이미지들 — 입력창 위 작은 썸네일이 줄지어(여러 개 다 보임, 안 덮임). X=전체
          취소(claude 입력의 [Image] 들도 \x15 로 비움). Enter 로 텍스트와 함께 전송. */}
      {hasPending && (
        <div style={{ display: 'flex', alignItems: 'center', flexWrap: 'wrap', gap: 6, padding: '8px 12px 0', background: 'var(--cth-cream-50)' }}>
          {pendingPreviews.map((src, i) => (
            <img key={i} src={src} alt="" style={{ width: 42, height: 42, objectFit: 'cover', borderRadius: 8, border: '1px solid var(--cth-cream-200)', display: 'block', flexShrink: 0 }} />
          ))}
          <button onClick={() => { setPendingPreviews([]); void sendToPane(surfaceId, '\x15', false); }} title="첨부 전체 취소" style={{
            width: 20, height: 20, borderRadius: 999, border: 'none', cursor: 'pointer',
            background: 'var(--cth-coral)', color: '#fff', fontSize: 13, lineHeight: 1,
            display: 'flex', alignItems: 'center', justifyContent: 'center', padding: 0, flexShrink: 0,
          }}>×</button>
          <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-500)' }}>{pendingPreviews.length}장 첨부 · Enter로 전송</span>
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
          onDragOver={(e) => { e.preventDefault(); }}
          onDrop={onDropImage}
          disabled={sending}
          placeholder="학생에게 지시 — 치는 대로 터미널에 실시간 · 이미지 드롭 첨부 · Enter 전송"
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
      )}

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
      {charPicker && (
        <CharacterPicker
          title="캐릭터 변경"
          note={`${avatarChar} → 바꾸면 이 학생의 claude 대화가 리셋돼요.`}
          onPick={(name) => { void swapCharacter(surfaceId, name); setCharPicker(false); }}
          onClose={() => setCharPicker(false)}
        />
      )}

    </div>
  );
}
