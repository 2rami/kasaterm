// chrome.debugger 승격 경로. --remote-debugging-port 와는 별개 채널이라 Chrome 136+ 의 기본 프로필
// 차단 정책과 무관하다 — 평소 쓰는 프로필과 열어둔 탭을 그대로 조작할 수 있는 유일한 길.
const VERSION = '1.3'
const AUTO_DETACH_MS = 15000
// 진행 중인 명령이 있어도 영원히 붙어 있지는 않는다. 도구 쪽 최대 타임아웃(45초)보다 뒤에 둬서,
// 정상적으로 오래 걸리는 한 방은 살리고 응답이 영영 오지 않는 좀비만 걷는다.
const BUSY_DETACH_MS = 60000

const sessions = new Map() // tabId -> {pinned:Set<string>, console:[], network:Map, idleTimer, inflight, domains:Set}

export function isAttached(tabId) {
  return sessions.has(tabId)
}

export function sessionInfo() {
  return [...sessions.entries()].map(([tabId, s]) => ({
    tabId, pinned: [...s.pinned], inflight: s.inflight, consoleCount: s.console.length, networkCount: s.network.size,
  }))
}

// ⚠️보내는 동안은 idle 이 아니다. 명령이 도는 중에도 유휴 타이머가 그대로 돌면, 15초를 넘기는
// 한 방(20초 대기 스크립트 같은 것)이 자기가 처리되는 도중에 스스로 끊긴다 — 크롬은
// "Detached while handling command" 로 답하고, 도구 타임아웃(45초)은 구경도 못 한다.
// 그래서 명령 수를 세어, 하나라도 떠 있으면 짧은 유휴 시계를 쓰지 않는다(2026-08-05 실측).
// ⚠️응답이 영영 오지 않는 CDP 명령이 있다(특정 탭 상태에서 `watch` 가 30초 침묵한 실측 보고,
// 2026-08-05 아로나. 확장 소켓은 멀쩡했으니 워커가 죽은 게 아니라 그 명령만 멈춘 것이다).
// 무한정 기다리면 호출자는 도구 타임아웃까지 아무 정보도 못 받는다 — 어느 명령이 멈췄는지 이름을
// 달아 빠르게 실패시켜야 재시도든 우회든 할 수 있다. 다만 기본값은 무제한이다: `Runtime.evaluate`
// 처럼 **의도적으로** 오래 걸리는 명령을 시계로 죽이면 안 된다. 상한은 부르는 쪽이 정한다.
function send(tabId, method, params = {}, { timeoutMs = 0 } = {}) {
  const s = sessions.get(tabId)
  if (s) s.inflight++
  touch(tabId)
  return new Promise((resolve, reject) => {
    let settled = false
    // 타임아웃 뒤 콜백이 늦게 오면 inflight 가 두 번 줄어 유휴 계산이 어긋난다. 한 번만 정산한다.
    const finish = (fn, arg) => {
      if (settled) return
      settled = true
      clearTimeout(timer)
      if (s) s.inflight--
      touch(tabId)
      fn(arg)
    }
    const timer = timeoutMs
      ? setTimeout(() => finish(reject, new Error(`CDP_TIMEOUT: ${method} 이 ${timeoutMs}ms 안에 응답하지 않았습니다.`)), timeoutMs)
      : null
    chrome.debugger.sendCommand({ tabId }, method, params, (result) => {
      const err = chrome.runtime.lastError
      if (err) finish(reject, new Error(`CDP ${method} 실패: ${err.message}`))
      else finish(resolve, result)
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
  const s = { pinned: new Set(), console: [], network: new Map(), idleTimer: null, inflight: 0, domains: new Set(), reason }
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
  s.idleTimer = setTimeout(() => { detach(tabId).catch(() => {}) }, s.inflight > 0 ? BUSY_DETACH_MS : AUTO_DETACH_MS)
}

// `<도메인>.enable` 은 즉시 끝나야 하는 명령이라 상한을 둔다 — 이것이 멈추면 부른 도구가 통째로 침묵한다.
const ENABLE_TIMEOUT_MS = 8000

export async function ensureDomain(tabId, domain) {
  const s = await attach(tabId)
  if (s.domains.has(domain)) return s
  await send(tabId, `${domain}.enable`, {}, { timeoutMs: ENABLE_TIMEOUT_MS })
  s.domains.add(domain)
  return s
}

// 실패한 도메인 이름을 돌려준다 — 부분 성공을 감추면 로그가 왜 비어 있는지 알 수 없다.
export async function pin(tabId, what) {
  const s = await attach(tabId, what)
  // ⚠️핀을 먼저 등록한다. 도메인 활성화가 늦어도 그 사이 세션이 유휴로 떨어지면 안 된다.
  s.pinned.add(what)
  clearTimeout(s.idleTimer)

  const domains = what === 'console' ? ['Runtime', 'Log'] : what === 'network' ? ['Network'] : []
  // 순차로 기다리면 하나가 멈출 때 호출 전체가 막힌다. 병렬로 보내고 결과를 모은다.
  const settled = await Promise.allSettled(domains.map((d) => ensureDomain(tabId, d)))
  const failed = domains.filter((_, i) => settled[i].status === 'rejected')
  return { session: s, failed }
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
  return { dispatched: true, x, y }
}

export async function hover(tabId, { x, y }) {
  await attach(tabId)
  await send(tabId, 'Input.dispatchMouseEvent', { type: 'mouseMoved', x, y })
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
  return { dispatched: true }
}

// ★터치는 마우스와 다르게 **보이지 않는 탭에서 멈춘다**. 마우스·키보드는 hidden 탭에서도 즉시
// ack 이 오는데(그래서 지금까지 배경 탭 조작이 됐다), `Input.dispatchTouchEvent` 는 제스처 인식기를
// 거치고 그것이 hidden 탭에서는 돌지 않아 **응답이 영영 오지 않는다** — 2026-08-11 실측: 배경 탭에서
// touchStart 하나가 45초 도구 타임아웃까지 침묵했고, 탭을 앞으로 보내니 같은 명령이 즉시 `{}` 로 왔다.
// 그래서 ①부르는 쪽(tools.swipe)이 먼저 탭을 앞으로 보내고 ②여기서는 짧은 상한을 걸어, 그래도 멈추면
// 45초가 아니라 몇 초 만에 **원인을 이름에 달아** 실패시킨다. 침묵은 재시도도 우회도 못 하게 만든다.
const TOUCH_TIMEOUT_MS = 5000

const touchPoint = (x, y) => [{ x: Math.round(x), y: Math.round(y), id: 1, radiusX: 12, radiusY: 12, force: 1 }]

export async function swipe(tabId, { from, to, steps = 12 }) {
  await attach(tabId)
  const t = (type, points) => send(tabId, 'Input.dispatchTouchEvent', { type, touchPoints: points }, { timeoutMs: TOUCH_TIMEOUT_MS })
  // ⚠️touchStart 가 실패하면 **제스처가 열린 채로 남는다** — 그 뒤의 클릭·스크롤이 눌린 손가락이
  // 하나 더 있는 것처럼 어긋난다. 어디서 깨지든 손가락을 떼고 나간다.
  try {
    await t('touchStart', touchPoint(from.x, from.y))
    for (let i = 1; i <= steps; i++) {
      const p = i / steps
      await t('touchMove', touchPoint(from.x + (to.x - from.x) * p, from.y + (to.y - from.y) * p))
    }
    await t('touchEnd', [])
  } catch (e) {
    await t('touchEnd', []).catch(() => {})
    throw e
  }
  return { dispatched: true, from, to, steps }
}

export async function wheel(tabId, { x, y, deltaX = 0, deltaY = 0 }) {
  await attach(tabId)
  await send(tabId, 'Input.dispatchMouseEvent', { type: 'mouseWheel', x, y, deltaX, deltaY })
  return { dispatched: true }
}

export async function insertText(tabId, text) {
  await attach(tabId)
  await send(tabId, 'Input.insertText', { text })
  return { inserted: text.length }
}

export async function typeText(tabId, text) {
  await attach(tabId)
  for (const ch of text) {
    const code = ch.charCodeAt(0)
    await send(tabId, 'Input.dispatchKeyEvent', { type: 'keyDown', text: ch, key: ch, windowsVirtualKeyCode: code })
    await send(tabId, 'Input.dispatchKeyEvent', { type: 'keyUp', key: ch, windowsVirtualKeyCode: code })
  }
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
  return { pressed: key }
}

export async function screenshot(tabId, { fullPage = false, format = 'png', quality, clip } = {}) {
  await attach(tabId)
  await ensureDomain(tabId, 'Page')
  // clip 을 줄 때도 captureBeyondViewport 가 필요하다 — 요소가 스크롤 아래에 있으면
  // 그것 없이는 뷰포트에 걸린 부분만 찍히고 나머지가 잘린다.
  const params = { format, captureBeyondViewport: fullPage || !!clip }
  if (format === 'jpeg' && quality) params.quality = quality
  if (clip) {
    params.clip = { ...clip, scale: 1 }
  } else if (fullPage) {
    const { cssContentSize } = await send(tabId, 'Page.getLayoutMetrics')
    if (cssContentSize) {
      params.clip = { x: 0, y: 0, width: cssContentSize.width, height: cssContentSize.height, scale: 1 }
    }
  }
  const { data } = await send(tabId, 'Page.captureScreenshot', params)
  return data
}

// ⚠️`replMode` 와 `awaitPromise` 는 같이 못 쓴다 — replMode 가 이기면서 Promise 가 resolve 되기
// 전에 직렬화돼 **`{}` 만 돌아온다**(실측). 값을 받는 쪽이 훨씬 중요하므로 replMode 를 버리고,
// 대신 top-level await 는 async IIFE 로 감싸 되살린다. 표현식이 우선이고(대부분 `await fetch(…)`
// 꼴이다) 그게 SyntaxError 면 statement 로 한 번 더 — 그때는 `return` 이 호출자 몫이다.
export async function evaluate(tabId, expression, { awaitPromise = true } = {}) {
  await ensureDomain(tabId, 'Runtime')
  // ⚠️`return` 도 감싸기 조건에 넣어야 한다. `await` 만 봤을 때는 await 없이 `return` 만 쓴 코드가
  // 감싸이지 않은 채 최상위로 나가 `Illegal return statement` 로 죽었다 — 값을 돌려주려면 return 을
  // 쓰라고 안내해 두고 정작 그 형태가 깨지고 있었다(2026-08-05 실측).
  const needsWrap = awaitPromise && /\b(await|return)\b/.test(expression) && !/^\s*\(\s*async/.test(expression)
  const forms = needsWrap
    ? [`(async () => (${expression}))()`, `(async () => { ${expression} })()`]
    : [expression]

  let last
  for (const form of forms) {
    const res = await send(tabId, 'Runtime.evaluate', {
      expression: form, awaitPromise, returnByValue: true, userGesture: true,
    })
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
  return { attached: files.length, selector: sel, ref }
}

export { send as raw }
