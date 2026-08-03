// chrome.debugger 승격 경로. --remote-debugging-port 와는 별개 채널이라 Chrome 136+ 의 기본 프로필
// 차단 정책과 무관하다 — 평소 쓰는 프로필과 열어둔 탭을 그대로 조작할 수 있는 유일한 길.
const VERSION = '1.3'
const AUTO_DETACH_MS = 15000

const sessions = new Map() // tabId -> {pinned:Set<string>, console:[], network:Map, idleTimer, domains:Set}

export function isAttached(tabId) {
  return sessions.has(tabId)
}

export function sessionInfo() {
  return [...sessions.entries()].map(([tabId, s]) => ({
    tabId, pinned: [...s.pinned], consoleCount: s.console.length, networkCount: s.network.size,
  }))
}

function send(tabId, method, params = {}) {
  return new Promise((resolve, reject) => {
    chrome.debugger.sendCommand({ tabId }, method, params, (result) => {
      const err = chrome.runtime.lastError
      if (err) reject(new Error(`CDP ${method} 실패: ${err.message}`))
      else resolve(result)
    })
  })
}

export async function attach(tabId, reason = 'command') {
  if (sessions.has(tabId)) {
    touch(tabId)
    return sessions.get(tabId)
  }
  await new Promise((resolve, reject) => {
    chrome.debugger.attach({ tabId }, VERSION, () => {
      const err = chrome.runtime.lastError
      if (!err) return resolve()
      if (/Another debugger|already attached/i.test(err.message)) {
        reject(new Error(`DEVTOOLS_CONFLICT: 이 탭에 이미 다른 디버거(개발자도구)가 붙어 있습니다. DevTools 를 닫고 다시 시도하세요. (${err.message})`))
      } else {
        reject(new Error(`ATTACH_FAILED: ${err.message}`))
      }
    })
  })
  const s = { pinned: new Set(), console: [], network: new Map(), idleTimer: null, domains: new Set(), reason }
  sessions.set(tabId, s)
  touch(tabId)
  return s
}

export async function detach(tabId) {
  const s = sessions.get(tabId)
  if (!s) return { detached: false }
  clearTimeout(s.idleTimer)
  sessions.delete(tabId)
  await new Promise((resolve) => chrome.debugger.detach({ tabId }, () => { void chrome.runtime.lastError; resolve() }))
  return { detached: true }
}

// 배너를 오래 띄우지 않는다. 관측(console/network)을 켠 탭만 붙어 있는다.
function touch(tabId) {
  const s = sessions.get(tabId)
  if (!s) return
  clearTimeout(s.idleTimer)
  if (s.pinned.size > 0) return
  s.idleTimer = setTimeout(() => { detach(tabId).catch(() => {}) }, AUTO_DETACH_MS)
}

export async function ensureDomain(tabId, domain) {
  const s = await attach(tabId)
  if (s.domains.has(domain)) return s
  await send(tabId, `${domain}.enable`)
  s.domains.add(domain)
  return s
}

export async function pin(tabId, what) {
  const s = await attach(tabId, what)
  if (what === 'console') {
    await ensureDomain(tabId, 'Runtime')
    await ensureDomain(tabId, 'Log')
  } else if (what === 'network') {
    await ensureDomain(tabId, 'Network')
  }
  s.pinned.add(what)
  clearTimeout(s.idleTimer)
  return s
}

export async function unpin(tabId, what) {
  const s = sessions.get(tabId)
  if (!s) return
  s.pinned.delete(what)
  touch(tabId)
}

export function drain(tabId, kind, { clear = false } = {}) {
  const s = sessions.get(tabId)
  if (!s) return []
  if (kind === 'console') {
    const out = s.console.slice()
    if (clear) s.console.length = 0
    return out
  }
  const out = [...s.network.values()]
  if (clear) s.network.clear()
  return out
}

chrome.debugger.onDetach.addListener((source, reason) => {
  const s = sessions.get(source.tabId)
  if (!s) return
  clearTimeout(s.idleTimer)
  sessions.delete(source.tabId)
  // canceled_by_user = 선생님이 그 탭에서 DevTools 를 열었다는 뜻. 조용한 경로로 폴백해야 한다.
  self.__ccLastDetach = { tabId: source.tabId, reason, at: Date.now() }
})

chrome.debugger.onEvent.addListener((source, method, params) => {
  const s = sessions.get(source.tabId)
  if (!s) return

  if (method === 'Runtime.consoleAPICalled') {
    s.console.push({
      type: params.type,
      text: (params.args || []).map(preview).join(' '),
      ts: params.timestamp,
    })
  } else if (method === 'Log.entryAdded') {
    s.console.push({ type: params.entry.level, text: params.entry.text, url: params.entry.url, ts: params.entry.timestamp })
  } else if (method === 'Runtime.exceptionThrown') {
    const d = params.exceptionDetails
    s.console.push({ type: 'exception', text: d.exception?.description || d.text, url: d.url, ts: params.timestamp })
  } else if (method === 'Network.requestWillBeSent') {
    s.network.set(params.requestId, {
      id: params.requestId, url: params.request.url, method: params.request.method,
      type: params.type, status: null, ts: params.timestamp,
    })
  } else if (method === 'Network.responseReceived') {
    const r = s.network.get(params.requestId)
    if (r) { r.status = params.response.status; r.mimeType = params.response.mimeType }
  } else if (method === 'Network.loadingFailed') {
    const r = s.network.get(params.requestId)
    if (r) { r.status = 'failed'; r.error = params.errorText }
  }

  if (s.console.length > 500) s.console.splice(0, s.console.length - 500)
  if (s.network.size > 500) {
    const keys = [...s.network.keys()].slice(0, s.network.size - 500)
    for (const k of keys) s.network.delete(k)
  }
})

function preview(arg) {
  if (arg.value !== undefined) return String(arg.value)
  if (arg.description) return arg.description
  if (arg.preview) return JSON.stringify(arg.preview.properties?.map((p) => `${p.name}:${p.value}`) || arg.preview)
  return arg.type
}

const MODIFIER_BITS = { alt: 1, ctrl: 2, control: 2, meta: 4, cmd: 4, command: 4, shift: 8 }
const KEY_CODES = {
  Enter: 13, Tab: 9, Escape: 27, Backspace: 8, Delete: 46, ArrowUp: 38, ArrowDown: 40,
  ArrowLeft: 37, ArrowRight: 39, Home: 36, End: 35, PageUp: 33, PageDown: 34, ' ': 32,
}

export function modifierMask(modifiers) {
  if (!modifiers) return 0
  return String(modifiers).split('+').reduce((m, k) => m | (MODIFIER_BITS[k.trim().toLowerCase()] || 0), 0)
}

export async function click(tabId, { x, y, button = 'left', clickCount = 1, modifiers = 0 }) {
  await attach(tabId)
  const base = { x, y, button, clickCount, modifiers }
  await send(tabId, 'Input.dispatchMouseEvent', { ...base, type: 'mousePressed' })
  await send(tabId, 'Input.dispatchMouseEvent', { ...base, type: 'mouseReleased' })
  touch(tabId)
  return { dispatched: true, x, y }
}

export async function hover(tabId, { x, y }) {
  await attach(tabId)
  await send(tabId, 'Input.dispatchMouseEvent', { type: 'mouseMoved', x, y })
  touch(tabId)
  return { dispatched: true }
}

export async function drag(tabId, { from, to }) {
  await attach(tabId)
  await send(tabId, 'Input.dispatchMouseEvent', { type: 'mousePressed', x: from.x, y: from.y, button: 'left', clickCount: 1 })
  const steps = 10
  for (let i = 1; i <= steps; i++) {
    await send(tabId, 'Input.dispatchMouseEvent', {
      type: 'mouseMoved', button: 'left',
      x: Math.round(from.x + ((to.x - from.x) * i) / steps),
      y: Math.round(from.y + ((to.y - from.y) * i) / steps),
    })
  }
  await send(tabId, 'Input.dispatchMouseEvent', { type: 'mouseReleased', x: to.x, y: to.y, button: 'left', clickCount: 1 })
  touch(tabId)
  return { dispatched: true }
}

export async function wheel(tabId, { x, y, deltaX = 0, deltaY = 0 }) {
  await attach(tabId)
  await send(tabId, 'Input.dispatchMouseEvent', { type: 'mouseWheel', x, y, deltaX, deltaY })
  touch(tabId)
  return { dispatched: true }
}

export async function insertText(tabId, text) {
  await attach(tabId)
  await send(tabId, 'Input.insertText', { text })
  touch(tabId)
  return { inserted: text.length }
}

export async function typeText(tabId, text) {
  await attach(tabId)
  for (const ch of text) {
    const code = ch.charCodeAt(0)
    await send(tabId, 'Input.dispatchKeyEvent', { type: 'keyDown', text: ch, key: ch, windowsVirtualKeyCode: code })
    await send(tabId, 'Input.dispatchKeyEvent', { type: 'keyUp', key: ch, windowsVirtualKeyCode: code })
  }
  touch(tabId)
  return { typed: text.length }
}

export async function pressKey(tabId, key, modifiers = 0) {
  await attach(tabId)
  const vk = KEY_CODES[key] ?? (key.length === 1 ? key.toUpperCase().charCodeAt(0) : 0)
  const isChar = key.length === 1
  const common = { key, code: isChar ? `Key${key.toUpperCase()}` : key, windowsVirtualKeyCode: vk, modifiers }
  await send(tabId, 'Input.dispatchKeyEvent', { ...common, type: isChar && !modifiers ? 'keyDown' : 'rawKeyDown', ...(isChar && !modifiers ? { text: key } : {}) })
  if (key === 'Enter') await send(tabId, 'Input.dispatchKeyEvent', { ...common, type: 'char', text: '\r' })
  await send(tabId, 'Input.dispatchKeyEvent', { ...common, type: 'keyUp' })
  touch(tabId)
  return { pressed: key }
}

export async function screenshot(tabId, { fullPage = false, format = 'png', quality } = {}) {
  await attach(tabId)
  await ensureDomain(tabId, 'Page')
  const params = { format, captureBeyondViewport: fullPage }
  if (format === 'jpeg' && quality) params.quality = quality
  if (fullPage) {
    const { cssContentSize } = await send(tabId, 'Page.getLayoutMetrics')
    if (cssContentSize) {
      params.clip = { x: 0, y: 0, width: cssContentSize.width, height: cssContentSize.height, scale: 1 }
    }
  }
  const { data } = await send(tabId, 'Page.captureScreenshot', params)
  touch(tabId)
  return data
}

// ⚠️`replMode` 와 `awaitPromise` 는 같이 못 쓴다 — replMode 가 이기면서 Promise 가 resolve 되기
// 전에 직렬화돼 **`{}` 만 돌아온다**(실측). 값을 받는 쪽이 훨씬 중요하므로 replMode 를 버리고,
// 대신 top-level await 는 async IIFE 로 감싸 되살린다. 표현식이 우선이고(대부분 `await fetch(…)`
// 꼴이다) 그게 SyntaxError 면 statement 로 한 번 더 — 그때는 `return` 이 호출자 몫이다.
export async function evaluate(tabId, expression, { awaitPromise = true } = {}) {
  await ensureDomain(tabId, 'Runtime')
  const needsWrap = awaitPromise && /\bawait\b/.test(expression) && !/^\s*\(\s*async/.test(expression)
  const forms = needsWrap
    ? [`(async () => (${expression}))()`, `(async () => { ${expression} })()`]
    : [expression]

  let last
  for (const form of forms) {
    const res = await send(tabId, 'Runtime.evaluate', {
      expression: form, awaitPromise, returnByValue: true, userGesture: true,
    })
    touch(tabId)
    last = res.exceptionDetails
    if (!last) return res.result?.value ?? res.result?.description ?? null
    // 감싸기 때문에 생긴 문법 오류만 다음 형태로 넘어간다. 진짜 런타임 오류는 그대로 알린다.
    if (last.exception?.className !== 'SyntaxError') break
  }
  throw new Error(`JS_ERROR: ${last.exception?.description || last.text}`)
}

export async function setFileInputFiles(tabId, files, { ref, selector } = {}) {
  await ensureDomain(tabId, 'DOM')
  const { root } = await send(tabId, 'DOM.getDocument', { depth: -1, pierce: true })
  const sel = selector || 'input[type=file]'
  const { nodeId } = await send(tabId, 'DOM.querySelector', { nodeId: root.nodeId, selector: sel })
  if (!nodeId) throw new Error(`FILE_INPUT_NOT_FOUND: 선택자 "${sel}" 에 맞는 파일 입력이 없습니다.`)
  await send(tabId, 'DOM.setFileInputFiles', { nodeId, files })
  touch(tabId)
  return { attached: files.length, selector: sel, ref }
}

export { send as raw }
