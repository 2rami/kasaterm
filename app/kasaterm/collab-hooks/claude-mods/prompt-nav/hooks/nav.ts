// 대화 줄의 차례·높이에서 스크롤 위치와 프롬프트 자리를 셈한다. `$` 를 모르는 순수 셈이라
// 시험이 그대로 부른다.

// u=프롬프트, n=그려지되 프롬프트가 아닌 사람 줄(배경 작업 알림·명령 출력), a=답 글, t=도구 줄, s=압축 요약.
export type Kind = 'u' | 'n' | 'a' | 't' | 's'

export type Item = { id: string; k: Kind; l: number; c: number; t: string }

export type Seen = { first: number; last: number; of: number }

export type Model = {
  items: Item[]
  heights: Map<string, number>
  onScreen: Map<string, Seen>
  notPrompt: Set<string>
  cols: number
  // 화면 맨 윗줄이 걸린 줄과 그 줄 안의 몇째 행인가. 엔진은 스크롤 때 **가장자리** 줄만 다시
  // 알려 주므로, 큰 점프로 화면을 벗어난 줄은 낡은 보고가 남는다 — 그것들 중 맨 위를 맨 윗줄로
  // 치면 틀린다. 그래서 확실한 신호(위가 잘린 줄, 더 위에서 보인 줄, 내가 보낸 스크롤)로만 옮긴다.
  top: { id: string; first: number } | null
}

export type Prompt = { id: string; row: number; text: string }

export type Metrics = {
  total: number
  top: number | null
  current: number
  prompts: Prompt[]
}

export type Op =
  | { op: 'prev' }
  | { op: 'next' }
  | { op: 'first' }
  | { op: 'last' }
  | { op: 'prompt'; index: number }
  | { op: 'row'; row: number }
  | { op: 'bottom' }

export type Target = { id: string; block: 'start' | 'end' }

// 그린 적 없는 줄의 높이 짐작. 엔진이 줄마다 위에 빈 줄 하나를 두므로 +1 이다.
export function estimate(item: Item, cols: number): number {
  if (item.k === 't' || item.k === 's') return 3
  if (item.k === 'n') return 2
  const width = Math.max(20, cols - 4)
  return Math.max(item.l, Math.ceil(item.c / width)) + 1
}

function positions(m: Model): { pos: number[]; total: number } {
  const pos: number[] = []
  let total = 0
  for (const item of m.items) {
    pos.push(total)
    total += m.heights.get(item.id) ?? estimate(item, m.cols)
  }
  return { pos, total }
}

function isPrompt(m: Model, item: Item): boolean {
  return item.k === 'u' && !m.notPrompt.has(item.id)
}

// 보고 하나를 받아 맨 윗줄을 옮긴다. 위가 잘렸으면(first > 0) 그 줄이 곧 맨 윗줄이고, 온전히
// 보이는 줄이 지금 맨 윗줄보다 위에 있으면 맨 윗줄이 거기까지 올라온 것이다. 맨 윗줄이던 줄이
// 화면을 벗어나면 남은 보고 가운데 맨 위로 짐작한다.
export function observe(m: Model, id: string, on: Seen | null, index: (id: string) => number | undefined) {
  if (on) {
    m.onScreen.set(id, on)
    m.heights.set(id, on.of)
    const at = index(id)
    const was = m.top ? index(m.top.id) : undefined
    if (on.first > 0 || was === undefined || (at !== undefined && at < was)) m.top = { id, first: on.first }
    else if (m.top?.id === id) m.top = { id, first: on.first }
    return
  }
  m.onScreen.delete(id)
  if (m.top?.id !== id) return
  m.top = null
  let best: number | undefined
  for (const [other, seen] of m.onScreen) {
    const at = index(other)
    if (at !== undefined && (best === undefined || at < best)) {
      best = at
      m.top = { id: other, first: seen.first }
    }
  }
}

// plugin·다른 세션이 보낸 줄은 엔진의 머리말을 달고 그려진다. 눈금 글은 그 뒤 본문의 첫 줄이다 —
// bin/transcript-items.py 의 FRAMING 과 같은 규칙.
const FRAMING = [' plugin sent a message:', 'Another Claude session sent a message:']

export function promptLine(text: string): string {
  const lines = text.trim().split('\n')
  const head = lines[0] ?? ''
  const body = lines.length > 1 && FRAMING.some(f => head.endsWith(f)) ? lines.slice(1).join('\n').trim() : text.trim()
  return (body.split('\n')[0] ?? '').slice(0, 80)
}

// 기록에 없던 줄(이번 턴의 질문·답)을 그려진 차례로 붙인다. 바로 앞의 같은 글 사람 줄이 새 id 로
// 다시 그려진 것이면 그 자리를 바꿔 낀다 — /compact 는 친 줄을 압축 뒤 명령 줄로, tell 은 대기 줄을
// 자리표시 줄·머리말 붙은 실제 줄로 갈아 그린다(2026-10-06 실측).
export function append(m: Model, index: Map<string, number>, item: Item) {
  const at = m.items.length - 1
  const last = m.items[at]
  if (item.k === 'u' && last?.k === 'u' && last.t === item.t) {
    index.delete(last.id)
    m.heights.delete(last.id)
    m.onScreen.delete(last.id)
    if (m.top?.id === last.id) m.top = { id: item.id, first: m.top.first }
    m.items[at] = item
    index.set(item.id, at)
    return
  }
  index.set(item.id, m.items.length)
  m.items.push(item)
}

export function metrics(m: Model): Metrics {
  const { pos, total } = positions(m)
  const prompts: Prompt[] = []
  let topRow: number | null = null
  m.items.forEach((item, i) => {
    if (isPrompt(m, item)) prompts.push({ id: item.id, row: pos[i] ?? 0, text: item.t })
    if (m.top?.id === item.id) topRow = (pos[i] ?? 0) + m.top.first
  })
  let current = -1
  if (topRow !== null) {
    const top: number = topRow
    prompts.forEach((p, i) => {
      if (p.row <= top) current = i
    })
  }
  return { total, top: topRow, current, prompts }
}

// 「앞 프롬프트」는 지금 턴의 머리가 화면 맨 위에 없으면 그 머리로, 있으면 하나 앞으로 —
// 터미널의 표식 오가기와 같은 규칙이다.
export function target(m: Model, op: Op): Target | null {
  const at = metrics(m)
  const { prompts } = at
  const pick = (i: number): Target | null => {
    const p = prompts[i]
    return p ? { id: p.id, block: 'start' } : null
  }
  switch (op.op) {
    case 'first':
      return pick(0)
    case 'last':
      return pick(prompts.length - 1)
    case 'prompt':
      return pick(op.index)
    case 'prev': {
      if (at.top === null) return pick(prompts.length - 1)
      const cur = prompts[at.current]
      if (cur && cur.row < at.top) return pick(at.current)
      return pick(at.current - 1)
    }
    // 마지막 프롬프트 다음은 대화 끝이다 — 터미널 스크롤백 쪽(kasaterm)도 같은 키로 맨 아래에 선다.
    case 'next':
      return pick(at.current + 1) ?? target(m, { op: 'bottom' })
    case 'bottom': {
      const last = m.items[m.items.length - 1]
      return last ? { id: last.id, block: 'end' } : null
    }
    case 'row': {
      const { pos } = positions(m)
      let found = -1
      pos.forEach((start, i) => {
        if (start <= op.row) found = i
      })
      const item = m.items[Math.max(0, found)]
      return item ? { id: item.id, block: 'start' } : null
    }
  }
}

export function parseItems(stdout: string): Item[] {
  const raw: unknown = JSON.parse(stdout)
  if (!Array.isArray(raw)) return []
  const out: Item[] = []
  for (const row of raw) {
    if (!Array.isArray(row)) continue
    const [id, k, l, c, t] = row as unknown[]
    if (typeof id !== 'string' || (k !== 'u' && k !== 'n' && k !== 'a' && k !== 't' && k !== 's')) continue
    out.push({ id, k, l: Number(l) || 1, c: Number(c) || 0, t: typeof t === 'string' ? t : '' })
  }
  return out
}

// kasaterm 이 남긴 요청 — 오래된 것(이미 한 번호, 5초 넘은 것)은 버린다. 사람이 같은 단축키를
// 직접 눌렀을 때 지난 요청이 다시 실리면 안 된다.
export function freshRequest(text: string, done: number, now: number): (Op & { seq: number }) | null {
  let raw: unknown
  try {
    raw = JSON.parse(text)
  } catch {
    return null
  }
  if (typeof raw !== 'object' || raw === null) return null
  const r = raw as Record<string, unknown>
  const seq = Number(r.seq)
  const at = Number(r.at)
  if (!Number.isFinite(seq) || seq <= done || !Number.isFinite(at) || Math.abs(now - at) > 5000) return null
  switch (r.op) {
    case 'prev':
    case 'next':
    case 'first':
    case 'last':
    case 'bottom':
      return { op: r.op, seq }
    case 'prompt':
      return Number.isInteger(r.index) ? { op: 'prompt', index: Number(r.index), seq } : null
    case 'row':
      return Number.isFinite(Number(r.row)) ? { op: 'row', row: Number(r.row), seq } : null
    default:
      return null
  }
}

export function parseCommand(args: string): Op | null {
  const a = args.trim().toLowerCase()
  if (a === '' || a === 'prev' || a === 'up' || a === '이전') return { op: 'prev' }
  if (a === 'next' || a === 'down' || a === '다음') return { op: 'next' }
  if (a === 'first' || a === '처음') return { op: 'first' }
  if (a === 'last' || a === '마지막') return { op: 'last' }
  if (a === 'bottom' || a === '끝') return { op: 'bottom' }
  const n = Number(a)
  return Number.isInteger(n) && n >= 1 ? { op: 'prompt', index: n - 1 } : null
}
