import { test } from 'node:test'
import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'

const SERVER = fileURLToPath(new URL('./server.mjs', import.meta.url))

// 목록·도움말·인자 거절만 부른다 — 어느 것도 브리지에 붙기 전에 끝나야 한다. 붙었다면 stderr 에 bridge 줄이 남는다.
function run(mode, ...args) {
  const r = spawnSync(process.execPath, [SERVER, mode, ...args], { encoding: 'utf8', timeout: 15000 })
  assert.doesNotMatch(r.stderr, /bridge/, `${mode} ${args.join(' ')} 이 브리지에 붙었다`)
  return r
}

const commands = (listing) => [...listing.matchAll(/^ {2}([a-z_]+)(?= |$)/gm)].map((m) => m[1]).filter((name) => name !== 'kc' && name !== 'kj')

test('kc and the MCP tool list stay without go, login is in both', () => {
  const kc = commands(run('--cli').stdout)
  assert.ok(kc.length > 40, kc.join(','))
  assert.ok(!kc.includes('go'))
  assert.ok(kc.includes('login'))
  assert.deepEqual(commands(run('--kj').stdout), ['go', 'login'])
})

test('kj names itself in help and errors', () => {
  const list = run('--kj')
  assert.equal(list.status, 0)
  assert.match(list.stdout, /^kj — /)
  assert.match(run('--kj', 'go', '--help').stdout, /^kj go <goal> <tabId> /)
  const unknown = run('--kj', 'click', '1')
  assert.equal(unknown.status, 1)
  assert.match(unknown.stderr, /모르는 명령 "click" — kj --help/)
  assert.match(run('--cli', 'go', 'x', '1').stderr, /모르는 도구 "go" — kc --help/)
})

test('kj refuses an empty or zero tab before touching a browser', () => {
  for (const [args, why] of [
    [['go', '목표', ''], /--tabId 은 정수여야/],
    [['go', '목표', '0'], /BAD_ARGS: .*\n.*tabId/],
    [['login', ''], /--tabId 은 정수여야/],
    [['login', '0'], /BAD_TAB_ID/],
  ]) {
    const r = run('--kj', ...args)
    assert.equal(r.status, 1, args.join(' '))
    assert.match(r.stderr, why, args.join(' '))
  }
})
