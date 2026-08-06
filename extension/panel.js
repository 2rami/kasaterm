// 지금 누가(어느 세션이) 어떤 탭을 잡고 무슨 작업을 하는지 보여주는 화면.
// 아이콘 팝업(popup.html)과 상주 사이드 패널(sidepanel.html)이 이 파일을 함께 쓴다.
// 페이지에서 온 제목·URL 은 반드시 textContent 로만 넣는다(남의 사이트가 준 문자열이다).
import { hostOf } from './url.js'
import { PORT } from './port.js'

const rootEl = document.getElementById('root')
const connEl = document.getElementById('conn')
const connText = document.getElementById('conn-text')
let lastSig = null

// 제품명·포트는 각각 manifest 와 port.js 한 곳에만 둔다 — 화면 문구가 그걸 따라온다.
const PRODUCT = chrome.runtime.getManifest().name
const brandEl = document.querySelector('.brand')
if (brandEl) brandEl.textContent = PRODUCT

function ask(op, extra = {}) {
  return new Promise((resolve) => {
    chrome.runtime.sendMessage({ __ccPopup: true, op, ...extra }, (res) => {
      void chrome.runtime.lastError
      resolve(res || null)
    })
  })
}

function el(tag, cls, text) {
  const n = document.createElement(tag)
  if (cls) n.className = cls
  if (text != null) n.textContent = text
  return n
}

function tabRow(t) {
  const b = el('button', 'tab')
  const fav = el('img', 'fav')
  if (t.favIconUrl) {
    fav.src = t.favIconUrl
    fav.addEventListener('error', () => fav.removeAttribute('src'))
  }
  b.append(fav, el('span', 't', t.title), el('span', 'h', hostOf(t.url)))
  if (t.busy) b.appendChild(el('i', 'mark'))
  b.addEventListener('click', async () => {
    await ask('focus', { tabId: t.tabId, windowId: t.windowId })
    window.close()
  })
  const li = document.createElement('li')
  li.appendChild(b)
  return li
}

// 이 세션이 잡은 탭을 크롬 탭 그룹으로 묶는다/푼다. 누를 때만 움직인다 — 자동으로는 안 묶는다.
function groupButton(s) {
  const b = el('button', 'grp', s.grouped ? '풀기' : '묶기')
  b.addEventListener('click', async () => {
    b.disabled = true
    const res = await ask(s.grouped ? 'ungroup' : 'group', { key: s.key })
    if (res && res.ok === false && res.error) b.textContent = res.error.slice(0, 12)
    lastSig = null
    await tick()
  })
  return b
}

// --- 화면 표시 설정 --------------------------------------------------------

const PARTS = [['frame', '테두리'], ['chip', '칩'], ['cursor', '커서']]
const POS = [['tl', '좌상'], ['tr', '우상'], ['bl', '좌하'], ['br', '우하']]
const DISPLAY_FALLBACK = { off: false, frame: true, chip: true, cursor: true, pos: 'tr', dx: 12, dy: 12 }
// 모서리 버튼은 자리 되돌리기를 겸한다 — 칩을 끌어 옮긴 뒤 원래 자리로 보내는 유일한 길이다.
const EDGE = 12

async function apply(patch) {
  await ask('setDisplay', { patch })
  lastSig = null
  await tick()
}

// 페이지 위 표시를 사람이 끄고 옮기는 줄. 세션이 없어도 미리 정해둘 수 있어 항상 맨 위에 둔다.
// 우상단 칩이 사이트의 계정 메뉴·닫기 버튼을 가리는 일이 잦아 위치를 고르게 했다.
function displayBar(d) {
  const wrap = el('div', d.off ? 'disp off' : 'disp')

  const sw = el('button', 'sw', d.off ? '숨김 해제' : '전부 숨김')
  sw.title = '단축키 ⌥⇧O'
  sw.addEventListener('click', () => apply({ off: !d.off }))
  const head = el('div', 'disp-h')
  head.append(el('span', 'disp-t', '화면 표시'), sw)

  const tgs = el('div', 'tgs')
  for (const [k, label] of PARTS) {
    const b = el('button', d[k] ? 'tg on' : 'tg', label)
    b.disabled = d.off
    b.addEventListener('click', () => apply({ [k]: !d[k] }))
    tgs.appendChild(b)
  }

  const poss = el('div', 'poss')
  for (const [v, label] of POS) {
    const b = el('button', d.pos === v ? 'pos on' : 'pos')
    b.dataset.v = v
    b.title = `칩 위치 — ${label} (끌어 옮긴 자리는 여기로 되돌아갑니다)`
    b.setAttribute('aria-label', `칩 위치 ${label}`)
    // 칩을 꺼둔 채 위치를 고르는 건 아무 일도 일어나지 않는 조작이다
    b.disabled = d.off || !d.chip
    b.addEventListener('click', () => apply({ pos: v, dx: EDGE, dy: EDGE }))
    poss.appendChild(b)
  }

  const row = el('div', 'disp-r')
  row.append(tgs, poss)
  wrap.append(head, row)
  return wrap
}

function card(s) {
  const c = el('div', 'card')

  const ava = el('span', s.busy ? 'ava busy' : 'ava')
  const img = el('img')
  if (s.avatar) img.src = s.avatar
  const dot = el('i', 'dot')
  if (!s.busy) dot.style.background = s.color
  ava.append(img, dot)

  const meta = el('div', 'meta')
  const name = el('div', 'name')
  name.appendChild(el('span', null, s.name))
  if (s.paneId) name.appendChild(el('span', 'pane', s.paneId))
  meta.append(name, el('div', 'task', s.task || '작업명 없음'))

  const who = el('div', 'who')
  who.append(ava, meta, el('span', s.busy ? 'state busy' : 'state', s.busy ? '조작 중' : '대기'))
  if (s.tabs.length) who.appendChild(groupButton(s))
  c.appendChild(who)

  if (!s.tabs.length) {
    c.appendChild(el('div', 'notabs', '아직 잡은 탭이 없습니다.'))
  } else {
    const ul = el('ul', 'tabs')
    for (const t of s.tabs) ul.appendChild(tabRow(t))
    c.appendChild(ul)
  }
  if (s.log?.length) c.appendChild(activityList(s.log))
  return c
}

function hhmmss(ms) {
  const d = new Date(ms)
  return [d.getHours(), d.getMinutes(), d.getSeconds()].map((n) => String(n).padStart(2, '0')).join(':')
}

// 무엇을 했는지 시간순(최신 위). 사이드 패널은 세로가 기니 더 많이 보여준다.
function activityList(log) {
  const limit = document.body.classList.contains('panel') ? 12 : 5
  const wrap = el('div', 'acts')
  for (const a of log.slice(0, limit)) {
    const row = el('div', a.failed ? 'act failed' : 'act')
    row.append(el('span', 'ts', hhmmss(a.at)), el('span', 'al', a.label))
    wrap.appendChild(row)
  }
  return wrap
}

function render(state) {
  const sessions = state?.sessions || []
  const connected = !!state?.connected
  connEl.className = connected ? 'conn on' : 'conn off'
  connText.textContent = connected ? '브리지 연결됨' : '브리지 없음'

  rootEl.replaceChildren()
  if (!connected) {
    rootEl.appendChild(el('div', 'warn', `브리지(127.0.0.1:${PORT})에 붙어 있지 않습니다. 터미널에서 브라우저 툴을 한 번 쓰면 브리지가 자동으로 뜹니다.`))
  }
  rootEl.appendChild(displayBar({ ...DISPLAY_FALLBACK, ...(state?.display || {}) }))
  if (!sessions.length) {
    const e = el('div', 'empty')
    e.appendChild(el('b', null, '아직 이 크롬을 조작한 세션이 없습니다'))
    e.appendChild(document.createTextNode('터미널에서 브라우저 툴을 쓰면 누가 어느 탭을 보고 있는지 여기에 나옵니다.'))
    rootEl.appendChild(e)
    return
  }
  const body = el('div', 'body')
  for (const s of sessions) body.appendChild(card(s))
  rootEl.appendChild(body)
}

// 프사 dataURL 은 25KB 라 비교에서 뺀다 — 어차피 신원이 바뀌면 이름이 같이 바뀐다.
function sig(state) {
  return JSON.stringify({
    c: state?.connected,
    d: state?.display,
    s: (state?.sessions || []).map((s) => [
      s.key, s.name, s.paneId, s.task, s.busy, s.grouped,
      s.log?.length, s.log?.[0]?.at,
      s.tabs.map((t) => [t.tabId, t.title, t.url, t.busy]),
    ]),
  })
}

async function tick() {
  const state = await ask('state')
  const g = sig(state)
  if (g === lastSig) return
  lastSig = g
  render(state)
}

// 사이드 패널은 한 번 열면 탭을 옮겨다녀도 계속 떠 있다 — 아이콘을 누르지 않아도 보이는 유일한 방법이다.
// `chrome.sidePanel.open` 은 사용자 제스처 안에서 불러야 하므로 windowId 를 미리 받아둔다.
const openBtn = document.getElementById('open-panel')
if (openBtn) {
  let myWindowId = null
  chrome.windows.getCurrent().then((w) => { myWindowId = w.id }).catch(() => {})
  openBtn.addEventListener('click', () => {
    if (myWindowId == null) return
    chrome.sidePanel.open({ windowId: myWindowId })
    window.close()
  })
}

tick()
// service worker 가 막 깨어난 참이면 세션 복구가 한 박자 늦는다. 화면이 떠 있는 동안만 훑는다.
setInterval(tick, 1000)
