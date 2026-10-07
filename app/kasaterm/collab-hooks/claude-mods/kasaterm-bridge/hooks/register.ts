import type { EngineInterface, Register } from 'claude-code'

import {
  activityLabel,
  askKey,
  clip,
  gitTouch,
  launchedTask,
  mergeTasks,
  permissionPreview,
  running,
  stopTasks,
  taskNotification,
  usageEvent,
} from './bridge'
import type { BridgeEvent, Task } from './bridge'

const MOD_VERSION = '0.1.0'
const QUEUE_CAP = 500
// 긴 폴링 한 번의 한도(앱이 이보다 오래 쥐지 않는다). 승인 요청은 앱의 만료(10분)를 넘겨 다시 열지 않는다.
const INBOX_WAIT_MS = 20000
const PERMISSION_ROUNDS = 26
const SAY_CAP = 2000
const RESULT_CAP = 600
// 보낸 쪽지가 버려졌다는 한 줄 — 칸을 보는 사람이 놓치지 않을 만큼 둔다. 포인터를 올리면 더 머문다.
const NOTICE_TOAST_MS = 20000

type Ask = { id: string; tool: string; agent?: string }

// 모듈 변수는 다시 실릴 때 처음으로 돌아간다 — 정본은 앱이 쥐고, 여기는 보낼 줄과 지금 일의 짐작만 쥔다.
const io = {
  base: '',
  surface: '',
  session: '',
  pid: 0,
  queue: [] as BridgeEvent[],
  flushing: false,
  turnOpen: false,
  asks: new Map<string, Ask>(),
  permissions: new Set<string>(),
  question: '',
  others: [] as Task[],
  lastTasks: '',
  pumping: false,
  listening: false,
  // 다음 턴을 연 프롬프트가 사람·다른 세션의 말이 아니면(백그라운드 끝남 알림) 활동에 「시킴」으로 안 싣는다.
  quiet: false,
}

function resting(): boolean {
  return !io.turnOpen && io.permissions.size === 0 && io.question === ''
}

function send($: EngineInterface, event: Omit<BridgeEvent, 'at'>) {
  if (!io.base) return
  if (io.queue.length >= QUEUE_CAP) io.queue.shift()
  io.queue.push({ ...event, at: Date.now() } as BridgeEvent)
  void flush($)
}

// 한 번에 하나만 보낸다 — 차례가 곧 뜻이다(턴 시작 → 도구 → 턴 끝).
async function flush($: EngineInterface) {
  if (io.flushing) return
  io.flushing = true
  try {
    while (io.queue.length > 0 && io.base) {
      const events = io.queue.splice(0, io.queue.length)
      try {
        await post($, '/claude-mod/event', { v: 1, surface: io.surface, session: io.session, events })
      } catch {
        // 앱이 없거나 재시작 중이다 — 보낸 줄은 버린다. 다시 뜬 앱은 다음 인사로 이 칸을 다시 안다.
      }
    }
  } finally {
    io.flushing = false
  }
}

async function post($: EngineInterface, path: string, body: unknown) {
  return $.http.fetch(`${io.base}${path}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  })
}

// claude 프로세스의 pid — 앱은 이것이 그 칸에 지금 도는 claude 와 같을 때만 이 mod 를 정본으로 친다.
async function claudePid($: EngineInterface): Promise<number> {
  try {
    const run = await $.process.run(['/bin/sh', '-c', 'echo $PPID'], { timeoutMs: 5000 })
    const pid = Number(run.stdout.trim())
    return Number.isInteger(pid) && pid > 1 ? pid : 0
  } catch {
    return 0
  }
}

async function hello($: EngineInterface) {
  if (!io.base) return
  io.session = await $.session.id()
  const version = await $.session.version()
  send($, { kind: 'hello', mod: MOD_VERSION, claude: version.version, pid: io.pid, cwd: await $.session.cwd() })
  await measure($)
  await reportTasks($)
}

async function boot($: EngineInterface) {
  const port = await $.env.get('KASASPACE_MCP_PORT')
  const surface = await $.env.get('KASATERM_PANE_ID')
  if (!port || !surface || !/^\d+$/.test(port)) return
  io.base = `http://127.0.0.1:${port}`
  io.surface = surface
  // 이 claude 가 낳는 settings 훅·명령이 「mod 가 알린다」를 안다 — 같은 일을 하던 옛 훅이 쉰다.
  await $.env.set('KASATERM_MOD_BRIDGE', '1')
  io.pid = await claudePid($)
  await hello($)
  $.clock.every(1000, () => {
    void pump($)
    void listen($)
  })
}

async function measure($: EngineInterface) {
  const usage = await $.session.usage()
  send($, usageEvent(usage, Date.now()))
}

async function reportTasks($: EngineInterface) {
  const agents = await $.agent.list()
  const tasks = running(mergeTasks(agents, io.others))
  const key = JSON.stringify(tasks)
  if (key === io.lastTasks) return
  io.lastTasks = key
  send($, { kind: 'background', tasks })
}

// 거울(다른 기기·폰) 대화 입력 받기 — 쉬는 동안만 받은편지함을 연다. 받은 글은 입력칸을 안 거치고 턴 하나로
// 들어간다. tell·done 은 여기로 안 온다 — 앱이 입력칸에 붙여넣고 Enter 를 친다(일하는 중에도 그 턴 안으로).
async function pump($: EngineInterface) {
  if (io.pumping || !io.base || !io.session || !resting()) return
  io.pumping = true
  try {
    const query = `surface=${encodeURIComponent(io.surface)}&session=${encodeURIComponent(io.session)}&wait_ms=${INBOX_WAIT_MS}`
    const res = await $.http.fetch(`${io.base}/claude-mod/inbox?${query}`)
    if (!res.ok) return
    const { messages } = JSON.parse(res.text) as { messages?: { id: string; body: string }[] }
    for (const letter of messages ?? []) {
      // 엔진이 거절해도 다시 내주지 않는다 — 두 번 들어가는 것보다 낫다. 앱은 끝났다는 것만 받는다.
      await $.prompt.submit({ text: letter.body }).catch(() => undefined)
      await post($, '/claude-mod/inbox/ack', { surface: io.surface, session: io.session, id: letter.id })
    }
  } catch {
    // 앱이 없으면 다음 박자에 다시 연다.
  } finally {
    io.pumping = false
  }
}

// 보낸 칸 알림 — 앱이 맡긴 한 줄(보낸 쪽지가 버려짐)을 토스트로만 띄운다. 프롬프트로 넣으면 이 칸의 턴을
// 깨워 일을 끊는다 — 토스트는 대화에도 모델에도 안 들어간다. 그래서 일하는 중에도 받는다.
async function listen($: EngineInterface) {
  if (io.listening || !io.base) return
  io.listening = true
  try {
    const query = `surface=${encodeURIComponent(io.surface)}&wait_ms=${INBOX_WAIT_MS}`
    const res = await $.http.fetch(`${io.base}/claude-mod/notices?${query}`)
    if (!res.ok) return
    const { notices } = JSON.parse(res.text) as { notices?: string[] }
    for (const text of notices ?? []) $.ui.toast(text, { timeoutMs: NOTICE_TOAST_MS })
  } catch {
    // 앱이 없으면 다음 박자에 다시 연다.
  } finally {
    io.listening = false
  }
}

async function askRemote(
  $: EngineInterface,
  request: Record<string, unknown>,
): Promise<{ decision: 'allow' | 'deny'; message: string } | null> {
  for (let round = 0; round < PERMISSION_ROUNDS; round++) {
    const res = await post($, '/claude-mod/permission', { v: 1, surface: io.surface, session: io.session, request })
    if (!res.ok) return null
    const answer = JSON.parse(res.text) as { decision?: string; message?: string; pending?: boolean }
    if (answer.decision === 'allow' || answer.decision === 'deny') {
      return { decision: answer.decision, message: answer.message ?? '' }
    }
    if (!answer.pending) return null
  }
  return null
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    const result = await next(e)
    await boot($)
    return result
  })

  // /clear·/resume 는 같은 프로세스에서 세션 id 만 바꾸고 session.start 를 다시 안 낸다.
  on('session.end', async ($, e, next) => {
    const result = await next(e)
    if (e.reason === 'clear' || e.reason === 'resume') {
      io.turnOpen = false
      io.permissions.clear()
      io.question = ''
      io.others = []
      $.clock.after(300, () => void hello($))
    } else {
      send($, { kind: 'bye', reason: e.reason })
      await flush($)
    }
    return result
  })

  on('turn.start', async ($, e, next) => {
    io.turnOpen = true
    send($, { kind: 'turn', phase: 'start', turn: e.turnId, text: io.quiet ? '' : clip(e.text, SAY_CAP) })
    io.quiet = false
    return next(e)
  })

  on('turn.complete', async ($, e, next) => {
    const result = await next(e)
    if (e.agentId === undefined) {
      io.turnOpen = false
      io.question = ''
      send($, { kind: 'turn', phase: 'end', turn: e.turnId, reason: e.reason, answer: clip(result.text, SAY_CAP) })
      void pump($)
    }
    void reportTasks($)
    return result
  })

  on('session.compact', async ($, e, next) => {
    if (e.agentId !== undefined || e.trigger === 'precompute') return next(e)
    send($, { kind: 'compact', phase: 'start', trigger: e.trigger })
    try {
      return await next(e)
    } finally {
      send($, { kind: 'compact', phase: 'end', trigger: e.trigger })
    }
  })

  // 압축의 끝을 한 번 더 — 다시 실린 mod 는 그 앞 압축의 finally 를 잃는다.
  on('classic.PostCompact', async ($, e, next) => {
    send($, { kind: 'compact', phase: 'end', trigger: e.trigger })
    return next(e)
  })

  on('session.measure', async ($, e, next) => {
    send($, usageEvent(e, Date.now()))
    return next(e)
  })

  on('tool.check', async ($, e, next) => {
    const verdict = await next(e)
    if (verdict.decision === 'ask' && e.tool_use_id) {
      io.asks.set(askKey(e.tool, e.input), { id: e.tool_use_id, tool: e.tool })
    }
    return verdict
  })

  // 엔진의 승인 창은 이 훅과 함께 뜬다 — 여기서 기다리는 동안에도 자리의 사람은 그 창으로 답한다. 원격 결정이
  // 먼저 오면 그것으로, 아니면 엔진 몫으로 돌려준다(사람이 먼저 답하면 엔진이 이 훅의 늦은 답을 버린다).
  on('classic.PermissionRequest', async ($, e, next) => {
    const below = await next(e)
    if (below.decision || below.block || !io.base) return below
    const key = askKey(e.tool_name, e.tool_input)
    const ask = io.asks.get(key)
    io.asks.delete(key)
    const id = ask?.id ?? `perm-${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`
    io.permissions.add(id)
    const request = {
      id,
      tool: e.tool_name,
      input: e.tool_input,
      preview: permissionPreview(e.tool_name, e.tool_input),
      cwd: e.cwd,
      agent: ask?.agent ?? null,
      suggestions: e.permission_suggestions ?? [],
      created_at_ms: Date.now(),
    }
    try {
      const remote = await askRemote($, request)
      if (remote?.decision === 'allow') return { ...below, decision: { behavior: 'allow' } }
      if (remote?.decision === 'deny') {
        return { ...below, decision: { behavior: 'deny', message: remote.message || '원격에서 거절했어요' } }
      }
    } catch {
      // 앱에 못 닿으면 엔진 창 혼자 묻는다.
    }
    return below
  })

  on('tool.call', async ($, e, next) => {
    const id = e.tool_use_id ?? ''
    const main = e.agentId === undefined
    send($, { kind: 'tool', phase: 'start', id, tool: e.tool, label: activityLabel(e.tool, e), agent: e.agentId ?? null })
    if (main && e.tool === 'AskUserQuestion') {
      io.question = id
      send($, { kind: 'question', phase: 'start', id })
    }
    let failed = true
    let ran = false
    let text = ''
    try {
      const result = await next(e)
      ran = !('deny' in result && result.deny !== undefined)
      failed = !ran || result.isError === true
      text = clip(result.deny ?? result.text ?? '', RESULT_CAP)
      if (!failed) {
        const task = launchedTask(e.tool, e, result.result)
        if (task) {
          io.others = [...io.others.filter(t => t.id !== task.id), task]
          void reportTasks($)
        }
      }
      // Task·KillShell 은 다른 판의 이름이다 — 이 판의 도구 표에 없어 글자로 견준다.
      if (['Agent', 'Task', 'TaskStop', 'KillShell'].includes(String(e.tool))) void reportTasks($)
      return result
    } finally {
      if (io.permissions.delete(id)) {
        send($, { kind: 'permission', phase: 'resolved', id, outcome: failed ? 'denied' : 'ran' })
      }
      if (main && io.question === id) {
        io.question = ''
        send($, { kind: 'question', phase: 'end', id })
      }
      send($, { kind: 'tool', phase: 'end', id, tool: e.tool, error: failed, text, agent: e.agentId ?? null })
      // 실패한 명령도 파일을 반쯤 바꿨을 수 있다. 고치기 도구는 실패면 아무것도 안 썼다.
      const touch = ran ? gitTouch(String(e.tool), e) : null
      if (touch && (!failed || touch.tool === 'Bash')) {
        void $.session.cwd().then(
          cwd => send($, { kind: 'git', ...touch, cwd }),
          () => send($, { kind: 'git', ...touch, cwd: '' }),
        )
      }
    }
  })

  // Enter 순간이다 — turn.start 는 UserPromptSubmit 훅이 다 돈 뒤(실측 2초 남짓)라, 쉬던 칸은 여기서 먼저 「일함」을
  // 알린다. 훅이 프롬프트를 버리면 되돌린다. 백그라운드 끝남 알림이면 그 작업을 목록에서 내린다.
  on('prompt.submit', async ($, e, next) => {
    if (e.origin.kind === 'task-notification') {
      const done = taskNotification(e.text)
      if (done) {
        io.others = io.others.filter(t => t.id !== done.id)
        void reportTasks($)
      }
    }
    const early = e.turnId === undefined && !io.turnOpen
    if (early) {
      io.turnOpen = true
      io.quiet = e.origin.kind === 'task-notification'
      send($, { kind: 'turn', phase: 'start', turn: '', text: io.quiet ? '' : clip(e.text, SAY_CAP) })
    }
    const result = await next(e)
    if (early && 'drop' in result && result.drop !== undefined) {
      io.turnOpen = false
      send($, { kind: 'turn', phase: 'end', turn: '', reason: 'dropped' })
    }
    return result
  })

  // 턴이 멈출 때 엔진이 주는 지금 도는 작업 목록이 정본이다 — 놓친 끝남을 여기서 바로잡는다.
  on('classic.Stop', async ($, e, next) => {
    io.others = stopTasks(e.background_tasks).filter(t => t.type !== 'subagent')
    void reportTasks($)
    return next(e)
  })

  // 대화 행이 쌓였다 — 대화 보기(폰·데스크톱)가 기록 파일을 다시 읽을 때다. 내용은 파일이 정본이라 싣지 않는다.
  on('session.append', async ($, e, next) => {
    const stored = await next(e)
    if (e.agentId === undefined && io.base && e.message.type !== 'attachment') {
      send($, { kind: 'row', uuid: stored.uuid ?? e.uuid, door: e.door })
    }
    return stored
  })
}
