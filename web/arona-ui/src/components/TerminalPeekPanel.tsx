import { type CSSProperties, useEffect, useMemo, useRef, useState } from 'react';
import { fetchConversation, fetchTranscriptRaw, fetchSessionTranscriptRaw, fetchSubagents, fetchSubagentTranscriptRaw, fetchPeek, fetchSentImages, imageFileUrl, openFile, sendToPane, pasteToActiveTerminal, revealTerminal, closeAgent, swapCharacter, type Turn } from '@/lib/mcp';
import { SpritePortrait } from './SpritePortrait';
import { CharacterPicker } from './CharacterPicker';
import { Markdown } from './Markdown';
import { ToolUseCard } from './tool-use-card';
import { ThinkingBlock } from './thinking-block';
import { buildToolMap, type ToolMap } from '@/lib/build-tool-map';
import type { SessionEvent } from '@/lib/types';
import { AnsiText } from './AnsiText';
import { useStore, type SubagentInfo } from '@/store';

// 대화 본문 = transcript jsonl(/transcript-raw, raw SessionEvent[]). ccsv 파서·per-tool
// 렌더를 이식해 Bash/Edit/Read 도구 호출이 카톡 버블 사이에 카드로 인터리브된다(거노:
// 데스크탑 앱처럼 보기좋게). 캡처 프록시(/conversation)는 AskUserQuestion 선택지·라이브
// streaming·effort/model 표시의 보너스 소스로만 — 프록시 꺼지면 그 부분만 비활성(본문은
// jsonl 로 멀쩡). /model·권한 프롬프트는 화면(peek) 폴백.

// board.model 은 상태바 파싱 표시명("Opus 4.8 (1M context)") 우선 — claude- id 면 포맷,
// 아니면(이미 표시명) 그대로. id 의 `[1m]` 마커(1M 컨텍스트 변형)는 떼고 " (1M)" 로 표기 —
// 안 떼면 `-(\d+)-(\d+)$` 버전 포맷이 [1m] 접미사 때문에 안 먹어 "Opus-4-8[1m]" 로 깨졌다.
const shortModel = (m?: string) => {
  if (!m) return '';
  if (!m.startsWith('claude-')) return m;
  const has1m = /\[1m\]/i.test(m);
  const base = m.replace(/\[1m\]/i, '').replace('claude-', '').replace(/-(\d+)-(\d+)$/, ' $1.$2').replace(/^./, (c) => c.toUpperCase());
  return has1m ? `${base} (1M)` : base;
};
const shortCwd = (p?: string) => (!p ? '' : p.split('/').filter(Boolean).slice(-2).join('/'));
// effort 표시 — ultracode 는 xhigh 와 thinking budget 이 같아 모델칩처럼 구분이 필요(거노). 프록시가
// output_config.effort 로 "ultracode" 를 그대로 주므로, 그 값일 때만 풀어서 표기한다.
const shortEffort = (e?: string | null) => (!e ? '' : e === 'ultracode' ? 'ultracode (xhigh+workflows)' : e);


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
  // compact 직후 들어오는 요약 이어가기 메시지("This session is being continued…")는 user 턴이지만
  // isMeta 가 없어 선생님 노란 버블로 통째로 샜다(거노: 엄청 길게 내가 보냈다고 뜸). 시작 문구로 격리.
  return /\[Request interrupted|^\s*##\s*Context Usage|^\s*Caveat:\s|^\s*This session is being continued from a previous conversation/i.test(text);
}

// 프록시 strip_meta(proxy.rs)의 TS 미러 — transcript 경로(events)는 프록시를 안 거치니 여기서
// 같은 격리를 한다. 시스템이 user 턴에 주입하는 메타 블록(task-notification·system-reminder·
// caveat 등)을 통째로 제거. 다 지우고 빈 문자열이면 호출부가 그 버블을 스킵(거노: 서브에이전트
// /background 알림이 내 노란 말풍선으로 샜다). command 태그는 parseSlashCommand 가 카드로 승격.
const META_BLOCKS: [string, string][] = [
  ['<system-reminder>', '</system-reminder>'],
  ['<command-message>', '</command-message>'],
  ['<command-name>', '</command-name>'],
  ['<command-args>', '</command-args>'],
  ['<local-command-stdout>', '</local-command-stdout>'],
  ['<task-notification>', '</task-notification>'],
  ['<local-command-caveat>', '</local-command-caveat>'],
];
function stripMeta(text: string): string {
  let s = text;
  for (const [open, close] of META_BLOCKS) {
    for (;;) {
      const start = s.indexOf(open);
      if (start < 0) break;
      const rel = s.indexOf(close, start + open.length);
      if (rel < 0) { s = s.slice(0, start); break; } // 닫힘 없으면 이후 전부 버림(잘린 래퍼)
      s = s.slice(0, start) + s.slice(rel + close.length);
    }
  }
  // 이미지 플레이스홀더 줄 제거 — 실제 이미지는 image 블록으로 따로 렌더.
  s = s.split('\n').filter((l) => {
    const t = l.trim();
    return !(t.startsWith('[Image: source:') || t.startsWith('[Image: original ') || (t.startsWith('[Image #') && t.endsWith(']')));
  }).join('\n');
  return s.trim();
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

// ISO timestamp → "오후 2:47"(카톡식 메시지 시각). 파싱 실패면 빈 문자열.
function fmtClock(iso?: string): string {
  if (!iso) return '';
  const d = new Date(iso);
  if (isNaN(d.getTime())) return '';
  let h = d.getHours();
  const ampm = h < 12 ? '오전' : '오후';
  h = h % 12; if (h === 0) h = 12;
  return `${ampm} ${h}:${String(d.getMinutes()).padStart(2, '0')}`;
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
  | { kind: 'bubble'; role: string; text: string; uuid?: string; ts?: string; cancelled?: boolean }
  | { kind: 'tool'; toolUse: { id?: string; name?: string; input?: unknown }; pair: ReturnType<ToolMap['get']> }
  | { kind: 'thinking'; text: string }
  | { kind: 'command'; commandName: string; commandArgs?: string; commandMessage?: string }
  | { kind: 'local-command'; stdout: string }
  | { kind: 'qa'; qa: { q: string; a: string }[] }
  | { kind: 'launch'; subagentType?: string; description?: string }
  | { kind: 'system'; text: string };

// AskUserQuestion tool_result content("...answered: \"질문\"=\"답\". ...")에서 질문↔답 쌍 추출.
// multiSelect 답은 한 쌍 안에 콤마로 묶여 온다. content 가 배열이면 text 블록을 잇는다.
function parseAnsweredPairs(content: unknown): Map<string, string> {
  const m = new Map<string, string>();
  const s = typeof content === 'string'
    ? content
    : Array.isArray(content) ? content.map((x) => (x as { text?: string })?.text ?? '').join('') : '';
  const re = /"([^"]+)"\s*=\s*"([^"]*)"/g;
  let mm: RegExpExecArray | null;
  while ((mm = re.exec(s))) m.set(mm[1], mm[2]);
  return m;
}

// 한 user 텍스트가 슬래시 명령/로컬 출력 태그를 품으면 카드/버블 아이템으로, 아니면 일반
// 버블로 푸시. 시스템 주입 잔여는 isSystemInjectionText 로 계속 숨긴다.
function pushUserText(items: RenderItem[], text: string, ts?: string): void {
  const parsed = parseSlashCommand(text);
  if (parsed?.kind === 'command') {
    items.push({ kind: 'command', commandName: parsed.commandName, commandArgs: parsed.commandArgs, commandMessage: parsed.commandMessage });
  } else if (parsed?.kind === 'local-command') {
    if (parsed.stdout.trim()) items.push({ kind: 'local-command', stdout: parsed.stdout });
  } else {
    // 메타 블록(task-notification 등) 제거 후 남는 게 있고 시스템 주입 잔여가 아닐 때만 버블로.
    const clean = stripMeta(text);
    if (clean && !isSystemInjectionText('user', clean)) {
      items.push({ kind: 'bubble', role: 'user', text: clean, ts });
    }
  }
}

// keepSidechain: 서브에이전트 단독 뷰는 jsonl 전체가 isSidechain:true 이므로 살려야 한다.
// 메인 대화에선 sidechain(소환된 서브에이전트 줄)이 노이즈라 기본 제외.
function eventsToItems(events: SessionEvent[], toolMap: ToolMap, keepSidechain = false): RenderItem[] {
  const items: RenderItem[] = [];
  for (const ev of events) {
    if (!keepSidechain && (ev as { isSidechain?: boolean }).isSidechain) continue;
    if (ev.type === 'system') {
      const text = flattenSystem(ev);
      if (text) items.push({ kind: 'system', text });
      continue;
    }
    if (ev.type !== 'user' && ev.type !== 'assistant') continue;
    const role = ev.type;
    const uuid = (ev as { uuid?: string }).uuid;
    const ts = (ev as { timestamp?: string }).timestamp; // 카톡식 메시지 시각
    const content = (ev as { message?: { content?: unknown } }).message?.content;
    // esc 취소 — content 가 "[Request interrupted by user]" 마커인 user 이벤트. esc 는 학생이 답하던
    // 도중에 누르므로 마커 직전엔 거의 항상 (중단된) assistant 턴이 있다. 예전엔 그 assistant 버블에서
    // break 해 "답 나옴=취소 아님"으로 처리했는데, 그 탓에 진짜 취소가 안 잡혔다(거노: 취소 표시 안 됨).
    // → assistant/thinking/tool 은 건너뛰고, 가장 가까운 user 버블을 끊긴 프롬프트로 표시(회색+취소선).
    // 마커 자체는 안 띄운다.
    if (role === 'user') {
      const flat = typeof content === 'string' ? content
        : Array.isArray(content) ? content.map((b) => (b && typeof b === 'object' && 'text' in b ? String((b as { text?: string }).text ?? '') : '')).join(' ') : '';
      if (/^\s*\[Request interrupted by user/.test(flat)) {
        for (let k = items.length - 1; k >= 0; k--) {
          const prev = items[k];
          if (prev.kind === 'bubble' && prev.role === 'user') { prev.cancelled = true; break; }
        }
        continue;
      }
    }
    if (typeof content === 'string') {
      if (role === 'user') pushUserText(items, content, ts);
      else if (content.trim()) items.push({ kind: 'bubble', role, text: content, uuid, ts });
    } else if (Array.isArray(content)) {
      for (const block of content) {
        if (!block || typeof block !== 'object') continue;
        const b = block as { type?: string; text?: string; thinking?: string; id?: string; name?: string; input?: unknown };
        if (b.type === 'text' && typeof b.text === 'string' && b.text.trim()) {
          if (role === 'user') pushUserText(items, b.text, ts);
          else items.push({ kind: 'bubble', role, text: b.text, uuid, ts });
        } else if (b.type === 'thinking' && typeof b.thinking === 'string' && b.thinking.trim()) {
          items.push({ kind: 'thinking', text: b.thinking });
        } else if (b.type === 'tool_use' && b.name === 'AskUserQuestion') {
          // 답변 완료된 질문만 "선생님이 답함" 카드로(진행 중 미답은 라이브 menu 카드가 전담).
          const pair = b.id ? toolMap.get(b.id) : undefined;
          const answered = pair?.toolResult?.content;
          if (answered != null) {
            const ans = parseAnsweredPairs(answered);
            const qsRaw = (b.input as { questions?: unknown })?.questions;
            const qs = Array.isArray(qsRaw) ? (qsRaw as { question?: string; header?: string }[]) : [];
            const qa = qs.map((q) => ({ q: q.question ?? q.header ?? '질문', a: ans.get(q.question ?? '') ?? ans.get(q.header ?? '') ?? '—' }));
            if (qa.length) items.push({ kind: 'qa', qa });
          }
        } else if (b.type === 'tool_use' && b.name === 'Agent') {
          // 서브에이전트 소환 — 전체 카드(프롬프트/결과) 대신 마커 한 줄(상세는 ↳ 드릴인).
          const inp = b.input as { subagent_type?: string; description?: string };
          items.push({ kind: 'launch', subagentType: inp?.subagent_type, description: inp?.description });
        } else if (b.type === 'tool_use') {
          items.push({ kind: 'tool', toolUse: { id: b.id, name: b.name, input: b.input }, pair: b.id ? toolMap.get(b.id) : undefined });
        }
      }
    }
  }
  return items;
}

// 작업 중 큐(예약)된 메시지 — claude 가 enqueue/remove 를 transcript 의 `queue-operation`
// 이벤트로 기록한다(거노: 작업 중 보낸 예약 메시지가 GUI 에 안 떴다). FIFO 로 enqueue push /
// remove shift → 남은 게 현재 대기 중. task-notification 등 시스템 주입도 enqueue 되니
// stripMeta 후 빈 건 제외하고, 사람이 친 진짜 텍스트만 "예약" 버블로 띄운다.
function queuedMessages(events: SessionEvent[]): { text: string; ts?: string }[] {
  const q: { text: string; ts?: string }[] = [];
  for (const ev of events) {
    if ((ev as { type?: string }).type !== 'queue-operation') continue;
    const op = (ev as { operation?: string }).operation;
    if (op === 'enqueue') q.push({ text: typeof (ev as { content?: unknown }).content === 'string' ? (ev as { content: string }).content : '', ts: (ev as { timestamp?: string }).timestamp });
    else if (op === 'remove') q.shift();
  }
  // compact 이어가기 요약("This session is being continued…")도 큐로 들어와 노란 예약 버블로
  // 샜다(거노: 엄청 길게 내가 보냈다고 뜸) → stripMeta 후 시스템 주입 격리까지 적용.
  return q.map((e) => ({ text: stripMeta(e.text), ts: e.ts })).filter((e) => e.text && !isSystemInjectionText('user', e.text));
}

// 예약(큐) 메시지가 "보낸 시각" — clean 텍스트 → 첫 enqueue timestamp. 큐가 나중에 처리돼 본문
// user 버블로 들어가면 표시 시각이 "처리 시각"이라, 재시작 후 큐가 한꺼번에 처리되면 다 끝에 몰려
// 보였다(거노: 대화 사이에 안 껴있다). 이 맵으로 본문 버블 시각을 보낸(enqueue) 시각으로 되돌린다.
function enqueueTimes(events: SessionEvent[]): Map<string, string> {
  const m = new Map<string, string>();
  for (const ev of events) {
    if ((ev as { type?: string }).type !== 'queue-operation') continue;
    if ((ev as { operation?: string }).operation !== 'enqueue') continue;
    const raw = typeof (ev as { content?: unknown }).content === 'string' ? (ev as { content: string }).content : '';
    const clean = stripMeta(raw).trim();
    const ts = (ev as { timestamp?: string }).timestamp;
    if (clean && ts && !m.has(clean)) m.set(clean, ts);
  }
  return m;
}

// 큐↔본문 매칭용 정규화 — 처리된 예약이 큐에서 안 빠져 맨 아래 우르르 남았다(거노). 공백 collapse +
// 이미지 토큰([Image #N]) 제거로 본문 user 버블과 느슨하게 비교한다(재시작 시 remove 누락·텍스트
// 분리로 정확 일치가 안 됐다). 정규화 후 빈 문자열이면 매칭하지 않는다(false positive 방지).
function normForMatch(t: string): string {
  return t.replace(/\[Image #\d+\]/g, ' ').replace(/\s+/g, ' ').trim();
}

// 마지막 /effort 설정값 — ultracode 는 API 에 xhigh 로 실려(공식: ultracode 라는 effort 값은 없음,
// 최대 xhigh + 워크플로 오케스트레이션은 CLI 측) 프록시로는 xhigh 와 구분 불가다. 대신 /effort 실행
// stdout("Set effort level to ultracode")이 transcript 에 남으니 그걸 역순으로 찾아 최신값을 잡는다.
function lastSetEffort(events: SessionEvent[]): string | null {
  for (let i = events.length - 1; i >= 0; i--) {
    const c = (events[i] as { message?: { content?: unknown } }).message?.content;
    const flat = typeof c === 'string' ? c
      : Array.isArray(c) ? c.map((b) => (b && typeof b === 'object' && 'text' in b ? String((b as { text?: string }).text ?? '') : '')).join(' ') : '';
    const m = flat.match(/Set effort level to (\w+)/);
    if (m) return m[1].toLowerCase();
  }
  return null;
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
  /** 서브에이전트 드릴인 — 부모 pane(parentSurface)이 소환한 서브에이전트(agentId)의
   *  대화를 따로 보는 모드. 읽기 전용(입력 없음). 있으면 surfaceId 는 빈 값('')으로 들어온다. */
  subagent?: { parentSurface: string; agentId: string; agentType: string; label: string };
  /** 메타칸 서브에이전트 칩 클릭 → 부모가 별도 타일로 그 서브에이전트 대화를 연다. */
  onOpenSubagent?: (parentSurface: string, agentId: string, agentType: string, label: string) => void;
  /** 제목 더블클릭 → 이 타일을 임시 전체화면(터미널 toggle_pane_zoom 의 arona 판). */
  onToggleZoom?: () => void;
  /** 현재 이 타일이 줌(전체화면) 상태 — 헤더 아이콘 표시용. */
  zoomed?: boolean;
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
  const m = screen.match(/\b(low|medium|high|xhigh|max|ultracode)\b\s*·\s*\/effort/i);
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

// 말풍선 본문 — 텍스트 + 첨부 이미지. claude 가 붙여넣은 이미지는 transcript 에
// "[Image: source: <abs-path>]" 텍스트로 남는다 → 그 토큰을 실제 <img> 로 치환(거노:
// 이미지가 텍스트로 보이던 것). 이미지 토큰이 없으면 기존 렌더(user=span / assistant=md).
// 경로는 실제 이미지 확장자로 끝나고 따옴표·꺾쇠·개행이 없어야 한다 — transcript 가
// 이 토큰 포맷을 다루는 소스코드/문서를 담을 때 "[Image: source: ..."] 문자열이 코드
// 한복판에서 잡혀 가짜 경로로 로드되던 오탐(400) 차단.
const IMG_TOKEN = /\[Image:\s*source:\s*([^\]\n"'`<>]+\.(?:png|jpe?g|gif|webp|bmp))\s*\]/gi;
function BubbleBody({ text, mine }: { text: string; mine: boolean }) {
  const plain = (t: string, key: string) => mine
    ? <span key={key} style={{ whiteSpace: 'pre-wrap', wordBreak: 'break-word' }}>{t}</span>
    : <Markdown key={key} text={t} />;
  if (!IMG_TOKEN.test(text)) return plain(text, 't');
  IMG_TOKEN.lastIndex = 0;
  const parts: React.ReactNode[] = [];
  let last = 0, m: RegExpExecArray | null, k = 0;
  while ((m = IMG_TOKEN.exec(text)) !== null) {
    const before = text.slice(last, m.index);
    if (before.trim()) parts.push(plain(before, `t${k}`));
    parts.push(
      <img key={`i${k}`} src={imageFileUrl(m[1].trim())} alt="" style={{
        display: 'block', maxWidth: '100%', maxHeight: 240, borderRadius: 10,
        objectFit: 'contain', marginTop: parts.length ? 6 : 0,
      }} />
    );
    last = m.index + m[0].length; k++;
  }
  const tail = text.slice(last);
  if (tail.trim()) parts.push(plain(tail, 'tt'));
  return <>{parts}</>;
}

// 입력창 폐기 자리에 그 학생의 현재 작업(TaskCreate)을 고정 — 터미널 상단 task 트리와 동형.
// 진행중(채운 원) → 대기(빈 원) → 완료(체크) 순. 소스는 transcript tool_use(TaskCreate/Update):
// /pane-tasks 는 task store 디렉토리 id 가 transcript uuid 와 달라 매핑이 깨져 빈값이 됐다(거노).
interface StripTask { id: string; subject: string; status: string }
const TASK_ORDER: Record<string, number> = { in_progress: 0, pending: 1, completed: 2 };
// transcript 의 TaskCreate("Task #N created…" result 로 id) + TaskUpdate(taskId/status)를 누적 재구성.
function tasksFromEvents(events: SessionEvent[], toolMap: ToolMap): StripTask[] {
  // compact 이어가기 경계 이후만 — Task #N 번호가 compact 마다 리셋돼 충돌하고, 옛 세대의 완료
  // 태스크가 계속 끌려와 stale 했다(거노: 업무탭엔 없는데 하단엔 옛 태스크가 떴다). 마지막 경계부터.
  let start = 0;
  for (let i = 0; i < events.length; i++) {
    const ev = events[i];
    if ((ev as { isCompactSummary?: boolean }).isCompactSummary) { start = i + 1; continue; }
    const c = (ev as { message?: { content?: unknown } }).message?.content;
    const flat = typeof c === 'string' ? c : Array.isArray(c) ? c.map((b) => (b && typeof b === 'object' && 'text' in b ? String((b as { text?: string }).text ?? '') : '')).join(' ') : '';
    if (/^\s*This session is being continued from a previous conversation/.test(flat)) start = i + 1;
  }
  const tasks = new Map<string, StripTask>();
  const resultText = (id?: string): string => {
    if (!id) return '';
    const c = toolMap.get(id)?.toolResult?.content;
    if (typeof c === 'string') return c;
    if (Array.isArray(c)) return c.map((x) => (x && typeof x === 'object' && 'text' in x ? String((x as { text?: string }).text ?? '') : '')).join(' ');
    return '';
  };
  for (const ev of events.slice(start)) {
    const content = (ev as { message?: { content?: unknown } }).message?.content;
    if (!Array.isArray(content)) continue;
    for (const block of content) {
      const b = block as { type?: string; id?: string; name?: string; input?: unknown };
      if (b.type !== 'tool_use') continue;
      if (b.name === 'TaskCreate') {
        const subject = (b.input as { subject?: string })?.subject ?? '';
        const m = resultText(b.id).match(/Task #(\d+)/);
        if (m) tasks.set(m[1], { id: m[1], subject, status: 'pending' });
      } else if (b.name === 'TaskUpdate') {
        const inp = b.input as { taskId?: string; status?: string; subject?: string };
        const t = inp?.taskId != null ? tasks.get(String(inp.taskId)) : undefined;
        if (t) { if (inp.status) t.status = inp.status; if (inp.subject) t.subject = inp.subject; }
      }
    }
  }
  return [...tasks.values()];
}
function TaskStrip({ tasks }: { tasks: StripTask[] }) {
  const [collapsed, setCollapsed] = useState(false); // 헤더 클릭으로 목록 접기/펼치기(거노)
  const active = useMemo(
    () => tasks
      .filter((t) => t.status !== 'deleted')
      .sort((a, b) => (TASK_ORDER[a.status] ?? 3) - (TASK_ORDER[b.status] ?? 3)),
    [tasks],
  );
  // 진행/대기 작업이 하나도 없으면(다 완료거나 비었으면) 목록 대신 안내만 — 끝난 태스크가 계속
  // 떠 "지금 할 게 없는데 태스크가 있다"는 혼란을 막는다(거노).
  const hasLive = active.some((t) => t.status === 'in_progress' || t.status === 'pending');
  if (!hasLive) return (
    <div style={{
      padding: '9px 12px', background: 'var(--cth-cream-50)', borderTop: '1px solid var(--cth-cream-200)',
      fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-300)', textAlign: 'center',
    }}>읽기 전용 뷰어 · 입력은 터미널에서</div>
  );
  const done = active.filter((t) => t.status === 'completed').length;
  return (
    <div style={{
      borderTop: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-50)',
      maxHeight: 168, display: 'flex', flexDirection: 'column',
    }}>
      <button onClick={() => setCollapsed((c) => !c)} title={collapsed ? '작업 펼치기' : '작업 접기'} style={{
        padding: '7px 12px 4px', fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700,
        letterSpacing: 0.3, color: 'var(--cth-ink-300)', textTransform: 'uppercase',
        display: 'flex', alignItems: 'center', justifyContent: 'space-between',
        border: 'none', background: 'transparent', cursor: 'pointer', width: '100%',
      }}>
        <span style={{ display: 'inline-flex', alignItems: 'center', gap: 5 }}>
          <svg width="9" height="9" viewBox="0 0 16 16" style={{ transform: collapsed ? 'rotate(-90deg)' : 'none', transition: 'transform 120ms ease' }}><path d="M3 6l5 5 5-5" stroke="currentColor" strokeWidth="2" fill="none" strokeLinecap="round" strokeLinejoin="round" /></svg>
          현재 작업
        </span>
        <span style={{ fontWeight: 600 }}>{done}/{active.length}</span>
      </button>
      {!collapsed && (
      <div style={{ overflowY: 'auto', padding: '0 12px 8px', display: 'flex', flexDirection: 'column', gap: 3 }}>
        {active.map((t) => {
          const ip = t.status === 'in_progress';
          const cp = t.status === 'completed';
          return (
            <div key={t.id} style={{ display: 'flex', alignItems: 'flex-start', gap: 7, lineHeight: 1.35 }}>
              <span style={{ flexShrink: 0, marginTop: 2, width: 13, height: 13, display: 'inline-flex' }}>
                {cp ? (
                  <svg width="13" height="13" viewBox="0 0 16 16"><circle cx="8" cy="8" r="7" fill="var(--cth-mint)" /><path d="M4.5 8.2l2.2 2.2 4.8-4.8" stroke="#fff" strokeWidth="1.8" fill="none" strokeLinecap="round" strokeLinejoin="round" /></svg>
                ) : ip ? (
                  <svg width="13" height="13" viewBox="0 0 16 16"><circle cx="8" cy="8" r="7" fill="var(--cth-sky)" /><circle cx="8" cy="8" r="2.6" fill="#fff" /></svg>
                ) : (
                  <svg width="13" height="13" viewBox="0 0 16 16"><circle cx="8" cy="8" r="6.2" fill="none" stroke="var(--cth-ink-300)" strokeWidth="1.6" /></svg>
                )}
              </span>
              <span style={{
                fontFamily: 'var(--cth-font-ui)', fontSize: 12,
                fontWeight: ip ? 700 : 500,
                color: cp ? 'var(--cth-ink-300)' : ip ? 'var(--cth-ink-900)' : 'var(--cth-ink-700)',
              }}>{t.subject}</span>
            </div>
          );
        })}
      </div>
      )}
    </div>
  );
}

// 학생별 인연·재화 strip — 전역 Footer 합계 대신 그 학생 값을 채팅방 하단에(거노: 통합).
// 인연 = 컨텍스트 사용량%, 재화 = 누적 입력토큰(💎)·비용$(🪙).
function StudentStats({ contextPct = 0, tokensIn = 0, costUsd = 0 }: { contextPct?: number; tokensIn?: number; costUsd?: number }) {
  const pct = Math.max(0, Math.min(100, Math.round(contextPct)));
  return (
    <div style={{ display: 'flex', alignItems: 'center', gap: 8, padding: '6px 12px', borderTop: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-50)' }}>
      <svg width="13" height="13" viewBox="0 0 16 16" style={{ flexShrink: 0 }}>
        <path d="M8 13.5 2.5 8a3 3 0 0 1 4.2-4.2L8 5l1.3-1.2A3 3 0 0 1 13.5 8L8 13.5Z" fill="var(--cth-coral)" stroke="var(--cth-ink-900)" strokeWidth="1.2" strokeLinejoin="round" />
      </svg>
      <div title={`컨텍스트 사용량 ${pct}%`} style={{ position: 'relative', flex: 1, minWidth: 36, maxWidth: 130, height: 8, borderRadius: 999, background: 'var(--cth-cream-200)', overflow: 'hidden' }}>
        <div style={{ position: 'absolute', inset: 0, width: `${pct}%`, background: 'linear-gradient(90deg,#FF8FB1,#FF6B6B)', borderRadius: 999, transition: 'width .5s cubic-bezier(0.22,1,0.36,1)' }} />
      </div>
      <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 10, color: 'var(--cth-ink-500)', whiteSpace: 'nowrap' }}>{pct}%</span>
      <div style={{ flex: 1 }} />
      <span title={`누적 입력 ${tokensIn.toLocaleString()} 토큰`} style={{ display: 'inline-flex', alignItems: 'center', gap: 4, fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: 600, color: 'var(--cth-ink-700)' }}>
        <svg width="13" height="13" viewBox="0 0 16 16" style={{ flexShrink: 0 }}><path d="M5 2h6l3 4-6 8-6-8 3-4Z" fill="var(--cth-sky)" stroke="var(--cth-ink-900)" strokeWidth="1.2" strokeLinejoin="round" /></svg>
        {fmtTok(tokensIn)}
      </span>
      <span title={`누적 비용 $${costUsd.toFixed(4)}`} style={{ display: 'inline-flex', alignItems: 'center', gap: 4, fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: 600, color: 'var(--cth-ink-700)' }}>
        <svg width="13" height="13" viewBox="0 0 16 16" style={{ flexShrink: 0 }}><circle cx="8" cy="8" r="6.2" fill="var(--cth-lemon)" stroke="var(--cth-ink-900)" strokeWidth="1.2" /><path d="M8 5.5v5M6.5 8h3" stroke="var(--cth-ink-900)" strokeWidth="1.1" opacity="0.7" /></svg>
        ${costUsd.toFixed(2)}
      </span>
    </div>
  );
}

// 화면(raw 터미널)은 '터미널 보기'로 보면 되므로 여기엔 두지 않는다.
export function TerminalPeekPanel({ surfaceId, title, onClose, embedded, session, subagent, onOpenSubagent, onToggleZoom, zoomed }: TerminalPeekPanelProps) {
  const offline = !!session;
  const isSub = !!subagent;
  const [subList, setSubList] = useState<SubagentInfo[]>([]);
  const agent = useStore((s) => s.agents.find((a) => a.id === surfaceId));
  // 서브 모드: 부모(오케스트레이터) 학생을 board 에서 조회 — user 턴 아바타로 쓴다.
  const parentAgent = useStore((s) => s.agents.find((a) => a.id === (subagent?.parentSurface ?? '__none__')));
  // 아바타는 board(라이브) 캐릭터명 우선 — title(클릭 시점 고정)이 pane id('%3')로 깨졌을 때
  // 보강(거노: 프사 %). board 도 id 면 SpritePortrait 가 사람 실루엣으로 막는다.
  // 서브 모드의 assistant 턴은 학생이 아닌 서브에이전트라 agentType 으로(미도리와 구분 = 실루엣).
  const avatarChar = isSub
    ? (subagent!.agentType || subagent!.label || 'agent')
    : (agent?.character && !/^%?\d+$/.test(agent.character) ? agent.character : title);
  // 서브 모드 user(부모 지시) 턴 아바타 — 부모 학생 스프라이트. 없으면 title 의 prefix(부모명).
  const parentAvatarChar = parentAgent?.character && !/^%?\d+$/.test(parentAgent.character)
    ? parentAgent.character
    : (title.split('↳')[0].trim() || title);
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
  const [mode, setMode] = useState<string | null>(null); // 권한 모드(transcript permissionMode) — 헤더 칩
  const [convModel, setConvModel] = useState(''); // 캡처 프록시가 요청에서 잡은 model(거노: 화면스크랩 대신 프록시 소스)
  const [effortMenu, setEffortMenu] = useState<number | null>(null); // /effort 슬라이더 현재 idx(뜬 동안)
  const [metaOpen, setMetaOpen] = useState(false); // 하단 메타 칸 펼침 — 평소 접힘, 누르면 모델·effort·권한·브랜치·cwd·서브에이전트(거노)
  const [flash, setFlash] = useState<'ok' | 'err' | null>(null);
  // window.confirm 은 wry webview(macOS)에서 무반응 — 자체 확인 모달로 종료·compact 확인(거노).
  const [confirm, setConfirm] = useState<{ msg: string; sub?: string; danger?: boolean; yes: string; onYes: () => void } | null>(null);
  const [charPicker, setCharPicker] = useState(false); // 캐릭터 변경 팝업(헤더 버튼)
  const [titleHover, setTitleHover] = useState(false); // 제목 더블클릭 줌 — hover 시 인터랙션 힌트
  const [atTop, setAtTop] = useState(true); // 스크롤 맨위 — true 면 ↑ 버튼 숨김
  const [atBottom, setAtBottom] = useState(true); // 스크롤 맨아래 — true 면 ↓ 버튼 숨김
  // 슬래시 자동완성 — 입력이 '/' 로 시작하면 claude-code 명령 드롭다운(↑↓ 선택·Tab/Enter 완성).
  const [navIdx, setNavIdx] = useState(0); // 선택지 카드 키보드 네비(↑↓) 하이라이트
  // /context 출력 정리본 — GUI 모달 새로 만들지 말고(거노) 터미널 /context 화면(peek)을
  // 정리해 대화창 안에 보여준다. null=비활성, 그 외=정리된 그리드 텍스트.
  const [ctxView, setCtxView] = useState<string | null>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
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
    setMode(null);
    setImages([]);
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
    // 서브에이전트 드릴인: 부모 pane 의 subagents/agent-<id>.jsonl 을 폴링(진행 중이면
    // 계속 append 되므로). 라이브 소스(conv/peek/menu)는 이 surface 엔 없어 skip.
    if (subagent) {
      const tickSub = async () => {
        const evts = await fetchSubagentTranscriptRaw(subagent.parentSurface, subagent.agentId);
        if (stopped) return;
        setEvents(evts);
        setLoaded(true);
      };
      void tickSub();
      const ivSub = setInterval(tickSub, 1500);
      return () => { stopped = true; clearInterval(ivSub); };
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
  }, [surfaceId, offline, session?.id, session?.cwd, subagent?.parentSurface, subagent?.agentId]);

  // 메타칸 드릴인 목록 — 라이브 부모 pane 일 때만 그 pane 의 서브에이전트(완료분 포함)를
  // 폴링. subagents/ 디렉토리 읽기라 가볍다. 오프라인/서브에이전트 모드엔 불필요.
  useEffect(() => {
    if (offline || isSub || !surfaceId) { setSubList([]); return; }
    let stopped = false;
    const load = () => { void fetchSubagents(surfaceId).then((s) => { if (!stopped) setSubList(s); }); };
    load();
    const iv = setInterval(load, 3000);
    return () => { stopped = true; clearInterval(iv); };
  }, [offline, isSub, surfaceId]);

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

  // 이미지 드롭(아로나 대화창 어디든) → 그 학생 claude 에 첨부. dataTransfer 의 첫
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

  // jsonl events → 카톡 렌더 아이템(text 버블 + per-tool 카드 + thinking). tool_use_id
  // 페어링(buildToolMap)으로 카드가 결과/diff/stats 까지 그린다.
  const toolMap = useMemo(() => buildToolMap(events), [events]);
  // 입력창 자리 task 고정 — transcript 의 TaskCreate/Update 로 현재 작업 목록 재구성.
  const taskList = useMemo(() => tasksFromEvents(events, toolMap), [events, toolMap]);
  // 턴별 소요시간 — 마지막 assistant uuid → ms. 그 버블 아래 시계 푸터.
  const durationMap = useMemo(() => turnDurations(events), [events]);
  // 턴별 출력 토큰 — assistant uuid → output_tokens. 시계 푸터 옆 "↓N"(완료 응답·정확).
  const tokenMap = useMemo(() => turnTokens(events), [events]);
  const items = useMemo(() => {
    if (events.length) {
      const list = eventsToItems(events, toolMap, isSub);
      // 큐를 거쳐 처리된 user 버블은 표시 시각을 보낸(enqueue) 시각으로(거노: 재시작 후 몰림).
      const qts = enqueueTimes(events);
      if (qts.size) for (const it of list) {
        if (it.kind === 'bubble' && it.role === 'user') {
          const t = qts.get(it.text.trim());
          if (t) it.ts = t;
        }
      }
      // 보낸 시각순 정렬 — 큐를 거친 메시지는 위에서 ts 를 보낸(enqueue) 시각으로 되돌렸지만 위치는
      // 처리 시각 순(맨 아래)이라 시각만 맞고 몰려 보였다(거노: 시간별로 정렬하면 될 듯). ts 로 stable
      // sort 하고, ts 없는 아이템(thinking/tool/qa/launch)은 직전 ts 를 상속해 같은 턴에 붙어 따라간다.
      let prevTs = 0;
      const keyed = list.map((it, i) => {
        const raw = (it as { ts?: string }).ts;
        const t = raw ? Date.parse(raw) : NaN;
        const eff = Number.isNaN(t) ? prevTs : (prevTs = t);
        return { it, eff, i };
      });
      keyed.sort((a, b) => a.eff - b.eff || a.i - b.i);
      return keyed.map((k) => k.it);
    }
    // jsonl 이 아직 안 써진 진행 중 구간 — 캡처 프록시 텍스트 대화로 라이브 폴백. conv.turns 는
    // 프록시가 strip_meta 로 정제하지만, user 턴은 한 번 더 격리해(이중 안전) 남는 게 있을 때만.
    return convTurns
      .map((t) => ({ role: t.role, text: t.role === 'user' ? stripMeta(t.text) : t.text }))
      .filter((t) => t.text.trim() && !isSystemInjectionText(t.role, t.text))
      .map((t) => ({ kind: 'bubble', role: t.role, text: t.text }) as RenderItem);
  }, [events, toolMap, convTurns, isSub]);
  // optimistic 으로 남긴 내 발화(myChoices) 중 transcript(items)에 아직 안 잡힌 것만 — 작업
  // 중 미리 보낸 메시지를 즉시 띄우되(거노), 다음 폴에서 transcript 에 같은 텍스트가 뜨면
  // 중복 제거. 메뉴 선택(label)은 transcript 에 raw 키로 들어가 매칭 안 돼 계속 남는다(의도).
  const pendingChoices = useMemo(
    () => myChoices.filter((c) => !items.some((it) =>
      it.kind === 'bubble' && (it as { role: string }).role === c.role
        && (it as { text: string }).text.trim() === c.text.trim())),
    [myChoices, items],
  );
  // 현재 큐(예약) 대기 중인 사용자 메시지 — transcript queue-operation 기반. 작업 중 보낸 게
  // 처리되기 전까지 "예약" 버블로(거노). 이미 본문(items)에 같은 텍스트가 떴으면(=처리됨) 제외.
  const queued = useMemo(() => {
    const userTexts = items
      .filter((it) => it.kind === 'bubble' && (it as { role: string }).role === 'user')
      .map((it) => normForMatch((it as { text: string }).text))
      .filter(Boolean);
    // 작업 중 보낸 큐 메시지는 처리해도 transcript 에 깨끗한 user 턴으로 안 남는다(queue-operation +
    // system-reminder 로만 기록) → 텍스트 매칭으로는 영영 못 뺀다(거노: 처리됐는데 밑에 다 남음).
    // 그래서 시각으로 판정: 마지막 학생(assistant) 응답보다 이전에 보낸 예약 = 내가 받아 처리한 것.
    let lastRespTs = 0;
    for (const it of items) {
      if (it.kind === 'bubble' && (it as { role: string }).role === 'assistant') {
        const raw = (it as { ts?: string }).ts;
        const t = raw ? Date.parse(raw) : NaN;
        if (!Number.isNaN(t) && t > lastRespTs) lastRespTs = t;
      }
    }
    return queuedMessages(events).filter((q) => {
      const n = normForMatch(q.text);
      if (!n) return false; // 정규화 후 빈(이미지/메타만) — 큐로 안 띄움
      // ① 본문 user 턴과 부분포함되면(idle 때 보낸 정식 발화) 처리됨 → 제외
      const matched = userTexts.some((u) =>
        u === n || (n.length >= 8 && u.includes(n)) || (u.length >= 8 && n.includes(u)));
      if (matched) return false;
      // ② 마지막 응답보다 이전 enqueue = 내가 받아 처리한 작업 중 메시지 → 제외. 응답 이후 보낸 것만 대기.
      const qt = q.ts ? Date.parse(q.ts) : NaN;
      if (lastRespTs && !Number.isNaN(qt) && qt < lastRespTs) return false;
      return true;
    });
  }, [events, items]);
  // 표시용 effort — 프록시(effort)는 ultracode 를 xhigh 로만 보므로, transcript 의 마지막 /effort 가
  // ultracode 이고 프록시도 xhigh(또는 미관측)면 ultracode 로 표기(거노: xhigh↔ultracode 구분).
  const displayEffort = useMemo(() => {
    const cmd = lastSetEffort(events);
    if (cmd === 'ultracode' && (effort === 'xhigh' || !effort)) return 'ultracode';
    return effort;
  }, [events, effort]);
  // 로딩 점 판정용 — 마지막 말풍선이 선생님(user)이면 학생이 아직 답하기 전.
  const lastBubbleRole = useMemo(() => {
    for (let i = items.length - 1; i >= 0; i--) if (items[i].kind === 'bubble') return (items[i] as { role: string }).role;
    return undefined;
  }, [items]);

  // 메타칸엔 진행 중 서브에이전트만(거노: 완료된 게 다 떠서 범람). async 서브는 board 가
  // '진행 중'을 못 잡으니, board 매칭 OR transcript 최근수정(60초)으로 진행을 추정한다.
  // 한번 연 타일은 완료돼도 유지되니(별도 타일), 끝난 건 목록에서만 빠진다.
  const runningSubs = useMemo(() => {
    const now = Date.now() / 1000;
    const live = new Set(agent?.subagents ?? []);
    return subList.filter((s) => live.has(s.description) || now - s.mtime < 60);
  }, [subList, agent?.subagents]);

  return (
    <div
      style={{
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
        <span
          onDoubleClick={onToggleZoom}
          onMouseEnter={() => { if (onToggleZoom) setTitleHover(true); }}
          onMouseLeave={() => setTitleHover(false)}
          title={onToggleZoom ? (zoomed ? '더블클릭 — 전체화면 해제' : '더블클릭 — 임시 전체화면') : undefined}
          style={{ fontFamily: 'var(--cth-font-display)', fontSize: 15, fontWeight: 700, color: titleHover ? 'var(--cth-sky)' : 'var(--cth-ink-900)', minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', userSelect: 'none', cursor: onToggleZoom ? 'pointer' : 'default', padding: '2px 6px', margin: '-2px -6px', borderRadius: 7, background: titleHover ? 'var(--cth-sky-light)' : 'transparent', transition: 'background .12s, color .12s' }}>
          {onToggleZoom && titleHover && (
            <svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.7" strokeLinecap="round" strokeLinejoin="round" style={{ marginRight: 4, verticalAlign: '-1px' }}>
              {zoomed
                ? <path d="M9 3h4v4M13 3l-4 4M7 13H3V9M3 13l4-4" />
                : <path d="M3 7V3h4M13 9v4H9M3 3l4 4M13 13l-4-4" />}
            </svg>
          )}
          {title} {offline ? (
            <span style={{ marginLeft: 2, padding: '1px 7px', borderRadius: 6, background: 'var(--cth-cream-200)', color: 'var(--cth-ink-500)', fontWeight: 700, fontSize: 10 }}>오프라인 · 읽기 전용</span>
          ) : isSub ? (
            <span style={{ marginLeft: 2, padding: '1px 7px', borderRadius: 6, background: 'var(--cth-sky-light)', color: 'var(--cth-sky)', fontWeight: 700, fontSize: 10 }}>서브에이전트 · 읽기 전용</span>
          ) : (
            <span style={{ color: 'var(--cth-ink-300)', fontWeight: 400, fontSize: 13 }}>{surfaceId}</span>
          )}
        </span>
        <div style={{ flex: 1 }} />
        {!offline && !isSub && (<>
        <button
          onClick={() => setCharPicker(true)}
          title="캐릭터 변경 (대화 리셋)"
          style={{
            height: 28, padding: '0 10px', borderRadius: 8, border: '1px solid var(--cth-cream-200)', cursor: 'pointer',
            background: 'var(--cth-cream-100)', color: 'var(--cth-ink-700)',
            fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 600,
            display: 'inline-flex', alignItems: 'center', whiteSpace: 'nowrap', flexShrink: 0
          }}
        >캐릭터</button>
        <button
          onClick={() => void onKill()}
          title="학생 종료 (pane 닫기)"
          style={{
            height: 28, padding: '0 10px', borderRadius: 8, border: 'none', cursor: 'pointer',
            background: 'var(--cth-coral)', color: '#fff',
            fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 600,
            display: 'inline-flex', alignItems: 'center', whiteSpace: 'nowrap', flexShrink: 0
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
            display: 'inline-flex', alignItems: 'center', justifyContent: 'center', flexShrink: 0
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
              // 선생님이 claude 의 질문(AskUserQuestion)에 답한 기록 — 중앙 카드(질문 muted + 선택 답 칩).
              if (it.kind === 'qa') {
                return (
                  <div key={i} style={{ display: 'flex', justifyContent: 'center', marginBottom: 10 }}>
                    <div style={{ maxWidth: '88%', width: '100%', padding: '8px 12px', borderRadius: 12, background: 'color-mix(in srgb, #FEE500 14%, var(--cth-cream-50))', border: '1px solid color-mix(in srgb, #FEE500 50%, var(--cth-cream-200))' }}>
                      <div style={{ display: 'flex', alignItems: 'center', gap: 6, marginBottom: 6, fontFamily: 'var(--cth-font-ui)', fontSize: 11, fontWeight: 700, color: 'var(--cth-ink-500)' }}>
                        <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" style={{ flexShrink: 0 }}>
                          <path d="M6 6a2 2 0 1 1 2.7 1.9c-.5.2-.7.5-.7 1V10" /><circle cx="8" cy="12.5" r="0.6" fill="currentColor" stroke="none" />
                        </svg>
                        선생님이 질문에 답함
                      </div>
                      <div style={{ display: 'flex', flexDirection: 'column', gap: 6 }}>
                        {it.qa.map((p, qi) => (
                          <div key={qi} style={{ display: 'flex', flexDirection: 'column', gap: 3 }}>
                            <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 12, color: 'var(--cth-ink-500)' }}>{p.q}</span>
                            <span style={{ alignSelf: 'flex-start', padding: '2px 9px', borderRadius: 999, background: '#FEE500', color: '#3A2E00', fontFamily: 'var(--cth-font-ui)', fontSize: 12, fontWeight: 700 }}>{p.a}</span>
                          </div>
                        ))}
                      </div>
                    </div>
                  </div>
                );
              }
              // 서브에이전트 소환 마커 — 중앙 한 줄(상세는 메타칸 ↳ 드릴인).
              if (it.kind === 'launch') {
                return (
                  <div key={i} style={{ display: 'flex', justifyContent: 'center', marginBottom: 10 }}>
                    <div style={{ display: 'inline-flex', alignItems: 'center', gap: 7, maxWidth: '88%', padding: '5px 12px', borderRadius: 999, background: 'var(--cth-sky-light)', border: '1px solid var(--cth-sky)', fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-sky)' }}>
                      <svg width="13" height="13" viewBox="0 0 16 16" style={{ flexShrink: 0 }}><path d="M4 3v6a3 3 0 0 0 3 3h6" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" /></svg>
                      <span style={{ fontWeight: 700 }}>서브에이전트 시작</span>
                      {it.subagentType && <span style={{ fontWeight: 600, color: 'var(--cth-ink-700)' }}>· {it.subagentType}</span>}
                      {it.description && <span style={{ color: 'var(--cth-ink-500)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>· {it.description}</span>}
                    </div>
                  </div>
                );
              }
              // 서브 모드는 "선생님↔학생"이 아니라 "부모(미도리)↔서브에이전트" 대화 — 노란 우측
              // 버블(mine) 없이 둘 다 좌측 아바타로. user 턴=부모 지시, assistant 턴=서브 응답(거노).
              const mine = !isSub && it.role === 'user';
              const cancelled = !!it.cancelled; // esc 로 취소한 프롬프트 — 노란색 대신 회색
              const bubbleChar = isSub && it.role === 'user' ? parentAvatarChar : avatarChar;
              // 턴 소요시간 — assistant 버블이 그 턴의 마지막이면 시계 푸터(거노 데스크탑 앱풍).
              const durMs = !mine && it.uuid ? durationMap.get(it.uuid) : undefined;
              const tokOut = !mine && it.uuid ? tokenMap.get(it.uuid) : undefined; // 완료 응답 출력 토큰(transcript usage)
              const clock = fmtClock(it.ts); // 카톡식 메시지 시각(오후 2:47)
              const timeEl = clock ? <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 10, color: 'var(--cth-ink-300)', flexShrink: 0, whiteSpace: 'nowrap', paddingBottom: 2 }}>{clock}</span> : null;
              // 메신저: 선생님(user)=우측 카톡 노랑, 학생(assistant)=좌측 아바타+흰 말풍선. 시각은 카톡처럼
              // 버블 옆 바닥에(선생님=왼쪽, 학생=오른쪽). 취소된 프롬프트는 회색+취소선 점선 테두리.
              return (
                <div key={i} style={{ display: 'flex', justifyContent: mine ? 'flex-end' : 'flex-start', marginBottom: 10, gap: 8, alignItems: 'flex-end' }}>
                  {!mine && (
                    <div style={{ width: 34, height: 34, borderRadius: 11, overflow: 'hidden', flexShrink: 0, background: 'var(--cth-cream-100)', border: '1px solid var(--cth-cream-200)', display: 'flex', alignItems: 'flex-end', justifyContent: 'center' }}>
                      <SpritePortrait character={bubbleChar} scale={1.5} bust />
                    </div>
                  )}
                  {mine && timeEl}
                  <div style={{ maxWidth: '72%', display: 'flex', flexDirection: 'column', alignItems: 'flex-start' }}>
                    {mine && cancelled && (
                      <span style={{ fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700, color: 'var(--cth-ink-300)', marginBottom: 3, alignSelf: 'flex-end' }}>취소된 프롬프트</span>
                    )}
                    <div style={{
                      padding: '8px 12px',
                      borderRadius: 14,
                      borderTopLeftRadius: mine ? 14 : 4,
                      borderTopRightRadius: mine ? 4 : 14,
                      background: cancelled ? 'var(--cth-cream-100)' : mine ? '#FEE500' : (isSub && it.role === 'user') ? 'var(--cth-sky-light)' : 'var(--cth-cream-50)',
                      color: cancelled ? 'var(--cth-ink-500)' : mine ? '#3A2E00' : 'var(--cth-ink-900)',
                      border: cancelled ? '1px dashed var(--cth-ink-300)' : mine ? 'none' : (isSub && it.role === 'user') ? '1px solid var(--cth-sky)' : '1px solid var(--cth-cream-200)',
                      boxShadow: cancelled ? 'none' : '0 1px 3px rgba(21, 41, 74, 0.08)',
                      fontFamily: 'var(--cth-font-ui)', fontSize: 13, lineHeight: 1.55,
                      wordBreak: 'break-word', maxWidth: '100%',
                      textDecoration: cancelled ? 'line-through' : 'none',
                      opacity: cancelled ? 0.8 : 1,
                    }}>
                      {it.text && <BubbleBody text={it.text} mine={mine && !cancelled} />}
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
                  {!mine && timeEl}
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

            {/* 예약(큐) 메시지 — 작업 중 보내 아직 처리 전인 내 메시지. 점선+"예약" 배지로 대기 중
                표시(거노). 처리된 건 위 부분포함 매칭으로 빠지고, 진짜 미처리만 시각순으로 남는다. */}
            {queued.map((q, i) => {
              const qclock = fmtClock(q.ts); // 보낸(enqueue) 시각 — 언제 보낸 예약인지(거노)
              return (
              <div key={`queued-${i}`} style={{ display: 'flex', justifyContent: 'flex-end', marginBottom: 10 }}>
                <div style={{ maxWidth: '72%', display: 'flex', flexDirection: 'column', alignItems: 'flex-end', gap: 3 }}>
                  <span style={{ display: 'inline-flex', alignItems: 'center', gap: 4, fontFamily: 'var(--cth-font-ui)', fontSize: 10, fontWeight: 700, color: 'var(--cth-ink-300)' }}>
                    <svg width="10" height="10" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" style={{ flexShrink: 0 }}><circle cx="8" cy="8" r="6.3" /><path d="M8 4.7V8l2.2 1.3" /></svg>
                    예약 · 대기 중{qclock && ` · ${qclock}`}
                  </span>
                  <div style={{
                    padding: '8px 12px', borderRadius: 14, borderTopRightRadius: 4,
                    background: 'color-mix(in srgb, #FEE500 35%, var(--cth-cream-50))',
                    color: '#3A2E00', border: '1px dashed #E0C200',
                    fontFamily: 'var(--cth-font-ui)', fontSize: 13, lineHeight: 1.55, wordBreak: 'break-word',
                  }}>
                    <BubbleBody text={q.text} mine />
                  </div>
                </div>
              </div>
              );
            })}

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

      {/* 학생별 인연·재화 — 전역 Footer 대신 채팅방 하단에 통합(거노). 라이브 학생만. */}
      {!offline && !isSub && agent && (
        <StudentStats contextPct={agent.contextPct} tokensIn={agent.tokensIn} costUsd={agent.costUsd} />
      )}

      {/* 학생 메타(하단) — 평소엔 접힌 요약 칸 한 줄, 누르면 모델·effort·권한·브랜치·cwd·
          서브에이전트 펼침(거노: 칸 하나 만들어 누르면 나오게). 인연·재화는 위 strip 으로. */}
      {!offline && agent && (convModel || agent.model || agent.branch || agent.cwd || runningSubs.length > 0) && (
        <div style={{ borderTop: '1px solid var(--cth-cream-200)', background: 'var(--cth-cream-50)' }}>
          <button onClick={() => setMetaOpen((v) => !v)} title={metaOpen ? '세부정보 접기' : '세부정보 펼치기'}
            onKeyDown={(e) => {
              // 보조 ↓ — 메타칸 포커스 시에만. 접혀있으면 펼치고, 펼친 채면 첫 서브에이전트로 진입.
              if (e.key === 'ArrowDown' && runningSubs.length > 0) {
                e.preventDefault();
                if (!metaOpen) setMetaOpen(true);
                else { const s = runningSubs[0]; onOpenSubagent?.(surfaceId, s.agentId, s.agentType, s.description || s.agentType); }
              }
            }}
            style={{
            width: '100%', display: 'flex', alignItems: 'center', gap: 6,
            padding: '6px 12px', border: 'none', background: 'transparent', cursor: 'pointer',
            fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-500)', textAlign: 'left',
          }}>
            <svg width="11" height="11" viewBox="0 0 16 16" style={{ flexShrink: 0, transform: metaOpen ? 'rotate(90deg)' : 'none', transition: 'transform .12s' }}>
              <path d="M6 3l5 5-5 5" stroke="currentColor" strokeWidth="2" fill="none" strokeLinecap="round" strokeLinejoin="round" />
            </svg>
            <span style={{ flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
              {[shortModel(agent.model || convModel), displayEffort && `effort: ${shortEffort(displayEffort)}`, modeLabel(mode), agent.branch && `⎇ ${agent.branch}`].filter(Boolean).join('  ·  ') || '세부정보'}
            </span>
            {runningSubs.length > 0 && (
              <span style={{
                flexShrink: 0, padding: '1px 7px', borderRadius: 999, background: 'var(--cth-sky-light)',
                color: 'var(--cth-sky)', fontSize: 10, fontWeight: 700,
              }}>서브 {runningSubs.length}</span>
            )}
          </button>
          {metaOpen && (
            <div style={{ display: 'flex', flexWrap: 'wrap', gap: 6, alignItems: 'center', padding: '0 12px 8px' }}>
              {(convModel || agent.model) && <MetaChip label={shortModel(agent.model || convModel)} onClick={() => void sendToPane(surfaceId, '/model', true, false)} />}
              {(convModel || agent.model) && <MetaChip label={displayEffort ? `effort: ${shortEffort(displayEffort)}` : 'effort'} onClick={() => void sendToPane(surfaceId, '/effort', true, false)} />}
              {modeLabel(mode) && <MetaChip label={modeLabel(mode)!} tone={mode === 'bypassPermissions' ? 'danger' : undefined} title="claude 권한 모드 (shift+tab 로 전환)" />}
              {agent.branch && <MetaChip label={`⎇ ${agent.branch}`} onClick={() => setConfirm({
                msg: '변경사항을 볼까요?',
                sub: `${agent.branch} 브랜치의 미커밋 변경(/diff)을 학생에게 띄워요.`,
                yes: '변경 보기',
                onYes: () => { void sendToPane(surfaceId, '/diff', true, false); },
              })} />}
              {agent.cwd && <MetaChip label={shortCwd(agent.cwd)} dim />}
              {/* 진행 중 서브에이전트만(완료는 숨김 — 거노). 클릭하면 부모가 별도 타일로
                  그 대화를 연다. 한번 열면 완료돼도 타일은 유지. */}
              {runningSubs.map((s) => {
                const text = s.description || s.agentType || s.agentId;
                return (
                  <MetaChip
                    key={s.agentId}
                    label={`↳ ● ${text}`}
                    title={`${s.agentType} · 클릭하면 이 서브에이전트 대화를 따로 열어요`}
                    onClick={() => onOpenSubagent?.(surfaceId, s.agentId, s.agentType, text)}
                  />
                );
              })}
            </div>
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
      ) : isSub ? (
        // 서브에이전트 대화는 읽기 전용.
        <div style={{
          padding: '9px 12px', background: 'var(--cth-cream-50)', borderTop: '1px solid var(--cth-cream-200)',
          fontFamily: 'var(--cth-font-ui)', fontSize: 11, color: 'var(--cth-ink-300)', textAlign: 'center',
        }}>서브에이전트 대화 · 읽기 전용</div>
      ) : (
        // 입력창 폐기(거노: 어차피 뷰어) — 그 자리에 이 학생의 현재 작업(TaskCreate) 고정.
        <TaskStrip tasks={taskList} />
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
