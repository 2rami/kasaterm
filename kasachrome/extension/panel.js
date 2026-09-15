// 지금 누가(어느 세션이) 어떤 탭을 잡고 무슨 작업을 하는지 보여주는 화면.
// 아이콘 팝업(popup.html)과 상주 사이드 패널(sidepanel.html)이 이 파일을 함께 쓴다.
// 페이지에서 온 제목·URL 은 반드시 textContent 로만 넣는다(남의 사이트가 준 문자열이다).
import { hostOf } from './url.js'
import { PORT } from './port.js'

const rootEl = document.getElementById('root')
const connEl = document.getElementById('conn')
const connText = document.getElementById('conn-text')
let lastSig = null

// 화면 둘. 「작업」은 1초마다 갱신되는 현황(누가 어느 탭을 잡고 무슨 작업 중인지)이고,
// 레이아웃툴은 사람이 가끔 켜고 끄는 도구라 성격이 다르다 — 한 화면에 세로로 쌓아두면
// 매초 바뀌는 목록 위에 안 바뀌는 단추가 얹혀 어느 쪽도 눈에 안 들어온다.
const VIEWS = [['work', '작업'], ['device', '기기'], ['layout', '레이아웃툴']]
const VIEW_KEY = 'panelView'
const navEl = document.getElementById('nav')
const navButtons = new Map()
let view = 'work'

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
    if (document.body.classList.contains('popup')) window.close()
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

// --- 기기 뷰 ----------------------------------------------------------------

// 사람이 지금 보는 탭의 화면 크기를 한 번에 바꾸는 줄. 터미널의 학생에게 「폰으로 봐 줘」 하고
// 기다리는 것 말고 다른 길이 하나는 있어야 한다 — 레이아웃을 눈으로 보는 것은 사람이 하는 일이다.
// ⚠️창 크기를 바꾸는 게 아니라 이 탭에만 거는 override 다. 창은 이 브라우저를 함께 쓰는 모두의
// 것이라 건드리면 남의 설정이 통째로 날아가고, 크롬은 500px 아래로 좁히지도 못한다.
const DEVICES = [
  { key: null, label: '원래대로', icon: 'reset', hint: '이 탭의 기기 뷰를 끕니다' },
  { key: 'iphone-15-pro', label: '폰', icon: 'phone', w: 393, h: 852 },
  { key: 'ipad-air', label: '태블릿', icon: 'tablet', w: 820, h: 1180 },
  { key: 'laptop', label: '노트북', icon: 'laptop', w: 1440, h: 900 },
  { key: 'desktop-1080p', label: 'FHD', icon: 'monitor', w: 1920, h: 1080 },
  { key: 'desktop-4k', label: '4K', icon: 'monitor', w: 3840, h: 2160 },
]

// 이모지 대신 직접 그린다. 기기 실루엣은 글자보다 빨리 읽히고, 이모지는 OS 마다 다른 그림이 나온다.
const ICONS = {
  // 세로로 긴 몸체 + 위쪽 스피커.
  phone: 'M7.5 2.5h9a1 1 0 0 1 1 1v17a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1v-17a1 1 0 0 1 1-1ZM10 5h4',
  // 폰보다 넓고 아래에 홈 버튼.
  tablet: 'M5 2.5h14a1 1 0 0 1 1 1v17a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1v-17a1 1 0 0 1 1-1ZM12 18.8v.01',
  // 화면 + 바닥으로 벌어지는 받침.
  laptop: 'M5 5.5h14a1 1 0 0 1 1 1v9H4v-9a1 1 0 0 1 1-1ZM2 18.5h20',
  // 화면 + 목 + 스탠드.
  monitor: 'M3 4.5h18a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1v-10a1 1 0 0 1 1-1ZM12 16.5v3M8.5 19.5h7',
  // 되돌리는 화살표.
  reset: 'M4 11a8 8 0 1 1 2.3 5.7M4 5.5V11h5.5',
  // 눕힌 몸체를 도는 화살표.
  rotate: 'M3 8.5h13a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1v-5a1 1 0 0 1 1-1ZM16.5 6a6 6 0 0 1 5 5M21.5 6.5V11H17',
}

function deviceIcon(kind) {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg')
  svg.setAttribute('viewBox', '0 0 24 24')
  svg.setAttribute('aria-hidden', 'true')
  svg.setAttribute('fill', 'none')
  svg.setAttribute('stroke', 'currentColor')
  svg.setAttribute('stroke-width', '1.5')
  svg.setAttribute('stroke-linecap', 'round')
  svg.setAttribute('stroke-linejoin', 'round')
  const path = document.createElementNS(svg.namespaceURI, 'path')
  path.setAttribute('d', ICONS[kind] || ICONS.monitor)
  svg.append(path)
  return svg
}

// 지금 걸린 것이 어느 버튼인지. 기기 이름이 아니라 **크기**로 맞춘다 — 학생이 도구로 건 기기는
// 이 목록에 없을 수 있고(Pixel 7·Surface 등), 그때 아무 버튼도 안 눌린 것으로 두는 편이 정직하다.
// 뒤집힌 크기도 같은 기기로 본다. 안 그러면 가로로 눕히는 순간 눌린 버튼이 사라져, 방금 누른
// 사람이 「기기 뷰가 꺼졌나」로 읽는다.
function currentDevice(emul) {
  if (!emul?.on) return null
  const upright = DEVICES.find((d) => d.w === emul.width && d.h === emul.height)
  if (upright) return { ...upright, landscape: false }
  const flipped = DEVICES.find((d) => d.w === emul.height && d.h === emul.width)
  return flipped ? { ...flipped, landscape: true } : null
}

function devicePane(emul, tabId) {
  const wrap = el('div', 'dev')
  const head = el('div', 'disp-h')
  head.append(el('span', 'disp-t', '기기 뷰'))
  wrap.append(head)

  if (tabId == null) {
    wrap.append(el('div', 'lay-n', '지금 보고 있는 탭을 찾지 못했습니다. 페이지를 열고 다시 시도해 주세요.'))
    return wrap
  }

  const note = el('div', 'lay-n')
  const here = currentDevice(emul)
  const grid = el('div', 'dev-grid')
  for (const d of DEVICES) {
    const on = d.key ? here?.key === d.key : !emul?.on
    const b = el('button', on ? 'dev-b on' : 'dev-b')
    b.title = d.hint || `${d.w}×${d.h}`
    b.setAttribute('aria-pressed', String(on))
    b.append(deviceIcon(d.icon), el('span', 'dev-l', d.label))
    if (d.w) b.append(el('span', 'dev-s', `${d.w}×${d.h}`))
    b.addEventListener('click', async () => {
      for (const other of grid.children) other.disabled = true
      const r = await ask('emulate', { args: d.key ? { tabId, device: d.key } : { tabId, off: true } })
      if (r && r.ok === false) {
        note.textContent = r.error || '기기 뷰를 바꾸지 못했습니다'
        wrap.classList.add('bad')
        for (const other of grid.children) other.disabled = false
        return
      }
      lastSig = null
      await tick()
    })
    grid.append(b)
  }
  wrap.append(grid)

  // 눕히기. 기기가 걸려 있어야 뒤집을 것이 있으므로 그때만 누를 수 있다 — 안 그러면 「뭐가
  // 안 되는 거지」가 되고, 그 답이 화면에 없다.
  const flip = el('button', here?.landscape ? 'dev-flip on' : 'dev-flip')
  flip.disabled = !here
  flip.setAttribute('aria-pressed', String(!!here?.landscape))
  flip.title = here ? `${here.label} 을 ${here.landscape ? '세로로 세웁니다' : '가로로 눕힙니다'}` : '기기를 먼저 고르세요'
  flip.append(deviceIcon('rotate'), el('span', 'dev-l', here?.landscape ? '세로로 세우기' : '가로로 눕히기'))
  flip.addEventListener('click', async () => {
    if (!here) return
    flip.disabled = true
    const r = await ask('emulate', { args: { tabId, device: here.key, landscape: !here.landscape } })
    if (r && r.ok === false) {
      note.textContent = r.error || '방향을 바꾸지 못했습니다'
      wrap.classList.add('bad')
      flip.disabled = false
      return
    }
    lastSig = null
    await tick()
  })
  wrap.append(flip)

  // 창보다 큰 화면은 줄여서 넣는다. 그 사실을 안 밝히면 「왜 글자가 작지」가 페이지 탓으로 읽힌다.
  note.textContent = emul?.on
    ? (emul.scale < 0.999
      ? `${emul.width}×${emul.height} · 창에 맞춰 ${Math.round(emul.scale * 100)}% 로 줄여 보여줍니다. CSS 크기는 그대로라 반응형 규칙은 제 값으로 걸립니다.`
      : `${emul.width}×${emul.height} 로 보고 있습니다.`)
    // 폰·태블릿은 UA 까지 바꿔서 다시 불러온다. 안 그러면 이미 받아둔 데스크톱 HTML 이 폰 폭에
    // 남아 세로로 끝없이 늘어난다 — 그게 「폰 크기가 안 맞는다」로 보인다.
    : '누르면 이 탭에만 걸립니다. 창 크기는 그대로예요 — 창은 이 브라우저를 함께 쓰는 모두의 것이라 건드리지 않습니다. 폰·태블릿은 서버가 모바일 화면을 주도록 페이지를 다시 불러옵니다.'
  wrap.append(note)
  return wrap
}

// --- 레이아웃툴 -------------------------------------------------------------

// 편집기를 사람이 직접 붙이는 화면. 여기 없으면 터미널의 에이전트에게 「켜 줘」 하는 길밖에 없는데,
// 화면을 만지는 것은 사람이라 그때마다 부탁하게 된다.
// 켜져 있나만 미리 보여주고, 서버가 있나 없나는 누른 자리에서 알려 준다 — 포트를 훑는 일은
// 1초마다 도는 이 화면에 얹을 만큼 가볍지 않다.
function layoutPane(l) {
  // 붙을 수 없는 화면(확장 관리·웹스토어·새 탭)에서는 누를 수 있는 단추를 아예 내지 않는다.
  // 탭 자체는 그대로 둔다 — 페이지에 따라 탭이 나타났다 사라지면 누르던 자리가 옮겨 다닌다.
  if (!l?.ok) {
    const wrap = el('div', 'lay')
    const head = el('div', 'disp-h')
    head.append(el('span', 'disp-t', '레이아웃툴'))
    wrap.append(head, el('div', 'lay-n', '이 페이지에는 편집기를 넣을 수 없습니다. 크롬 내부 화면(chrome://)·웹스토어·새 탭이 그렇습니다. 고치려는 화면을 연 뒤 다시 열어 주세요.'))
    return wrap
  }

  const wrap = el('div', l.on ? 'lay on' : 'lay')
  const b = el('button', 'sw', l.on ? '끄기' : '켜기')
  b.title = '이 탭을 그대로 만지는 편집기'
  const head = el('div', 'disp-h')
  head.append(el('span', 'disp-t', '레이아웃툴'), b)

  const note = el('div', 'lay-n')
  note.textContent = l.on
    ? (l.src ? '고칠 소스 ' + l.src : '켜짐 — 화면 아래 바에서 편집 켜기')
    : '이 화면을 그대로 만진다'

  b.addEventListener('click', async () => {
    b.disabled = true
    b.textContent = '…'
    const r = await ask('layoutToggle', { tabId: l.tabId })
    if (r && r.ok === false) {
      // 대개 「그 주소를 맡은 서버가 없다」다 — 그 문장에 무엇을 띄우면 되는지가 들어 있다
      wrap.classList.add('bad')
      note.textContent = r.error || '켜지 못했습니다'
      b.disabled = false
      b.textContent = '켜기'
      return
    }
    lastSig = null
    await tick()
  })

  wrap.append(head, note)
  wrap.append(el('div', 'lay-n', '켜면 이 화면의 요소를 피그마처럼 잡아 옮기고 크기·색을 바꿀 수 있습니다. 만진 내역은 터미널의 학생이 browser_layout_edits 로 가져가 소스를 고칩니다 — 편집기의 「코드에 반영」 단추는 누르지 마세요.'))
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

function render(state, layout, emul, tabId) {
  const connected = !!state?.connected
  connEl.className = connected ? 'conn on' : 'conn off'
  connText.textContent = connected ? '브리지 연결됨' : '브리지 없음'

  rootEl.replaceChildren()
  if (!connected) {
    rootEl.appendChild(el('div', 'warn', `브리지(127.0.0.1:${PORT})에 붙어 있지 않습니다. 터미널에서 브라우저 툴을 한 번 쓰면 브리지가 자동으로 뜹니다.`))
  }
  if (view === 'device') {
    rootEl.appendChild(devicePane(emul, tabId))
    return
  }
  if (view === 'layout') {
    rootEl.appendChild(layoutPane(layout))
    return
  }
  rootEl.appendChild(displayBar({ ...DISPLAY_FALLBACK, ...(state?.display || {}) }))
  const sessions = state?.sessions || []
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
function sig(state, layout, emul) {
  return JSON.stringify({
    c: state?.connected,
    d: state?.display,
    l: layout && [layout.ok, layout.on, layout.src],
    e: emul && [emul.on, emul.width, emul.height, emul.scale],
    s: (state?.sessions || []).map((s) => [
      s.key, s.name, s.paneId, s.task, s.busy, s.grouped,
      s.log?.length, s.log?.[0]?.at,
      s.tabs.map((t) => [t.tabId, t.title, t.url, t.busy]),
    ]),
  })
}

// 사람이 지금 보고 있는 화면. 매번 다시 묻는다 — 팝업은 누를 때마다 새로 뜨지만 사이드패널은
// 열어 둔 채로 탭을 갈아타므로, 한 번 읽어 두면 없어진 탭을 계속 가리킨다(2026-09-03 실측:
// 「No tab with id」가 그대로 줄에 떴다).
async function activeTabId() {
  try {
    const [t] = await chrome.tabs.query({ active: true, currentWindow: true })
    return t?.id ?? null
  } catch { return null }
}

async function tick() {
  const my = await activeTabId()
  const [state, layout, emul] = await Promise.all([
    ask('state'),
    my == null ? null : ask('layoutState', { tabId: my }),
    my == null ? null : ask('emulateState', { tabId: my }),
  ])
  const g = sig(state, layout, emul)
  if (g === lastSig) return
  lastSig = g
  render(state, layout, emul, my)
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

// --- 화면 전환 --------------------------------------------------------------

function selectView(next, persist = true) {
  if (!VIEWS.some(([key]) => key === next)) return
  view = next
  for (const [key, button] of navButtons) {
    button.setAttribute('aria-selected', String(key === view))
    button.tabIndex = key === view ? 0 : -1
  }
  if (persist) chrome.storage.local.set({ [VIEW_KEY]: view }).catch(() => {})
  lastSig = null
  tick()
}

for (const [key, label] of VIEWS) {
  const button = el('button', null, label)
  button.setAttribute('role', 'tab')
  button.setAttribute('aria-controls', 'root')
  button.addEventListener('click', () => selectView(key))
  navEl.appendChild(button)
  navButtons.set(key, button)
}

navEl.addEventListener('keydown', (e) => {
  const keys = VIEWS.map(([key]) => key)
  const at = keys.indexOf(view)
  let next
  if (e.key === 'ArrowRight') next = (at + 1) % keys.length
  if (e.key === 'ArrowLeft') next = (at + keys.length - 1) % keys.length
  if (next == null) return
  e.preventDefault()
  selectView(keys[next])
  navButtons.get(keys[next]).focus()
})

selectView(view, false)
// 고른 화면은 남는다 — 팝업은 누를 때마다 새로 뜨는데 매번 첫 탭으로 돌아가면 레이아웃툴을
// 켜고 끄는 데 두 번씩 누르게 된다.
chrome.storage.local.get(VIEW_KEY).then((saved) => selectView(saved[VIEW_KEY] || 'work', false)).catch(() => {})

tick()
// service worker 가 막 깨어난 참이면 세션 복구가 한 박자 늦는다. 화면이 떠 있는 동안만 훑는다.
setInterval(tick, 1000)
