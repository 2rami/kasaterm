// 툴 핸들러. 기본은 조용한 경로(content script)로 돌고, 그게 no-op 이면 같은 동작을 CDP 로 재시도한다.
// 그래서 평소엔 디버깅 배너가 안 뜨지만 능력치는 CDP 와 동일하다.
import * as cdp from './cdp.js'
import { page, restricted } from './page.js'
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
  pointerCoarse: matchMedia('(pointer: coarse)').matches
})`
const GROUP_COLORS = new Set(['grey', 'blue', 'red', 'yellow', 'green', 'pink', 'purple', 'cyan', 'orange'])

// 이름 하나로 폭·높이·dpr 을 한꺼번에 맞춘다. 폭만 옮겨 적고 dpr 을 잊으면 레티나에서만 드러나는
// 이미지·보더 문제를 통째로 못 본다.
const DEVICES = {
  phone: { width: 390, height: 844, deviceScaleFactor: 3 },
  'iphone-se': { width: 375, height: 667, deviceScaleFactor: 2 },
  pixel: { width: 412, height: 915, deviceScaleFactor: 2.625 },
  tablet: { width: 768, height: 1024, deviceScaleFactor: 2 },
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

  // ★찍는 동안 우리 표시(칩·테두리·커서)를 걷는다. 그림은 대개 사람에게 보여주거나 문서에 넣으려고
  // 찍는 것이라, 에이전트가 페이지 위에 얹은 것이 함께 박히면 그 그림은 쓸 수 없다 — 촬영을
  // 에이전트가 하는 이상 치우는 것도 에이전트 몫이고, 사람이 미리 껐다 켤 일이 아니다.
  // overlay:true 로 남길 수 있다. 오버레이 자체가 잘 그려지는지 확인할 때가 그 경우다.
  async screenshot({ tabId, fullPage = false, format = 'png', quality, overlay = false }) {
    const id = await resolveTabId(tabId)
    const tab = await chrome.tabs.get(id)
    const hidden = overlay ? false : await hideForShot(id)
    try {
      // captureVisibleTab 은 배너가 안 뜨지만 활성 탭의 보이는 영역만 찍는다. 나머지는 CDP 가 필요하다.
      if (!fullPage && tab.active) {
        try {
          const dataUrl = await chrome.tabs.captureVisibleTab(tab.windowId, { format, ...(quality ? { quality } : {}) })
          return { data: dataUrl.split(',')[1], format, via: 'captureVisibleTab', overlayHidden: hidden }
        } catch { /* 권한·타이밍 문제면 CDP 로 내려간다 */ }
      }
      const data = await cdp.screenshot(id, { fullPage, format, quality })
      return { data, format, via: 'cdp', overlayHidden: hidden }
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
  async emulate_device({ tabId, device, width, height, deviceScaleFactor, mobile = true, fit = true, off } = {}) {
    const id = await resolveTabId(tabId)

    if (off) {
      if (cdp.isAttached(id)) {
        await cdp.raw(id, 'Emulation.clearDeviceMetricsOverride').catch(() => {})
        await cdp.raw(id, 'Emulation.setTouchEmulationEnabled', { enabled: false }).catch(() => {})
      }
      await cdp.unpin(id, 'emulation')
      const seen = await cdp.evaluate(id, PHONE_PROBE).catch(() => null)
      return {
        tabId: id, emulating: false, viewport: seen?.viewport ?? null,
        note: '핀을 풀었으니 유휴 15초 뒤 디버깅 배너도 걷힙니다.',
      }
    }

    const preset = DEVICES[String(device || 'phone').toLowerCase()]
    if (device && !preset) throw new Error(`UNKNOWN_DEVICE: ${device}. 쓸 수 있는 이름 — ${Object.keys(DEVICES).join(', ')}`)
    const w = Math.round(Number(width) || preset.width)
    const h = Math.round(Number(height) || preset.height)
    const dsf = Number(deviceScaleFactor) || preset.deviceScaleFactor
    if (!(w >= 100 && w <= 4000 && h >= 100 && h <= 4000)) {
      throw new Error(`BAD_SIZE: ${w}x${h} 는 폰 화면 크기가 아닙니다. 100~4000 사이로 주거나 device 이름을 쓰세요.`)
    }

    // ⚠️핀을 먼저 건다. override 를 걸고 나서 붙잡으면 그 사이에 타이머가 세션을 놓을 수 있다.
    await cdp.pin(id, 'emulation')

    // ★창이 페이지에 실제로 내주는 공간을 먼저 잰다. override 가 이미 걸려 있으면 innerHeight 도
    // outerHeight 도 그 값으로 덮여서, 창이 그보다 작아도 페이지는 알 방법이 없다 — 이것이
    // 「폰뷰인데 하단 네비바가 없다」의 정체다(2026-08-05 실측: 창 772 에 844 를 걸어 아래 72px 이
    // 창 밖으로 나갔고 bottom:0 인 탭바 783~844 가 통째로 잘렸다. 스크린샷은 CDP 라 844 전부를
    // 찍으니 이미지로는 멀쩡해 보여서 더 헷갈린다).
    await cdp.raw(id, 'Emulation.clearDeviceMetricsOverride').catch(() => {})
    const room = await measureRoom(id)

    // DevTools 기기 모드와 같은 처리다 — CSS 픽셀은 그대로 두고 화면에 그릴 때만 줄이므로
    // 미디어쿼리 분기는 하나도 바뀌지 않는다(실측: scale 0.915 에서 innerWidth 390 유지).
    const scale = fit && room ? Math.min(1, room.w / w, room.h / h) : 1
    const overflows = !!room && (w > room.w || h > room.h)

    await cdp.raw(id, 'Emulation.setDeviceMetricsOverride', {
      width: w, height: h, deviceScaleFactor: dsf, mobile: !!mobile,
      ...(scale < 1 ? { scale } : {}),
    })
    // ★크기만 바꾸면 폰이 되지 않는다. 폰에는 마우스가 없으므로 터치까지 켜야 `(hover: none)` 과
    // `(pointer: coarse)` 규칙이 걸린다 — 안 켜면 **실제 폰에서만 보이는 스타일을 못 본 채**
    // 「폰뷰 확인」이 끝난다(2026-08-05 실측: 크기만 바꾼 상태와 터치까지 켠 상태가 그 두 조건에서
    // 갈렸다. mission-control 에는 `(hover: none)` 규칙이 두 곳 있다). 창을 좁히는 우회로는
    // 애초에 재현할 수 없는 부분이다 — 마우스가 붙어 있는 한 hover 는 계속 hover 다.
    if (mobile) {
      await cdp.raw(id, 'Emulation.setTouchEmulationEnabled', { enabled: true, maxTouchPoints: 5 }).catch(() => {})
    }
    // 걸었다는 말만으로는 유지 여부를 모른다. 페이지가 실제로 무엇을 봤는지 함께 돌려준다.
    const seen = await cdp.evaluate(id, PHONE_PROBE).catch(() => null)
    return {
      tabId: id, emulating: true, ...(preset && !width && !height ? { device: String(device || 'phone').toLowerCase() } : {}),
      width: w, height: h, deviceScaleFactor: dsf, mobile: !!mobile,
      viewport: seen?.viewport ?? null,
      touchPoints: seen?.touchPoints ?? null,
      hoverNone: seen?.hoverNone ?? null,
      pointerCoarse: seen?.pointerCoarse ?? null,
      windowRoom: room ? `${room.w}x${room.h}` : null,
      scale: Number(scale.toFixed(3)),
      // 「걸렸다」와 「사람 눈에 다 보인다」는 다른 말이다. 후자를 명시적으로 돌려준다.
      fullyVisible: room ? Math.round(h * scale) <= room.h + 1 && Math.round(w * scale) <= room.w + 1 : null,
      ...(scale < 1
        ? { note: `창이 ${room.w}x${room.h} 라 ${Math.round(scale * 100)}% 로 축소해 넣었습니다. CSS 픽셀은 ${w}x${h} 그대로여서 미디어쿼리는 안 바뀝니다.` }
        : overflows
          ? { note: `⚠️창(${room.w}x${room.h})보다 커서 화면 밖으로 잘립니다. bottom 에 붙은 요소는 안 보입니다 — fit 을 켜면 축소해 맞춥니다.` }
          : {}),
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
