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

async function reply($: Engine, surface: 'terminal' | 'desktop' | 'mobile', props: { text: string; isFirstOfReply: boolean } = REPLY) {
  return $.ui.mount({ plugin: 'student-face', surface, component: 'AssistantMessage', props, viewport: { columns: 100, rows: 30, isFullscreen: true } })
}

test("a student's pane opens each reply with the face and the name in the student's colour", async ($, on) => {
  const asked = host(on, { name: '유우카', face: FACE })
  await $.session.start(START)
  expect(asked).toEqual(['http://127.0.0.1:4321/claude-mod/face?name=%EC%9C%A0%EC%9A%B0%EC%B9%B4'])
  const ui = await reply($, 'terminal')
  const [image] = await ui.findAll({ type: 'Image' })
  expect(image?.props).toMatchObject({ source: { file: FACE.file, format: 'png', generation: 7 }, columns: 4, rows: 2 })
  const name = (await ui.findAll({ type: 'Text' })).find(t => t.props.color === FACE.color)
  expect(name?.text).toBe('유우카')
  expect(name?.props.bold).toBe(true)
  await ui.unmount()
})

test('a surface without pictures gets the name alone', async ($, on) => {
  host(on, { name: '유우카', face: FACE })
  await $.session.start(START)
  for (const surface of ['desktop', 'mobile'] as const) {
    const ui = await reply($, surface)
    expect(await ui.findAll({ type: 'Image' })).toHaveLength(0)
    expect((await ui.findAll({ type: 'Text' })).some(t => t.text === '유우카')).toBe(true)
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

test('a student with no picture gets the name alone', async ($, on) => {
  host(on, { name: '미치루', face: { name: '미치루', color: '#FFB25E' } })
  await $.session.start(START)
  const ui = await reply($, 'terminal')
  expect(await ui.findAll({ type: 'Image' })).toHaveLength(0)
  expect((await ui.findAll({ type: 'Text' })).some(t => t.props.color === '#FFB25E' && t.text === '미치루')).toBe(true)
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
