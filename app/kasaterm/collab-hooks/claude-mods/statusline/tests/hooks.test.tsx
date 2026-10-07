import { expect, test } from 'claude-code/testing'
import type { On } from 'claude-code'

type World = { pane?: string; command?: string; head?: string; model?: string; effortSetting?: string }
type Sent = { surface: string; line: string }

const ENGINE = {
  at_ms: 1,
  session_id: 'sess-1',
  cwd: '/repo',
  model: { id: 'claude-opus-5-5[1m]', display_name: 'Opus 5.5 (1M context)' },
  effort: { level: 'xhigh' },
  context_window: { used_percentage: 3, context_window_size: 1000000, total_input_tokens: 30000 },
}

// 엔진·앱 자리 — 저장소 /repo, 엔진 스냅샷, 상태줄 명령(받은 입력을 한 줄로 되돌린다), 앱에 보낸 줄.
function world(on: On, w: World) {
  const sent: Sent[] = []
  const runs: { session_id?: string }[] = []
  on('session.start', ($, e) => ({ cwd: e.cwd }))
  on('session.id', () => ({ value: 'sess-1' }))
  on('session.cwd', () => ({ value: '/repo' }))
  on('session.model', () => ({ value: w.model ?? 'claude-opus-5-5[1m]' }))
  on('session.usage', () => ({ value: { startedAt: 0, context: { window: 1000000, percent: 3, tokens: 30000 }, rateLimits: [] } }))
  on('settings.read', () => ({ value: { statusLine: w.command ? { type: 'command', command: w.command } : undefined, effortLevel: w.effortSetting } }))
  on('env.get', ($, e) => {
    const env: Record<string, string | undefined> = { KASATERM_PANE_ID: w.pane, KASASPACE_MCP_PORT: '4321', TMPDIR: '/tmp/', HOME: '/home/t' }
    return { value: env[e.name] }
  })
  on('fs.stat', ($, e) => {
    if (e.path === '/repo/.git') return { value: { kind: 'dir', size: 0, mtimeMs: 0, isLink: false } }
    throw new Error('ENOENT')
  })
  on('fs.read', ($, e) => {
    if (e.path === '/repo/.git/HEAD') return { value: w.head ?? 'ref: refs/heads/main\n' }
    if (e.path === '/tmp/kasaterm-statusline/9-engine.json') return { value: JSON.stringify(ENGINE) }
    throw new Error('ENOENT')
  })
  on('process.run', ($, e) => {
    const input = JSON.parse(e.init?.stdin ?? '{}') as { model?: { display_name?: string }; effort?: { level?: string }; session_id?: string }
    runs.push(input)
    const model = input.model?.display_name ?? ''
    const effort = input.effort?.level ?? '-'
    return { value: { exitCode: 0, stdout: `${model} ${effort}\n`, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } }
  })
  on('http.fetch', ($, e) => {
    const body = JSON.parse(e.init?.body ?? '{}') as { surface: string; events: { kind: string; line: string }[] }
    for (const event of body.events) if (event.kind === 'status') sent.push({ surface: body.surface, line: event.line })
    return { value: { status: 200, ok: true, headers: {}, text: '{"ok":true}' } }
  })
  on('command.run', () => ({ text: '' }))
  on('classic.PostModelSwitch', () => ({}))
  on('tool.call', () => ({ result: 'ok' }))
  on('clock.every', () => ({ value: undefined }))
  on('ui.render', { component: 'SessionMode' }, () => ({ type: 'Box', props: {}, children: [] }))
  return { sent, runs }
}

const START = { cwd: '/repo', surface: 'terminal', isInteractive: true } as const
const KASATERM: World = { pane: '%9', command: 'kasaterm-cli statusline' }

async function settle() {
  for (let i = 0; i < 100; i++) await Promise.resolve()
}

test('in a kasaterm pane an effort change is drawn at once with the kasaterm line', async ($, on) => {
  const { sent, runs } = world(on, KASATERM)
  await $.session.start(START)
  await $.command.run({ command: 'effort', args: 'low', origin: { kind: 'composer' } } as never)
  await settle()
  expect(sent.at(-1)).toEqual({ surface: '%9', line: 'Opus 5.5 (1M context) low' })
  expect(runs.at(-1)?.session_id).toBe('sess-1')
})

test('a model switch waits for the engine line, which resets effort with the model', async ($, on) => {
  const w: World = { ...KASATERM }
  const { sent } = world(on, w)
  await $.session.start(START)
  w.model = 'claude-sonnet-5-5'
  await $.classic.PostModelSwitch({ from_model: 'claude-opus-5-5', to_model: 'claude-sonnet-5-5', requested_model: 'sonnet', source: 'command', context_tokens: 0 } as never)
  await settle()
  expect(sent.map(s => s.line)).not.toContain('Sonnet 5.5 xhigh')
  await $.command.run({ command: 'effort', args: 'medium', origin: { kind: 'composer' } } as never)
  await settle()
  expect(sent.at(-1)?.line).toBe('Sonnet 5.5 medium')
})

test('a branch switched from elsewhere is drawn at the next look', async ($, on) => {
  const w: World = { ...KASATERM }
  const { runs } = world(on, w)
  await $.session.start(START)
  await settle()
  const before = runs.length
  w.head = 'ref: refs/heads/feat/rain\n'
  await $.tool.call({ tool: 'Bash', input: { command: 'git checkout feat/rain' } } as never)
  await settle()
  expect(runs.length).toBe(before + 1)
})

test('a kasaterm pane with a status line the person chose is left alone', async ($, on) => {
  const { sent, runs } = world(on, { pane: '%9', command: 'my-own-statusline' })
  await $.session.start(START)
  await $.command.run({ command: 'effort', args: 'low', origin: { kind: 'composer' } } as never)
  await settle()
  expect(runs).toEqual([])
  expect(sent).toEqual([])
  const ui = await $.ui.mount({ plugin: 'statusline', surface: 'terminal', component: 'SessionMode', props: { modes: [] }, viewport: { columns: 120, rows: 30, isFullscreen: true } })
  expect(await ui.findAll({ type: 'Text' })).toHaveLength(0)
  await ui.unmount()
})

test('outside kasaterm the line is drawn under the prompt and follows /effort', async ($, on) => {
  world(on, { effortSetting: 'high' })
  await $.session.start(START)
  for (const surface of ['terminal', 'desktop'] as const) {
    const ui = await $.ui.mount({ plugin: 'statusline', surface, component: 'SessionMode', props: { modes: ['focus'] }, viewport: { columns: 120, rows: 30, isFullscreen: true } })
    const texts = (await ui.findAll({ type: 'Text' })).map(t => t.text)
    expect(texts).toContain(' Opus 5.5')
    expect(texts).toContain(' main')
    expect(texts).toContain(' repo')
    expect(texts).toContain('3%')
    expect(texts).toContain(' high')
    expect(texts.at(-1)).toBe('focus')
    await ui.unmount()
  }
  await $.command.run({ command: 'effort', args: 'max', origin: { kind: 'composer' } } as never)
  const ui = await $.ui.mount({ plugin: 'statusline', surface: 'terminal', component: 'SessionMode', props: { modes: [] }, viewport: { columns: 120, rows: 30, isFullscreen: true } })
  expect((await ui.findAll({ type: 'Text' })).map(t => t.text)).toContain(' max')
  await ui.unmount()
})
