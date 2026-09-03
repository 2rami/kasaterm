// 툴 핸들러. 기본은 조용한 경로(content script)로 돌고, 그게 no-op 이면 같은 동작을 CDP 로 재시도한다.
// 그래서 평소엔 디버깅 배너가 안 뜨지만 능력치는 CDP 와 동일하다.
import * as cdp from './cdp.js'
import { page, restricted } from './page.js'
import { lookupDevice, suggestDevices, deviceTable, uaOverrideFor } from './devices.js'
import { setTask, forgetTab, identityOf, showCursor, groupOwnTab, ungroupBeforeClose, ownTabCount, refreshGroupTitle, agentWindowOf, agentWindowsByGroups, rememberAgentWindow, forgetAgentWindow, otherOwners, listGroups, hideForShot, showAfterShot } from './sessions.js'

// 워커가 언제 떴는지. 이 값이 방금 태어난 것으로 나오면 직전 명령이 실패한 이유는 대개 워커가
// 도중에 죽은 것이다 — 끊김의 원인을 코드에서 찾기 전에 여기부터 본다.
const WORKER_STARTED = Date.now()
let jobSeq = 0

// 그룹에 속하지 않은 탭의 groupId. chrome.tabGroups.TAB_GROUP_ID_NONE 과 같은 값이지만,
// service worker 가 그 상수를 못 읽는 경우가 있어 직접 둔다.
const NO_GROUP = -1

// 폰뷰가 진짜로 걸렸는지 페이지에 직접 물어보는 값들. 크기만 보면 터치 관련 분기가 빠진 것을 놓친다.
const PHONE_PROBE = `({
  viewport: innerWidth + 'x' + innerHeight,
  touchPoints: navigator.maxTouchPoints,
  hoverNone: matchMedia('(hover: none)').matches,
  pointerCoarse: matchMedia('(pointer: coarse)').matches,
  userAgent: navigator.userAgent,
  chPlatform: navigator.userAgentData ? navigator.userAgentData.platform : null,
  chMobile: navigator.userAgentData ? navigator.userAgentData.mobile : null
})`
const GROUP_COLORS = new Set(['grey', 'blue', 'red', 'yellow', 'green', 'pink', 'purple', 'cyan', 'orange'])

// 기기 정의(이름·크기·dpr·UA)는 devices.js 에 있다. 이름 하나로 폭·높이·dpr·UA 를 한꺼번에 맞춘다 —
// 폭만 옮겨 적고 dpr 을 잊으면 레티나에서만 드러나는 이미지·보더 문제를 통째로 못 본다.

// override 를 걸기 전의 UA. off 로 끌 때 즉시 되돌리는 데 쓴다. 디버거를 떼도 크롬이 알아서
// 복원하지만 그건 유휴 15초 뒤라, 그 사이 그 탭을 쓰는 다른 pane 은 폰 UA 를 계속 보게 된다.
const originalUA = new Map()

// ★탭마다 「무엇을 걸어 뒀는지」. 페이지가 새로 뜨면 일부가 조용히 풀리기 때문이다 —
// 2026-08-26 실측: 크기 override 는 navigate 를 견디는데 **터치 에뮬레이션은 뒤로가기(bfcache
// 복원)에서 maxTouchPoints 5 → 0 으로 풀렸다**. 터치가 빠지면 `(hover: none)`·`(pointer: coarse)`
// 규칙이 안 걸려 실제 폰과 **다른 CSS 가 도는데 크기는 폰 그대로**라, 화면만 봐서는 절대 안 보인다.
// 확장을 재로드하면 디버거 세션이 통째로 끊겨 크기까지 풀리므로 storage.session 에 함께 둔다
// (worker 가 죽어도 남고, 브라우저를 껐다 켜면 저절로 비워진다).
// ⚠️storage.**local** 이다. session 을 쓰면 안 된다 — 2026-08-26 실측: `chrome.runtime.reload()`
// 로 확장을 재시작하면 storage.session 이 통째로 비워져서(기록 1건 → []) 정작 복구가 제일 필요한
// 그 순간에 되돌릴 근거가 사라진다. 확장 재로드는 디버거 세션을 전부 끊으므로 터치·UA 가 풀리는데,
// 그때 기록이 없으면 「크기만 폰인 채로」 남는다. local 은 브라우저를 껐다 켜도 남지만 탭 id 가
// 그때 바뀌므로, 읽을 때 실재하지 않는 탭을 걷어낸다.
const EMULATED_KEY = 'kc_emulated'
let emulated = null
let loadingEmulated = null

async function loadEmulated() {
  if (emulated) return emulated
  // ⚠️in-flight 를 공유한다. 예전처럼 빈 Map 을 먼저 대입하고 await 하면, 그 사이 들어온 호출이
  // **아직 안 채워진 빈 Map** 을 받아 「기록 없음」으로 판단한다(navigate 도구와 onUpdated 리스너가
  // 거의 동시에 부른다).
  if (loadingEmulated) return await loadingEmulated
  loadingEmulated = (async () => {
    const map = new Map()
    try {
      const v = await chrome.storage.local.get(EMULATED_KEY)
      const raw = v?.[EMULATED_KEY] || {}
      const live = new Set((await tabsQuery({})).map((t) => t.id))
      let dropped = false
      for (const [k, cfg] of Object.entries(raw)) {
        if (live.has(Number(k))) map.set(Number(k), cfg)
        else dropped = true
      }
      emulated = map
      if (dropped) void saveEmulated()
    } catch { emulated = map }
    return emulated
  })()
  try { return await loadingEmulated } finally { loadingEmulated = null }
}

// ⚠️저장 직전에 죽은 탭을 걷어낸다. onRemoved 리스너만으로는 안 된다 — 확장이 재시작하는 동안
// 닫힌 탭은 그 리스너가 못 잡고, 한 번 메모리에 올라온 뒤에는 loadEmulated 의 정리도 다시 안 돈다
// (2026-08-26 실측: 이미 사라진 탭의 기록이 계속 남아 있었다). storage.local 은 브라우저를 껐다
// 켜도 남으므로 이 정리가 없으면 유령 항목이 쌓인다.
async function pruneEmulated() {
  let dropped = false
  try {
    const live = new Set((await tabsQuery({})).map((t) => t.id))
    for (const k of [...(emulated || new Map()).keys()]) if (!live.has(k)) { emulated.delete(k); dropped = true }
  } catch { /* 탭 목록을 못 읽으면 정리만 건너뛴다 */ }
  return dropped
}

async function saveEmulated() {
  await pruneEmulated()
  const obj = Object.fromEntries([...(emulated || [])].map(([k, v]) => [String(k), v]))
  chrome.storage.local.set({ [EMULATED_KEY]: obj }).catch(() => {})
}

// 페이지가 새로 뜬 뒤 부른다. 어긋난 것만 다시 건다 — 멀쩡한데 setDeviceMetricsOverride 를 또 걸면
// 페이지가 쓸데없는 resize 이벤트를 받는다.
export async function reapplyEmulation(tabId) {
  const map = await loadEmulated()
  const cfg = map.get(tabId)
  if (!cfg) return null
  // 확장 재로드로 세션이 끊겼으면 붙는 것부터. pin 은 유휴 detach 대상에서 빼는 표시이기도 하다.
  // 세션이 새로 열린 경우엔 어긋난 것만 고치지 않고 전부 다시 건다 — 붙는 순간 크롬이 무엇을
  // 리셋했는지 알 수 없고, 그 상태로 「크기는 맞으니 놔둔다」로 판단하면 반쯤 걸린 채로 남는다.
  const wasAttached = cdp.isAttached(tabId)
  if (!wasAttached) await cdp.pin(tabId, 'emulation').catch(() => {})
  const seen = await cdp.evaluate(tabId, '({ w: innerWidth, t: navigator.maxTouchPoints })').catch(() => null)
  if (!seen) return null
  const fixed = []
  if (!wasAttached || seen.w !== cfg.width) {
    await cdp.raw(tabId, 'Emulation.setDeviceMetricsOverride', {
      width: cfg.width, height: cfg.height, deviceScaleFactor: cfg.dsf, mobile: cfg.mobile,
      ...(cfg.scale && cfg.scale < 1 ? { scale: cfg.scale } : {}),
    }).catch(() => {})
    fixed.push('metrics')
  }
  if (cfg.touch && (!wasAttached || !seen.t)) {
    await cdp.raw(tabId, 'Emulation.setTouchEmulationEnabled', { enabled: true, maxTouchPoints: 5 }).catch(() => {})
    fixed.push('touch')
  }
  // UA 는 페이지에서 읽어 비교하면 override 가 살아 있는지 알 수 있다. 값이 같으면 건드리지 않는다.
  if (cfg.userAgent) {
    const ua = await cdp.evaluate(tabId, 'navigator.userAgent').catch(() => null)
    if (!wasAttached || (ua && ua !== cfg.userAgent)) {
      await cdp.raw(tabId, 'Emulation.setUserAgentOverride', cfg.uaArgs || { userAgent: cfg.userAgent }).catch(() => {})
      fixed.push('ua')
    }
  }
  return fixed.length ? { tabId, fixed } : null
}

export async function forgetEmulation(tabId) {
  const map = await loadEmulated()
  if (map.delete(tabId)) await saveEmulated()
}

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

// 창이 페이지에 실제로 내주는 공간. ⚠️붙자마자 재면 안 된다 — attach 하면 크롬이 디버깅 인포바를
// 띄우고 그것이 슬라이드해 내려오는 동안 높이가 계속 줄어든다(실측: 직후 632 → 71ms 600 →
// 123ms 583 에서 안정). 그 과도기 값을 쓰면 창을 53px 크게 잡아 그만큼 화면 밖으로 밀려난다.
async function measureRoom(tabId) {
  let prev = null
  for (let i = 0; i < 8; i++) {
    const now = await cdp.evaluate(tabId, '({ w: innerWidth, h: innerHeight })').catch(() => null)
    if (!now) return prev
    if (prev && prev.w === now.w && prev.h === now.h) return now
    prev = now
    await new Promise((r) => setTimeout(r, 60))
  }
  return prev
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

async function clickOn(id, { ref, coordinate, button = 'left', clickCount = 1, modifiers, trusted = false, retry = true }, client) {
  const mods = cdp.modifierMask(modifiers)

  if (coordinate) {
    const [x, y] = coordinate
    await showCursor(client, id, x, y, true)
    const r = await cdp.click(id, { x, y, button, clickCount, modifiers: mods })
    return { ...r, via: 'cdp', reason: 'coordinate' }
  }
  if (!ref) throw new Error('MISSING_TARGET: ref 나 coordinate 중 하나가 필요합니다. find 나 read_page 로 ref 를 얻으세요.')

  if (trusted || button !== 'left' || clickCount > 1 || mods) {
    const { box, name } = await page(id, 'box', { ref })
    await showCursor(client, id, box.x, box.y, true)
    const r = await cdp.click(id, { x: box.x, y: box.y, button, clickCount, modifiers: mods })
    return { ...r, target: name, via: 'cdp', reason: trusted ? 'trusted' : 'modifier/button' }
  }

  const { box: aim, name: aimName } = await page(id, 'box', { ref })
  await showCursor(client, id, aim.x, aim.y, true)
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

// ── 레이아웃툴 ────────────────────────────────────────────────────────────────
// 화면을 브라우저에서 그대로 만지는 편집기. 사람이 눌러 옮긴 결과를 여기 붙어 있는
// 에이전트가 그대로 받아 소스를 고친다 — 예전에는 편집기의 「코드에 반영」이 창도 맥락도
// 없는 클로드를 새로 낳았고, 그 애는 무엇을 하는지 안 보이고 멈출 수도 없었다
// (2026-09-02 지시: 「카사크롬으로 보고 레이아웃툴로 수정하는 방식으로 가자,
// 지금 너 세션이면 너가 고치는거지」).
// 편집기 본체는 여기 사본을 두지 않는다. 그 폴더를 쥔 레이아웃툴 서버에서 그때그때
// 받아 오므로, 저쪽에서 편집기를 고치면 다음에 켤 때 바로 그것이 붙는다.
const LT_PORTS = [4300, 4301, 4302, 4303, 4304, 4305]
const ltLoop = (h) => h === 'localhost' || h === '127.0.0.1' || h === '[::1]'

// 서버가 맡은 곳과 이 탭이 같은 자리인가. 개발서버는 같은 자리를 localhost 로도
// 127.0.0.1 로도 부르므로 이름이 아니라 포트로 가른다.
function ltMine(tabUrl, who, port) {
  // 프록시로 서 있으면 목적지가 짝이고, 폴더를 직접 내주고 있으면 자기 주소가 짝이다
  const mine = [who.target, `http://localhost:${port}`].filter(Boolean)
  try {
    const a = new URL(tabUrl)
    return mine.some((u) => {
      const b = new URL(u)
      if (ltLoop(a.hostname) && ltLoop(b.hostname)) return (a.port || '80') === (b.port || '80')
      return a.host === b.host
    })
  } catch { return false }
}

async function ltServer(tabUrl) {
  for (const port of LT_PORTS) {
    try {
      // 안 떠 있는 포트는 곧장 튕기지만, 다른 것이 물고 늘어지면 여기서 멎는다
      const r = await fetch(`http://127.0.0.1:${port}/__layoutool/who`, {
        cache: 'no-store', signal: AbortSignal.timeout(700),
      })
      if (!r.ok) continue
      const who = await r.json()
      if (ltMine(tabUrl, who, port)) return { ...who, api: `http://127.0.0.1:${port}` }
    } catch {}
  }
  return null
}

const ltRun = async (tabId, func, args = []) => {
  const [r] = await chrome.scripting.executeScript({ target: { tabId }, world: 'MAIN', func, args })
  return r?.result
}

// 편집기를 켜 둔 탭. 화면이 갈리면 넣어 둔 것이 통째로 날아가므로 다시 넣어야 하는데,
// 그 판단 근거가 여기다. ⚠️storage.**local** 이다 — session 은 확장을 재시작하면 통째로
// 비워져서(폰뷰 기록에서 실측) 정작 되돌릴 근거가 필요한 순간에 사라진다. local 은 브라우저를
// 껐다 켜도 남고 그때 탭 id 가 바뀌므로, 읽을 때 실재하지 않는 탭을 걷어낸다.
const LT_KEY = 'kc_layout'
const ltBag = async () => {
  try {
    const raw = (await chrome.storage.local.get(LT_KEY))?.[LT_KEY] || {}
    const live = new Set((await tabsQuery({})).map((t) => t.id))
    const kept = Object.fromEntries(Object.entries(raw).filter(([k]) => live.has(Number(k))))
    if (Object.keys(kept).length !== Object.keys(raw).length) {
      chrome.storage.local.set({ [LT_KEY]: kept }).catch(() => {})
    }
    return kept
  } catch { return {} }
}
const ltMark = async (tabId, on, src = null) => {
  const bag = await ltBag()
  if (on) bag[String(tabId)] = { src }
  else delete bag[String(tabId)]
  await chrome.storage.local.set({ [LT_KEY]: bag }).catch(() => {})
}

// 편집기를 이 화면에 넣고 켠다. 이미 들어가 있으면 편집기가 스스로 물러나므로 두 번 넣어도
// 만지던 내역이 흔들리지 않는다 — 그래서 「이미 넣었나」를 따지지 않는다.
async function ltPlant(tabId, tabUrl) {
  const who = await ltServer(tabUrl)
  if (!who) throw new Error(`이 주소를 맡은 레이아웃툴 서버가 없습니다 (${tabUrl}) — 터미널에서 \`layoutool <개발서버 포트> --src <소스 폴더>\` 로 띄운 뒤 다시 부르세요.`)
  if (!who.src) throw new Error('레이아웃툴이 고칠 소스를 모른 채 떠 있습니다 — --src <폴더> 를 주고 다시 띄우세요.')
  const code = await (await fetch(who.api + '/__layoutool/editor.js', { cache: 'no-store' })).text()

  const put = (api, src) => {
    window.__layoutool_api = api
    window.__layoutool_canApply = true
    if (!window.__layoutool) {
      const s = document.createElement('script')
      s.textContent = src
      ;(document.head || document.documentElement).append(s)
      s.remove()
    }
    window.__layoutool_toggle?.(true)
    return !!window.__layoutool
  }
  let ok = await ltRun(tabId, put, [who.api, code])
  if (!ok) {
    // 인라인 스크립트를 막아 둔 페이지에서는 위가 조용히 씹힌다 — CDP 는 그 규칙 밖이다
    await cdp.evaluate(tabId, `window.__layoutool_api=${JSON.stringify(who.api)};window.__layoutool_canApply=true`)
    await cdp.evaluate(tabId, code)
    ok = await cdp.evaluate(tabId, 'window.__layoutool_toggle?.(true), !!window.__layoutool')
    if (!ok) throw new Error('편집기를 못 넣었습니다 — 이 페이지가 스크립트 주입을 막고 있습니다.')
  }
  return who
}

// 화면이 새로 뜬 뒤 부른다. 켜 둔 탭이면 다시 넣는다 — 로그인처럼 화면이 한 번 갈리는
// 자리에서 편집기가 소리 없이 사라지면, 사람 눈에는 「켰다고 했는데 없다」로만 보인다
// (2026-09-03 실측: mission-control 로그인 뒤 바가 통째로 없어졌다).
export async function reapplyLayout(tabId) {
  const bag = await ltBag()
  if (!bag[String(tabId)]) return null
  const tab = await chrome.tabs.get(tabId).catch(() => null)
  if (!tab || !/^https?:/.test(tab.url || '')) return null
  try {
    const who = await ltPlant(tabId, tab.url)
    await ltMark(tabId, true, who.src)
    return who
  } catch { return null }
}

// 아이콘 팝업이 이 탭의 지금 상태를 묻는다. ⚠️여기서 서버를 훑으면 안 된다 — 팝업은 1초마다
// 다시 묻는데 포트 훑기는 안 뜬 자리마다 기다리느라 그 사이에 안 끝난다. 켜져 있나만 창고에서
// 읽고, 서버가 있나 없나는 사람이 눌렀을 때 그 자리에서 알려 준다.
export async function layoutState(tabId) {
  const tab = await chrome.tabs.get(tabId).catch(() => null)
  if (!tab || !/^https?:/.test(tab.url || '')) return { ok: false }
  const at = (await ltBag())[String(tabId)]
  return { ok: true, tabId, on: !!at, src: at?.src || null }
}

export async function layoutToggle(tabId) {
  const tab = await chrome.tabs.get(tabId)
  if ((await ltBag())[String(tabId)]) {
    await ltMark(tabId, false)
    await ltRun(tabId, () => window.__layoutool_toggle?.(false)).catch(() => {})
    return { ok: true, on: false }
  }
  const who = await ltPlant(tabId, tab.url)
  await ltMark(tabId, true, who.src)
  return { ok: true, on: true, src: who.src }
}

export async function forgetLayout(tabId) {
  await ltMark(tabId, false)
}

const handlers = {
  async status(_args, ctx = {}) {
    const tabs = await tabsQuery({})
    // 죽은 탭 기록은 여기서 걷는다. onRemoved 를 놓친 항목은 이것 말고 걷힐 자리가 없고(저장은
    // 새로 걸 때만 돈다), 이미 닫힌 탭을 보여주는 진단은 사람을 헷갈리게만 한다.
    const emul = await loadEmulated()
    if (await pruneEmulated()) void saveEmulated()
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
      // 폰뷰를 걸어 둔 탭. 「걸었는데 데스크톱으로 보인다」를 진단할 때 여기부터 본다 — 목록에
      // 있는데 페이지가 그 폭이 아니면 재적용이 못 걸린 것이고, 아예 없으면 기록이 날아간 것이다.
      emulatedTabs: [...emul].map(([tabId, c]) => ({ tabId, size: `${c.width}x${c.height}`, touch: c.touch, ua: !!c.userAgent })),
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
        // 그룹에 속하지 않은 탭은 -1 이라 그대로 실으면 매 줄에 의미 없는 값이 붙는다.
        ...(t.groupId !== undefined && t.groupId !== NO_GROUP ? { groupId: t.groupId } : {}),
      })),
    }
  },

  // 기본은 백그라운드다. 사람이 보던 화면을 에이전트가 뺏으면 안 된다.
  // 애니메이션·미디어처럼 보이는 탭이어야 도는 것을 확인할 때만 active:true 나 activate_tab 을 쓴다.
  async new_tab({ url, active = false, windowId } = {}, ctx = {}) {
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
    // 내가 만든 탭이니 내 그룹에 넣는다 — 사람이 열어둔 탭 사이에 섞이지 않게.
    const groupId = await groupOwnTab(ctx.client, fresh.id)
    // active 를 돌려주는 이유: 백그라운드로 열었는데 앞으로 튀어나오면 여기서 바로 드러난다
    return { tabId: fresh.id, url: fresh.url, title: fresh.title, active: fresh.active, ...(groupId ? { groupId } : {}) }
  },

  // ⚠️창은 마지막 수단이다 — 사람 화면을 통째로 가리는 자원이라, 확인은 대부분 new_tab 의 백그라운드
  // 탭으로 끝난다. 여기까지 오는 건 특정 폭·높이가 필요할 때나 사람이 창을 달라고 했을 때뿐이다.
  // tabId 를 주면 이미 있는 탭을 그 창으로 떼어낸다.
  // ⚠️focused 기본값은 false 다. 새 창이 앞으로 튀어나오면 선생님이 보던 앱을 가린다 — 탭을
  // 백그라운드로 여는 것과 같은 이유이고, 여기서는 되돌릴 방법이 없으니 기본값이 더 중요하다.
  async new_window({ url, tabId, focused = false, width, height, incognito, reuse = true } = {}, ctx = {}) {
    // ★에이전트 창이 이미 있으면 — 그게 누가 연 것이든 — 거기에 연다. 세션마다 자기 창을 찾던 예전
    // 방식은 pane 이 넷이면 창을 넷 만들었다(실측: 사람 창 1 + 에이전트 창 3). 창은 나눠 쓰고 탭은
    // 세션별 그룹으로 갈리니 누구 것인지는 그대로 보인다.
    // 탭을 떼어내는 호출과 시크릿 창은 재사용할 수 없다 — 목적 자체가 별도 창이다.
    if (reuse && !tabId && !incognito) {
      const windowId = await agentWindowOf()
      if (windowId != null) {
        if (width && height) await chrome.windows.update(windowId, { width: Number(width), height: Number(height) }).catch(() => {})
        // ⚠️active:false 다. 창을 나눠 쓰므로 여기서 활성화하면 같은 창을 보던 다른 학생의 탭이 밀린다.
        const made = await handlers.new_tab({ url, active: false, windowId }, ctx)
        const win = await chrome.windows.get(windowId).catch(() => null)
        return {
          ...made, windowId, reused: true,
          width: win?.width ?? null, height: win?.height ?? null, focused: !!win?.focused,
        }
      }
    }

    const opts = { focused, ...(incognito ? { incognito: true } : {}) }
    if (width && height) Object.assign(opts, { width: Number(width), height: Number(height), left: 0, top: 0 })
    if (tabId) opts.tabId = await resolveTabId(tabId)
    else opts.url = normalizeUrl(url) || 'about:blank'

    const win = await chrome.windows.create(opts)
    const tab = win.tabs?.[0]
    if (!tab) throw new Error('WINDOW_HAS_NO_TAB: 창은 열렸지만 탭을 찾지 못했습니다.')
    // 다음 호출이 이 창을 찾아 쓰도록 기록한다. tabId 로 떼어낸 창은 빼는데, 그건 사람이 따로 보려고
    // 뜯어낸 탭일 수 있어서다 — 거기에 나중 확인용 탭이 쌓이면 사람이 뗀 이유가 무색해진다.
    if (!tabId) await rememberAgentWindow(win.id)
    if (url && !tabId) await waitForLoad(tab.id)
    const fresh = await chrome.tabs.get(tab.id)
    // ⚠️크롬은 창 폭에 하한이 있어 그보다 좁게 달라고 하면 조용히 넓혀 준다(390 을 요청하면 500 이
    // 온다 — 실측). 조용히 지나가면 좁은 폭에서 확인한 줄 알고 오판하므로 다를 때 반드시 알린다.
    // 하한보다 좁은 폭은 창이 아니라 Emulation.setDeviceMetricsOverride 로 봐야 한다.
    const forced = width && win.width !== Number(width)
    // url 로 새로 만든 탭만 내 그룹에 넣는다. tabId 로 떼어낸 것은 원래 사람 탭일 수 있다.
    const groupId = tabId ? null : await groupOwnTab(ctx.client, fresh.id)
    return {
      windowId: win.id, tabId: fresh.id, url: fresh.url, title: fresh.title,
      width: win.width, height: win.height, focused: win.focused,
      ...(groupId ? { groupId } : {}),
      ...(forced ? { note: `요청한 폭 ${width}px 이 아니라 ${win.width}px 로 열렸습니다(크롬 창 폭 하한). 더 좁은 폭은 cdp_raw 의 Emulation.setDeviceMetricsOverride 로 확인하세요.` } : {}),
    }
  },

  async close_window({ windowId, force = false } = {}, ctx = {}) {
    const tabs = await tabsQuery({ windowId: Number(windowId) })
    // ⚠️창은 이제 학생들이 나눠 쓴다. 내 확인이 끝났다고 창째 닫으면 같은 창에서 일하던 사람의 탭까지
    // 함께 사라지므로, 남의 탭이 보이면 멈추고 누구 것인지 알린다.
    const others = otherOwners(ctx.client, tabs.map((t) => t.id))
    if (others.length && !force) {
      throw new Error(`WINDOW_SHARED: 이 창에는 ${others.join(', ')} 의 탭도 있습니다. 내 탭만 close_tab 으로 닫거나, 정말 창째 닫아야 하면 force:true 로 부르세요.`)
    }
    await ungroupBeforeClose(tabs.map((t) => t.id))
    for (const t of tabs) { await cdp.detach(t.id).catch(() => {}); forgetTab(t.id) }
    await chrome.windows.remove(Number(windowId))
    await forgetAgentWindow(windowId)
    return { closed: Number(windowId), tabs: tabs.length, ...(others.length ? { alsoClosed: others } : {}) }
  },

  // ★흩어진 에이전트 창을 하나로 모은다. 탭을 **옮기는** 것이지 닫는 게 아니다 — 폰뷰 override·
  // 스크롤 위치·입력하던 값은 전부 탭에 붙어 있어 그대로 살아 따라온다. 그래서 다른 학생이 일하는
  // 중에 돌려도 그 사람 작업이 깨지지 않는다(닫았다 다시 여는 것과 다른 점이 정확히 이것이다).
  async tidy_windows({ dryRun = false } = {}) {
    const wins = await agentWindowsByGroups()
    if (wins.length < 2) return { windows: wins.length, movedGroups: 0, note: '합칠 에이전트 창이 없습니다.' }
    // 탭이 가장 많은 창으로 모은다 — 옮기는 탭이 가장 적으니 어긋날 여지도 가장 작다.
    const target = wins.reduce((a, b) => (b.tabs > a.tabs ? b : a))
    const sources = wins.filter((w) => w.windowId !== target.windowId)
    if (dryRun) {
      return { into: target.windowId, from: sources.map((w) => w.windowId), tabs: sources.reduce((n, w) => n + w.tabs, 0) }
    }

    let movedGroups = 0
    const failed = []
    for (const w of sources) {
      for (const g of await chrome.tabGroups.query({ windowId: w.windowId }).catch(() => [])) {
        // ⚠️그룹째 옮긴다. 탭만 옮기면 원래 그룹이 빈 껍데기로 탭바에 눌러앉는다 — 크롬이 그룹을
        // 자동 저장하는데 삭제 API 가 없어서 사람이 우클릭으로 지우는 수밖에 없다.
        await chrome.tabGroups.move(g.id, { windowId: target.windowId, index: -1 })
          .then(() => { movedGroups++ })
          .catch((e) => failed.push(`${g.title || g.id}: ${e.message}`))
      }
    }

    // 그룹은 창마다 따로 만들어지므로 모으고 나면 같은 이름이 여럿 나란히 선다. 제목이 같은 것끼리
    // 합쳐야 탭바가 실제로 정리된다 — 안 그러면 창만 줄고 눈에 보이는 어지러움은 그대로다.
    const after = await chrome.tabGroups.query({ windowId: target.windowId }).catch(() => [])
    const keepByTitle = new Map()
    for (const g of after) {
      const keep = keepByTitle.get(g.title)
      if (keep == null) { keepByTitle.set(g.title, g.id); continue }
      const tabs = await tabsQuery({ groupId: g.id })
      if (tabs.length) await chrome.tabs.group({ tabIds: tabs.map((t) => t.id), groupId: keep }).catch(() => {})
    }

    // 다음 new_window 가 이 창을 찾아 쓰도록 기록해 둔다. 안 그러면 정리하자마자 또 갈라진다.
    await rememberAgentWindow(target.windowId)
    return {
      into: target.windowId, movedGroups,
      windowsBefore: wins.length, windowsAfter: (await agentWindowsByGroups()).length,
      ...(failed.length ? { failed } : {}),
    }
  },

  // ★남은 내 탭 수를 함께 준다. 「다 쓴 탭은 즉시 닫는다」와 「끝나도 결과 페이지 하나는 남긴다」는
  // 둘 다 지키려면 지금 몇 개 남았는지를 알아야 하는데, 그걸 모르면 마지막 하나까지 닫아 놓고
  // 선생님께 「확인했습니다」만 남긴다. 막지는 않는다 — 0 이 되면 말로 알려줄 뿐이다.
  async close_tab({ tabId }, ctx = {}) {
    const id = await resolveTabId(tabId)
    // 그룹은 닫기 전에 알아 둔다 — ungroupBeforeClose 가 먼저 빼내므로 그 뒤엔 어느 그룹이었는지 모른다.
    const gid = (await chrome.tabs.get(id).catch(() => null))?.groupId ?? NO_GROUP
    await ungroupBeforeClose([id])
    await cdp.detach(id).catch(() => {})
    forgetTab(id)
    await chrome.tabs.remove(id)
    // 방 그룹 제목은 지금 탭을 갖고 있는 학생 이름으로 만들어진다. 내가 빠졌으면 이름도 빠져야 한다.
    if (gid !== NO_GROUP) await refreshGroupTitle(gid)
    const remaining = await ownTabCount(ctx.client)
    return {
      closed: id, remaining,
      ...(remaining === 0
        ? { note: '내가 연 탭이 하나도 안 남았습니다. 작업 결과로 보여줄 페이지가 있었다면 하나는 다시 열어 두세요.' }
        : {}),
    }
  },

  async set_task({ task }, ctx = {}) {
    return setTask(ctx.client, task)
  },

  async list_groups() {
    return await listGroups()
  },

  // 탭을 그룹에서 빼낸다. 마지막 탭이 빠지면 그룹은 크롬이 알아서 없앤다(그룹 삭제 API 는 없다).
  // ungroup 은 tabs 권한이면 되고 tabGroups 권한은 필요 없다.
  // ⚠️**명시적으로 부를 때만** 묶는다. 새 탭을 열 때 자동 편입하거나, 지정하지 않은 탭을 끌어오는
  // 동작을 절대 넣지 마라 — claude-in-chrome 이 금지된 이유가 정확히 그 자동 묶기였다(열어둔 탭을
  // 제멋대로 자기 그룹으로 묶고, 확장을 꺼도 12초마다 스스로 되살렸다). ungroup_tabs 와 짝이다.
  async group_tabs({ tabIds, title, color, groupId, collapsed } = {}) {
    const ids = (Array.isArray(tabIds) ? tabIds : [tabIds]).filter((v) => v != null).map(Number)
    if (!ids.length) throw new Error('NO_TAB_IDS: 묶을 탭을 tabIds 로 지정하세요 — 자동으로 고르지 않습니다.')
    if (color && !GROUP_COLORS.has(color)) {
      throw new Error(`BAD_COLOR: "${color}" 는 쓸 수 없습니다. ${[...GROUP_COLORS].join('|')} 중 하나여야 합니다.`)
    }
    // 없는 탭이 하나라도 섞이면 group 이 통째로 실패한다. 어느 것이 문제인지 집어서 알려준다.
    const live = await tabsQuery({})
    const known = new Map(live.map((t) => [t.id, t]))
    const missing = ids.filter((id) => !known.has(id))
    if (missing.length) throw new Error(`TAB_NOT_FOUND: ${missing.join(', ')} — list_tabs 로 확인하세요.`)

    // ⚠️크롬은 다른 창의 탭을 그룹에 넣을 때 그 탭을 그룹이 있는 창으로 옮긴다. 조용히 지나가면
    // 선생님 탭이 왜 사라졌는지 알 수 없으므로 미리 알린다.
    const wins = new Set(ids.map((id) => known.get(id).windowId))

    const gid = await chrome.tabs.group(
      groupId != null ? { tabIds: ids, groupId: Number(groupId) } : { tabIds: ids },
    )
    const patch = {}
    if (title != null) patch.title = String(title)
    if (color) patch.color = color
    if (collapsed != null) patch.collapsed = !!collapsed
    let group = null
    if (Object.keys(patch).length) group = await chrome.tabGroups.update(gid, patch).catch(() => null)
    if (!group) group = await chrome.tabGroups.get(gid).catch(() => null)

    return {
      groupId: gid,
      grouped: ids.length,
      title: group?.title ?? null,
      color: group?.color ?? null,
      collapsed: group?.collapsed ?? null,
      windowId: group?.windowId ?? null,
      ...(wins.size > 1 ? { note: `탭이 창 ${wins.size}개에 걸쳐 있어 크롬이 한 창으로 모았습니다.` } : {}),
    }
  },

  async ungroup_tabs({ tabIds } = {}) {
    const NONE = NO_GROUP
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
  // 사람이 직접 쳐야 하는 자리라 화면을 정말 넘겨야 한다면 ask_human 을 쓴다.
  async activate_tab({ tabId }) {
    const id = await resolveTabId(tabId)
    const tab = await chrome.tabs.update(id, { active: true })
    return { tabId: id, url: tab.url, windowFocused: false }
  },

  // ★화면을 뺏는 유일한 툴이고, 그래서 조건이 하나다 — **사람 손이 있어야 다음 줄로 못 가는 자리**.
  // 로그인·2단계 인증·캡차·결제 확인처럼 에이전트가 대신 칠 수 없는(그리고 쳐서도 안 되는) 것들이다.
  // 확인·검증·스크린샷은 전부 뒤에서 조용히 돈다: 잘 됐는지 보겠다고 화면을 가져오면 사람은 자기
  // 일을 못 한다. 그 경계를 파라미터가 아니라 툴 이름으로 나눈 이유가 이것이다 — focus:true 같은
  // 플래그는 아무 데나 붙지만, 이름이 「사람에게 묻는다」면 아무 데나 부르지 않는다.
  async ask_human({ tabId, reason } = {}, ctx = {}) {
    const id = await resolveTabId(tabId)
    const tab = await chrome.tabs.update(id, { active: true })
    // drawAttention 은 창이 이미 앞이면 무시되고, focused 는 뒤에 있을 때만 의미가 있다. 어느
    // 쪽이든 눈에 띄도록 둘 다 준다.
    await chrome.windows.update(tab.windowId, { focused: true, drawAttention: true }).catch(() => {})
    // 왜 불렀는지를 칩에 남긴다. 창만 튀어나오면 사람은 화면을 되찾고도 무슨 일인지 모른다.
    if (reason) setTask(ctx.client, `입력 필요 — ${reason}`)
    return {
      tabId: id, windowId: tab.windowId, url: tab.url, focused: true, reason: reason || null,
      note: '창을 앞으로 올렸습니다. 사람이 처리할 때까지 기다렸다가 이어서 진행하세요.',
    }
  },

  async navigate({ tabId, url }) {
    const id = await resolveTabId(tabId)
    if (url === 'back' || url === 'forward') {
      await (url === 'back' ? chrome.tabs.goBack(id) : chrome.tabs.goForward(id))
    } else {
      await chrome.tabs.update(id, { url: normalizeUrl(url) })
    }
    const status = await waitForLoad(id)
    // ★페이지가 새로 뜨면 폰뷰의 일부가 풀린다(터치는 뒤로가기에서, 크기는 확장 재로드에서).
    // 부르는 쪽은 알 방법이 없으므로 — 도구는 성공을 돌려주고 페이지만 데스크톱이 된다 — 여기서
    // 되돌린다. onUpdated 리스너와 겹쳐도 어긋난 것만 고치므로 두 번 걸리지 않는다.
    const restored = await reapplyEmulation(id).catch(() => null)
    const tab = await chrome.tabs.get(id)
    return {
      tabId: id, url: tab.url, title: tab.title, load: status,
      ...(restored ? { emulationRestored: restored.fixed } : {}),
    }
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

  async layout({ tabId, on = true }) {
    const id = await resolveTabId(tabId)
    if (!on) {
      await ltMark(id, false)
      await ltRun(id, () => window.__layoutool_toggle?.(false)).catch(() => {})
      return { tabId: id, on: false }
    }
    const tab = await chrome.tabs.get(id)
    const who = await ltPlant(id, tab.url)
    await ltMark(id, true, who.src)
    return { tabId: id, on: true, server: who.api, src: who.src, note: '사람이 화면을 만진 뒤 browser_layout_edits 로 가져오세요. 화면이 갈려도 저절로 다시 붙습니다.' }
  },

  // 만진 결과를 가져온다. 지시문은 레이아웃툴이 만들어 준다 — 여기서 따로 지어내면
  // 「좌표를 그대로 박지 마라」 같은 규칙이 이 길에서만 빠진다.
  async layout_edits({ tabId, raw = false }) {
    const id = await resolveTabId(tabId)
    const tab = await chrome.tabs.get(id)
    const data = await ltRun(id, () => window.__layoutool_edits?.() || null)
    if (!data) throw new Error('이 탭에 편집기가 없습니다 — browser_layout 으로 먼저 켜세요.')
    if (!data.edits?.length) return { tabId: id, count: 0, note: '아직 만진 것이 없습니다.' }
    const who = raw ? null : await ltServer(tab.url)
    if (!who) return { tabId: id, count: data.edits.length, viewport: data.viewport, edits: data.edits }
    const r = await fetch(who.api + '/__layoutool/brief', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ ...data, page: data.url }),
    })
    if (!r.ok) throw new Error('지시문을 못 받았습니다: ' + (await r.text()))
    const b = await r.json()
    return { tabId: id, root: b.root, page: b.page, count: b.count, brief: b.text }
  },

  // ★찍는 동안 우리 표시(칩·테두리·커서)를 걷는다. 그림은 대개 사람에게 보여주거나 문서에 넣으려고
  // 찍는 것이라, 에이전트가 페이지 위에 얹은 것이 함께 박히면 그 그림은 쓸 수 없다 — 촬영을
  // 에이전트가 하는 이상 치우는 것도 에이전트 몫이고, 사람이 미리 껐다 켤 일이 아니다.
  // overlay:true 로 남길 수 있다. 오버레이 자체가 잘 그려지는지 확인할 때가 그 경우다.
  async screenshot({ tabId, ref, padding = 0, fullPage = false, format = 'png', quality, overlay = false }) {
    const id = await resolveTabId(tabId)
    // 둘 다 주면 무엇을 원한 건지 알 수 없다. 조용히 하나를 고르면 엉뚱한 그림이 나간다.
    if (ref && fullPage) throw new Error('ref 와 fullPage 는 함께 쓸 수 없습니다 — 요소 하나를 찍을지 문서 전체를 찍을지 하나만 고르세요.')

    // 요소 스샷은 찍을 영역을 먼저 재야 한다. clip 은 CDP 만 받으므로 아래에서 경로가 갈린다.
    let clip = null
    let target = null
    if (ref) {
      const m = await page(id, 'rect', { ref })
      const pad = Math.max(0, padding)
      const x = Math.max(0, m.rect.x - pad)
      const y = Math.max(0, m.rect.y - pad)
      clip = {
        x, y,
        // 문서 밖까지 요청하면 그만큼 검게 찍히므로 문서 크기로 자른다
        width: Math.min(m.rect.width + pad * 2, Math.max(1, m.docSize.width - x)),
        height: Math.min(m.rect.height + pad * 2, Math.max(1, m.docSize.height - y)),
      }
      // ★무엇을 찍었는지 되돌려준다. ref 가 엉뚱한 요소를 가리켰을 때 부르는 쪽이
      // 알아챌 수 있는 유일한 단서다(ref 를 받아놓고 다른 걸 집던 upload_file 사고를 되풀이하지 않는다).
      target = { ref, name: m.name, role: m.role, clip: { ...clip } }
    }

    const tab = await chrome.tabs.get(id)
    const hidden = overlay ? false : await hideForShot(id)
    try {
      // captureVisibleTab 은 배너가 안 뜨지만 활성 탭의 보이는 영역만 찍는다. 나머지는 CDP 가 필요하다.
      // 영역을 지정할 방법이 없으므로 ref 가 있으면 이 길로 가지 않는다.
      // ⚠️여기서 「보이는 영역」은 창이 아니라 **렌더 표면**이다. 기기 에뮬로 화면이 축소돼 있으면 표면이 그
      // 축소된 크기로 줄어들어, 창에 남는 여백은 그림에 담기지 않는다(2026-08-26 실측: 창 1512x772 에 iPad Air 를
      // 걸었더니 캡처가 3024x1544 에서 1072x1544 로 줄었다 — 폭만 축소된 기기 폭이 되고 높이는 창 그대로였다).
      // 그래서 화면이 창 안 어디에 놓였는지·여백이 얼마나 남았는지는 이 그림으로 판정할 수 없다. 「스샷으로
      // 확인하라」는 지시가 이 도구에서는 성립하지 않는 경우가 여기다 — 그건 데스크톱 캡처로 봐야 한다.
      if (!fullPage && !clip && tab.active) {
        try {
          const dataUrl = await chrome.tabs.captureVisibleTab(tab.windowId, { format, ...(quality ? { quality } : {}) })
          return { data: dataUrl.split(',')[1], format, via: 'captureVisibleTab', overlayHidden: hidden }
        } catch { /* 권한·타이밍 문제면 CDP 로 내려간다 */ }
      }
      const data = await cdp.screenshot(id, { fullPage, format, quality, clip })
      return { data, format, via: 'cdp', overlayHidden: hidden, ...(target ? { target } : {}) }
    } finally {
      // ⚠️반드시 되돌린다. 던지고 나가는 길에 그냥 두면 그 탭은 담당 표시가 없는 채로 남는다.
      // content 쪽에도 자동 복구 타이머가 있지만 그건 이 줄이 못 돌았을 때의 안전망이다.
      if (hidden) await showAfterShot(id)
    }
  },

  async click({ tabId, ...rest }, ctx = {}) {
    const id = await resolveTabId(tabId)
    const out = await clickOn(id, rest, ctx.client)
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

  // 폰 제스처는 마우스 드래그로 재현되지 않는다 — 페이지가 보는 것은 touch 이벤트이고, 방향 락과
  // 임계 거리 판정이 전부 거기 달려 있다. drag 는 mousePressed 를 보내므로 그 코드에 아예 닿지 않는다.
  async swipe({ tabId, direction = 'left', distance, ref, coordinate, from, to, steps = 12 }) {
    const id = await resolveTabId(tabId)

    // ⚠️거절 사유를 **앞으로 끌어내기 전에** 본다. 순서가 반대면 어차피 못 할 일에 화면부터 바꾸고
    // 나서 거절해, 사람이 보던 탭만 빼앗기고 얻는 것이 없다. evaluate 는 숨은 탭에서도 잘 돈다.
    const env = await cdp.evaluate(id, '({ w: innerWidth, h: innerHeight, touch: navigator.maxTouchPoints })')
    // 터치를 안 받는 탭에 터치를 쏘면 이벤트가 조용히 버려져 「밀었는데 아무 일도 없다」가 된다.
    // 게다가 크기만 바꾼 화면은 `(pointer: coarse)` 가 안 걸려 실제 폰과 **다른 코드**가 돈다.
    if (!env?.touch) {
      throw new Error('NO_TOUCH_EMULATION: 이 탭은 터치를 받지 않습니다(maxTouchPoints 0). browser_emulate_device 로 폰뷰를 먼저 켜세요.')
    }

    // ★보이지 않는 탭에서는 터치가 통째로 멈춘다(cdp.swipe 주석의 실측). 창은 그대로 두고 탭만
    // 앞으로 보낸다 — 사람이 다른 앱을 보고 있으면 화면에서 달라지는 것이 없다.
    const tab = await chrome.tabs.get(id)
    const activated = !tab.active
    if (activated) await chrome.tabs.update(id, { active: true })

    let a, b
    if (from && to) {
      a = { x: from[0], y: from[1] }
      b = { x: to[0], y: to[1] }
    } else {
      a = coordinate ? { x: coordinate[0], y: coordinate[1] }
        : ref ? (await page(id, 'box', { ref })).box
          : { x: Math.round(env.w / 2), y: Math.round(env.h / 2) }
      // 화면 밖으로 나가는 손가락은 브라우저가 도중에 놓아 버린다 — 남은 자리에 맞춰 줄인다.
      const room = { left: a.x - 8, right: env.w - 8 - a.x, up: a.y - 8, down: env.h - 8 - a.y }
      const d = Math.max(0, Math.min(Number(distance) || 160, room[direction] ?? 0))
      if (d < 24) {
        throw new Error(`NO_ROOM: 시작점 (${a.x},${a.y}) 에서 ${direction} 으로 ${d}px 밖에 못 갑니다(뷰포트 ${env.w}x${env.h}). 시작점을 옮기거나 방향을 바꾸세요.`)
      }
      const axis = direction === 'left' || direction === 'right' ? 'x' : 'y'
      b = { ...a, [axis]: a[axis] + (direction === 'left' || direction === 'up' ? -d : d) }
    }

    const r = await cdp.swipe(id, { from: a, to: b, steps })
    return {
      ...r,
      ...(from && to ? {} : { direction }),
      // 앞으로 보냈다는 사실을 밝힌다 — 탭이 바뀐 것을 모르면 다음 조작이 엉뚱한 탭에 간 줄 안다.
      ...(activated ? { activated: true, note: '터치는 보이는 탭에서만 처리되므로 이 탭을 앞으로 보냈습니다(창은 그대로).' } : {}),
      page: await settle(id),
    }
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
    const failed = []
    if (wantConsole === true) { failed.push(...(await cdp.pin(id, 'console')).failed); on.push('console') }
    if (wantConsole === false) { await cdp.unpin(id, 'console'); off.push('console') }
    if (wantNetwork === true) { failed.push(...(await cdp.pin(id, 'network')).failed); on.push('network') }
    if (wantNetwork === false) { await cdp.unpin(id, 'network'); off.push('network') }
    // 켜지지 않은 도메인을 감추면 「로그가 왜 비어 있지」를 되짚을 수 없다. 핀 자체는 걸려 있다.
    const note = failed.length
      ? `${failed.join(', ')} 활성화가 실패했습니다 — 그 종류의 기록은 안 쌓입니다. 같은 탭에 다시 호출해 보세요.`
      : on.length ? '수집이 켜진 동안 그 탭에 디버깅 배너가 유지됩니다.' : undefined
    return { tabId: id, enabled: on, disabled: off, ...(failed.length ? { failedDomains: failed } : {}), note }
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

  // 폰뷰는 창이 아니라 탭에 건다. 창은 pane 여럿이 함께 쓰는 자원이라 누가 리사이즈하면 남의
  // 설정이 조용히 사라지고, 크롬 최소 창 폭이 500px 이어서 390 을 만들 수도 없다.
  // ★override 는 디버거 세션에 매여 있어서 세션을 놓으면 통째로 풀린다 — 유휴 15초에 detach 하는
  // 구조에서 「걸어두면 몇 분 뒤 데스크톱으로 돌아가 있다」의 원인이 이것이다(2026-08-05 아로나 실측).
  // 그래서 핀을 걸어 유휴 detach 대상에서 뺀다. 디버깅 배너가 남지만 폰뷰가 유지되는 쪽이 중요하고,
  // off:true 로 끄면 배너도 함께 걷힌다.
  async emulate_device({ tabId, device, width, height, deviceScaleFactor, mobile, touch, landscape, ua, fit = true, off, list } = {}) {
    // 목록은 탭이 없어도 답할 수 있어야 한다. 없는 이름을 넣어 오류 메시지로 목록을 캐내는 것은
    // 도구가 아니라 우회로다.
    if (list) {
      return {
        devices: deviceTable(),
        note: '가로는 이름 뒤에 -landscape 를 붙입니다(예: ipad-pro-11-landscape). landscape:true 로 줘도 같습니다. 크기를 직접 주면 device 의 UA 는 그대로 두고 뷰포트만 바뀝니다.',
      }
    }

    const id = await resolveTabId(tabId)

    if (off) {
      // ⚠️디버거가 떨어져 있어도 override 는 페이지에 남아 있을 수 있다(확장 재로드 실측: 세션이
      // 전부 끊겼는데 크기 override 는 그대로였다). 그때 「안 붙어 있으니 할 일 없음」으로 지나가면
      // off 가 성공을 돌려주고도 화면은 폰뷰 그대로다 — 기록이 있으면 다시 붙여서 확실히 푼다.
      if (!cdp.isAttached(id) && (await loadEmulated()).has(id)) await cdp.attach(id, 'emulation').catch(() => {})
      if (cdp.isAttached(id)) {
        // ⚠️clear 한 번으로는 안 풀리는 경우가 있다. 확장을 재로드하면 **앞 세션이 건 override 가
        // 페이지에 남는데**, 새 세션의 clear 는 「내가 건 것이 없다」며 조용히 no-op 이 된다
        // (2026-08-26 실측: clear 를 두 번 불러도 412x915 그대로였고, 0x0 으로 한 번 걸어 이 세션이
        // 소유자가 된 뒤 clear 하니 1512x772 로 돌아왔다). off 는 화면을 되돌리는 명령이니 리사이즈가
        // 한 번 더 가도 무방하다 — 안 풀리는 것보다 낫다.
        await cdp.raw(id, 'Emulation.setDeviceMetricsOverride', { width: 0, height: 0, deviceScaleFactor: 0, mobile: false }).catch(() => {})
        await cdp.raw(id, 'Emulation.clearDeviceMetricsOverride').catch(() => {})
        await cdp.raw(id, 'Emulation.setTouchEmulationEnabled', { enabled: false }).catch(() => {})
        // UA 는 clear 명령이 없다. 걸기 전에 재둔 값을 다시 걸어 되돌린다(디버거를 떼도 크롬이
        // 복원하지만 그건 유휴 15초 뒤라, 그 사이 이 탭을 쓰는 다른 pane 은 폰 UA 를 계속 본다).
        const back = originalUA.get(id) || (await loadEmulated()).get(id)?.restoreUA
        if (back) {
          await cdp.raw(id, 'Emulation.setUserAgentOverride', { userAgent: back }).catch(() => {})
          originalUA.delete(id)
        }
      }
      await cdp.unpin(id, 'emulation')
      await forgetEmulation(id)
      const seen = await cdp.evaluate(id, PHONE_PROBE).catch(() => null)
      return {
        tabId: id, emulating: false, viewport: seen?.viewport ?? null, userAgent: seen?.userAgent ?? null,
        note: '핀을 풀었으니 유휴 15초 뒤 디버깅 배너도 걷힙니다.',
      }
    }

    const asked = device || 'phone'
    const preset = lookupDevice(asked)
    if (!preset) {
      const near = suggestDevices(asked)
      throw new Error(`UNKNOWN_DEVICE: ${device}. ${near.length ? `가까운 이름 — ${near.join(', ')}. ` : ''}전체 목록은 list:true 로 보세요.`)
    }
    // landscape:true 는 이름 접미사와 같은 뜻이다. 둘 다 주면 두 번 뒤집지 않는다.
    const rotate = landscape === true && !preset.landscape
    const pw = rotate ? preset.height : preset.width
    const ph = rotate ? preset.width : preset.height
    const w = Math.round(Number(width) || pw)
    const h = Math.round(Number(height) || ph)
    const dsf = Number(deviceScaleFactor) || preset.dsf
    // mobile 과 touch 는 다른 것이다 — Surface Pro·Nest Hub 는 데스크톱 렌더인데 손가락이 닿는다.
    // 명시값이 가장 세고, mobile 만 명시했으면 그 뜻을 따라가고, 아무것도 없으면 기기 정의를 쓴다.
    const sizedByHand = width !== undefined || height !== undefined
    const isMobile = mobile === undefined ? !!preset.mobile : !!mobile
    const wantTouch = touch === undefined ? (mobile === undefined ? !!preset.touch : !!mobile) : !!touch
    if (!(w >= 100 && w <= 4000 && h >= 100 && h <= 4000)) {
      throw new Error(`BAD_SIZE: ${w}x${h} 는 화면 크기가 아닙니다. 100~4000 사이로 주거나 device 이름을 쓰세요.`)
    }

    // ⚠️핀을 먼저 건다. override 를 걸고 나서 붙잡으면 그 사이에 타이머가 세션을 놓을 수 있다.
    await cdp.pin(id, 'emulation')

    // ★창이 페이지에 실제로 내주는 공간을 먼저 잰다. override 가 이미 걸려 있으면 innerHeight 도
    // outerHeight 도 그 값으로 덮여서, 창이 그보다 작아도 페이지는 알 방법이 없다 — 이것이
    // 「폰뷰인데 하단 네비바가 없다」의 정체다(2026-08-05 실측: 창 772 에 844 를 걸어 아래 72px 이
    // 창 밖으로 나갔고 bottom:0 인 탭바 783~844 가 통째로 잘렸다. 스크린샷은 CDP 라 844 전부를
    // 찍으니 이미지로는 멀쩡해 보여서 더 헷갈린다).
    // ⚠️0x0 을 한 번 거쳐야 clear 가 먹는다. 앞 세션(확장 재로드 전, 또는 이 브라우저를 함께 쓰는
    // 다른 pane)이 건 override 는 새 세션의 clear 를 no-op 으로 흘려보내기 때문이다. 그러면 창 공간을
    // **직전에 걸어둔 기기 크기로** 재게 되고, 그 값으로 계산한 scale·fullyVisible 이 통째로 거짓이 된다
    // (2026-08-26 실측: 창이 1512x828 인데 834x1194 로 재서 scale 1 · fullyVisible true 가 나왔다.
    // 실제로는 아래 366px 이 창 밖이라, 하단 잘림을 잡으라고 만든 값이 정확히 그 경우를 놓친다).
    await cdp.raw(id, 'Emulation.setDeviceMetricsOverride', { width: 0, height: 0, deviceScaleFactor: 0, mobile: false }).catch(() => {})
    await cdp.raw(id, 'Emulation.clearDeviceMetricsOverride').catch(() => {})
    const room = await measureRoom(id)

    // ★UA 는 크기와 함께 가야 한다. 서버에서 모바일 뷰를 고르는 페이지는 UA 로 가르므로, 크기만
    // 바꾸면 **데스크톱 HTML 이 폰 폭에 들어간 화면**이 나온다 — 실제 폰에서는 볼 수 없는 화면이다.
    // ua:false 로 끄고, ua:'<문자열>' 로 직접 줄 수 있다.
    // 기기 이름 없이 크기만 준 호출에는 걸지 않는다 — 1440x900 을 요청한 사람에게 iPhone UA 를
    // 씌우면 그건 어떤 기기도 아닌 조합이 된다.
    const uaMode = ua === false ? 'off'
      : (typeof ua === 'string' && ua) ? 'custom'
        : (device !== undefined || !sizedByHand) ? 'preset' : 'off'
    // ★override 를 걸기 전의 「진짜 UA」를 확정한다. 메모리 Map 만 믿으면 안 된다 — 확장을 재로드하면
    // 그 Map 은 비는데 페이지의 override 는 남아 있어서, 그때 현재 UA 를 원본으로 저장하면 **폰 UA 를
    // 원본으로 기억한다**. 그러면 ua:false 나 off 가 폰 UA 로 「복원」하고, 도구는 성공을 돌려준다
    // (2026-08-26 실측: 재로드 뒤 off 를 걸었더니 크기·터치는 풀렸는데 UA 만 Android 로 남았다).
    const emulMap = await loadEmulated()
    let restoreUA = originalUA.get(id) || emulMap.get(id)?.restoreUA || null
    if (!restoreUA) {
      restoreUA = await cdp.evaluate(id, 'navigator.userAgent').catch(() => null)
    }
    if (restoreUA) originalUA.set(id, restoreUA)

    let uaSet = null
    let uaArgs = null
    if (uaMode === 'off') {
      // ⚠️「안 건다」로는 부족하다. 같은 탭에 앞서 건 override 가 살아 있으면 페이지는 계속 그 UA 를
      // 보는데 보고서에는 uaOverridden:false 만 남는다 — 그게 제일 나쁜 조합이다. 되돌려 놓는다.
      if (restoreUA) {
        await cdp.raw(id, 'Emulation.setUserAgentOverride', { userAgent: restoreUA }).catch(() => {})
      }
    } else {
      const override = uaMode === 'custom'
        ? { userAgent: ua, userAgentMetadata: undefined, platform: '' }
        : uaOverrideFor({ ...preset, mobile: isMobile })
      if (override) {
        const args = { userAgent: override.userAgent }
        if (override.platform) args.platform = override.platform
        if (override.userAgentMetadata) args.userAgentMetadata = override.userAgentMetadata
        uaArgs = args
        // metadata 를 거부하는 크롬 버전이 있으면 UA 문자열만이라도 건다. 조용히 통째로 실패하면
        // 「UA 도 바꿨다」는 보고만 남고 실제로는 데스크톱 UA 인 상태가 된다.
        const ok = await cdp.raw(id, 'Emulation.setUserAgentOverride', args).then(() => true).catch(() => false)
        if (ok) uaSet = override.userAgent
        else {
          const ok2 = await cdp.raw(id, 'Emulation.setUserAgentOverride', { userAgent: override.userAgent }).then(() => true).catch(() => false)
          if (ok2) uaSet = override.userAgent
        }
      }
    }

    // ★축소된 화면은 창 **왼쪽 위**에 붙고 남는 여백은 오른쪽·아래에 몰린다. 이걸 DevTools 의 기기 모드처럼
    // 창 가운데로 옮기는 것은 **이 층에서 불가능하다** — 다시 파기 전에 이 문단을 읽어라. DevTools 의 가운데
    // 배치는 CDP 가 하는 일이 아니라 프론트엔드가 렌더 표면을 자기 패널 안에 놓는 것이고, 그 배치는 브라우저
    // UI 층이라 확장이 닿지 못한다. 크롬에는 왼쪽 패널도 없어서 컨텐츠 영역 왼쪽에 공간을 만들 방법도 없다.
    // 2026-08-26 에 네 경로를 전부 실측해 막힌 것을 확인했다:
    //   · positionX/positionY — screenWidth 없이는 `View position should be on the screen` 으로 거부되고,
    //     screenWidth 를 함께 주면 성공을 돌려주는데 캡처가 바이트까지 동일하다(렌더 영향 0).
    //     대신 페이지가 보는 screen.width 만 창 폭으로 오염된다.
    //   · viewport:{x,y,width,height,scale} — x 오프셋도 scale 도 무시된다(테두리 두께가 그대로여서 scale 1 로 확인).
    //   · dontSetVisibleSize:true — 렌더 표면은 창 크기로 유지되지만 CSS 뷰포트가 기기 크기가 아니게 된다
    //     (innerWidth 가 창 폭 그대로). 에뮬레이션 자체가 무력화되므로 못 쓴다.
    //   · html 에 transform 주입 — 화면상으로는 정확히 가운데로 간다(좌우 여백이 같고 잘리지도 않는다). 그런데
    //     innerWidth 820→1566 · innerHeight 1180→2254 로 오염되고 position:fixed 가 깨진다(상단 고정 요소가
    //     스크롤을 따라 y=-900 으로 올라갔다). 미디어쿼리와 하단 네비 검증이 통째로 거짓이 되니 금지다.
    // 이 도구의 값어치는 「실제 폰에서 보이는 것과 같은 화면」이다. 가운데로 놓자고 그 전제를 깨는 스위치를
    // 달면 언젠가 그것을 켠 채로 낸 검증 결과가 나온다 — 그래서 옵션으로도 두지 않기로 했다(2026-08-26 결정).
    // 남는 길은 기기 크기 iframe 을 가운데 깐 래퍼 페이지뿐인데, X-Frame-Options/CSP 로 막히는 사이트가 있고
    // 모든 도구가 프레임 안을 보게 고쳐야 해서 판이 다르다.
    // ⚠️그리고 이 여백은 **스크린샷으로 판정할 수 없다**. 위 screenshot 의 주석에 적은 대로 captureVisibleTab 은
    // 창이 아니라 렌더 표면만 찍는데, 에뮬을 걸면 그 표면이 축소된 기기 크기로 줄어든다. 그림에는 기기 화면만
    // 꽉 차게 담기므로 여백이 아예 안 보이고, 「스샷이 멀쩡하니 여백이 없다」는 정반대 결론이 나온다.
    // 화면이 창 안 어디에 놓였는지를 봐야 하면 데스크톱 캡처로 봐라.
    // DevTools 기기 모드와 같은 처리다 — CSS 픽셀은 그대로 두고 화면에 그릴 때만 줄이므로
    // 미디어쿼리 분기는 하나도 바뀌지 않는다(실측: scale 0.915 에서 innerWidth 390 유지).
    const scale = fit && room ? Math.min(1, room.w / w, room.h / h) : 1
    const overflows = !!room && (w > room.w || h > room.h)

    await cdp.raw(id, 'Emulation.setDeviceMetricsOverride', {
      width: w, height: h, deviceScaleFactor: dsf, mobile: isMobile,
      ...(scale < 1 ? { scale } : {}),
    })
    // ★크기만 바꾸면 폰이 되지 않는다. 폰에는 마우스가 없으므로 터치까지 켜야 `(hover: none)` 과
    // `(pointer: coarse)` 규칙이 걸린다 — 안 켜면 **실제 폰에서만 보이는 스타일을 못 본 채**
    // 「폰뷰 확인」이 끝난다(2026-08-05 실측: 크기만 바꾼 상태와 터치까지 켠 상태가 그 두 조건에서
    // 갈렸다. mission-control 에는 `(hover: none)` 규칙이 두 곳 있다). 창을 좁히는 우회로는
    // 애초에 재현할 수 없는 부분이다 — 마우스가 붙어 있는 한 hover 는 계속 hover 다.
    await cdp.raw(id, 'Emulation.setTouchEmulationEnabled', wantTouch ? { enabled: true, maxTouchPoints: 5 } : { enabled: false }).catch(() => {})
    // 걸었다는 말만으로는 유지 여부를 모른다. 페이지가 실제로 무엇을 봤는지 함께 돌려준다.
    const seen = await cdp.evaluate(id, PHONE_PROBE).catch(() => null)

    // 페이지가 새로 뜰 때 되돌릴 수 있도록 남긴다. 여기 없으면 뒤로가기 한 번에 터치가 풀린 채로
    // 「폰뷰 확인」이 계속된다.
    emulMap.set(id, {
      width: w, height: h, dsf, mobile: isMobile, touch: wantTouch,
      scale: fit && room ? Math.min(1, room.w / w, room.h / h) : 1,
      userAgent: uaSet, uaArgs, restoreUA,
    })
    await saveEmulated()

    // ★「걸었다」와 「페이지가 그 폭으로 보고 있다」는 다른 말이다. 페이지가 뷰포트 메타로 더 넓은
    // 레이아웃 폭을 요구하면(데스크톱 전용 페이지가 흔히 그렇다) innerWidth 는 요청한 폭이 아니다 —
    // 그걸 모르고 재면 데스크톱 배치를 세로 결과로 읽는다. 요청값과 실측값을 나란히 돌려준다.
    const appliedWidth = seen?.viewport ? Number(String(seen.viewport).split('x')[0]) : null
    const appliedHeight = seen?.viewport ? Number(String(seen.viewport).split('x')[1]) : null
    const notes = []
    if (appliedWidth && appliedWidth !== w) {
      notes.push(`⚠️요청한 폭은 ${w} 인데 페이지가 실제로 본 폭은 ${appliedWidth} 입니다 — 이 페이지의 뷰포트 메타가 더 넓은 레이아웃을 요구했습니다(실제 폰에서도 그렇게 보입니다). 측정할 때는 appliedWidth 를 기준으로 하세요.`)
    }
    if (scale < 1 && room) notes.push(`창이 ${room.w}x${room.h} 라 ${Math.round(scale * 100)}% 로 축소해 넣었습니다. CSS 픽셀은 ${w}x${h} 그대로여서 미디어쿼리는 안 바뀝니다.`)
    else if (overflows && room) notes.push(`⚠️창(${room.w}x${room.h})보다 커서 화면 밖으로 잘립니다. bottom 에 붙은 요소는 안 보입니다 — fit 을 켜면 축소해 맞춥니다.`)
    // UA 로 가르는 것은 서버다. 이미 받아둔 문서는 안 바뀌므로 다시 요청해야 그 분기가 보인다.
    if (uaSet) notes.push('UA 는 다음 요청부터 서버에 전달됩니다 — 이미 열린 페이지의 서버 분기를 보려면 navigate 로 다시 여세요.')
    else if (uaMode === 'off' && sizedByHand && device === undefined) notes.push('기기 이름 없이 크기만 줘서 UA 는 그대로 둡니다. 기기 UA 까지 필요하면 device 를 함께 주세요.')
    return {
      tabId: id, emulating: true,
      // 기기 이름 없이 크기만 준 호출에 기본 프리셋 이름을 붙이면 「iPhone 12 Pro 인데 폭이 834」라는
      // 읽을 수 없는 조합이 된다. 그럴 땐 이름을 아예 안 붙이고 손으로 잡았다고만 밝힌다.
      ...(device === undefined && sizedByHand
        ? { device: null, sizedByHand: true }
        : { device: preset.resolvedKey, deviceLabel: preset.resolvedName, ...(sizedByHand ? { sizedByHand: true } : {}) }),
      width: w, height: h, deviceScaleFactor: dsf, mobile: isMobile, touch: wantTouch,
      landscape: !!(preset.landscape || rotate),
      viewport: seen?.viewport ?? null,
      appliedWidth, appliedHeight,
      touchPoints: seen?.touchPoints ?? null,
      hoverNone: seen?.hoverNone ?? null,
      pointerCoarse: seen?.pointerCoarse ?? null,
      userAgent: seen?.userAgent ?? null,
      uaOverridden: !!uaSet,
      chPlatform: seen?.chPlatform ?? null,
      chMobile: seen?.chMobile ?? null,
      windowRoom: room ? `${room.w}x${room.h}` : null,
      scale: Number(scale.toFixed(3)),
      // 「걸렸다」와 「사람 눈에 다 보인다」는 다른 말이다. 후자를 명시적으로 돌려준다.
      fullyVisible: room ? Math.round(h * scale) <= room.h + 1 && Math.round(w * scale) <= room.w + 1 : null,
      ...(notes.length ? { note: notes.join(' ') } : {}),
    }
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
    // ⚠️터치만 특별 취급한다. 마우스·키보드는 보이지 않는 탭에서도 즉시 ack 이 오지만
    // `Input.dispatchTouchEvent` 는 제스처 인식기를 타고, 그것이 hidden 탭에서 돌지 않아 **응답이
    // 영영 오지 않는다**(2026-08-11 실측: 45초 도구 타임아웃까지 침묵). 여기서 막지 않으면 45초를
    // 기다린 끝에 원인을 알 수 없는 타임아웃만 남는다 — raw 는 탈출구지 함정이 아니어야 한다.
    // 탭을 자동으로 앞에 보내지는 않는다. raw 로 오는 명령은 무엇을 하려는지 모르므로 마음대로
    // 화면을 바꾸지 않고, 무엇을 하면 되는지만 알린다(제스처 한 벌이면 browser_swipe 가 낫다).
    if (method === 'Input.dispatchTouchEvent') {
      const tab = await chrome.tabs.get(id).catch(() => null)
      if (tab && !tab.active) {
        throw new Error('HIDDEN_TAB_TOUCH: 보이지 않는 탭은 터치 이벤트를 처리하지 않아 이 명령이 응답 없이 멈춥니다. browser_activate_tab 으로 탭을 앞에 보내거나(창은 안 올라옵니다), 제스처 한 벌이면 browser_swipe 를 쓰세요.')
      }
    }
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
