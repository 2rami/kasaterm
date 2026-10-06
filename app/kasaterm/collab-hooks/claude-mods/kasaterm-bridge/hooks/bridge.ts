// 엔진 이벤트를 kasaterm 에 보낼 모양으로 바꾸는 순수 셈. `$` 를 모르므로 시험이 그대로 부른다.
// 계약은 레포의 docs/claude-mod-bridge.md.

export type Task = { id: string; type: string; status: string; label: string }

export type BridgeEvent = { kind: string; at: number; [field: string]: unknown }

const PREVIEW_CAP = 400
const LABEL_CAP = 160

function one(text: string, cap: number): string {
  const flat = text.replace(/\s+/g, ' ').trim()
  return flat.length > cap ? `${flat.slice(0, cap - 1)}…` : flat
}

function field(input: unknown, key: string): string {
  if (typeof input !== 'object' || input === null) return ''
  const value = (input as Record<string, unknown>)[key]
  return typeof value === 'string' ? value : ''
}

// 보드 활동에 싣는 글 — 줄바꿈은 살리고 길이만 줄인다.
export function clip(text: string, cap: number): string {
  return text.length > cap ? `${text.slice(0, cap - 1)}…` : text
}

// 승인 창이 무엇을 묻는지 한 줄로 — 원격 화면의 제목줄. 원문은 따로 실린다.
export function permissionPreview(tool: string, input: unknown): string {
  const pick = ['command', 'file_path', 'notebook_path', 'url', 'path', 'pattern', 'prompt', 'description']
    .map(key => field(input, key))
    .find(text => text !== '')
  return one(pick ?? (input === undefined ? '' : JSON.stringify(input)) ?? '', PREVIEW_CAP) || tool
}

// 보드 활동 한 줄 — 무엇을 했나. 옛 기록 판독(`activity_from_tail`)이 쓰던 모양과 같은 정도의 짧은 말.
export function activityLabel(tool: string, input: unknown): string {
  const text = field(input, 'description') || permissionPreview(tool, input)
  return one(text === tool ? '' : text, LABEL_CAP)
}

// tool.check 가 본 호출과 PermissionRequest 가 묻는 호출을 잇는 열쇠 — 후자는 tool_use_id 를 안 싣는다.
export function askKey(tool: string, input: unknown): string {
  return `${tool}\u0000${JSON.stringify(input ?? null)}`
}

// 백그라운드 알림 줄에서 끝난 작업의 id 와 상태를 읽는다(엔진이 쓰는 봉투 그대로).
export function taskNotification(text: string): { id: string; status: string } | null {
  const id = /<task-id>([^<]+)<\/task-id>/.exec(text)?.[1]?.trim()
  if (!id) return null
  const status = /<status>([^<]+)<\/status>/.exec(text)?.[1]?.trim() ?? 'completed'
  return { id, status }
}

// 도구 결과가 백그라운드로 넘긴 작업. Bash 의 run_in_background·Ctrl+B 와 Monitor.
export function launchedTask(tool: string, input: unknown, result: unknown): Task | null {
  if (typeof result !== 'object' || result === null) return null
  const r = result as Record<string, unknown>
  const id = typeof r.backgroundTaskId === 'string' ? r.backgroundTaskId : typeof r.taskId === 'string' ? r.taskId : ''
  if (!id) return null
  const type = tool === 'Bash' ? 'shell' : tool === 'Monitor' ? 'monitor' : tool.toLowerCase()
  return { id, type, status: 'running', label: activityLabel(tool, input) }
}

type StopTask = { id: string; type: string; status: string; description?: string; command?: string }

// classic.Stop 이 주는 지금 도는 백그라운드 목록 — 그 순간의 정본.
export function stopTasks(tasks: readonly StopTask[] | undefined): Task[] {
  return (tasks ?? []).map(t => ({
    id: t.id,
    type: t.type,
    status: t.status,
    label: one(t.description || t.command || t.type, LABEL_CAP),
  }))
}

type AgentRow = { id: string; description: string; type: string; status: string }

// 서브에이전트(`$.agent.list()`)와 그 밖의 작업을 하나로 — 서브에이전트는 엔진 목록이 정본이다.
export function mergeTasks(agents: readonly AgentRow[], others: readonly Task[]): Task[] {
  const merged: Task[] = agents
    .filter(a => a.type !== 'teammate')
    .map(a => ({ id: a.id, type: 'subagent', status: a.status, label: one(a.description || a.type, LABEL_CAP) }))
  for (const task of others) {
    if (task.type !== 'subagent' && !merged.some(t => t.id === task.id)) merged.push(task)
  }
  return merged
}

export function running(tasks: readonly Task[]): Task[] {
  return tasks.filter(t => t.status === 'running' || t.status === 'pending')
}

type Measure = {
  context: { tokens?: number; window: number; percent?: number }
  rateLimits: readonly { kind: string; percentUsed: number; resetsAt?: string }[]
  cost?: { usd: number }
}

export function usageEvent(m: Measure, at: number): BridgeEvent {
  return {
    kind: 'usage',
    at,
    context: { tokens: m.context.tokens ?? null, window: m.context.window, percent: m.context.percent ?? null },
    limits: m.rateLimits.map(l => ({ kind: l.kind, percent: l.percentUsed, resets_at: l.resetsAt ?? null })),
    cost_usd: m.cost?.usd ?? null,
  }
}

export type GitTouch = { tool: string; verb: string; paths: string[] }

// 저장소를 안 바꾸는 명령 — 턴마다 수십 번 도는 읽기에 Git 열이 git 을 다시 돌리지 않게.
const READ_ONLY = new Set([
  'cd', 'ls', 'cat', 'head', 'tail', 'less', 'grep', 'egrep', 'rg', 'ag', 'find', 'fd', 'wc', 'echo', 'printf',
  'pwd', 'which', 'type', 'file', 'stat', 'du', 'df', 'ps', 'date', 'env', 'printenv', 'true', 'false', 'sleep',
  'test', '[', 'tree', 'jq', 'sort', 'uniq', 'cut', 'tr', 'awk', 'diff', 'cmp', 'basename', 'dirname', 'realpath',
  'readlink', 'whoami', 'uname', 'id', 'hostname', 'lsof', 'pgrep', 'man', 'xxd', 'od', 'shasum', 'md5',
])
const GIT_READ_ONLY = new Set([
  'status', 'log', 'diff', 'show', 'rev-parse', 'ls-files', 'blame', 'grep', 'describe', 'shortlog', 'cat-file',
  'rev-list', 'reflog', 'help', 'version', 'merge-base', 'name-rev', 'for-each-ref', 'ls-tree', 'show-ref',
])
const GIT_VALUE_OPTIONS = new Set(['-C', '-c', '--git-dir', '--work-tree', '--namespace'])

function word(token: string): string {
  return token.replace(/^['"(]+|['");]+$/g, '').split('/').pop() ?? ''
}

// 한 토막의 명령 이름. 앞의 `X=y` 와 sudo·time 같은 머리는 건너뛴다 — 값(비밀일 수 있다)은 안 본다.
function segmentVerb(segment: string): string {
  const tokens = segment.trim().split(/\s+/).filter(Boolean)
  let i = 0
  while (i < tokens.length && (/^[A-Za-z_][A-Za-z0-9_]*=/.test(tokens[i]!) || ['sudo', 'time', 'nohup', 'exec', 'command'].includes(tokens[i]!))) i++
  const name = word(tokens[i] ?? '')
  if (name !== 'git') return name
  for (let j = i + 1; j < tokens.length; j++) {
    const token = tokens[j]!
    if (GIT_VALUE_OPTIONS.has(token)) {
      j++
      continue
    }
    if (!token.startsWith('-')) return `git ${word(token)}`
  }
  return 'git'
}

function writes(segment: string, verb: string): boolean {
  if (/(?:^|[^\d&>])>{1,2}\s*(?!&|\/dev\/null)\S/.test(segment)) return true
  if (verb === 'sed') return /\s-[a-zA-Z]*i/.test(segment)
  if (verb.startsWith('git ')) return !GIT_READ_ONLY.has(verb.slice(4))
  return verb !== '' && !READ_ONLY.has(verb)
}

const SAFE_VERB = /^(?:git )?[A-Za-z0-9._+-]{1,32}$/

// 「깃이 바뀌었을 수 있다」 — 파일을 고친 도구면 그 경로, 명령이면 첫 낱말(git 이면 하위 명령까지).
// 내용·인자는 싣지 않는다. 읽기만 하는 명령이면 null.
export function gitTouch(tool: string, input: unknown): GitTouch | null {
  if (tool === 'Edit' || tool === 'Write' || tool === 'MultiEdit' || tool === 'NotebookEdit') {
    const path = field(input, 'file_path') || field(input, 'notebook_path')
    return path ? { tool, verb: '', paths: [path] } : null
  }
  if (tool !== 'Bash') return null
  // heredoc 몸통은 명령이 아니다 — 여는 줄까지만 본다(`cat > f <<EOF` 의 쓰기는 거기서 잡힌다).
  const command = field(input, 'command').split('<<')[0] ?? ''
  const segments = command.split(/&&|\|\||[;|\n]|\$\(|`/)
  const verbs = segments.map(segment => ({ segment, verb: segmentVerb(segment) }))
  const writer = verbs.find(v => writes(v.segment, v.verb))
  if (!writer) return null
  const git = verbs.find(v => v.verb.startsWith('git ') && writes(v.segment, v.verb))
  const verb = git?.verb ?? writer.verb
  return { tool, verb: SAFE_VERB.test(verb) ? verb : '', paths: [] }
}
