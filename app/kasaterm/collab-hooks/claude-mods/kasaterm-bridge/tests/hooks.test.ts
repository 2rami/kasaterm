import { expect, mock, test } from 'claude-code/testing'
import type { On } from 'claude-code'

type Sent = { url: string; body: Record<string, unknown> | null }

// 엔진 자리 — 사건마다 엔진이 내놓는 답.
function engine(on: On, submitted: string[] = []) {
  on('session.start', ($, e) => ({ cwd: e.cwd }))
  on('session.version', () => ({ value: { version: '2.1.287', base: '2.1.287', builtAt: '' } }))
  on('session.cwd', () => ({ value: '/repo' }))
  on('session.usage', () => ({ value: { startedAt: 0, context: { window: 200000 }, rateLimits: [] } }))
  on('turn.start', ($, e) => ({ turnId: e.turnId }))
  on('turn.complete', ($, e) => ({ text: e.answer }))
  on('classic.PermissionRequest', () => ({}))
  on('tool.call', () => ({ result: 'ok' }))
  on('prompt.submit', ($, e) => {
    submitted.push(e.text)
    return { text: e.text }
  })
}

// 앱 자리 — mod 가 부르는 env·pid·HTTP 를 받아 적고, 경로마다 정해 둔 답을 준다.
function host(on: On, answers: Record<string, unknown[]> = {}, submitted: string[] = []) {
  const sent: Sent[] = []
  engine(on, submitted)
  on('env.get', ($, e, next) => {
    if (e.name === 'KASASPACE_MCP_PORT') return { value: '4321' }
    if (e.name === 'KASATERM_PANE_ID') return { value: '%9' }
    return next(e)
  })
  on('env.set', () => ({ value: undefined }))
  on('process.run', () => ({ value: { exitCode: 0, stdout: '4242\n', stderr: '', isStdoutTruncated: false, isStderrTruncated: false } }))
  on('session.id', () => ({ value: 'sess-1' }))
  on('agent.list', () => ({ value: [] }))
  on('http.fetch', ($, e) => {
    const body = e.init?.body ? (JSON.parse(e.init.body) as Record<string, unknown>) : null
    sent.push({ url: e.url, body })
    const path = new URL(e.url).pathname
    const queue = answers[path]
    const answer = queue && queue.length > 0 ? queue.shift() : { ok: true }
    return { value: { status: 200, ok: true, headers: {}, text: JSON.stringify(answer) } }
  })
  return sent
}

function events(sent: Sent[]) {
  return sent
    .filter(s => s.url.endsWith('/claude-mod/event'))
    .flatMap(s => (s.body?.events as { kind: string; phase?: string }[]) ?? [])
}

const START = { cwd: '/repo', surface: 'terminal', isInteractive: true } as const

async function settle() {
  for (let i = 0; i < 50; i++) await Promise.resolve()
}

test('session start says hello with the pane, the session and the claude pid', async ($, on) => {
  const sent = host(on)
  await $.session.start(START)
  await settle()
  const hello = sent.find(s => s.url === 'http://127.0.0.1:4321/claude-mod/event')
  expect(hello?.body).toMatchObject({ v: 1, surface: '%9', session: 'sess-1' })
  expect(events(sent)[0]).toMatchObject({ kind: 'hello', pid: 4242, mod: '0.1.0' })
})

test('a turn reports its start and end in order', async ($, on) => {
  const sent = host(on)
  await $.session.start(START)
  await $.turn.start({ text: 'go', turnId: 't1' })
  await $.turn.complete({ answer: 'done', durationMs: 5, isAborted: false, turnId: 't1', reason: 'answer' })
  await settle()
  const turns = events(sent).filter(e => e.kind === 'turn')
  expect(turns).toEqual([
    expect.objectContaining({ phase: 'start', turn: 't1' }),
    expect.objectContaining({ phase: 'end', turn: 't1', reason: 'answer' }),
  ])
})

test('a remote allow answers the permission request', async ($, on) => {
  const sent = host(on, { '/claude-mod/permission': [{ decision: 'none', pending: true }, { decision: 'allow', by: 'phone' }] })
  await $.session.start(START)
  const result = await $.classic.PermissionRequest({ tool_name: 'Bash', tool_input: { command: 'rm -rf build' } })
  expect(result.decision).toEqual({ behavior: 'allow' })
  const asks = sent.filter(s => s.url.endsWith('/claude-mod/permission'))
  expect(asks).toHaveLength(2)
  expect(asks[0]?.body?.request).toMatchObject({ tool: 'Bash', preview: 'rm -rf build', input: { command: 'rm -rf build' } })
})

test('a request answered at the desk hands the dialog back to the engine', async ($, on) => {
  host(on, { '/claude-mod/permission': [{ decision: 'none', pending: false }] })
  await $.session.start(START)
  const result = await $.classic.PermissionRequest({ tool_name: 'Edit', tool_input: { file_path: '/a' } })
  expect(result.decision).toBe(undefined)
})

test('a resting session never submits a prompt of its own — tell and mirror input come through the input box', async ($, on) => {
  const clock = mock.clock(on)
  const submitted: string[] = []
  const sent = host(on, {}, submitted)
  await $.session.start(START)
  await $.turn.start({ text: 'go', turnId: 't1' })
  await $.turn.complete({ answer: '', durationMs: 1, isAborted: false, turnId: 't1', reason: 'answer' })
  await clock.advance(3000)
  await settle()
  expect(submitted).toEqual([])
  expect(sent.some(s => s.url.includes('/claude-mod/inbox'))).toBe(false)
})

test('without kasaterm env the mod stays silent', async ($, on) => {
  const sent: Sent[] = []
  engine(on)
  on('env.get', () => ({ value: undefined }))
  on('http.fetch', ($, e) => {
    sent.push({ url: e.url, body: null })
    return { value: { status: 200, ok: true, headers: {}, text: '{}' } }
  })
  await $.session.start(START)
  await $.turn.start({ text: 'go', turnId: 't1' })
  await settle()
  expect(sent).toHaveLength(0)
})

test('file edits and commands send no git event — the app watches the files itself', async ($, on) => {
  const sent = host(on)
  await $.session.start(START)
  await $.tool.call({ tool: 'Edit', file_path: '/repo/a.rs', old_string: 'x', new_string: 'y' })
  await $.tool.call({ tool: 'Bash', command: 'git commit -m "do not ship this text"' })
  await settle()
  expect(events(sent).filter(e => e.kind === 'git')).toEqual([])
})

test('a dropped tell shows as a toast in the sending pane and never as a prompt', async ($, on) => {
  const clock = mock.clock(on)
  const submitted: string[] = []
  const toasts: { text: string; timeoutMs?: number }[] = []
  const notice = '쪽지 못 감 → 아즈사@맥북 — 기다리다 만료됐어요. «보드 걷기»'
  const sent = host(on, { '/claude-mod/notices': [{ notices: [notice] }] }, submitted)
  // 2.1.292 시험 엔진은 결과가 void 인 자리 훅도 { value } 를 돌려줘야 쓴다 — 없으면 건너뛰고 「구현 없음」으로 토스트를 버린다.
  on('ui.toast', ($, e) => {
    toasts.push(e)
    return { value: undefined }
  })
  await $.session.start(START)
  await $.turn.start({ text: 'go', turnId: 't1' })
  await clock.advance(1000)
  expect(toasts).toEqual([{ text: notice, timeoutMs: 20000 }])
  expect(submitted).toEqual([])
  const poll = sent.find(s => s.url.includes('/claude-mod/notices'))
  expect(poll?.url).toContain('surface=%259')
})
