// 툴 핸들러. 기본은 조용한 경로(content script)로 돌고, 그게 no-op 이면 같은 동작을 CDP 로 재시도한다.
// 그래서 평소엔 디버깅 배너가 안 뜨지만 능력치는 CDP 와 동일하다.
import * as cdp from './cdp.js'
import { page, restricted } from './page.js'
import { setTask, forgetTab, identityOf, showCursor } from './sessions.js'

// 워커가 언제 떴는지. 이 값이 방금 태어난 것으로 나오면 직전 명령이 실패한 이유는 대개 워커가
// 도중에 죽은 것이다 — 끊김의 원인을 코드에서 찾기 전에 여기부터 본다.
const WORKER_STARTED = Date.now()
let jobSeq = 0

function tabsQuery(q) {
  return new Promise((resolve) => chrome.tabs.query(q, resolve))
}

async function resolveTabId(tabId) {
  if (tabId) return Number(tabId)
  const [active] = await tabsQuery({ active: true, lastFocusedWindow: true })
  if (!active) throw new Error('NO_ACTIVE_TAB: 활성 탭을 찾지 못했습니다. list_tabs 로 확인하고 tabId 를 지정하세요.')
  return active.id
}

// 스킴이 없으면 chrome 이 확장 상대경로로 해석해버린다. localhost 는 http 가 기본이다.
function normalizeUrl(url) {
  if (!url) return 'about:blank'
  if (/^(https?|file|ftp|about|chrome|chrome-extension|devtools|data|blob|view-source):/i.test(url)) return url
  if (/^(localhost|127\.0\.0\.1|0\.0\.0\.0|\[::1\])(:\d+)?([/?#]|$)/i.test(url)) return `http://${url}`
  return `https://${url}`
}

function waitForLoad(tabId, timeoutMs = 20000) {
  return new Promise((resolve) => {
    let done = false
    const finish = (status) => {
      if (done) return
      done = true
      chrome.tabs.onUpdated.removeListener(listener)
      clearTimeout(timer)
      resolve(status)
    }
    const listener = (id, info) => { if (id === tabId && info.status === 'complete') finish('complete') }
    chrome.tabs.onUpdated.addListener(listener)
    const timer = setTimeout(() => finish('timeout'), timeoutMs)
    chrome.tabs.get(tabId).then((t) => { if (t.status === 'complete') finish('complete') }).catch(() => {})
  })
}

// 클릭이 네비게이션을 유발했으면 그것이 끝난 뒤에 결과를 준다. 안 그러면 곧바로 이어지는 read_page 가
// 이동 직전 페이지를 읽는다.
async function settle(tabId, ms = 300) {
  await new Promise((r) => setTimeout(r, ms))
  let tab = await chrome.tabs.get(tabId).catch(() => null)
  if (!tab) return { closed: true }
  if (tab.status === 'loading') {
    await waitForLoad(tabId, 15000)
    tab = await chrome.tabs.get(tabId).catch(() => null)
    return { navigated: true, url: tab?.url, title: tab?.title }
  }
  return { navigated: false, url: tab.url }
}

async function clickOn(id, { ref, coordinate, button = 'left', clickCount = 1, modifiers, trusted = false, retry = true }) {
  const mods = cdp.modifierMask(modifiers)

  if (coordinate) {
    const [x, y] = coordinate
    await showCursor(id, x, y, true)
    const r = await cdp.click(id, { x, y, button, clickCount, modifiers: mods })
    return { ...r, via: 'cdp', reason: 'coordinate' }
  }
  if (!ref) throw new Error('MISSING_TARGET: ref 나 coordinate 중 하나가 필요합니다. find 나 read_page 로 ref 를 얻으세요.')

  if (trusted || button !== 'left' || clickCount > 1 || mods) {
    const { box, name } = await page(id, 'box', { ref })
    await showCursor(id, box.x, box.y, true)
    const r = await cdp.click(id, { x: box.x, y: box.y, button, clickCount, modifiers: mods })
    return { ...r, target: name, via: 'cdp', reason: trusted ? 'trusted' : 'modifier/button' }
  }

  const { box: aim, name: aimName } = await page(id, 'box', { ref })
  await showCursor(id, aim.x, aim.y, true)
  const res = await page(id, 'click', { ref })
  if (res.changed) return { ...res, target: aimName, via: 'content' }
  if (!retry) {
    return { ...res, target: aimName, via: 'content', reason: 'no-op but retry disabled', note: '합성 클릭이 변화를 못 만들었지만 retry:false 라 재시도하지 않았습니다. 정말 안 먹었다면 trusted:true 로 다시 부르세요.' }
  }
  // 합성 클릭이 아무 변화도 못 만들었다. 진짜 입력으로 한 번 더 친다.
  const r = await cdp.click(id, { x: res.box.x, y: res.box.y, button: 'left', clickCount: 1, modifiers: 0 })
  return {
    ...r, target: aimName, via: 'cdp', reason: 'content-click-was-noop', visibilityState: res.visibilityState,
    // 합성 클릭이 실은 먹었는데 화면 변화가 늦었을 뿐이면 이 재시도가 두 번째 실행이 된다.
    doubleFireRisk: '클릭이 두 번 갔을 수 있습니다. 멱등하지 않은 버튼(제출·좋아요·수량)이면 결과를 확인하세요. 다음부터는 trusted:true(한 번만, 진짜 입력) 또는 retry:false 를 쓰세요.',
  }
}

const handlers = {
  async status(_args, ctx = {}) {
    const tabs = await tabsQuery({})
    return {
      connected: true,
      // 확장을 고쳤는데 동작이 그대로면 service worker 가 옛 코드를 물고 있는 것이다. 먼저 여기를 본다.
      version: chrome.runtime.getManifest().version,
      workerAgeMs: Date.now() - WORKER_STARTED,
      tabCount: tabs.length,
      // 프사 base64 는 25KB 라 응답에 실으면 안 된다
      identity: (() => {
        const s = identityOf(ctx.client)
        return s ? { name: s.name, slug: s.slug, paneId: s.paneId, color: s.groupColor } : null
      })(),
      debuggerSessions: cdp.sessionInfo(),
      lastDetach: self.__ccLastDetach || null,
      lastError: self.__ccLastError || null,
    }
  },

  async list_tabs({ windowId } = {}) {
    const tabs = await tabsQuery(windowId ? { windowId } : {})
    return {
      tabs: tabs.map((t) => ({
        tabId: t.id, windowId: t.windowId, title: t.title, url: t.url,
        active: t.active, audible: t.audible || false, status: t.status,
        attached: cdp.isAttached(t.id),
      })),
    }
  },

  // 기본은 백그라운드다. 사람이 보던 화면을 에이전트가 뺏으면 안 된다.
  // 애니메이션·미디어처럼 보이는 탭이어야 도는 것을 확인할 때만 active:true 나 activate_tab 을 쓴다.
  async new_tab({ url, active = false, windowId } = {}) {
    // ⚠️service worker 에서 부르면 create 의 active:false 가 무시되고 새 탭이 앞으로 나온다(실측 — 같은
    // 호출이 확장 페이지에서는 존중된다). 사람이 보던 탭을 뺏지 않도록 직전 활성 탭을 곧바로 되돌린다.
    // 되돌릴 대상은 반드시 "새 탭이 실제로 생긴 창"의 것이어야 한다 — 창이 여러 개일 때
    // 마지막 포커스 창을 기준으로 잡으면 엉뚱한 창만 되살리고 정작 튀어나온 탭은 그대로 남는다.
    const prevOf = new Map((await tabsQuery({ active: true })).map((t) => [t.windowId, t.id]))
    const tab = await chrome.tabs.create({ url: normalizeUrl(url), active, ...(windowId ? { windowId } : {}) })
    const restore = async () => {
      if (active) return
      const now = await chrome.tabs.get(tab.id).catch(() => null)
      if (!now?.active) return
      const back = prevOf.get(now.windowId)
      if (back && back !== tab.id) await chrome.tabs.update(back, { active: true }).catch(() => {})
    }
    await restore()
    if (url) await waitForLoad(tab.id)
    // 로딩이 끝나는 시점에 다시 앞으로 나오는 경우가 있어 한 번 더 확인한다
    await restore()
    const fresh = await chrome.tabs.get(tab.id)
    // active 를 돌려주는 이유: 백그라운드로 열었는데 앞으로 튀어나오면 여기서 바로 드러난다
    return { tabId: fresh.id, url: fresh.url, title: fresh.title, active: fresh.active }
  },

  async close_tab({ tabId }) {
    const id = await resolveTabId(tabId)
    await cdp.detach(id).catch(() => {})
    forgetTab(id)
    await chrome.tabs.remove(id)
    return { closed: id }
  },

  async set_task({ task }, ctx = {}) {
    return setTask(ctx.client, task)
  },

  // 탭을 그룹에서 빼낸다. 마지막 탭이 빠지면 그룹은 크롬이 알아서 없앤다(그룹 삭제 API 는 없다).
  // ungroup 은 tabs 권한이면 되고 tabGroups 권한은 필요 없다.
  async ungroup_tabs({ tabIds } = {}) {
    const NONE = -1
    const tabs = await tabsQuery({})
    const grouped = tabs.filter((t) => t.groupId !== undefined && t.groupId !== NONE)
    const targets = tabIds && tabIds.length ? tabIds.map(Number) : grouped.map((t) => t.id)
    if (!targets.length) return { ungrouped: 0, note: '그룹에 속한 탭이 없습니다.' }
    const before = [...new Set(grouped.filter((t) => targets.includes(t.id)).map((t) => t.groupId))]
    await chrome.tabs.ungroup(targets)
    const after = (await tabsQuery({})).filter((t) => t.groupId !== undefined && t.groupId !== NONE)
    return {
      ungrouped: targets.length,
      groupsRemoved: before.length,
      tabs: grouped.filter((t) => targets.includes(t.id)).map((t) => (t.title || '').slice(0, 30)),
      stillGrouped: after.length,
    }
  },

  // 창은 앞으로 끌어오지 않는다 — 사람이 다른 앱을 보고 있을 때 크롬이 튀어나오면 작업을 방해한다.
  // 크롬 창 안에서 탭만 바꾸므로 rAF·미디어는 정상적으로 돈다(그게 이 툴의 목적).
  async activate_tab({ tabId }) {
    const id = await resolveTabId(tabId)
    const tab = await chrome.tabs.update(id, { active: true })
    return { tabId: id, url: tab.url, windowFocused: false }
  },

  async navigate({ tabId, url }) {
    const id = await resolveTabId(tabId)
    if (url === 'back' || url === 'forward') {
      await (url === 'back' ? chrome.tabs.goBack(id) : chrome.tabs.goForward(id))
    } else {
      await chrome.tabs.update(id, { url: normalizeUrl(url) })
    }
    const status = await waitForLoad(id)
    const tab = await chrome.tabs.get(id)
    return { tabId: id, url: tab.url, title: tab.title, load: status }
  },

  async read_page({ tabId, filter = 'interactive', maxChars = 40000 }) {
    const id = await resolveTabId(tabId)
    return await page(id, 'snapshot', { filter, maxChars })
  },

  async get_text({ tabId, maxChars = 30000 }) {
    const id = await resolveTabId(tabId)
    return await page(id, 'text', { maxChars })
  },

  async find({ tabId, query }) {
    const id = await resolveTabId(tabId)
    return await page(id, 'find', { query })
  },

  async screenshot({ tabId, fullPage = false, format = 'png', quality }) {
    const id = await resolveTabId(tabId)
    const tab = await chrome.tabs.get(id)
    // captureVisibleTab 은 배너가 안 뜨지만 활성 탭의 보이는 영역만 찍는다. 나머지는 CDP 가 필요하다.
    if (!fullPage && tab.active) {
      try {
        const dataUrl = await chrome.tabs.captureVisibleTab(tab.windowId, { format, ...(quality ? { quality } : {}) })
        return { data: dataUrl.split(',')[1], format, via: 'captureVisibleTab' }
      } catch { /* 권한·타이밍 문제면 CDP 로 내려간다 */ }
    }
    const data = await cdp.screenshot(id, { fullPage, format, quality })
    return { data, format, via: 'cdp' }
  },

  async click({ tabId, ...rest }) {
    const id = await resolveTabId(tabId)
    const out = await clickOn(id, rest)
    return { ...out, page: await settle(id) }
  },

  async hover({ tabId, ref, coordinate }) {
    const id = await resolveTabId(tabId)
    let x, y
    if (coordinate) { [x, y] = coordinate } else {
      const { box } = await page(id, 'box', { ref })
      x = box.x; y = box.y
    }
    return await cdp.hover(id, { x, y })
  },

  async drag({ tabId, from, to, fromRef, toRef }) {
    const id = await resolveTabId(tabId)
    const a = fromRef ? (await page(id, 'box', { ref: fromRef })).box : { x: from[0], y: from[1] }
    const b = toRef ? (await page(id, 'box', { ref: toRef })).box : { x: to[0], y: to[1] }
    return await cdp.drag(id, { from: a, to: b })
  },

  async fill({ tabId, ref, value, trusted = false }) {
    const id = await resolveTabId(tabId)
    if (!trusted) {
      const res = await page(id, 'fill', { ref, value })
      if (res.matched) return { ...res, target: res.name, via: 'content' }
      // 값이 안 붙었다 = CodeMirror·검증 폼 계열. 실제 키 입력으로 다시 넣는다.
      await cdp.click(id, { x: res.box.x, y: res.box.y })
      await cdp.pressKey(id, 'a', cdp.modifierMask('meta'))
      await cdp.insertText(id, String(value))
      return { via: 'cdp', target: res.name, reason: 'content-fill-did-not-stick', box: res.box }
    }
    const { box, name } = await page(id, 'box', { ref })
    await cdp.click(id, { x: box.x, y: box.y })
    await cdp.pressKey(id, 'a', cdp.modifierMask('meta'))
    await cdp.insertText(id, String(value))
    return { via: 'cdp', target: name, reason: 'trusted', box }
  },

  async type({ tabId, text, trusted = false }) {
    const id = await resolveTabId(tabId)
    if (!trusted) {
      const res = await page(id, 'type', { text })
      if (res.changed) return { ...res, via: 'content' }
    }
    await cdp.insertText(id, text)
    return { via: 'cdp', typed: text.length }
  },

  async press_key({ tabId, key, modifiers, repeat = 1 }) {
    const id = await resolveTabId(tabId)
    const mods = cdp.modifierMask(modifiers)
    // 키보드는 합성이 씹히는 경우가 잦아 처음부터 진짜 입력으로 보낸다.
    for (let i = 0; i < repeat; i++) await cdp.pressKey(id, key, mods)
    return { pressed: key, repeat, via: 'cdp' }
  },

  async scroll({ tabId, direction = 'down', amount = 3, ref, coordinate }) {
    const id = await resolveTabId(tabId)
    const res = await page(id, 'scroll', { direction, amount, ref })
    if (res.changed) return { ...res, via: 'content' }
    // 숨은 탭이거나 커스텀 스크롤 컨테이너다. 진짜 휠을 굴린다.
    const delta = amount * 100
    const point = coordinate ? { x: coordinate[0], y: coordinate[1] } : { x: 400, y: 400 }
    await cdp.wheel(id, {
      ...point,
      deltaX: direction === 'left' ? -delta : direction === 'right' ? delta : 0,
      deltaY: direction === 'up' ? -delta : direction === 'down' ? delta : 0,
    })
    const after = await page(id, 'scroll', { direction, amount: 0 })
    return { via: 'cdp', reason: 'content-scroll-was-noop', at: after.to, visibilityState: res.visibilityState }
  },

  async scroll_to({ tabId, ref }) {
    const id = await resolveTabId(tabId)
    return await page(id, 'scroll_to', { ref })
  },

  // 30초를 넘기는 스크립트는 CDP 한 방으로 받을 수 없다. 명령이 응답을 기다리는 동안 확장 워커가
  // 잠들면 디버거 세션이 통째로 떨어지기 때문이다 — 20초는 값이 오고 35초는 응답조차 없다(실측).
  // 그래서 오래 걸리는 일은 페이지 안에서 돌리고 CDP 는 시작과 회수만 맡는다. 명령 하나하나가
  // 1초 미만이라 워커 수명과 무관해진다.
  // ⚠️작업은 그 페이지의 window 에 산다. 도중에 페이지가 이동하면 작업도 함께 사라진다.
  async eval_js({ tabId, code, background, jobId }) {
    const id = await resolveTabId(tabId)

    if (jobId) {
      const key = JSON.stringify(String(jobId))
      const r = await cdp.evaluate(id, `(async () => {
        const j = (window.__ccJobs || {})[${key}]
        if (!j) return { jobId: ${key}, missing: true }
        return { jobId: ${key}, done: j.done, value: j.value, error: j.error, elapsedMs: Date.now() - j.t0 }
      })()`)
      if (r && r.missing) {
        throw new Error(`JOB_NOT_FOUND: 작업 ${jobId} 이 이 탭에 없습니다. 페이지가 이동했다면 작업도 함께 사라집니다.`)
      }
      return r
    }

    if (!background) return { value: await cdp.evaluate(id, code) }

    const key = JSON.stringify(`job${++jobSeq}_${Date.now().toString(36)}`)
    return await cdp.evaluate(id, `(async () => {
      window.__ccJobs = window.__ccJobs || {}
      const j = { done: false, t0: Date.now() }
      window.__ccJobs[${key}] = j
      ;(async () => { ${code} })().then(
        (v) => { j.value = v; j.done = true },
        (e) => { j.error = String((e && e.message) || e); j.done = true },
      )
      return { jobId: ${key}, started: true }
    })()`)
  },

  async watch({ tabId, console: wantConsole, network: wantNetwork }) {
    const id = await resolveTabId(tabId)
    const on = []
    const off = []
    if (wantConsole === true) { await cdp.pin(id, 'console'); on.push('console') }
    if (wantConsole === false) { await cdp.unpin(id, 'console'); off.push('console') }
    if (wantNetwork === true) { await cdp.pin(id, 'network'); on.push('network') }
    if (wantNetwork === false) { await cdp.unpin(id, 'network'); off.push('network') }
    return { tabId: id, enabled: on, disabled: off, note: on.length ? '수집이 켜진 동안 그 탭에 디버깅 배너가 유지됩니다.' : undefined }
  },

  async console_logs({ tabId, pattern, onlyErrors = false, limit = 100, clear = false }) {
    const id = await resolveTabId(tabId)
    if (!cdp.isAttached(id)) {
      await cdp.pin(id, 'console')
      return { logs: [], note: '이 탭의 콘솔 수집을 지금 켰습니다. 붙기 전에 찍힌 로그는 없으니, 페이지를 새로고침하거나 동작을 다시 시켜본 뒤 이 툴을 한 번 더 부르세요.' }
    }
    let logs = cdp.drain(id, 'console', { clear })
    if (onlyErrors) logs = logs.filter((l) => /error|exception|assert/i.test(l.type))
    if (pattern) {
      const re = new RegExp(pattern, 'i')
      logs = logs.filter((l) => re.test(l.text || ''))
    }
    return { logs: logs.slice(-limit), total: logs.length }
  },

  async network_requests({ tabId, urlPattern, limit = 100, clear = false, onlyFailed = false }) {
    const id = await resolveTabId(tabId)
    if (!cdp.isAttached(id)) {
      await cdp.pin(id, 'network')
      return { requests: [], note: '이 탭의 네트워크 수집을 지금 켰습니다. 붙기 전 요청은 잡히지 않으니 페이지를 새로고침한 뒤 다시 부르세요.' }
    }
    let reqs = cdp.drain(id, 'network', { clear })
    if (urlPattern) reqs = reqs.filter((r) => r.url.includes(urlPattern))
    if (onlyFailed) reqs = reqs.filter((r) => r.status === 'failed' || (typeof r.status === 'number' && r.status >= 400))
    return { requests: reqs.slice(-limit), total: reqs.length }
  },

  async upload_file({ tabId, paths, selector, ref }) {
    const id = await resolveTabId(tabId)
    // 파일은 브라우저 프로세스가 직접 읽는다. 확장이 디스크에 접근할 필요가 없다.
    return await cdp.setFileInputFiles(id, paths, { selector, ref })
  },

  async wait_for({ tabId, text, textGone, ms, timeoutMs = 15000 }) {
    const id = await resolveTabId(tabId)
    if (ms) { await new Promise((r) => setTimeout(r, ms)); return { waited: ms } }
    const started = Date.now()
    while (Date.now() - started < timeoutMs) {
      const { text: body } = await page(id, 'text', { maxChars: 200000 })
      if (text && body.includes(text)) return { found: text, elapsed: Date.now() - started }
      if (textGone && !body.includes(textGone)) return { gone: textGone, elapsed: Date.now() - started }
      await new Promise((r) => setTimeout(r, 400))
    }
    throw new Error(`WAIT_TIMEOUT: ${timeoutMs}ms 안에 조건이 만족되지 않았습니다.`)
  },

  async resize_window({ tabId, width, height }) {
    const id = await resolveTabId(tabId)
    const tab = await chrome.tabs.get(id)
    const win = await chrome.windows.update(tab.windowId, { width, height, state: 'normal' })
    return { windowId: win.id, width: win.width, height: win.height }
  },

  async attach_debugger({ tabId }) {
    const id = await resolveTabId(tabId)
    await cdp.attach(id, 'manual')
    return { tabId: id, attached: true, note: '유휴 15초 뒤 자동으로 떨어집니다. 계속 붙여두려면 watch 로 수집을 켜세요.' }
  },

  async detach_debugger({ tabId }) {
    const id = await resolveTabId(tabId)
    return await cdp.detach(id)
  },

  // 언팩 확장은 reload() 로 디스크에서 다시 읽힌다. 코드를 고칠 때마다 사람이 재로드할 필요가 없어진다.
  async dev_reload() {
    setTimeout(() => chrome.runtime.reload(), 300)
    return { reloading: true, note: '확장을 재시작합니다. 1~2초 뒤 브리지에 다시 붙습니다.' }
  },

  async cdp_raw({ tabId, method, params }) {
    const id = await resolveTabId(tabId)
    await cdp.attach(id, 'raw')
    return await cdp.raw(id, method, params || {})
  },
}

export async function dispatch(tool, args, ctx = {}) {
  const fn = handlers[tool]
  if (!fn) throw new Error(`UNKNOWN_TOOL: ${tool}`)
  return await fn(args || {}, ctx)
}

// background 가 상태 배지를 그릴 대상 탭을 고르는 데 쓴다. tools 의 resolveTabId 와 같은 규칙이어야
// 배지가 실제 조작 대상과 어긋나지 않는다.
export async function targetTabOf(args = {}) {
  if (args.tabId) return Number(args.tabId)
  const [active] = await tabsQuery({ active: true, lastFocusedWindow: true })
  return active?.id || null
}
