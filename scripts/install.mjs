#!/usr/bin/env node
// 처음 설치할 때 한 번 돌린다. 자동으로 할 수 있는 건 다 하고, 사람이 해야만 하는 한 단계를 정확히 알려준다.
//   node scripts/install.mjs           설치
//   node scripts/install.mjs --verify  붙었는지 확인만
//
// 이 파일은 node_modules 가 아직 없는 상태에서 처음 돌아간다. 그래서 의존성을 최상위에서
// import 하지 않는다 — `import { WebSocket } from 'ws'` 가 맨 위에 있던 동안은 정작 npm install 을
// 실행하는 줄에 닿기도 전에 ERR_MODULE_NOT_FOUND 로 죽었다(설치 스크립트가 설치 전에는 못 돌았다).
import { execFileSync, spawnSync } from 'node:child_process'
import { existsSync, readFileSync } from 'node:fs'
import { createConnection } from 'node:net'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import { homedir } from 'node:os'
import { PORT } from '../extension/port.js'

const ROOT = dirname(dirname(fileURLToPath(import.meta.url)))
const KEY = JSON.parse(readFileSync(join(ROOT, 'package.json'), 'utf8')).name
const EXT = join(ROOT, 'extension')
const SERVER = join(ROOT, 'mcp', 'server.mjs')
const verifyOnly = process.argv.includes('--verify')

const say = (...a) => console.log(...a)

function run(cmd, args) {
  const r = spawnSync(cmd, args, { encoding: 'utf8' })
  return { ok: r.status === 0, out: `${r.stdout || ''}${r.stderr || ''}`.trim() }
}

function copy(text) {
  try { execFileSync('pbcopy', { input: text }); return true } catch { return false }
}

// 포트를 누가 잡고 있는지. 열려 있기만 하면 우리 것으로 단정하지 않는다 — 남의 앱이면 안내가 달라야 한다.
function probePort() {
  return new Promise((resolve) => {
    const sock = createConnection({ host: '127.0.0.1', port: PORT })
    sock.setTimeout(1200)
    sock.on('connect', () => { sock.destroy(); resolve('taken') })
    sock.on('timeout', () => { sock.destroy(); resolve('taken') })
    sock.on('error', () => resolve('free'))
  })
}

async function bridgeStatus() {
  let WebSocket
  try { ({ WebSocket } = await import('ws')) } catch { return null }
  return new Promise((resolve) => {
    let sock
    const done = (v) => { try { sock?.close() } catch {}; clearTimeout(timer); resolve(v) }
    const timer = setTimeout(() => done(null), 2000)
    try { sock = new WebSocket(`ws://127.0.0.1:${PORT}`) } catch { return done(null) }
    sock.on('error', () => done(null))
    sock.on('open', () => sock.send(JSON.stringify({ type: 'hello', role: 'client' })))
    sock.on('message', (raw) => {
      try {
        const m = JSON.parse(raw.toString())
        done(m.type === 'status' ? m : null)
      } catch { done(null) }
    })
  })
}

if (verifyOnly) {
  // 의존성이 없으면 브리지에 물어볼 수단 자체가 없다. "브리지 없음" 으로 뭉뚱그리면 원인을 오진한다.
  if (!existsSync(join(ROOT, 'node_modules'))) {
    say('아직 설치 전입니다 — 먼저 `node scripts/install.mjs` 를 돌리세요.')
    process.exit(1)
  }
  const st = await bridgeStatus()
  if (!st) {
    say(`브리지에 못 붙었습니다 (127.0.0.1:${PORT}). 터미널에서 브라우저 툴을 한 번 쓰면 자동으로 뜹니다.`)
    process.exit(1)
  }
  say(`브리지 연결됨 · 확장 ${st.extension ? '붙어 있음' : '안 붙음 — chrome://extensions 에서 켜졌는지 확인하세요'}`)
  process.exit(st.extension ? 0 : 1)
}

const major = Number(process.versions.node.split('.')[0])
if (major < 18) {
  say(`Node ${process.versions.node} 은 너무 낮습니다. 18 이상이 필요합니다.`)
  process.exit(1)
}

if (!existsSync(join(ROOT, 'node_modules'))) {
  say('의존성을 설치합니다…')
  const r = run('npm', ['install', '--prefix', ROOT])
  if (!r.ok) { say(r.out); process.exit(1) }
}

if (await probePort() === 'taken' && !(await bridgeStatus())) {
  say(`⚠ 127.0.0.1:${PORT} 을 다른 앱이 쓰고 있습니다.`)
  say(`   extension/port.js 의 PORT 를 비어 있는 번호로 바꾸고 다시 실행하세요.`)
  process.exit(1)
}

const claudeJson = join(homedir(), '.claude.json')
let already = false
try {
  already = !!JSON.parse(readFileSync(claudeJson, 'utf8')).mcpServers?.[KEY]
} catch { /* 파일이 없거나 아직 안 만들어졌다 */ }

const addArgs = ['mcp', 'add-json', KEY,
  JSON.stringify({ command: 'node', args: [SERVER] }), '-s', 'user']

if (already) {
  say(`MCP 등록됨: ${KEY} (이미 있어 건너뜁니다)`)
} else if (run('which', ['claude']).ok) {
  const r = run('claude', addArgs)
  say(r.ok ? `MCP 등록 완료: ${KEY}` : `MCP 등록 실패:\n${r.out}`)
  if (!r.ok) process.exit(1)
} else {
  const cmd = `claude ${addArgs.map((a) => (a.startsWith('{') ? `'${a}'` : a)).join(' ')}`
  say('claude CLI 가 PATH 에 없습니다. 아래를 직접 실행하세요' + (copy(cmd) ? ' (클립보드에 복사했습니다)' : '') + ':')
  say(`  ${cmd}`)
}

say('')
say('마지막 한 단계는 사람이 해야 합니다 — 크롬에는 확장을 설치하는 API 가 없습니다.')
say('(--load-extension 플래그는 새 프로필로 크롬을 새로 띄울 때만 먹는데, 이 도구의 존재 이유가')
say(' 평소 쓰던 그 프로필이라 쓸 수 없습니다.)')
say('')
say('  1. chrome://extensions 를 연다')
say('  2. 우상단 「개발자 모드」를 켠다')
say('  3. 「압축해제된 확장 프로그램을 로드」를 누르고 아래 폴더를 고른다')
say(`     ${EXT}${copy(EXT) ? '   (클립보드에 복사했습니다)' : ''}`)
say('')
say('그 뒤 `node scripts/install.mjs --verify` 로 확인하세요.')
