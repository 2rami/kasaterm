import { dispatch, targetTabOf, reapplyEmulation, forgetEmulation, reapplyLayout, forgetLayout, layoutState, layoutToggle } from './tools.js'
import { setBridgeSender, bridgeResolve } from './bridge-ask.js'
import { openSession, closeSession, markBusy, markDone, forgetTab, refreshAction, restoreOverlay, snapshot, groupTabs, ungroupTabs, addActivity, clearPanes, repaintAll } from './sessions.js'
import { getDisplay, setDisplay } from './display.js'
import { hostOf } from './url.js'
import { PORT } from './port.js'

// 이름을 URL 로 두면 전역 URL 생성자를 가려 hostOf 가 조용히 폴백을 탄다
const BRIDGE_URL = `ws://127.0.0.1:${PORT}`

// 미리 대상 탭을 정할 수 없는 툴 — 아무 탭도 건드리지 않거나(status·list_tabs),
// 대상이 실행 결과로만 정해진다(new_tab). 여기서 활성 탭을 잡으면 엉뚱한 탭이 조작 중으로 켜진 채 남는다.
const NO_TAB_TOOLS = new Set(['status', 'list_tabs', 'set_task', 'dev_reload', 'new_tab'])

// 툴 호출을 사람이 읽는 한 줄로 옮긴다. null 이면 기록하지 않는다 — 상태 조회까지 남기면
// 정작 무엇을 했는지가 묻힌다. ⚠️입력값은 절대 넣지 않는다(비밀번호·검색어가 그대로 남는다).
function describe(tool, args = {}, result = {}) {
  const t = result?.target
  switch (tool) {
    case 'click': return t ? `클릭 — ${t}` : '클릭'
    case 'fill': return t ? `입력 — ${t}` : '입력'
    case 'type': return '타이핑'
    case 'press_key': return `키 — ${args.key}`
    case 'navigate': return `이동 — ${hostOf(args.url)}`
    case 'new_tab': return `새 탭 — ${hostOf(result?.url || args.url)}`
    case 'new_window': return `새 창 — ${hostOf(result?.url || args.url)}`
    case 'close_tab': return '탭 닫기'
    case 'close_window': return '창 닫기'
    case 'activate_tab': return '탭 전환'
    // reason 은 사람에게 보이라고 쓰는 짧은 사유라 그대로 남긴다(set_task 의 작업명과 같은 성격).
    case 'ask_human': return args.reason ? `도움 요청 — ${args.reason}` : '도움 요청'
    case 'screenshot': return '화면 확인'
    case 'read_page': case 'get_text': return '페이지 읽기'
    case 'find': return `요소 찾기 — ${args.query}`
    case 'scroll': case 'scroll_to': return '스크롤'
    case 'hover': return '마우스 올리기'
    case 'drag': return '끌어놓기'
    case 'eval_js': return '스크립트 실행'
    case 'upload_file': return '파일 업로드'
    case 'console_logs': return '콘솔 확인'
    case 'network_requests': return '네트워크 확인'
    case 'wait_for': return '대기'
    case 'resize_window': return '창 크기 조절'
    case 'ungroup_tabs': return '탭 그룹 해제'
    case 'set_task': return `작업명 — ${args.task}`
    case 'cdp_raw': return `CDP — ${args.method}`
    default: return null
  }
}

let ws = null
let backoff = 500
let connecting = false
// 세션 open/close 는 저장소를 읽느라 비동기라, 그냥 두면 뒤따라온 첫 호출이 세션보다 먼저 실행돼
// 신원 없이 처리된다(작업명이 조용히 버려졌다). 세션 처리는 한 줄로 세우고 호출은 그 뒤에 태운다.
let sessionChain = Promise.resolve()

// 크롬 프로필마다 확장 인스턴스가 따로 뜨고 storage 도 프로필별로 갈린다 — 여기 심은 id 가
// 곧 프로필의 신원이다. 브리지는 이 id 로 어느 크롬에 명령을 보낼지 가른다.
let profileCache = null
async function profileOf() {
  if (profileCache) return profileCache
  let saved = {}
  try { saved = (await chrome.storage.local.get('kc_profile')).kc_profile || {} } catch {}
  let id = saved.id
  if (!id) {
    id = (crypto.randomUUID?.() || String(Date.now())).slice(0, 8)
    try { await chrome.storage.local.set({ kc_profile: { ...saved, id } }) } catch {}
  }
  let label = saved.label || ''
  if (!label) {
    // 로그인 안 된 프로필은 빈 문자열이 온다 — 그때는 아래 hint 로 사람이 알아본다.
    try { label = (await chrome.identity.getProfileUserInfo({ accountStatus: 'ANY' }))?.email || '' } catch {}
  }
  profileCache = { id, label }
  return profileCache
}

// 라벨이 비면 어느 크롬인지 사람이 못 고른다. 열린 탭에서 흔한 도메인을 뽑아 단서로 준다.
async function profileHint() {
  try {
    const tabs = await chrome.tabs.query({})
    const hosts = new Map()
    for (const t of tabs) {
      const m = /^https?:\/\/([^/:]+)/.exec(t.url || '')
      if (!m) continue
      const h = m[1].replace(/^www\./, '')
      hosts.set(h, (hosts.get(h) || 0) + 1)
    }
    const top = [...hosts.entries()].sort((a, b) => b[1] - a[1]).slice(0, 3).map(([h]) => h)
    return `탭 ${tabs.length}개${top.length ? ' · ' + top.join(', ') : ''}`
  } catch { return '' }
}

function connect() {
  if (connecting || (ws && ws.readyState <= 1)) return
  connecting = true
  try {
    ws = new WebSocket(BRIDGE_URL)
  } catch {
    connecting = false
    schedule()
    return
  }

  ws.onopen = async () => {
    connecting = false
    backoff = 500
    // await 사이에 재연결이 겹치면 ws 가 딴 소켓으로 바뀐다 — 지금 열린 이 소켓을 잡아 둔다.
    const sock = ws
    const profile = await profileOf()
    const hello = { type: 'hello', role: 'extension', profile: { ...profile, hint: await profileHint() } }
    if (sock.readyState === 1) sock.send(JSON.stringify(hello))
    refreshAction(true)
  }

  ws.onmessage = async (ev) => {
    let msg
    try { msg = JSON.parse(ev.data) } catch { return }
    if (msg.type === 'ping') return

    // 우리가 브리지에 물어본 것의 답
    if (msg.type === 'layoutool') { bridgeResolve(msg); return }

    if (msg.type === 'session') {
      sessionChain = sessionChain
        .then(() => (msg.action === 'open' ? openSession(msg.client, msg.identity) : closeSession(msg.client)))
        .catch(() => {})
      return
    }

    if (msg.type !== 'call') return
    await sessionChain
    const ctx = { client: msg.client }
    let tabId = null
    try {
      if (!NO_TAB_TOOLS.has(msg.tool)) {
        tabId = await targetTabOf(msg.args).catch(() => null)
        if (tabId) await markBusy(msg.client, tabId)
      }
      const result = await dispatch(msg.tool, msg.args, ctx)
      // 새로 만든 탭에도 곧바로 오버레이를 띄운다 — 누가 연 창인지 바로 보이게.
      // ⚠️new_window 를 빠뜨리면 별도 창으로 연 페이지는 조작하기 전까지 임자 없는 탭으로 남는다.
      // 그 사이 다른 pane 이 만지면 그쪽이 첫 점유자가 되어 칩 순서가 뒤바뀐다(실측).
      if ((msg.tool === 'new_tab' || msg.tool === 'new_window') && result?.tabId) {
        tabId = result.tabId
        await markBusy(msg.client, tabId)
      }
      const label = describe(msg.tool, msg.args, result)
      if (label) addActivity(msg.client, label, tabId)
      if (tabId) markDone(msg.client, tabId)
      reply({ type: 'result', id: msg.id, ok: true, result })
    } catch (e) {
      // 실패도 남긴다 — "눌렀는데 왜 안 됐지" 를 되짚을 때 성공만 보이면 그림이 반쪽이다
      const label = describe(msg.tool, msg.args, {})
      if (label) addActivity(msg.client, label, tabId, false)
      if (tabId) markDone(msg.client, tabId)
      reply({ type: 'result', id: msg.id, ok: false, error: String(e && e.message ? e.message : e) })
    }
  }

  ws.onclose = () => {
    connecting = false
    ws = null
    refreshAction(false)
    schedule()
  }

  ws.onerror = () => { try { ws.close() } catch {} }
}

function reply(obj) {
  if (ws && ws.readyState === 1) ws.send(JSON.stringify(obj))
}

setBridgeSender(reply)

function schedule() {
  backoff = Math.min(backoff * 2, 15000)
  setTimeout(connect, backoff)
}

function connected() {
  return !!(ws && ws.readyState === 1)
}

chrome.tabs.onRemoved.addListener((tabId) => { forgetTab(tabId); forgetEmulation(tabId).catch(() => {}); forgetLayout(tabId).catch(() => {}) })

// 페이지가 새로 뜨면 오버레이가 통째로 날아간다. 담당 세션이 있는 탭이면 칩을 다시 붙인다.
chrome.tabs.onUpdated.addListener((tabId, info) => {
  if (info.status !== 'complete') return
  restoreOverlay(tabId).catch(() => {})
  // 폰뷰도 같은 이유로 되돌린다. 새 문서에서 터치 에뮬레이션이 풀린 채 남으면 크기만 폰이고
  // `(pointer: coarse)` 규칙은 안 걸리는 상태가 되는데, 그건 화면만 봐서는 구분이 안 된다.
  reapplyEmulation(tabId).catch(() => {})
  // 레이아웃툴 편집기도 마찬가지다. 새 문서에는 안 들어가 있어서, 켜 둔 탭인데 화면만 갈려도
  // 사람 눈에는 편집기가 고장난 것으로 보인다.
  reapplyLayout(tabId).catch(() => {})
})

// 확장 아이콘 팝업이 상태를 물어온다. 팝업이 열렸다는 건 service worker 가 막 깨어났을 수도 있다는
// 뜻이라 여기서 브리지 연결도 한 번 확인한다(세션은 확장이 붙는 즉시 브리지가 다시 알려준다).
// setDisplay 는 페이지의 content script 도 보낸다 — 사람이 칩을 끌어 옮겼을 때다. 웹페이지는
// externally_connectable 이 없으면 여기로 메시지를 못 보내지만, 값 검증은 display.js 가 따로 한다.
chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (!msg || msg.__ccPopup !== true) return
  if (msg.op === 'state') {
    connect()
    snapshot(connected())
      .then(sendResponse)
      .catch((e) => sendResponse({ connected: connected(), sessions: [], error: String(e?.message || e) }))
    return true
  }
  if (msg.op === 'focus') {
    chrome.tabs.update(msg.tabId, { active: true })
      .then(() => chrome.windows.update(msg.windowId, { focused: true }))
      .then(() => sendResponse({ ok: true }))
      .catch((e) => sendResponse({ ok: false, error: String(e?.message || e) }))
    return true
  }
  if (msg.op === 'setDisplay') {
    setDisplay(msg.patch || {})
      .then(async (d) => { await repaintAll(); sendResponse(d) })
      .catch((e) => sendResponse({ error: String(e?.message || e) }))
    return true
  }
  // 레이아웃툴 편집기를 사람이 직접 켜고 끄는 줄. 터미널의 에이전트를 거치지 않는 길이 하나는
  // 있어야 한다 — 화면을 만지는 것은 사람이고, 그때마다 부탁할 수는 없다.
  if (msg.op === 'layoutState') {
    layoutState(msg.tabId)
      .then(sendResponse)
      .catch(() => sendResponse({ ok: false }))
    return true
  }
  if (msg.op === 'layoutToggle') {
    layoutToggle(msg.tabId)
      .then(sendResponse)
      .catch((e) => sendResponse({ ok: false, error: String(e?.message || e) }))
    return true
  }
  if (msg.op === 'group' || msg.op === 'ungroup') {
    const run = msg.op === 'group' ? groupTabs : ungroupTabs
    run(msg.key)
      .then(sendResponse)
      .catch((e) => sendResponse({ ok: false, error: String(e?.message || e) }))
    return true
  }
})

// 칩이 하필 지금 보려던 것을 가렸을 때 팝업을 열어 항목을 찾는 건 세 단계다. 한 번에 전부 걷는다.
// 개별 토글은 그대로 두고 이 스위치만 덮어쓰므로, 되돌리면 끄기 전 조합이 그대로 돌아온다.
chrome.commands.onCommand.addListener((cmd) => {
  if (cmd !== 'toggle-overlay') return
  getDisplay()
    .then((d) => setDisplay({ off: !d.off }))
    .then(() => repaintAll())
    .catch(() => {})
})

// service worker 가 잠들면 소켓도 같이 죽는다. alarm 이 깨워서 다시 붙인다.
chrome.alarms.create('cc-keepalive', { periodInMinutes: 0.5 })
chrome.alarms.onAlarm.addListener(() => connect())

// 브라우저를 새로 켰다 = 탭 id 가 전부 갈렸다. 저장소에 남은 세션 레코드는 이 시점에 무효다.
chrome.runtime.onStartup.addListener(() => { clearPanes(); connect() })
chrome.runtime.onInstalled.addListener(() => connect())

connect()
refreshAction(false)
