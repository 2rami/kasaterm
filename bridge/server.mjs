#!/usr/bin/env node
// 확장(1개)과 MCP 클라이언트(N개) 사이의 중계. 별도 데몬인 이유: pane 마다 Claude 가 떠서
// MCP 프로세스는 여러 개인데 확장이 붙을 수 있는 포트는 하나뿐이다.
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

let extension = null
let nextId = 1
let nextClient = 1
const pending = new Map() // bridgeId -> {client, clientId, timer, tool}
const clients = new Map() // sock -> {key, identity}

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
  for (const sock of clients.keys()) send(sock, { type: 'status', extension: !!extension })
}

// 확장이 뒤늦게 붙거나 재로드되면 지금 살아 있는 세션을 다시 알려준다.
function replaySessions() {
  for (const { key, identity } of clients.values()) {
    if (identity) send(extension, { type: 'session', action: 'open', client: key, identity })
  }
}

function failPending(reason) {
  for (const [, p] of pending) {
    clearTimeout(p.timer)
    send(p.client, { type: 'result', id: p.clientId, ok: false, error: reason })
  }
  pending.clear()
}

wss.on('connection', (sock) => {
  let role = null

  sock.on('message', (raw) => {
    let msg
    try { msg = JSON.parse(raw.toString()) } catch { return }

    if (msg.type === 'hello') {
      role = msg.role === 'extension' ? 'extension' : 'client'
      if (role === 'extension') {
        // 확장이 재로드되면 옛 소켓은 버린다. 마지막에 붙은 것이 진짜.
        if (extension && extension !== sock) { try { extension.close() } catch {} }
        extension = sock
        log('extension connected')
        replaySessions()
        broadcastStatus()
      } else {
        const key = `c${nextClient++}`
        const identity = msg.identity || null
        clients.set(sock, { key, identity })
        if (identity) {
          log(`client ${key} = ${identity.name} (${identity.paneId || 'pane?'})`)
          send(extension, { type: 'session', action: 'open', client: key, identity })
        }
        send(sock, { type: 'status', extension: !!extension, client: key })
      }
      return
    }

    if (role === 'client' && msg.type === 'call') {
      if (!extension) {
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
      pending.set(bridgeId, { client: sock, clientId: msg.id, timer, tool: msg.tool })
      send(extension, {
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
    if (sock === extension) {
      extension = null
      log('extension disconnected')
      failPending('EXTENSION_DISCONNECTED: 명령 처리 중 확장 연결이 끊겼습니다.')
      broadcastStatus()
    }
    const info = clients.get(sock)
    if (info) {
      clients.delete(sock)
      if (info.identity) send(extension, { type: 'session', action: 'close', client: info.key })
    }
    if (IDLE_EXIT_MS && clients.size === 0 && !extension) {
      setTimeout(() => { if (clients.size === 0 && !extension) process.exit(0) }, IDLE_EXIT_MS)
    }
  })

  sock.on('error', () => {})
})

// MV3 service worker 는 유휴 30초면 잠든다. WS 수신은 수명을 연장하므로 주기적으로 깨워 둔다.
setInterval(() => {
  if (extension) send(extension, { type: 'ping', t: Date.now() })
}, 20000)

process.on('SIGTERM', () => process.exit(0))
process.on('SIGINT', () => process.exit(0))
