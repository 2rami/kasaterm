import test from 'node:test'
import assert from 'node:assert/strict'
import { humanOnPhone, withHumanNote, showHuman, PHONE_NOTE } from './human-route.mjs'

test('phone target is read from kasaterm settings only when it says phone', () => {
  assert.ok(humanOnPhone({ open_url_target: 'phone' }))
  assert.ok(humanOnPhone({ open_url_target: ' phone ' }))
  for (const settings of [{}, { open_url_target: '' }, { open_url_target: 'book' }, null, undefined]) {
    assert.ok(!humanOnPhone(settings), JSON.stringify(settings))
  }
})

test('tab and window results carry the phone note only in phone mode', () => {
  const opened = { tabId: 7, groupId: 3 }
  assert.equal(withHumanNote(opened, false), opened)
  assert.deepEqual(withHumanNote(opened, true), { tabId: 7, groupId: 3, humanOnPhone: true, note: PHONE_NOTE })
  assert.deepEqual(withHumanNote('done', true), { result: 'done', humanOnPhone: true, note: PHONE_NOTE })
  assert.deepEqual(withHumanNote([1, 2], true), { result: [1, 2], humanOnPhone: true, note: PHONE_NOTE })
})

test('showHuman sends the page through kasaterm and returns the phone link', async () => {
  const result = await showHuman('http://localhost:3000/deals/7', {
    env: { KASATERM_PANE_ID: '%12' }, base: 'http://127.0.0.1:49876',
    fetch: async (url) => {
      assert.equal(url.pathname, '/open-url')
      assert.equal(url.searchParams.get('url'), 'http://localhost:3000/deals/7')
      assert.equal(url.searchParams.get('pane'), '%12')
      return { ok: true, json: async () => ({ ok: true, target: 'phone', url: 'https://a-b-c.trycloudflare.com/deals/7' }) }
    },
  })
  assert.equal(result.target, 'phone')
  assert.equal(result.url, 'https://a-b-c.trycloudflare.com/deals/7')
  assert.match(result.note, /a-b-c\.trycloudflare\.com\/deals\/7/)
})

test('showHuman reports the chosen machine when the human is not on the phone', async () => {
  const result = await showHuman('https://example.com/x', {
    env: {}, base: 'http://127.0.0.1:49876',
    fetch: async () => ({ ok: true, json: async () => ({ ok: true, target: 'book' }) }),
  })
  assert.deepEqual(result, { ok: true, target: 'book', url: 'https://example.com/x', note: 'book 기기 브라우저에서 열었습니다.' })
})

test('showHuman refuses non-http input and surfaces kasaterm errors', async () => {
  await assert.rejects(showHuman('file:///tmp/x', { env: {}, base: 'http://127.0.0.1:1' }), /SHOW_HUMAN_BAD_URL/)
  await assert.rejects(showHuman('http://localhost:3000', {
    env: {}, base: 'http://127.0.0.1:49876',
    fetch: async () => ({ ok: true, json: async () => ({ ok: false, error: '앱이 없어요' }) }),
  }), /SHOW_HUMAN_FAILED: 앱이 없어요/)
})
