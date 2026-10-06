export const LEVELS = ['low', 'medium', 'high', 'xhigh', 'max'] as const

export type Context = { percent?: number; window: number; tokens?: number }
export type Fact<T> = { value: T; at: number }
/** 엔진에 묻기 전에 이벤트로 먼저 안 값. 시각은 안 때(ms)다. effort 의 빈 글자는 「effort 없음」이다. */
export type Facts = { model?: Fact<string>; effort?: Fact<string>; cwd?: Fact<string>; context?: Fact<Context> }

/** kasaterm 상태줄 명령이 마지막으로 받은 엔진 입력(`<칸>-engine.json`). */
export type Engine = {
  at_ms?: number
  session_id?: string
  cwd?: string
  model?: { id?: string; display_name?: string }
  effort?: { level?: string }
  context_window?: { used_percentage?: number; context_window_size?: number; total_input_tokens?: number }
}

export type Seg = { text: string; color?: string; bold?: boolean; dim?: boolean }

/** 엔진이 상태줄에 주는 표시명(「Opus 5.5」)을 모델 id 로 짓는다 — 모델을 막 바꿔 엔진 입력에 아직 없을 때만 쓴다. */
export function displayName(id: string): string {
  const bare = id.replace(/\[[^\]]*\]$/, '')
  const [vendor, family = '', ...rest] = bare.split('-')
  const version = rest.filter(p => /^\d{1,2}$/.test(p))
  if (vendor !== 'claude' || !/^[a-z]+$/.test(family) || version.length === 0) return bare
  return `${family.charAt(0).toUpperCase()}${family.slice(1)} ${version.join('.')}`
}

export function effortLevel(effort: unknown): string | undefined {
  return typeof effort === 'string' && (LEVELS as readonly string[]).includes(effort) ? effort : undefined
}

/** `.git/HEAD` 의 글자 → 상태줄의 브랜치 이름. 떨어진 HEAD 는 `HEAD`(kasaterm 상태줄과 같다). */
export function branchOf(head: string): string {
  const text = head.trim()
  if (!text) return ''
  if (!text.startsWith('ref:')) return 'HEAD'
  const name = text.slice(4).trim()
  return name.startsWith('refs/heads/') ? name.slice('refs/heads/'.length) : name
}

/**
 * kasaterm 상태줄 명령에 넣을 입력 — 엔진이 마지막으로 넘긴 입력 위에 그보다 늦게 안 사실만 얹는다.
 * 엔진은 1초 남짓마다 다시 넘기므로, 이벤트로 잘못 안 값이 있어도 그 안에 엔진 값이 이긴다.
 */
export function statuslineInput(engine: Engine, facts: Facts, session: string): Record<string, unknown> {
  const at = engine.at_ms ?? 0
  const fresh = <T>(fact?: Fact<T>) => (fact && fact.at > at ? fact.value : undefined)
  const input: Record<string, unknown> = { session_id: session || engine.session_id, cwd: fresh(facts.cwd) ?? engine.cwd }
  const model = fresh(facts.model)
  input.model =
    model === undefined
      ? engine.model
      : { id: model, display_name: model === engine.model?.id ? engine.model?.display_name : displayName(model) }
  const effort = fresh(facts.effort)
  if (effort === undefined) {
    if (engine.effort) input.effort = engine.effort
  } else if (effort) {
    input.effort = { level: effort }
  }
  const context = fresh(facts.context)
  input.context_window =
    context === undefined
      ? engine.context_window
      : { used_percentage: context.percent ?? 0, context_window_size: context.window, total_input_tokens: context.tokens ?? 0 }
  return input
}

// kasaterm 상태줄(`kasaterm-cli statusline`)의 색·글리프 — kasaterm 밖 claude 에서도 같은 줄로 보이게.
const C_MODEL = '#7aa2f7'
const C_GIT = '#73daca'
const C_DIR = '#bb9af7'
const C_CTX = '#ff9e64'
const C_DANGER = '#f7768e'
const C_SEP = '#565f89'
const C_EFFORT: Record<string, string> = { low: '#565f89', medium: '#7aa2f7', high: '#e0af68', xhigh: '#f7768e', max: '#bb9af7' }
type Icons = { model: string; git: string; folder: string; effort: string }
const NERD: Icons = { model: '\uf233', git: '\ue0a0', folder: '\uf07b', effort: '\uf0e7' }
export const ICONS: Record<string, Icons> = {
  'nerd-font': { model: '', git: '', folder: '', effort: '' },
  unicode: { model: '>', git: '⎇', folder: '▸', effort: '↯' },
  plain: { model: 'M', git: 'git', folder: 'dir', effort: 'E' },
}

export function windowLabel(window: number): string {
  if (window >= 1_000_000) return '1M'
  return window > 0 ? `${Math.floor(window / 1000)}k` : ''
}

export type Plain = { model: string; window: number; percent?: number; branch: string; cwd: string; effort?: string }

/** kasaterm 밖 claude 의 상태줄 — kasaterm 상태줄이 칸 밖에서 짓는 줄과 같은 차례·색. 구분자는 따로 넣는다. */
export function plainParts(v: Plain, icon = 'nerd-font'): Seg[][] {
  const ic = ICONS[icon] ?? NERD
  const parts: Seg[][] = []
  if (v.model) {
    const name = displayName(v.model)
    const win = windowLabel(v.window)
    parts.push([{ text: `${ic.model} ${name}`, color: C_MODEL, bold: true }, ...(win ? [{ text: ` ${win}`, dim: true }] : [])])
  }
  if (v.branch) parts.push([{ text: `${ic.git} ${v.branch}`, color: C_GIT }])
  const dir = v.cwd.replace(/\/+$/, '').split('/').pop() ?? ''
  parts.push([{ text: `${ic.folder} ${dir}`, color: C_DIR }])
  const percent = Math.round(v.percent ?? 0)
  parts.push([{ text: `${percent}%`, color: percent >= 90 ? C_DANGER : C_CTX }])
  if (v.effort) parts.push([{ text: `${ic.effort} ${v.effort}`, color: C_EFFORT[v.effort] ?? C_MODEL }])
  return parts
}

export const SEPARATOR: Seg = { text: '┃', color: C_SEP, dim: true }
