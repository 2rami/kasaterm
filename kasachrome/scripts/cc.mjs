#!/usr/bin/env node
// MCP 를 거치지 않고 브리지를 직접 두드리는 CLI. MCP 등록이 아직 안 붙은 세션에서 검증할 때 쓴다.
//   node scripts/cc.mjs '[["list_tabs",{}],["navigate",{"tabId":1,"url":"example.com"}]]'
import { WebSocket } from 'ws'
import { resolveIdentity } from '../mcp/identity.mjs'
import { PORT } from '../extension/port.js'

const ws = new WebSocket(`ws://127.0.0.1:${PORT}`)
let id = 0
const pending = new Map()

await new Promise((resolve, reject) => {
  ws.once('open', resolve)
  ws.once('error', (e) => reject(new Error(`브리지에 못 붙었습니다: ${e.message}. npm run bridge 로 켜세요.`)))
})
ws.send(JSON.stringify({ type: 'hello', role: 'client', identity: resolveIdentity() }))
ws.on('message', (raw) => {
  const m = JSON.parse(raw.toString())
  if (m.type !== 'result') return
  const p = pending.get(m.id)
  if (p) { pending.delete(m.id); p(m) }
})

function call(tool, args = {}, timeoutMs = 30000) {
  const myId = ++id
  return new Promise((resolve) => {
    pending.set(myId, resolve)
    ws.send(JSON.stringify({ type: 'call', id: myId, tool, args, timeoutMs }))
  })
}

const steps = JSON.parse(process.argv[2])
const limit = Number(process.argv[3] || 2500)
for (const [tool, args] of steps) {
  const r = await call(tool, args)
  const body = r.ok ? r.result : `ERROR: ${r.error}`
  let s = typeof body === 'string' ? body : JSON.stringify(body)
  // 스크린샷 base64 가 통째로 쏟아지는 것을 막는다
  s = s.replace(/"data":"[A-Za-z0-9+/=]{200,}"/g, (m) => `"data":"<base64 ${m.length}자>"`)
  console.log(`\n### ${tool} ${JSON.stringify(args)}\n${s.length > limit ? s.slice(0, limit) + '\n…(잘림)' : s}`)
}
ws.close()
process.exit(0)
