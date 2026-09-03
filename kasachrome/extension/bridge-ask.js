// 확장에서 브리지로 「물어보고 답을 받는」 요청. 브리지가 보내오는 명령(call)은 background 가
// 받아 처리하지만, 반대로 확장이 먼저 묻는 길은 없었다 — 프로세스를 띄우는 것처럼 브라우저가
// 못 하는 일을 부탁하려면 이 방향이 필요하다.
// 소켓은 background 가 쥐고 있으므로 여기에는 배선만 둔다(서로 import 하면 순환이 된다).

let sender = null
let seq = 0
const waiting = new Map()

export function setBridgeSender(fn) { sender = fn }

// background 의 onmessage 가 답을 이리로 넘긴다.
export function bridgeResolve(msg) {
  const p = waiting.get(msg.id)
  if (!p) return
  waiting.delete(msg.id)
  clearTimeout(p.timer)
  p.resolve(msg)
}

export function askBridge(type, extra = {}, timeoutMs = 20000) {
  return new Promise((resolve, reject) => {
    if (!sender) return reject(new Error('BRIDGE_NOT_CONNECTED: 브리지에 붙어 있지 않습니다'))
    const id = `x${(seq += 1)}`
    const timer = setTimeout(() => {
      waiting.delete(id)
      reject(new Error('BRIDGE_TIMEOUT: 브리지가 시간 안에 답하지 않았습니다'))
    }, timeoutMs)
    waiting.set(id, { resolve, timer })
    try { sender({ type, id, ...extra }) } catch (e) { waiting.delete(id); clearTimeout(timer); reject(e) }
  })
}
