import { test } from 'node:test'
import assert from 'node:assert/strict'
import { loginFill, loginHostOf } from './login.js'
import { conceal, MASK } from './conceal.js'
import { explicitTabId } from './tab-id.js'

const PASSWORD = 'login-test-pass-91'

function fakeChrome(url, result) {
  const runs = []
  globalThis.chrome = {
    tabs: { get: async () => ({ url }) },
    scripting: { executeScript: async (spec) => { runs.push(spec); return [{ result }] } },
  }
  return runs
}

test('host comes from the page address and only secure pages qualify', () => {
  assert.equal(loginHostOf('https://Example.COM/login?x=1'), 'example.com')
  assert.equal(loginHostOf('https://example.com:8443/'), 'example.com:8443')
  assert.equal(loginHostOf('http://localhost:3000/login'), 'localhost:3000')
  assert.equal(loginHostOf('http://127.0.0.1:8080/'), '127.0.0.1:8080')
  assert.throws(() => loginHostOf('http://example.com/login'), /LOGIN_INSECURE_PAGE/)
  assert.throws(() => loginHostOf('chrome://settings'), /LOGIN_INSECURE_PAGE/)
  assert.throws(() => loginHostOf('not a url'), /LOGIN_BAD_PAGE/)
})

test('fill refuses when the tab moved to another host and never runs in the page', async () => {
  const runs = fakeChrome('https://evil.example/login', { ok: true })
  await assert.rejects(loginFill(5, { host: 'good.example', username: 'u', password: PASSWORD }), (e) => {
    assert.match(e.message, /^LOGIN_HOST_CHANGED:/)
    assert.ok(!e.message.includes(PASSWORD))
    return true
  })
  assert.equal(runs.length, 0)
})

test('fill runs in the top frame only, returns no values, and later results hide the password', async () => {
  const runs = fakeChrome('https://good.example/login', { ok: true, host: 'good.example', filled: ['username', 'password'], missed: [] })
  const result = await loginFill(5, { host: 'good.example', username: 'u', password: PASSWORD })
  assert.deepEqual(result, { host: 'good.example', filled: ['username', 'password'], missed: [] })
  assert.deepEqual(runs[0].target, { tabId: 5, frameIds: [0] })
  assert.equal(runs[0].args[1], 'good.example', 'the page re-checks the same host it is given')
  const later = conceal({ value: `password=${encodeURIComponent(PASSWORD)}&next=/`, text: `typed ${PASSWORD}` })
  assert.equal(later.value, `password=${MASK}&next=/`)
  assert.equal(later.text, `typed ${MASK}`)
})

test('page-side host mismatch surfaces as LOGIN_HOST_CHANGED', async () => {
  fakeChrome('https://good.example/login', { ok: false, code: 'LOGIN_HOST_CHANGED', host: 'evil.example' })
  await assert.rejects(loginFill(5, { host: 'good.example', password: PASSWORD }), /^Error: LOGIN_HOST_CHANGED: .*evil\.example/)
})

test('explicit tab 0 and empty strings are refused, absence means the active tab', () => {
  assert.equal(explicitTabId(undefined), null)
  assert.equal(explicitTabId(null), null)
  assert.equal(explicitTabId(12), 12)
  assert.equal(explicitTabId('12'), 12)
  for (const bad of [0, '', ' ', -1, 1.5, 'x', true]) assert.throws(() => explicitTabId(bad), /BAD_TAB_ID/, JSON.stringify(bad))
})
