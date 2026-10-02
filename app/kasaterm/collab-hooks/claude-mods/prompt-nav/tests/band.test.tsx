import { expect, test } from 'claude-code/testing'
import type { On } from 'claude-code'

// 엔진 자리: 띠에 아무것도 그리지 않는 빈 상자.
function engine(on: On) {
  on('ui.render', { component: 'AbovePrompt' }, () => ({ type: 'Box', props: {}, children: [] }))
}

const BAND = {
  plugin: 'prompt-nav',
  component: 'AbovePrompt',
  props: { hasSurvey: false, isWorking: false, maxRows: 6, bodyColumns: 80, scroll: { offset: 0, bodyRows: 6, contentRows: 0 }, view: {} },
  viewport: { columns: 80, rows: 30, isFullscreen: true },
} as const

test('the terminal band carries the three hidden keybinding buttons', async ($, on) => {
  engine(on)
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  const buttons = await ui.findAll({ type: 'Button' })
  expect(buttons.map(b => b.key)).toEqual(['prompt-nav-prev', 'prompt-nav-next', 'prompt-nav-arm'])
  await ui.unmount()
})

test('other surfaces get the engine band untouched', async ($, on) => {
  engine(on)
  for (const surface of ['desktop', 'vscode'] as const) {
    const ui = await $.ui.mount({ ...BAND, surface })
    expect(await ui.findAll({ type: 'Button' })).toHaveLength(0)
    await ui.unmount()
  }
})

test('a survey keeps the band to itself', async ($, on) => {
  engine(on)
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal', props: { ...BAND.props, hasSurvey: true } })
  expect(await ui.findAll({ type: 'Button' })).toHaveLength(0)
  await ui.unmount()
})
