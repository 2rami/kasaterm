import { expect, test } from 'claude-code/testing'
import type { On } from 'claude-code'
import type { TestBody } from 'claude-code/testing'

type Engine = Parameters<TestBody>[0]

type Host = { name?: string; face?: Record<string, unknown>; down?: boolean }

const FACE = { name: '유우카', color: '#7A5FD4', file: '/tmp/kasaterm-faces/yuuka.png', generation: 7 }

// 엔진과 앱 자리 — 답 블록은 글자 하나로 그리고, 앱은 정해 둔 학생을 돌려준다.
function host(on: On, h: Host) {
  const asked: string[] = []
  on('session.start', ($, e) => ({ cwd: e.cwd }))
  on('turn.start', ($, e) => ({ turnId: e.turnId }))
  on('ui.render', { component: 'AssistantMessage' }, ($, e) => ({ type: 'Text', props: {}, children: [e.props.text] }))
  on('ui.render', { component: 'Spinner' }, ($, e) => ({ type: 'Text', props: {}, children: [e.props.message ?? e.props.word] }))
  on('ui.render', { component: 'TurnDuration' }, ($, e) => ({ type: 'Text', props: {}, children: [`${e.props.word} for 3s`] }))
  on('env.get', ($, e, next) => {
    if (e.name === 'KASATERM_CHARACTER') return { value: h.name }
    if (e.name === 'KASASPACE_MCP_PORT') return { value: '4321' }
    return next(e)
  })
  on('http.fetch', ($, e) => {
    asked.push(e.url)
    if (h.down) throw new Error('connection refused')
    return { value: { status: 200, ok: true, headers: {}, text: JSON.stringify(h.face ?? {}) } }
  })
  return asked
}

const START = { cwd: '/repo', surface: 'terminal', isInteractive: true } as const
const REPLY = { text: '고쳤어요', isFirstOfReply: true }

let turns = 0
async function reply($: Engine, surface: 'terminal' | 'desktop' | 'mobile', props: { text: string; isFirstOfReply: boolean } = REPLY, opts: { newTurn?: boolean; id?: string } = {}) {
  if (opts.newTurn ?? true) await $.turn.start({ text: '해 줘', turnId: `t${++turns}` })
  return $.ui.mount({ plugin: 'student-face', surface, component: 'AssistantMessage', requestId: opts.id ?? `m${turns}`, props, viewport: { columns: 100, rows: 30, isFullscreen: true } })
}

test("a student's pane puts the face beside the first reply of a turn, without the name", async ($, on) => {
  const asked = host(on, { name: '유우카', face: FACE })
  await $.session.start(START)
  expect(asked).toEqual(['http://127.0.0.1:4321/claude-mod/face?name=%EC%9C%A0%EC%9A%B0%EC%B9%B4'])
  const ui = await reply($, 'terminal')
  const [image] = await ui.findAll({ type: 'Image' })
  expect(image?.props).toMatchObject({ source: { file: FACE.file, format: 'png', generation: 7 }, columns: 4, rows: 2 })
  expect((await ui.findAll({ type: 'Text' })).some(t => t.text === '유우카')).toBe(false)
  await ui.unmount()
})

test('a surface without pictures is left as the engine draws it', async ($, on) => {
  host(on, { name: '유우카', face: FACE })
  await $.session.start(START)
  for (const surface of ['desktop', 'mobile'] as const) {
    const ui = await reply($, surface)
    expect(await ui.findAll({ type: 'Image' })).toHaveLength(0)
    expect(await ui.findAll({ type: 'Box' })).toHaveLength(0)
    await ui.unmount()
  }
})

test('only the first block of a reply carries the face', async ($, on) => {
  host(on, { name: '유우카', face: FACE })
  await $.session.start(START)
  const ui = await reply($, 'terminal', { text: '이어서', isFirstOfReply: false })
  expect(await ui.findAll({ type: 'Image' })).toHaveLength(0)
  expect(await ui.findAll({ type: 'Box' })).toHaveLength(0)
  await ui.unmount()
})

test('a pane without a student is left as the engine draws it', async ($, on) => {
  const asked = host(on, {})
  await $.session.start(START)
  expect(asked).toEqual([])
  const ui = await reply($, 'terminal')
  expect(await ui.findAll({ type: 'Box' })).toHaveLength(0)
  await ui.unmount()
})

test('with the character look turned off nothing is drawn, and turning it back on returns once the app is asked again', async ($, on) => {
  const h: Host = { name: '유우카', face: {} }
  host(on, h)
  await $.session.start(START)
  const off = await reply($, 'terminal')
  expect(await off.findAll({ type: 'Box' })).toHaveLength(0)
  await off.unmount()
  h.face = FACE
  await $.session.start(START)
  const on_ = await reply($, 'terminal')
  expect(await on_.findAll({ type: 'Image' })).toHaveLength(1)
  await on_.unmount()
})

test('a student with no picture gets no face on the reply', async ($, on) => {
  host(on, { name: '미치루', face: { name: '미치루', color: '#FFB25E' } })
  await $.session.start(START)
  const ui = await reply($, 'terminal')
  expect(await ui.findAll({ type: 'Box' })).toHaveLength(0)
  await ui.unmount()
})

test('when the app cannot be reached the face it already knew stays', async ($, on) => {
  const h: Host = { name: '유우카', face: FACE }
  host(on, h)
  await $.session.start(START)
  h.down = true
  await $.session.start(START)
  const ui = await reply($, 'terminal')
  expect(await ui.findAll({ type: 'Image' })).toHaveLength(1)
  await ui.unmount()
})

test('one face per turn: a later reply block in the same turn has none, and the first keeps its face on redraw', async ($, on) => {
  host(on, { name: '유우카', face: FACE })
  await $.session.start(START)
  const first = await reply($, 'terminal', REPLY, { id: 'a' })
  expect(await first.findAll({ type: 'Image' })).toHaveLength(1)
  await first.unmount()
  const later = await reply($, 'terminal', { text: '도구 뒤 글', isFirstOfReply: true }, { newTurn: false, id: 'b' })
  expect(await later.findAll({ type: 'Image' })).toHaveLength(0)
  await later.unmount()
  const again = await reply($, 'terminal', REPLY, { newTurn: false, id: 'a' })
  expect(await again.findAll({ type: 'Image' })).toHaveLength(1)
  await again.unmount()
})

test('the working line says what the student is doing, in Korean', async ($, on) => {
  host(on, { name: '유우카', face: FACE })
  await $.session.start(START)
  const ui = await $.ui.mount({ plugin: 'student-face', surface: 'terminal', component: 'Spinner', props: { word: 'Sauteing', message: null, suffix: '…', mode: 'tool-use' } })
  expect((await ui.findAll({ type: 'Text' })).some(t => t.text === '유우카가 확인하는 중')).toBe(true)
  await ui.unmount()
  const busy = await $.ui.mount({ plugin: 'student-face', surface: 'terminal', component: 'Spinner', props: { word: 'Sauteing', message: 'Compacting', suffix: '…', mode: 'requesting' } })
  expect((await busy.findAll({ type: 'Text' })).some(t => t.text === 'Compacting')).toBe(true)
  await busy.unmount()
})

test('the turn line is in the student colour with the face as its mark', async ($, on) => {
  host(on, { name: '유우카', face: FACE })
  await $.session.start(START)
  const ui = await $.ui.mount({ plugin: 'student-face', surface: 'terminal', component: 'TurnDuration', props: { word: 'Baked', durationMs: 67000 } })
  const [image] = await ui.findAll({ type: 'Image' })
  expect(image?.props).toMatchObject({ columns: 2, rows: 1 })
  const line = (await ui.findAll({ type: 'Text' })).find(t => t.text === '1분 7초 걸려 끝났어요')
  expect(line?.props.color).toBe(FACE.color)
  await ui.unmount()
})

test('the face takes the bullet\'s place: the first block is drawn as markdown beside it, without the engine\'s row', async ($, on) => {
  host(on, { name: '유우카', face: FACE })
  await $.session.start(START)
  const ui = await reply($, 'terminal', { text: '**고쳤어요** 이제 돼요', isFirstOfReply: true })
  expect(await ui.findAll({ type: 'Image' })).toHaveLength(1)
  const [md] = await ui.findAll({ type: 'Markdown' })
  expect(md?.props.text).toBe('**고쳤어요** 이제 돼요')
  expect((await ui.findAll({ type: 'Text' })).some(t => t.text === '**고쳤어요** 이제 돼요')).toBe(false)
  await ui.unmount()
})
