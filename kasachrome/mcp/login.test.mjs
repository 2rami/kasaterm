import { after, test } from 'node:test'
import assert from 'node:assert/strict'
import { chmodSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { appendAudit, login, maskName, parseCredential, readSecret, secretError, secretRef } from './login.mjs'

const PASSWORD = 'SuperSecretPassword!'
const USER = 'tomsmith'
const HOST = 'the-internet.herokuapp.com'

const dirs = []
const tempDir = (prefix) => { const dir = mkdtempSync(join(tmpdir(), prefix)); dirs.push(dir); return dir }
after(() => { for (const dir of dirs) rmSync(dir, { recursive: true, force: true }) })

function fakeCli(body) {
  const dir = tempDir('kc-login-test-')
  const file = join(dir, 'kasaterm-cli')
  writeFileSync(file, `#!/bin/sh\n${body}\n`)
  chmodSync(file, 0o755)
  return file
}

function fakeBrowser({ fields = { username: true, password: true }, host = HOST, fill } = {}) {
  const calls = []
  const call = async (tool, args) => {
    calls.push({ tool, args })
    if (tool === 'login_target') return { host, fields }
    if (tool === 'login_fill') return fill ? fill(args) : { host, filled: ['username', 'password'].filter((k) => args[k]), missed: [] }
    throw new Error(`unexpected ${tool}`)
  }
  return { call, calls }
}

const leaks = (value) => JSON.stringify(value).includes(PASSWORD)

test('secret ref is web:<host> with the account after the last #', () => {
  assert.equal(secretRef(HOST), `web:${HOST}`)
  assert.equal(secretRef('localhost:3000', USER), `web:localhost:3000#${USER}`)
  assert.throws(() => secretRef(HOST, 'a#b'), /LOGIN_BAD_ACCOUNT/)
  assert.throws(() => secretRef(HOST, 'a\nb'), /LOGIN_BAD_ACCOUNT/)
  assert.throws(() => secretRef('evil.com/path'), /LOGIN_BAD_HOST/)
  assert.throws(() => secretRef(HOST, 'x'.repeat(300)), /LOGIN_BAD_ACCOUNT/)
})

test('credential is JSON from the register script, otherwise the whole value is the password', () => {
  assert.deepEqual(parseCredential(JSON.stringify({ username: USER, password: PASSWORD })), { username: USER, password: PASSWORD })
  assert.deepEqual(parseCredential(PASSWORD, USER), { username: USER, password: PASSWORD })
  assert.deepEqual(parseCredential(PASSWORD), { username: null, password: PASSWORD })
  // JSON.parse 오류문은 입력을 싣는다 — 던지지 않고 통째로 비밀번호로 본다.
  assert.deepEqual(parseCredential('{not json' + PASSWORD), { username: null, password: '{not json' + PASSWORD })
})

test('kasaterm-cli refusal codes become login errors without stderr noise', () => {
  assert.match(secretError('kasaterm: 폰에서…\nkasaterm-cli: 폰에서 거절했어요 (denied)\n', 'web:x').message, /^LOGIN_DENIED:/)
  assert.match(secretError('kasaterm-cli: 2분 안에 허락이 오지 않았어요 (expired)', 'web:x').message, /^LOGIN_TIMEOUT:/)
  const missing = secretError('kasaterm-cli: 이 맥 키체인에 그 항목이 없어요 (not_found)', `web:${HOST}#${USER}`).message
  assert.match(missing, /^LOGIN_NOT_REGISTERED:/)
  assert.ok(missing.includes(`login-add.mjs ${HOST}`))
  assert.match(secretError('kasaterm-cli: 뭔가 (weird_code)', 'web:x').message, /^LOGIN_SECRET_FAILED: 뭔가/)
})

test('readSecret returns stdout on success and never puts stdout into an error', async () => {
  const ok = fakeCli(`printf '%s' '${PASSWORD}'`)
  assert.equal(await readSecret(`web:${HOST}`, { cli: ok }), PASSWORD)

  const bad = fakeCli(`printf '%s' '${PASSWORD}'; echo 'kasaterm-cli: 폰에서 거절했어요 (denied)' >&2; exit 2`)
  const error = await readSecret(`web:${HOST}`, { cli: bad }).catch((e) => e)
  assert.match(error.message, /^LOGIN_DENIED:/)
  assert.ok(!error.message.includes(PASSWORD))

  const slow = fakeCli('sleep 5')
  assert.match((await readSecret('web:x', { cli: slow, timeoutMs: 200 }).catch((e) => e)).message, /^LOGIN_TIMEOUT:/)
  assert.match((await readSecret('web:x', { cli: '/nonexistent/kasaterm-cli' }).catch((e) => e)).message, /^LOGIN_NO_KASATERM:/)
})

test('login fills from the tab host, not from anything the caller passes', async () => {
  const { call, calls } = fakeBrowser()
  const asked = []
  const audit = []
  const result = await login({ tabId: 7, host: 'evil.example', showUsername: false }, {
    call, read: async (ref) => { asked.push(ref); return JSON.stringify({ username: USER, password: PASSWORD }) }, audit: (e) => audit.push(e),
    identity: { name: '유즈', paneId: '%3', sessionId: 's1' },
  })
  assert.deepEqual(asked, [`web:${HOST}`])
  assert.deepEqual(calls[1], { tool: 'login_fill', args: { tabId: 7, host: HOST, username: USER, password: PASSWORD } })
  assert.equal(result.host, HOST)
  assert.deepEqual(result.filled, ['username', 'password'])
  assert.equal(result.username, maskName(USER))
  assert.ok(!leaks(result) && !JSON.stringify(result).includes(USER), 'result carries neither password nor username')
  assert.equal(audit.length, 1)
  assert.deepEqual(audit[0].who, { name: '유즈', pane: '%3', session: 's1' })
  assert.equal(audit[0].service, `web:${HOST}`)
  assert.equal(audit[0].result, 'filled')
  assert.ok(!leaks(audit) && !JSON.stringify(audit).includes(USER), 'audit carries no values')
})

test('showUsername reveals the username, never the password', async () => {
  const { call } = fakeBrowser()
  const result = await login({ tabId: 7, account: USER, showUsername: true }, { call, read: async () => PASSWORD, audit: () => {} })
  assert.equal(result.username, USER)
  assert.equal(result.account, USER)
  assert.ok(!leaks(result))
})

test('a refused approval fails and is recorded without values', async () => {
  const { call, calls } = fakeBrowser()
  const audit = []
  const error = await login({ tabId: 7 }, {
    call, read: async () => { throw new Error('LOGIN_DENIED: 폰에서 거절했습니다.') }, audit: (e) => audit.push(e),
  }).catch((e) => e)
  assert.match(error.message, /^LOGIN_DENIED:/)
  assert.equal(calls.length, 1, 'nothing was filled')
  assert.equal(audit[0].result, 'LOGIN_DENIED')
})

test('an error that echoes the password is scrubbed before it reaches the model', async () => {
  const { call } = fakeBrowser({ fill: () => { throw new Error(`LOGIN_PAGE_FAILED: value=${PASSWORD}`) } })
  const audit = []
  const error = await login({ tabId: 7 }, { call, read: async () => PASSWORD, audit: (e) => audit.push(e) }).catch((e) => e)
  assert.match(error.message, /^LOGIN_PAGE_FAILED:/)
  assert.ok(!error.message.includes(PASSWORD))
  assert.ok(!leaks(audit))
})

test('the username-only screen of a two-step sign-in fills only the username', async () => {
  const { call, calls } = fakeBrowser({ fields: { username: true, password: false } })
  const result = await login({ tabId: 7 }, { call, read: async () => JSON.stringify({ username: USER, password: PASSWORD }), audit: () => {} })
  assert.deepEqual(calls[1].args, { tabId: 7, host: HOST, username: USER, password: null })
  assert.deepEqual(result.filled, ['username'])
})

test('login refuses a missing or zero tab before touching the secret', async () => {
  let read = 0
  for (const tabId of [undefined, 0, -1, 1.5, '3']) {
    const error = await login({ tabId }, { call: async () => ({}), read: async () => { read++; return PASSWORD }, audit: () => {} }).catch((e) => e)
    assert.match(error.message, /^BAD_TAB_ID:/)
  }
  assert.equal(read, 0)
})

test('audit file is private to the user', () => {
  const file = join(tempDir('kc-audit-'), 'sub', 'credential-access.jsonl')
  appendAudit({ host: HOST, result: 'filled' }, file)
  assert.equal(statSync(file).mode & 0o777, 0o600)
  const line = JSON.parse(readFileSync(file, 'utf8').trim())
  assert.equal(line.host, HOST)
  assert.ok(line.at)
})
