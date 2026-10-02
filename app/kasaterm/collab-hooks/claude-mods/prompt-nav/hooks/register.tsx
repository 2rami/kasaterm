import type { EngineInterface, Register } from 'claude-code'

import { freshRequest, metrics, observe, parseCommand, parseItems, target } from './nav'
import type { Item, Kind, Model, Op, Seen } from './nav'

// 사람이 직접 보낸 줄만 프롬프트다. 이어 온 기록의 줄은 출처 도장이 없어 unclassified 로 온다.
const PROMPT_ORIGINS = new Set(['composer', 'bridge', 'sdk', 'unclassified'])

const model: Model = { items: [], heights: new Map(), onScreen: new Map(), notPrompt: new Set(), cols: 80, top: null }
const index = new Map<string, number>()
const io = {
  state: '',
  request: '',
  session: '',
  done: 0,
  seq: 0,
  last: '',
  pending: false,
  armed: null as (Op & { seq: number }) | null,
  armedAt: 0,
  loaded: false,
  fullscreen: false,
  agentView: false,
}

function replace(items: Item[]) {
  model.items = items
  index.clear()
  items.forEach((item, i) => index.set(item.id, i))
  io.loaded = true
}

// 기록 도우미가 모르는 줄은 그 뒤에 새로 생긴 줄이다(이번 턴의 질문·답). 그려진 차례로 붙인다.
function remember(id: string, k: Kind, text: string) {
  if (!io.loaded || io.agentView || index.has(id)) return
  const lines = text.split('\n')
  index.set(id, model.items.length)
  model.items.push({
    id,
    k,
    l: lines.length,
    c: lines.reduce((n, line) => n + line.length, 0),
    t: k === 'u' ? (lines[0] ?? '').slice(0, 80) : '',
  })
}

function see(
  $: EngineInterface,
  id: string,
  k: Kind,
  text: string,
  on: Seen | null | undefined,
  viewport: { columns: number; isFullscreen?: boolean } | undefined,
) {
  if (io.agentView) return
  if (viewport) {
    if (viewport.columns !== model.cols) {
      // 폭이 바뀌면 잰 높이가 모두 틀린다 — 다시 보일 때 새로 잰다.
      model.cols = viewport.columns
      model.heights.clear()
    }
    if (viewport.isFullscreen !== undefined) io.fullscreen = viewport.isFullscreen
  }
  remember(id, k, text)
  if (on !== undefined) observe(model, id, on, other => index.get(other))
  schedule($)
}

function schedule($: EngineInterface) {
  if (io.pending || !io.state) return
  io.pending = true
  $.clock.after(200, () => {
    io.pending = false
    void flush($)
  })
}

async function flush($: EngineInterface) {
  const m = metrics(model)
  const body = {
    session: io.session,
    armed: io.armed?.seq ?? 0,
    done: io.done,
    fullscreen: io.fullscreen,
    cols: model.cols,
    total: m.total,
    top: m.top,
    current: m.current,
    prompts: m.prompts.map(p => [p.row, p.text]),
  }
  const key = JSON.stringify(body)
  if (key === io.last) return
  io.last = key
  io.seq += 1
  await $.fs.write(io.state, JSON.stringify({ v: 1, seq: io.seq, at: await $.clock.now(), ...body }))
}

// 점프하면 화면을 떠난 줄은 다시 보고되지 않으므로 지금까지의 보고를 비우고 새 보고로만 맨
// 윗줄을 정한다. 스크롤이 끝날 때까지 보고가 하나도 없으면(이미 그 자리) 목표를 맨 윗줄로 둔다.
async function go($: EngineInterface, op: Op): Promise<string | undefined> {
  const to = target(model, op)
  if (!to) return 'no prompt there'
  const before = { onScreen: new Map(model.onScreen), top: model.top }
  model.onScreen.clear()
  model.top = null
  const result = await $.ui.scroll({ to: { requestId: to.id }, block: to.block })
  if (result.deny) {
    model.onScreen = before.onScreen
    model.top = before.top
  } else if (model.top === null && to.block === 'start') {
    model.top = { id: to.id, first: 0 }
  }
  return result.deny
}

async function readRequest($: EngineInterface): Promise<(Op & { seq: number }) | null> {
  if (!io.request) return null
  try {
    return freshRequest(await $.fs.read(io.request), io.done, await $.clock.now())
  } catch {
    return null
  }
}

// 대화 줄 스크롤은 누름 처리에서 파일 읽기 같은 기다림보다 먼저 와야 사람의 입력으로 인정된다
// (읽고 나서 부르면, 다른 누름이 읽던 것을 기다려도 not person-initiated). 그래서 두 번 누른다:
// kasaterm 이 요청 파일을 쓰고 장전 화음(ctrl+x b)을 보내면 여기서 읽어 쥐고 상태 파일의 armed 로
// 알린다. 그것을 본 kasaterm 이 앞 단추 단축키를 보내면 쥔 요청으로 곧장 스크롤한다.
async function arm($: EngineInterface) {
  const request = await readRequest($)
  if (!request) return
  io.armed = request
  io.armedAt = Date.now()
  await flush($)
}

async function press($: EngineInterface, fallback: Op) {
  // 장전하고 2초 안에 안 쏜 요청은 버린다 — 사람이 나중에 같은 단축키를 눌렀을 때 실리면 안 된다.
  const request = io.armed && Date.now() - io.armedAt < 2000 ? io.armed : null
  io.armed = null
  await go($, request ?? fallback)
  if (!request) return schedule($)
  // kasaterm 은 done 을 보고 다음 요청(끄는 중의 다음 자리)을 보낸다 — 묶어 쓰기를 기다리지 않는다.
  io.done = request.seq
  await flush($)
}

async function load($: EngineInterface, path: string) {
  const script = `${$.plugin.root}/bin/transcript-items.py`
  for (const python of ['python3', 'python']) {
    try {
      const run = await $.process.run([python, script, path], { timeoutMs: 20000 })
      if (run.exitCode === 0 && !run.isStdoutTruncated) {
        replace(parseItems(run.stdout))
        schedule($)
        return
      }
    } catch {
      // 다음 이름으로 다시 찾는다.
    }
  }
  replace([])
  schedule($)
}

async function boot($: EngineInterface) {
  const dir = await $.env.get('KASATERM_PROMPT_NAV_DIR')
  const pane = await $.env.get('KASATERM_PANE_ID')
  if (dir && pane) {
    io.state = `${dir}/${pane}.json`
    io.request = `${dir}/${pane}.req.json`
  }
  io.session = await $.session.id()
  await $.command.register({
    name: 'prompt-nav',
    description: '내 프롬프트로 이동 (prev·next·first·last·번호)',
    argumentHint: '[prev|next|first|last|N]',
    immediate: true,
  })
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    const result = await next(e)
    await boot($)
    return result
  })

  on('classic.SessionStart', async ($, e, next) => {
    const result = await next(e)
    io.session = e.session_id
    io.loaded = false
    void load($, e.transcript_path)
    return result
  })

  on('command.run', { command: 'prompt-nav' }, async ($, e) => {
    const op = parseCommand(e.args)
    const deny = op ? await go($, op) : 'prev · next · first · last · 번호 중 하나'
    return deny ? { text: `prompt-nav: ${deny}` } : {}
  })

  on('ui.render', { component: 'UserMessage' }, ($, e, next) => {
    if (!PROMPT_ORIGINS.has(e.props.origin.kind)) model.notPrompt.add(e.requestId)
    see($, e.requestId, 'u', e.props.text, e.props.onScreen, e.viewport)
    return next(e)
  })

  on('ui.render', { component: 'AssistantMessage' }, ($, e, next) => {
    see($, e.requestId, 'a', e.props.text, e.props.onScreen, e.viewport)
    return next(e)
  })

  on('ui.render', { component: 'ToolUse' }, ($, e, next) => {
    see($, e.requestId, 't', '', e.props.onScreen, e.viewport)
    return next(e)
  })

  // 단추는 보이지 않는다. 엔진 단축키 동작(diff 창의 파일 목록 위·아래, diff 기준 바꾸기)에 걸어
  // 그 창이 없는 평소 화면에서 Ctrl·Option+↑↓ 와 ctrl+x b 가 누른다 — 사람의 입력이라 대화 줄을
  // 스크롤할 수 있다.
  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    io.agentView = e.props.view.agentId !== undefined
    const below = await next(e)
    if (e.surface !== 'terminal' || e.props.hasSurvey) return below
    const { Box, Button } = $.ui.resolve(e)
    return (
      <Box flexDirection="column">
        {below}
        <Box display="none">
          <Button key="prompt-nav-prev" label="prev" action="app:diffFileListUp" onPress={() => void press($, { op: 'prev' })} />
          <Button key="prompt-nav-next" label="next" action="app:diffFileListDown" onPress={() => void press($, { op: 'next' })} />
          <Button key="prompt-nav-arm" label="arm" action="app:cycleDiffBase" onPress={() => void arm($)} />
        </Box>
      </Box>
    )
  })
}
