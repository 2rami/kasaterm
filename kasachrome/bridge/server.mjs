#!/usr/bin/env node
// 확장(크롬 프로필마다 1개)과 MCP 클라이언트(N개) 사이의 중계. 별도 데몬인 이유: pane 마다
// Claude 가 떠서 MCP 프로세스는 여러 개인데 확장이 붙을 수 있는 포트는 하나뿐이다.
// 확장은 프로필 단위로 여러 개가 동시에 붙는다 — 클라이언트가 고른 프로필로 호출을 보낸다.
import { WebSocketServer, WebSocket } from 'ws'
import { appendFileSync, mkdirSync, renameSync, statSync, readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { homedir } from 'node:os'
import { join, dirname } from 'node:path'
import { PORT } from '../extension/port.js'

// 배포판이 개인판과 같은 로그 폴더를 쓰지 않도록 패키지 이름을 따라간다
const PKG = JSON.parse(readFileSync(join(dirname(dirname(fileURLToPath(import.meta.url))), 'package.json'), 'utf8'))
const HOME = join(homedir(), `.${PKG.name}`)
const LOG = join(HOME, 'bridge.log')
// 크롬이 떠 있는 한 확장이 붙어 있어 이 조건은 성립하지 않는다 — 실제로는 크롬을 끄고
// 마지막 MCP 도 끊긴 뒤에만 물러난다. 남의 맥에 영구 상주하는 데몬을 남기지 않으려는 것.
const IDLE_EXIT_MS = 5 * 60 * 1000
const LOG_MAX = 1024 * 1024

try { mkdirSync(HOME, { recursive: true }) } catch {}

function log(...parts) {
  const line = `[${new Date().toISOString()}] ${parts.join(' ')}\n`
  try {
    // 상한 없이 쌓으면 몇 달 뒤 수십 MB 가 된다. 한 세대만 남기고 갈아엎는다.
    if (statSync(LOG).size > LOG_MAX) renameSync(LOG, `${LOG}.1`)
  } catch { /* 파일이 아직 없다 */ }
  try { appendFileSync(LOG, line) } catch {}
  process.stderr.write(line)
}

// 프로필마다 확장 하나. 예전 확장은 profile 을 안 보내므로 DEFAULT_PROFILE 로 접어 넣는다.
const DEFAULT_PROFILE = 'default'
const extensions = new Map() // profileId -> {sock, label, hint, since}
let nextId = 1
let nextClient = 1
const pending = new Map() // bridgeId -> {client, clientId, timer, tool}
const clients = new Map() // sock -> {key, identity, profile}

// 목록 순서는 붙은 순. 클라이언트가 프로필을 안 고르면 첫 번째를 쓰므로, 순서가 흔들리면
// 같은 pane 의 연속 호출이 서로 다른 크롬으로 갈라진다 — Map 의 삽입 순서에 기댄다.
function profileList() {
  return [...extensions.entries()].map(([id, e]) => ({ id, label: e.label, hint: e.hint, since: e.since }))
}

// 프로필을 안 고른 클라이언트가 어디로 갈지. 파일이 없으면 먼저 붙은 확장인데, 크롬을
// 두 개 띄우면 어느 쪽이 먼저 붙을지는 그날 사정이라 아무 pane 이나 남의 프로필로 샌다.
// 파일에는 프로필 id 나 label(계정 메일) 조각을 적는다 — id 는 랜덤 8자라 못 외운다.
const DEFAULT_FILE = join(HOME, 'default-profile')
function defaultWanted() {
  try { return readFileSync(DEFAULT_FILE, 'utf8').trim() } catch { return '' }
}

function resolveProfile(want) {
  if (!want) return null
  if (extensions.has(want)) return want
  const lower = want.toLowerCase()
  for (const [id, e] of extensions) if ((e.label || '').toLowerCase().includes(lower)) return id
  return null
}

function extFor(sock) {
  const want = clients.get(sock)?.profile
  if (want && extensions.has(want)) return extensions.get(want).sock
  const fallback = resolveProfile(defaultWanted())
  if (fallback) return extensions.get(fallback).sock
  const first = extensions.values().next().value
  return first ? first.sock : null
}

const wss = new WebSocketServer({ host: '127.0.0.1', port: PORT })

wss.on('listening', () => log(`bridge listening on 127.0.0.1:${PORT}`))

// 포트가 잡혀 있다고 곧장 "다른 브리지" 로 단정하면, 남의 앱이 쓰고 있을 때 우리가 조용히 성공
// 종료해 버린다 — 호출한 쪽엔 BRIDGE_UNREACHABLE 만 남고 진짜 원인은 이 로그에만 묻힌다.
// 실제로 우리 브리지인지 한 번 물어보고 갈린다.
wss.on('error', async (err) => {
  if (err.code !== 'EADDRINUSE') {
    log('server error', err.message)
    process.exit(1)
  }
  if (await ourBridge()) {
    log(`port ${PORT} already served by our bridge — exiting`)
    process.exit(0)
  }
  log(`PORT_TAKEN: 127.0.0.1:${PORT} 을 다른 앱이 쓰고 있습니다. extension/port.js 의 PORT 를 바꾸고 확장을 재로드하세요.`)
  process.exit(2)
})

// hello 를 보내고 status 가 돌아오면 우리 브리지다. 아니면 응답이 없거나 형식이 다르다.
function ourBridge() {
  return new Promise((resolve) => {
    let sock
    const done = (v) => { try { sock?.close() } catch {}; clearTimeout(timer); resolve(v) }
    const timer = setTimeout(() => done(false), 1500)
    try {
      sock = new WebSocket(`ws://127.0.0.1:${PORT}`)
    } catch { return done(false) }
    sock.on('error', () => done(false))
    sock.on('open', () => sock.send(JSON.stringify({ type: 'hello', role: 'client' })))
    sock.on('message', (raw) => {
      try { done(JSON.parse(raw.toString()).type === 'status') } catch { done(false) }
    })
  })
}

function send(sock, obj) {
  if (sock && sock.readyState === 1) {
    try { sock.send(JSON.stringify(obj)) } catch (e) { log('send failed', e.message) }
  }
}

function broadcastStatus() {
  const profiles = profileList()
  for (const sock of clients.keys()) {
    send(sock, { type: 'status', extension: extensions.size > 0, profiles, selected: clients.get(sock)?.profile || null })
  }
}

// 확장이 뒤늦게 붙거나 재로드되면 지금 살아 있는 세션을 다시 알려준다.
// 방금 붙은 그 확장에만 보낸다 — 전부에 뿌리면 다른 프로필 오버레이에 남의 칩이 뜬다.
function replaySessions(sock) {
  for (const { key, identity } of clients.values()) {
    if (identity) send(sock, { type: 'session', action: 'open', client: key, identity })
  }
}

function failPendingOf(extSock, reason) {
  for (const [id, p] of pending) {
    if (p.ext !== extSock) continue
    clearTimeout(p.timer)
    pending.delete(id)
    send(p.client, { type: 'result', id: p.clientId, ok: false, error: reason })
  }
}

wss.on('connection', (sock) => {
  let role = null

  sock.on('message', (raw) => {
    let msg
    try { msg = JSON.parse(raw.toString()) } catch { return }

    if (msg.type === 'hello') {
      role = msg.role === 'extension' ? 'extension' : 'client'
      if (role === 'extension') {
        const pid = msg.profile?.id || DEFAULT_PROFILE
        // 같은 프로필의 확장이 재로드되면 옛 소켓은 버린다. 마지막에 붙은 것이 진짜.
        // 다른 프로필이면 공존한다 — 여기서 끊으면 두 크롬이 서로를 밀어낸다.
        const prev = extensions.get(pid)
        if (prev && prev.sock !== sock) { try { prev.sock.close() } catch {} }
        sock.profileId = pid
        extensions.set(pid, {
          sock,
          label: msg.profile?.label || '',
          hint: msg.profile?.hint || '',
          since: Date.now(),
        })
        log(`extension connected: ${pid}${msg.profile?.label ? ` (${msg.profile.label})` : ''}`)
        replaySessions(sock)
        broadcastStatus()
      } else {
        const key = `c${nextClient++}`
        const identity = msg.identity || null
        clients.set(sock, { key, identity, profile: msg.profile || null })
        if (identity) {
          log(`client ${key} = ${identity.name} (${identity.paneId || 'pane?'})`)
          const ext = extFor(sock)
          if (ext) send(ext, { type: 'session', action: 'open', client: key, identity })
        }
        send(sock, { type: 'status', extension: extensions.size > 0, client: key, profiles: profileList(), selected: clients.get(sock).profile })
      }
      return
    }

    if (role === 'client' && msg.type === 'profiles') {
      send(sock, { type: 'profiles', id: msg.id, profiles: profileList(), selected: clients.get(sock)?.profile || null })
      return
    }

    if (role === 'client' && msg.type === 'select') {
      const info = clients.get(sock)
      if (!info) return
      const want = msg.profile ? resolveProfile(msg.profile) : null
      if (msg.profile && !want) {
        send(sock, { type: 'select', id: msg.id, ok: false, error: `NO_SUCH_PROFILE: ${msg.profile}. 붙어 있는 것: ${[...extensions.entries()].map(([id, e]) => `${id}(${e.label || '이름없음'})`).join(', ') || '(없음)'}`, profiles: profileList() })
        return
      }
      // 프로필을 바꾸면 옛 크롬의 오버레이에 이 pane 칩이 남는다 — 떼고 새 쪽에 붙인다.
      const before = extFor(sock)
      info.profile = want || null
      const after = extFor(sock)
      if (info.identity && before !== after) {
        if (before) send(before, { type: 'session', action: 'close', client: info.key })
        if (after) send(after, { type: 'session', action: 'open', client: info.key, identity: info.identity })
      }
      send(sock, { type: 'select', id: msg.id, ok: true, profiles: profileList(), selected: info.profile })
      return
    }

    if (role === 'client' && msg.type === 'call') {
      const ext = extFor(sock)
      if (!ext) {
        send(sock, {
          type: 'result', id: msg.id, ok: false,
          error: 'EXTENSION_NOT_CONNECTED: 크롬 확장이 브리지에 붙어 있지 않습니다. chrome://extensions 에서 확장이 켜져 있는지 확인하세요.',
        })
        return
      }
      const bridgeId = nextId++
      const timeoutMs = Number(msg.timeoutMs) || 30000
      const timer = setTimeout(() => {
        pending.delete(bridgeId)
        send(sock, { type: 'result', id: msg.id, ok: false, error: `TIMEOUT: ${msg.tool} 이 ${timeoutMs}ms 안에 응답하지 않았습니다.` })
      }, timeoutMs)
      pending.set(bridgeId, { client: sock, clientId: msg.id, timer, tool: msg.tool, ext })
      send(ext, {
        type: 'call', id: bridgeId, tool: msg.tool, args: msg.args || {},
        client: clients.get(sock)?.key || null,
      })
      return
    }

    if (role === 'extension' && msg.type === 'result') {
      const p = pending.get(msg.id)
      if (!p) return
      clearTimeout(p.timer)
      pending.delete(msg.id)
      send(p.client, { type: 'result', id: p.clientId, ok: msg.ok, result: msg.result, error: msg.error })
      return
    }

    if (role === 'extension' && msg.type === 'log') {
      log('ext:', msg.text)
    }
  })

  sock.on('close', () => {
    const pid = sock.profileId
    if (pid && extensions.get(pid)?.sock === sock) {
      extensions.delete(pid)
      log(`extension disconnected: ${pid}`)
      // 이 확장으로 나간 호출만 깬다 — 남은 프로필의 진행 중 호출까지 죽이면 안 된다.
      failPendingOf(sock, 'EXTENSION_DISCONNECTED: 명령 처리 중 확장 연결이 끊겼습니다.')
      broadcastStatus()
    }
    const info = clients.get(sock)
    if (info) {
      const ext = extFor(sock)
      clients.delete(sock)
      if (info.identity && ext) send(ext, { type: 'session', action: 'close', client: info.key })
    }
    if (IDLE_EXIT_MS && clients.size === 0 && extensions.size === 0) {
      setTimeout(() => { if (clients.size === 0 && extensions.size === 0) process.exit(0) }, IDLE_EXIT_MS)
    }
  })

  sock.on('error', () => {})
})

// MV3 service worker 는 유휴 30초면 잠든다. WS 수신은 수명을 연장하므로 주기적으로 깨워 둔다.
setInterval(() => {
  const t = Date.now()
  for (const { sock } of extensions.values()) send(sock, { type: 'ping', t })
}, 20000)

process.on('SIGTERM', () => process.exit(0))
process.on('SIGINT', () => process.exit(0))
