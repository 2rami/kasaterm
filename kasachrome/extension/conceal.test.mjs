import { test } from 'node:test'
import assert from 'node:assert/strict'
import { conceal, remember, MASK } from './conceal.js'

const T0 = 1_000_000

test('nothing remembered means results pass through untouched', () => {
  const value = { text: 'hello' }
  assert.equal(conceal(value, T0), value)
})

test('a remembered password is hidden in every shape a page or request carries it', () => {
  const secret = 'Pa ss"wo&rd!'
  remember(secret, T0)
  const out = conceal({
    page: `field: ${secret}`,
    json: JSON.stringify({ password: secret }),
    body: `user=a&password=${encodeURIComponent(secret).replace(/%20/g, '+')}`,
    url: `https://x/?p=${encodeURIComponent(secret)}`,
    list: [secret, 3, null, { deep: secret }],
  }, T0 + 1)
  const text = JSON.stringify(out)
  assert.ok(!text.includes('wo&rd') && !text.includes('wo%26rd'), text)
  assert.equal(out.page, `field: ${MASK}`)
  assert.equal(out.list[0], MASK)
  assert.equal(out.list[1], 3)
  assert.equal(out.list[3].deep, MASK)
})

test('screenshot-sized base64 is left alone so images do not break', () => {
  remember('AAAA', T0)
  const blob = 'AAAA'.repeat(6000)
  assert.equal(conceal({ data: blob }, T0 + 1).data, blob)
})

test('short values are not remembered and old ones expire', () => {
  remember('abc', T0)
  assert.equal(conceal({ t: 'abc' }, T0 + 1).t, 'abc')
  remember('expiring-secret', T0)
  assert.equal(conceal({ t: 'expiring-secret' }, T0 + 1).t, MASK)
  assert.equal(conceal({ t: 'expiring-secret' }, T0 + 31 * 60 * 1000).t, 'expiring-secret')
})
