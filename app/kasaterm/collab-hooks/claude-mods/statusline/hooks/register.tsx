import type { EngineInterface, Register } from 'claude-code'

import { branchOf, effortLevel, plainParts, SEPARATOR, statuslineInput } from './line'
import type { Context, Engine, Facts } from './line'

// 바깥에서 바뀌는 것(다른 칸의 git checkout, 셸 cd)을 알아채는 간격. 파일 하나와 경로 하나를 읽는 값싼 일이다.
const POLL_MS = 150

// 모듈 변수는 다시 실릴 때 처음으로 돌아간다 — session.start 가 다시 채운다.
const io = {
  // kasaterm: kasaterm 상태줄을 바로 덧그린다 · plain: kasaterm 밖이라 같은 줄을 직접 그린다 · off: 사람이 고른 상태줄이 있는 칸
  mode: 'off' as 'kasaterm' | 'plain' | 'off',
  base: '',
  pane: '',
  engineFile: '',
  facts: {} as Facts,
  cwd: '',
  head: '',
  headPath: '',
  // kasaterm 밖에서 그릴 값 — 엔진 입력이 없으니 이벤트와 설정으로만 안다.
  plain: { model: '', window: 0, percent: undefined as number | undefined, effort: undefined as string | undefined, icon: 'nerd-font' },
  last: '',
  running: false,
  again: false,
}

async function headPath($: EngineInterface, cwd: string): Promise<string> {
  let dir = cwd.replace(/\/+$/, '') || '/'
  for (let depth = 0; depth < 64; depth++) {
    const dot = `${dir === '/' ? '' : dir}/.git`
    try {
      const stat = await $.fs.stat(dot)
      if (stat.kind === 'dir') return `${dot}/HEAD`
      if (stat.kind === 'file') {
        const gitdir = /^gitdir:\s*(.+)$/m.exec(await $.fs.read(dot))?.[1]?.trim()
        if (gitdir) return `${gitdir.startsWith('/') ? gitdir : `${dir}/${gitdir}`}/HEAD`
      }
    } catch {
      // 없으면 위로.
    }
    if (dir === '/') return ''
    dir = dir.replace(/\/[^/]+$/, '') || '/'
  }
  return ''
}

async function readHead($: EngineInterface): Promise<string> {
  if (!io.headPath) return ''
  try {
    return await $.fs.read(io.headPath)
  } catch {
    return ''
  }
}

function changed($: EngineInterface) {
  if (io.mode === 'kasaterm') void redraw($)
  else if (io.mode === 'plain') $.ui.invalidate('ui.render')
}

// 경로·브랜치는 이벤트가 없는 바깥 손에도 바뀐다 — 짧게 다시 읽어 달라졌을 때만 다시 그린다.
// 도구·명령 뒤에도 부르므로 던지지 않는다(던지면 그 도구의 결과가 깨진다).
async function poll($: EngineInterface) {
  try {
    const cwd = await $.session.cwd()
    let moved = false
    if (cwd !== io.cwd) {
      io.cwd = cwd
      io.facts.cwd = { value: cwd, at: Date.now() }
      io.headPath = await headPath($, cwd)
      moved = true
    }
    const head = await readHead($)
    if (head !== io.head) {
      io.head = head
      moved = true
    }
    if (moved) changed($)
  } catch {
    // 다음 박자에 다시 읽는다.
  }
}

async function readEngine($: EngineInterface): Promise<Engine | null> {
  try {
    return JSON.parse(await $.fs.read(io.engineFile)) as Engine
  } catch {
    return null
  }
}

// kasaterm 칸 — 엔진의 마지막 입력에 막 안 사실을 얹어 kasaterm 상태줄 명령으로 같은 줄을 짓고 앱에 보낸다.
// 앱은 엔진이 자기 줄을 다시 그릴 때까지만 이 줄을 덧그린다. 짓는 동안 온 바뀜은 끝난 뒤 한 번 더 짓는다.
async function redraw($: EngineInterface) {
  if (io.running) {
    io.again = true
    return
  }
  io.running = true
  try {
    do {
      io.again = false
      const engine = await readEngine($)
      if (!engine) return
      const session = await $.session.id()
      const input = statuslineInput(engine, io.facts, session)
      if (!input) continue
      const run = await $.process.run(['kasaterm-cli', 'statusline'], {
        stdin: JSON.stringify(input),
        env: { KASATERM_STATUSLINE_DRAW_ONLY: '1' },
        timeoutMs: 3000,
      })
      const line = run.stdout.replace(/\n+$/, '')
      if (run.exitCode !== 0 || !line || line === io.last) continue
      // 짓는 사이 엔진이 새 입력으로 자기 줄을 그렸다 — 옛 입력으로 지은 줄을 보내면 앱이 그 새 줄을 다음 엔진
      // 박자까지 덮는다. 새 입력으로 다시 짓는다.
      if ((await readEngine($))?.at_ms !== engine.at_ms) {
        io.again = true
        continue
      }
      io.last = line
      await $.http.fetch(`${io.base}/claude-mod/event`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ v: 1, surface: io.pane, session, events: [{ kind: 'status', line }] }),
      })
    } while (io.again)
  } catch {
    // 앱이나 명령에 못 닿으면 엔진 상태줄이 혼자 그린다.
  } finally {
    io.running = false
  }
}

function learnContext($: EngineInterface, context: Context) {
  io.facts.context = { value: { percent: context.percent, window: context.window, tokens: context.tokens }, at: Date.now() }
  io.plain.window = context.window
  io.plain.percent = context.percent
  changed($)
}

function learnModel($: EngineInterface, model: string) {
  if (!model || model === io.facts.model?.value) return
  io.facts.model = { value: model, at: Date.now() }
  io.plain.model = model
  changed($)
}

function learnEffort($: EngineInterface, effort: string) {
  if (io.facts.effort?.value === effort) return
  io.facts.effort = { value: effort, at: Date.now() }
  io.plain.effort = effort || undefined
  changed($)
}

async function boot($: EngineInterface) {
  const pane = (await $.env.get('KASATERM_PANE_ID')) ?? ''
  const port = (await $.env.get('KASASPACE_MCP_PORT')) ?? ''
  const settings = await $.settings.read()
  const command = String((settings.statusLine as { command?: unknown } | undefined)?.command ?? '')
  if (pane) {
    // kasaterm 칸이라도 사람이 자기 상태줄을 골랐으면(「상태줄 직접 설정」) 손대지 않는다.
    if (!command.includes('kasaterm-cli statusline') || !/^\d+$/.test(port)) return
    const tmp = ((await $.env.get('TMPDIR')) || '/tmp').replace(/\/+$/, '')
    io.mode = 'kasaterm'
    io.base = `http://127.0.0.1:${port}`
    io.pane = pane
    io.engineFile = `${tmp}/kasaterm-statusline/${pane.replace(/^%/, '')}-engine.json`
  } else {
    io.mode = 'plain'
    io.plain.effort = effortLevel((await $.env.get('CLAUDE_EFFORT')) ?? '') ?? effortLevel(settings.effortLevel)
    try {
      const home = (await $.env.get('HOME')) ?? ''
      const config = JSON.parse(await $.fs.read(`${home}/.claude/statusline-config.json`)) as { icon_set?: string }
      if (config.icon_set) io.plain.icon = config.icon_set
    } catch {
      // 설정이 없으면 Nerd Font 글리프.
    }
  }
  io.plain.model = await $.session.model()
  const usage = await $.session.usage()
  io.plain.window = usage.context.window
  io.plain.percent = usage.context.percent
  await poll($)
  $.clock.every(POLL_MS, () => void poll($))
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    const result = await next(e)
    await boot($)
    return result
  })

  on('classic.PostModelSwitch', async ($, e, next) => {
    const result = await next(e)
    if (io.mode !== 'off') {
      learnModel($, e.to_model)
      learnContext($, (await $.session.usage()).context)
    }
    return result
  }).catch(($, e, next) => next(e))

  // /effort·/model·/cd 는 명령이 끝나야 값이 바뀐다. /effort 는 다른 이벤트가 없어 인자로 안다.
  on('command.run', async ($, e, next) => {
    const result = await next(e)
    if (io.mode === 'off') return result
    if (e.command === 'effort') {
      const level = effortLevel(e.args.trim().split(/\s+/)[0])
      if (level) learnEffort($, level)
    }
    try {
      learnModel($, await $.session.model())
    } catch {
      // 모델은 PostModelSwitch 가 다시 알린다.
    }
    await poll($)
    return result
  })

  // 요청마다 엔진이 실제로 쓴 effort — 모델 고르기 창에서 바꾼 것도 첫 요청에 바로잡힌다.
  on('turn.step', async function* ($, e, next) {
    if (io.mode !== 'off' && e.agentId === undefined) {
      const level = effortLevel(e.effort)
      if (level) learnEffort($, level)
    }
    return yield* next(e)
  })

  on('session.measure', async ($, e, next) => {
    if (io.mode !== 'off') learnContext($, e.context)
    return next(e)
  })

  on('classic.CwdChanged', async ($, e, next) => {
    const result = await next(e)
    if (io.mode !== 'off') await poll($)
    return result
  }).catch(($, e, next) => next(e))

  // git checkout·cd 는 대개 Bash 로 온다 — 끝나는 순간 다시 읽는다.
  on('tool.call', { tool: 'Bash' }, async ($, e, next) => {
    const result = await next(e)
    if (io.mode !== 'off') await poll($)
    return result
  })

  // kasaterm 밖: 상태줄 자리가 비어 있으니(엔진은 명령 상태줄만 그린다) 프롬프트 아래 모드 자리에 같은 줄을 그린다.
  // 폭을 줄 전체로 잡으면 엔진이 힌트 줄 아래 제 줄로 내린다.
  on('ui.render', { component: 'SessionMode' }, ($, e, next) => {
    if (io.mode !== 'plain' || !io.plain.model) return next(e)
    const { Box, Text } = $.ui.resolve(e)
    const parts = plainParts({ ...io.plain, branch: branchOf(io.head), cwd: io.cwd }, io.plain.icon)
    const columns = e.viewport?.columns ?? 80
    return (
      <Box flexDirection="row" width={Math.max(20, columns - 4)}>
        {parts.flatMap((part, index) => [
          ...(index > 0
            ? [
                <Text key={`sep-${index}`} color={SEPARATOR.color} dimColor>
                  {` ${SEPARATOR.text} `}
                </Text>,
              ]
            : []),
          ...part.map((seg, at) => (
            <Text key={`seg-${index}-${at}`} color={seg.color} bold={seg.bold} dimColor={seg.dim}>
              {seg.text}
            </Text>
          )),
        ])}
        <Box flexGrow={1} />
        <Text dimColor>{e.props.modes.join(' & ')}</Text>
      </Box>
    )
  })
}
