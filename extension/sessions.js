// 이 크롬을 조작하는 신원(터미널의 에이전트)을 브라우저 쪽에 그려주는 층.
// 표시는 전부 페이지 위 오버레이로 한다 — 테두리 글로우(신원 색), 우상단 칩(아바타 + 작업명),
// 조작 지점의 아바타 커서. 크롬 탭 그룹은 글자와 8색뿐이라 이미지를 못 넣어 쓰지 않는다.
// 글로우와 커서는 조작하는 동안만, 칩은 세션이 끝날 때까지 남는다 — 담당 신원은 상시 보여야 한다.
// 이미지 합성은 background 에서 — content script 에서 canvas 를 쓰면 페이지 CSP 에 막힌다.
// 셋 다 사람이 끌 수 있다(display.js) — 우상단 칩이 사이트의 계정 메뉴를 가리는 일이 있어서다.
import { page } from './page.js'
import { getDisplay } from './display.js'

const sessions = new Map() // paneKey -> {identity, task, tabs:Set, busy:Set}
const clientPane = new Map() // clientKey -> paneKey
// ★한 탭에 여럿이 붙을 수 있다. 브라우저는 하나뿐이고 pane 은 여럿이라 같은 페이지를 둘이 보는 일이
// 실제로 생긴다 — 예전엔 나중에 만진 쪽이 앞사람을 조용히 밀어내서, 둘이 붙어 있는데 화면에는 한
// 명만 보이고 밀려난 쪽 탭 목록에서도 그 탭이 사라졌다.
// tabId -> Map<paneKey, 마지막으로 만진 시각>. 순서는 먼저 잡은 순(Map 이 삽입 순을 지킨다).
// 시각까지 담는 이유는 칩에 누구를 남길지 정하기 위해서다 — occupantsOf 를 함께 볼 것.
const tabOwner = new Map()
const iconCache = new Map() // `${slug}:${state}:${size}` -> {imageData, dataUrl}
const offTimers = new Map() // tabId -> timeout
let lastActive = null

const BUSY_COLOR = '#f08c00'
const DONE_COLOR = '#2f9e44'
// 제품명은 manifest 한 곳에만 둔다 — 배포판은 그 파일만 치환하면 화면 전체가 따라온다.
const PRODUCT = chrome.runtime.getManifest().name
// 사람이 "얘가 방금 뭘 했지" 를 되짚을 만큼만. 길게 쌓아 봐야 읽지 않고 저장소만 먹는다.
const ACTIVITY_MAX = 30
// 호출이 끝날 때마다 즉시 끄면 연속 작업 사이에 오버레이가 깜빡인다. 잠깐 물고 있다가 내린다.
// 내려도 사라지는 게 아니라 idle(칩만) 로 낮아진다 — 담당 신원은 세션이 끝날 때까지 계속 보여야 한다.
const OVERLAY_LINGER_MS = 2500

// 이 값이 그대로 chrome.storage 의 pane:* 키가 된다. 브리지가 주는 client 번호(c1,c2…)는
// 브리지 프로세스가 다시 뜰 때마다 1부터 다시 세므로 폴백으로 쓰면 다음 실행이 이전 세션의
// 탭 목록·작업명을 물려받는다. 신원 해석기가 항상 paneId 를 채우므로 폴백은 필요 없다.
function paneKeyOf(identity) {
  return identity?.paneId || identity?.sessionId || null
}

function sessionOf(client) {
  const key = clientPane.get(client)
  return key ? sessions.get(key) : null
}

// 실패를 조용히 삼키면 오버레이가 안 뜨는 이유를 영영 못 본다. status 로 꺼내 볼 수 있게 남긴다.
function note(where, e) {
  self.__ccLastError = { where, at: Date.now(), msg: String((e && e.message) || e) }
}

// MV3 service worker 는 유휴 30초면 종료되고 그때 위 Map 이 통째로 날아간다. 작업명만이라도 남겨
// 되살리면 오버레이 칩이 이름만 남은 상태로 리셋되지 않는다.
// 되살리는 목적이 그 분 단위 공백이라 오래된 레코드는 쓸모가 없다. 오히려 해롭다 — 크롬 탭 id 는
// 브라우저를 껐다 켜면 재사용되므로, 어제 저장된 탭 목록이 오늘 전혀 다른 탭에 칩을 붙인다.
const RESTORE_TTL_MS = 30 * 60 * 1000

async function loadPersisted(key) {
  try {
    const v = await chrome.storage.local.get(`pane:${key}`)
    const rec = v[`pane:${key}`]
    if (!rec) return null
    if (Date.now() - (rec.at || 0) > RESTORE_TTL_MS) {
      chrome.storage.local.remove(`pane:${key}`).catch(() => {})
      return null
    }
    return rec
  } catch {
    return null
  }
}

function persist(key) {
  const s = sessions.get(key)
  if (!s) return
  chrome.storage.local
    .set({ [`pane:${key}`]: { at: Date.now(), task: s.task, tabs: [...s.tabs], made: [...s.made], groups: [...s.groups], log: s.log } })
    .catch(() => {})
}

// 브라우저를 새로 켜면 탭 id 가 전부 갈리므로 남은 레코드는 전부 무효다.
export async function clearPanes() {
  try {
    const all = await chrome.storage.local.get(null)
    const stale = Object.keys(all).filter((k) => k.startsWith('pane:'))
    if (stale.length) await chrome.storage.local.remove(stale)
  } catch { /* 지우지 못해도 TTL 이 받아준다 */ }
}

export async function openSession(client, identity) {
  const key = paneKeyOf(identity)
  if (!key) return note('session', new Error('NO_PANE_KEY: 신원에 paneId 가 없습니다'))
  clientPane.set(client, key)
  const prev = sessions.get(key)
  const saved = prev ? null : await loadPersisted(key)
  sessions.set(key, {
    identity,
    task: prev?.task ?? saved?.task ?? null,
    tabs: prev?.tabs || new Set(saved?.tabs || []),
    busy: prev?.busy || new Set(),
    // 이 세션이 **직접 연** 탭. `tabs` 와 다르다 — 그쪽엔 markBusy 가 claim 한 「조작만 한 남의 탭」도
    // 들어 있어서 내가 치울 몫을 세는 데 못 쓴다. 그룹 멤버십으로 대신하던 것을 여기로 옮겼다:
    // 방 단위로 그룹을 공유하면서 같은 그룹에 남의 탭이 들어오게 됐기 때문이다.
    made: prev?.made || new Set(saved?.made || []),
    // 이 세션이 만든 탭 그룹. 사람이 직접 만든 그룹을 건드리지 않으려면 우리 것을 알고 있어야 한다.
    groups: prev?.groups || new Set(saved?.groups || []),
    log: prev?.log || saved?.log || [],
  })
  for (const tabId of sessions.get(key).tabs) claimTab(tabId, key)
  lastActive = key
  refreshAction()
  // 확장 재로드·service worker 재기동 뒤에도 담당 탭에 칩이 돌아오게 한다. 세션 open 을 붙잡아 두면
  // 뒤따라온 첫 호출이 늦어지므로 기다리지 않는다.
  paintSession(key).catch(() => {})
}

export function closeSession(client) {
  const key = clientPane.get(client)
  clientPane.delete(client)
  if (!key) return
  // 같은 pane 을 쓰는 다른 연결이 남아 있으면 세션을 유지한다(MCP 재연결 중일 수 있다).
  if ([...clientPane.values()].includes(key)) return
  const s = sessions.get(key)
  if (!s) return
  // 탭은 남긴다. 사람이 보던 페이지를 세션이 끝났다고 닫아버리면 안 된다.
  // ⚠️표시를 걷는 것은 **마지막 한 명**이 나갈 때뿐이다. 같은 탭에 아직 다른 pane 이 붙어 있는데
  // 오버레이를 통째로 끄면 남은 사람이 계속 조작하는 페이지가 아무도 안 잡은 것처럼 보인다.
  const left = []
  for (const tabId of s.tabs) {
    const owners = tabOwner.get(tabId)
    owners?.delete(key)
    if (owners?.size) { left.push(tabId); continue }
    tabOwner.delete(tabId)
    clearTimeout(offTimers.get(tabId))
    offTimers.delete(tabId)
    page(tabId, 'overlay', { state: 'off', lingerMs: 0 }).catch(() => {})
  }
  sessions.delete(key)
  // 세션을 지운 뒤에 칠해야 나간 사람이 목록에 남지 않는다.
  for (const tabId of left) paintTab(tabId).catch(() => {})
  if (lastActive === key) lastActive = [...sessions.keys()].pop() || null
  refreshAction()
}

export function setTask(client, task) {
  const s = sessionOf(client)
  if (!s) return { applied: false, reason: 'NO_SESSION: 이 클라이언트에 열린 세션이 없습니다.' }
  s.task = task
  persist(clientPane.get(client))
  // 칩이 떠 있는 탭들의 문구를 즉시 갱신한다(대기 중인 탭도 포함)
  paintSession(clientPane.get(client)).catch(() => {})
  refreshAction()
  return { applied: true, identity: s.identity.name, task }
}

export function identityOf(client) {
  return sessionOf(client)?.identity || null
}

// 이미 다른 세션이 잡고 있어도 뺏지 않고 함께 잡는다. 둘 다 조작할 수 있는 게 사실이므로
// 표시도 그래야 한다 — 뺏는 쪽으로 만들면 화면이 사실과 다른 말을 한다.
function claimTab(tabId, key, at) {
  let owners = tabOwner.get(tabId)
  if (!owners) tabOwner.set(tabId, (owners = new Map()))
  // at 없이 부르는 쪽은 저장소에서 되살린 경우다. 자리만 잡고 시각은 0 으로 둔다 — 언제 만졌는지
  // 모르는 것을 방금 만진 것으로 적으면 칩에 남길 사람을 그 값으로 잘못 고른다.
  if (at != null) owners.set(key, at)
  else if (!owners.has(key)) owners.set(key, 0)
}

export function forgetTab(tabId) {
  const owners = tabOwner.get(tabId)
  tabOwner.delete(tabId)
  clearTimeout(offTimers.get(tabId))
  offTimers.delete(tabId)
  for (const key of owners?.keys() || []) {
    const s = sessions.get(key)
    if (s) { s.tabs.delete(tabId); s.busy.delete(tabId); s.made.delete(tabId) }
  }
}

function anyBusy() {
  for (const s of sessions.values()) if (s.busy.size) return true
  return false
}

// --- 오버레이 -------------------------------------------------------------

// 이 탭에 붙어 있는 사람들. 순서는 먼저 잡은 쪽이 앞이다 — 화면에 뜨는 순서가 호출 순서에 따라
// 뒤바뀌면 사람이 매번 다시 읽어야 한다.
async function occupantsOf(tabId) {
  const owners = tabOwner.get(tabId)
  if (!owners) return []
  const live = [...owners].filter(([key]) => sessions.has(key))
  if (!live.length) return []
  // ★한 번 만진 탭은 세션이 끝날 때까지 주인으로 남는다(대기 중에도 담당이 보이게 하려던 것이다).
  // 그런데 pane 이 반나절씩 살아서, 아침에 스샷 한 장 찍고 딴 일 하는 학생까지 계속 쌓였다 —
  // 화면이 「지금 보고 있다」가 아니라 「이 세션 동안 한 번 만졌다」를 말하게 된 것이다. 그래서
  // 칩에는 **조작 중인 사람들과 가장 최근에 만진 한 명**만 남긴다. 주인 자리에서 빼는 게 아니라
  // 보여주지 않을 뿐이므로 그 학생이 다시 만지면 즉시 돌아오고, 팝업의 탭 목록도 그대로다.
  const last = live.reduce((a, b) => (b[1] > a[1] ? b : a))[0]
  const out = []
  for (const [key] of live) {
    const s = sessions.get(key)
    const busy = s.busy.has(tabId)
    if (!busy && key !== last) continue
    let avatar = null
    try { avatar = (await compose(s.identity, 'plain', 64)).dataUrl } catch (e) { note('overlay-avatar', e) }
    out.push({
      name: s.identity.name,
      color: s.identity.headerColor || '#6BCF7F',
      avatar,
      task: s.task || null,
      busy,
    })
  }
  return out
}

// 탭 하나를 지금 상태에 맞게 다시 칠한다. 세션이 아니라 **탭**을 기준으로 삼는 이유는 여럿이
// 붙어 있을 수 있어서다 — 세션 기준으로 칠하면 나중에 칠한 쪽이 앞사람 표시를 덮어쓴다.
// 한 명이라도 조작 중이면 조작 중으로 본다.
async function paintTab(tabId) {
  const occupants = await occupantsOf(tabId)
  if (!occupants.length) return
  await page(tabId, 'overlay', {
    state: occupants.some((o) => o.busy) ? 'on' : 'idle',
    occupants,
    display: await getDisplay(),
  }).catch((e) => {
    // 크롬 내부 페이지와 그새 닫힌 탭은 정상적인 실패다. 나머지만 남겨 status 로 볼 수 있게 한다.
    if (!/RESTRICTED_PAGE|No tab with id/.test(String(e.message))) note('overlay', e)
  })
}

// 세션이 맡은 탭 전부를 다시 칠한다. 죽은 탭은 여기서 정리한다.
async function paintSession(key) {
  const s = sessions.get(key)
  if (!s) return
  for (const tabId of [...s.tabs]) {
    const alive = await chrome.tabs.get(tabId).catch(() => null)
    if (!alive) { forgetTab(tabId); continue }
    await paintTab(tabId).catch(() => {})
  }
}

// 표시 설정을 바꾸면 이미 칠해둔 탭들은 옛 설정 그대로 남는다. 담당 탭 전부를 다시 칠한다 —
// 설정을 껐는데 보던 탭에서 그대로 보이면 설정이 안 먹은 것으로 읽힌다.
export async function repaintAll() {
  for (const key of [...sessions.keys()]) await paintSession(key).catch(() => {})
}

// 페이지가 새로 뜨면 오버레이가 통째로 날아간다. 담당 세션이 있는 탭이면 다시 그린다.
export async function restoreOverlay(tabId) {
  if (!tabOwner.has(tabId)) return
  await paintTab(tabId).catch(() => {})
}

// 조작 지점에 아바타 커서를 찍는다. 사람이 "지금 어디를 누르는지" 눈으로 따라갈 수 있게.
// ⚠️누가 움직이는 커서인지 함께 보낸다. 한 탭에 둘이 붙어 있을 때 커서를 칩과 같은 이미지로 두면
// 방금 클릭한 사람이 아니라 목록 첫 사람 얼굴이 찍힌다 — 「누가 지금 어디를 누르는지」가 커서의
// 유일한 존재 이유이므로 그게 틀리면 없느니만 못하다.
export async function showCursor(client, tabId, x, y, click = false) {
  // 세션 없는 탭에 커서만 보내면 content script 가 오버레이 호스트를 만들어 둔다 — 화면엔 안 보이지만
  // (호스트가 opacity:0) 방문한 모든 페이지에 빈 div 가 남는다. 형제 함수들과 같은 자리에서 막는다.
  if (!tabId || x == null || y == null || !tabOwner.has(tabId)) return
  const s = sessionOf(client)
  let who = null
  if (s) {
    let avatar = null
    try { avatar = (await compose(s.identity, 'plain', 64)).dataUrl } catch (e) { note('cursor-avatar', e) }
    who = { avatar, color: s.identity.headerColor || '#6BCF7F' }
  }
  await page(tabId, 'cursor', { x, y, click, ...(who || {}) }).catch(() => {})
}

// ★스크린샷에 우리 표시가 함께 찍히지 않게 한 장 동안만 걷는다. 사람에게 보여줄 그림, 하물며
// 공개 경로에 올라가는 그림에 칩("미도리 · 릴리스 컷 촬영")과 조작 테두리가 박혀 통째로 다시
// 찍어야 했다(2026-08-10). 촬영은 에이전트가 알아서 하는 일이니 치우는 것도 에이전트 몫이다.
// tabOwner 가드: 오버레이는 담당이 있는 탭에만 그려져 있으므로 그 밖에서는 왕복이 낭비다.
export async function hideForShot(tabId) {
  if (!tabOwner.has(tabId)) return false
  const r = await page(tabId, 'shot', { hide: true }).catch(() => null)
  return !!r?.hidden
}

export async function showAfterShot(tabId) {
  await page(tabId, 'shot', { hide: false }).catch(() => {})
}

export async function markBusy(client, tabId) {
  const s = sessionOf(client)
  if (!s) return
  const key = clientPane.get(client)
  lastActive = key
  if (tabId) {
    // 조작한 탭도 세션이 맡은 탭이다. 여기서 등록해야 세션이 끝날 때 칩을 걷어내고,
    // 그 전까지는 대기 중에도 누가 이 탭을 잡고 있는지 계속 보인다.
    // 만진 시각을 여기서 남긴다 — 칩에 누구를 보일지가 이 값으로 갈린다(occupantsOf).
    // ⚠️`tabOwner.get(tabId) !== key` 라는 조건이 있었는데 컬렉션과 문자열을 견주는 것이라 늘 참이었다.
    // fresh 가 항상 참이 되어 호출마다 저장소에 썼다. 판정은 탭이 새로 들어왔는지 하나면 된다.
    const fresh = !s.tabs.has(tabId)
    claimTab(tabId, key, Date.now())
    s.tabs.add(tabId)
    if (fresh) persist(key)
    s.busy.add(tabId)
    clearTimeout(offTimers.get(tabId))
    offTimers.delete(tabId)
    await paintTab(tabId)
  }
  refreshAction()
}

export function markDone(client, tabId) {
  const s = sessionOf(client)
  if (!s) return
  if (tabId) {
    s.busy.delete(tabId)
    clearTimeout(offTimers.get(tabId))
    offTimers.set(tabId, setTimeout(() => {
      offTimers.delete(tabId)
      // close_tab 은 타이머를 지우지만 그 뒤 markDone 이 다시 건다. 사라진 탭이면 여기서 멈춘다.
      if (!tabOwner.has(tabId)) return
      // 내가 끝났어도 같은 탭을 다른 pane 이 아직 조작 중일 수 있다. paintTab 이 탭 전체를 보고
      // 정하므로 여기서 idle 을 단정하지 않는다.
      paintTab(tabId).catch(() => {})
      refreshAction()
    }, OVERLAY_LINGER_MS))
  }
  refreshAction()
}

// --- 이미지 합성 (툴바 아이콘 · 오버레이 아바타) ----------------------------

async function loadBitmap(dataUrl) {
  const res = await fetch(dataUrl)
  return await createImageBitmap(await res.blob())
}

function drawBadge(ctx, size, state) {
  if (state === 'plain') return
  const r = size * 0.30
  const cx = size - r - size * 0.04
  const cy = size - r - size * 0.04
  const color = state === 'busy' ? BUSY_COLOR : DONE_COLOR

  ctx.beginPath()
  ctx.arc(cx, cy, r, 0, Math.PI * 2)
  ctx.fillStyle = '#ffffff'
  ctx.fill()
  ctx.lineWidth = Math.max(1, size * 0.03)
  ctx.strokeStyle = color
  ctx.stroke()

  ctx.strokeStyle = color
  ctx.fillStyle = color
  const u = r * 0.52

  if (state === 'busy') {
    // 모래시계: 위아래 삼각형이 허리에서 만난다
    ctx.beginPath()
    ctx.moveTo(cx - u, cy - u)
    ctx.lineTo(cx + u, cy - u)
    ctx.lineTo(cx, cy)
    ctx.closePath()
    ctx.moveTo(cx - u, cy + u)
    ctx.lineTo(cx + u, cy + u)
    ctx.lineTo(cx, cy)
    ctx.closePath()
    ctx.fill()
    ctx.lineWidth = Math.max(1, size * 0.035)
    ctx.beginPath()
    ctx.moveTo(cx - u * 1.15, cy - u)
    ctx.lineTo(cx + u * 1.15, cy - u)
    ctx.moveTo(cx - u * 1.15, cy + u)
    ctx.lineTo(cx + u * 1.15, cy + u)
    ctx.stroke()
  } else if (state === 'done') {
    ctx.lineWidth = Math.max(1.5, size * 0.06)
    ctx.lineCap = 'round'
    ctx.lineJoin = 'round'
    ctx.beginPath()
    ctx.moveTo(cx - u * 0.85, cy)
    ctx.lineTo(cx - u * 0.15, cy + u * 0.7)
    ctx.lineTo(cx + u * 0.9, cy - u * 0.7)
    ctx.stroke()
  }
}

// 배경색 위에서 읽히는 글자색. 밝은 배지 위의 흰 글자는 16px 아이콘에서 통째로 사라진다.
function textOn(hex) {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex || '')
  if (!m) return '#ffffff'
  const n = parseInt(m[1], 16)
  const lum = ((n >> 16) & 255) * 0.299 + ((n >> 8) & 255) * 0.587 + (n & 255) * 0.114
  return lum > 150 ? '#1a1d21' : '#ffffff'
}

async function compose(identity, state, size) {
  const key = `${identity.slug || identity.name}:${state}:${size}`
  const hit = iconCache.get(key)
  if (hit) return hit

  const canvas = new OffscreenCanvas(size, size)
  const ctx = canvas.getContext('2d')
  ctx.clearRect(0, 0, size, size)

  if (identity.profile) {
    const bmp = await loadBitmap(identity.profile)
    ctx.save()
    ctx.beginPath()
    ctx.arc(size / 2, size / 2, size / 2, 0, Math.PI * 2)
    ctx.clip()
    // 원본은 전신 도트라 통째로 줄이면 작은 크기에서 누군지 알 수 없다. 머리 쪽을 잘라 확대한다.
    const crop = Math.round(Math.min(bmp.width, bmp.height) * 0.58)
    const sx = Math.round((bmp.width - crop) / 2)
    const sy = Math.round(bmp.height * 0.04)
    ctx.drawImage(bmp, sx, sy, crop, crop, 0, 0, size, size)
    ctx.restore()
    bmp.close?.()
  } else {
    ctx.beginPath()
    ctx.arc(size / 2, size / 2, size / 2, 0, Math.PI * 2)
    ctx.fillStyle = identity.headerColor || '#868e96'
    ctx.fill()
    // 프사가 없는 신원 — 민색 원은 여럿이 붙었을 때 전부 같은 점이 된다. 첫 글자로 가른다.
    ctx.fillStyle = textOn(identity.headerColor)
    ctx.font = `600 ${Math.round(size * 0.46)}px -apple-system, "Apple SD Gothic Neo", sans-serif`
    ctx.textAlign = 'center'
    ctx.textBaseline = 'middle'
    ctx.fillText([...(identity.name || '?')][0].toUpperCase(), size / 2, size / 2 + size * 0.03)
  }

  if (state === 'offline') {
    // 브리지가 끊긴 상태 — 채도를 죽여 한눈에 구분되게
    const px = ctx.getImageData(0, 0, size, size)
    const d = px.data
    for (let i = 0; i < d.length; i += 4) {
      const g = d[i] * 0.299 + d[i + 1] * 0.587 + d[i + 2] * 0.114
      d[i] = d[i + 1] = d[i + 2] = g
      d[i + 3] = Math.round(d[i + 3] * 0.55)
    }
    ctx.putImageData(px, 0, 0)
  } else {
    drawBadge(ctx, size, state)
  }

  const imageData = ctx.getImageData(0, 0, size, size)
  const blob = await canvas.convertToBlob({ type: 'image/png' })
  const dataUrl = `data:image/png;base64,${b64(await blob.arrayBuffer())}`
  const out = { imageData, dataUrl }
  iconCache.set(key, out)
  return out
}

function b64(buf) {
  const bytes = new Uint8Array(buf)
  let s = ''
  const CHUNK = 0x8000
  for (let i = 0; i < bytes.length; i += CHUNK) {
    s += String.fromCharCode.apply(null, bytes.subarray(i, i + CHUNK))
  }
  return btoa(s)
}

// --- 활동 기록 ------------------------------------------------------------

// 무엇을 했는지 사람이 읽을 한 줄로 쌓는다. 값(입력 내용)은 절대 넣지 않는다 —
// 비밀번호·검색어가 그대로 남는다. 대상은 요소 "이름"까지만.
export function addActivity(client, label, tabId, ok = true) {
  const s = sessionOf(client)
  if (!s || !label) return
  s.log.push({ at: Date.now(), label, tabId: tabId || null, ...(ok ? {} : { failed: true }) })
  if (s.log.length > ACTIVITY_MAX) s.log.splice(0, s.log.length - ACTIVITY_MAX)
  persist(clientPane.get(client))
}

// --- 탭 그룹 --------------------------------------------------------------

// 묶는 대상은 **우리가 직접 연 탭**뿐이다. 사람이 열어둔 탭은 조작만 하고 그룹으로 옮기지 않는다 —
// 열어둔 탭이 제멋대로 재배치되는 것이 claude-in-chrome 이 금지된 이유다. 팝업의 「묶기」 버튼은
// 세션이 잡은 탭까지 한꺼번에 묶는 별개 경로다(groupTabs).

// ★그룹은 학생마다가 아니라 **방마다** 하나다(2026-08-15 지시). 방 = 같은 폴더에서 도는 pane 들.
// 학생마다 만들면 방 하나에 다섯이 붙어 있을 때 그룹이 다섯 개 뜬다(같은 날 관측: 넷이 탭을 하나씩
// 물고 그룹 넷을 차지하고 있었다). 방으로 묶으면 탭바에 방 수만큼만 뜬다.
const ROOM_GROUPS_KEY = 'roomGroups'
let roomGroups = null

// storage.session 인 이유는 agentWindows 와 같다 — worker 가 죽어도 남고, 브라우저를 껐다 켜면
// 저절로 비워진다(그룹 id 는 재사용되므로 남아 있으면 남의 그룹을 우리 것이라 가리킨다).
async function roomGroupMap() {
  if (roomGroups) return roomGroups
  try {
    const v = await chrome.storage.session.get(ROOM_GROUPS_KEY)
    roomGroups = new Map(Object.entries(v[ROOM_GROUPS_KEY] || {}))
  } catch { roomGroups = new Map() }
  return roomGroups
}

function saveRoomGroups() {
  chrome.storage.session.set({ [ROOM_GROUPS_KEY]: Object.fromEntries(roomGroups || []) }).catch(() => {})
}

// 합류 후보. 방을 알면 방 그룹을, 모르면(kasaterm 밖) 예전처럼 자기 그룹을 쓴다.
// ⚠️세션 것만 보면 안 된다 — 먼저 연 학생이 이미 나갔어도 그 그룹에 탭이 남아 있으면 거기 합쳐야
// 방에 그룹이 둘 생기지 않는다. 그래서 세션이 아니라 저장소를 정본으로 둔다.
async function candidateGroups(s) {
  const team = s.identity?.team
  if (!team) return [...s.groups]
  const map = await roomGroupMap()
  return [...(map.get(team) || [])]
}

async function rememberRoomGroup(team, groupId) {
  const map = await roomGroupMap()
  const list = map.get(team) || []
  if (!list.includes(groupId)) { map.set(team, [...list, groupId]); saveRoomGroups() }
}

async function forgetRoomGroup(team, groupId) {
  const map = await roomGroupMap()
  const list = map.get(team) || []
  if (!list.includes(groupId)) return
  const left = list.filter((g) => g !== groupId)
  if (left.length) map.set(team, left)
  else map.delete(team)
  saveRoomGroups()
}

// 그룹 제목. 「폴더 · 이름,이름 +N」 — 2026-08-15 지시로 방 이름만 쓰지 않고 누가 붙어 있는지 함께
// 보인다. 전부 나열하지 않는 이유는 폭이다: 다섯 명을 늘어놓으면 뒤가 통째로 잘려 몇 명인지조차
// 안 남지만, +N 은 잘리기 전에 읽힌다. 판정은 `made` — 그 학생이 **직접 연** 탭이 이 그룹에 있는지다.
async function roomTitleOf(room, groupId) {
  const tabs = await new Promise((r) => chrome.tabs.query({ groupId }, r)).catch(() => [])
  const ids = new Set(tabs.map((t) => t.id))
  const names = []
  for (const s of sessions.values()) {
    const n = s.identity?.name
    if (!n || names.includes(n)) continue
    for (const id of s.made) if (ids.has(id)) { names.push(n); break }
  }
  if (!names.length) return room
  const rest = names.length - 2
  return rest > 0 ? `${room} · ${names.slice(0, 2).join(',')} +${rest}` : `${room} · ${names.join(',')}`
}

// 창을 넘나드는 그룹은 없다. 창마다 따로 묶고, 그 창에 이미 이 방 그룹이 있으면 거기 합친다.
async function groupInWindow(s, windowId, tabIds) {
  const team = s.identity?.team || null
  const room = s.identity?.room || null
  let target = null
  for (const gid of await candidateGroups(s)) {
    const g = await chrome.tabGroups.get(gid).catch(() => null)
    if (!g) { s.groups.delete(gid); if (team) await forgetRoomGroup(team, gid); continue }
    if (g.windowId === windowId) { target = gid; break }
  }
  const groupId = await chrome.tabs.group(
    target ? { tabIds, groupId: target } : { tabIds, createProperties: { windowId } },
  )
  s.groups.add(groupId)
  if (team) await rememberRoomGroup(team, groupId)
  // 탭 그룹은 이미지를 못 받는다(글자 + 8색뿐). 아바타는 페이지 칩에 있으니 여기선 이름과 색만.
  // 색은 방 이름에서 뽑은 것을 쓴다 — 학생 색을 쓰면 먼저 연 사람이 누구냐에 따라 방 색이 바뀐다.
  const title = room ? await roomTitleOf(room, groupId) : (s.identity?.name || PRODUCT)
  const color = (room && s.identity?.roomColor) || s.identity?.groupColor || 'grey'
  await chrome.tabGroups.update(groupId, { title, color }).catch((e) => note('group-title', e))
  return groupId
}

// 그룹에서 탭이 빠지면 제목의 이름 목록도 달라진다. 닫기 경로에서 불러 준다 — 안 하면 이미 나간
// 학생 이름이 탭바에 계속 남는다. 우리 그룹이 아니면 아무것도 하지 않는다.
export async function refreshGroupTitle(groupId) {
  if (!groupId || groupId < 0) return
  let room = null
  for (const s of sessions.values()) {
    if (!s.groups.has(groupId)) continue
    room = s.identity?.room || null
    break
  }
  if (!room) return
  const g = await chrome.tabGroups.get(groupId).catch(() => null)
  if (!g) return
  await chrome.tabGroups.update(groupId, { title: await roomTitleOf(room, groupId) }).catch((e) => note('group-retitle', e))
}

// new_tab/new_window 로 **내가 만든** 탭만 자기 그룹에 넣는다. 여럿이 한 브라우저를 쓸 때
// 누구 탭인지 이름과 색으로 갈리고, 사람이 열어둔 탭 사이에 섞이지 않는다.
// ⚠️`markBusy` 가 claim 하는 「조작한 기존 탭」은 절대 여기 넣지 마라 — 사람이 열어둔 탭이
// 제멋대로 그룹으로 옮겨 다니는 것이 claude-in-chrome 이 금지된 이유다. 내가 만든 것만 내가 묶는다.
export async function groupOwnTab(client, tabId) {
  const s = sessionOf(client)
  if (!s) return null
  const t = await chrome.tabs.get(tabId).catch(() => null)
  if (!t) return null
  // 묶기의 성패와 무관하게 「내가 연 탭」으로 먼저 등록한다 — 그룹이 실패해도 치울 몫은 내 것이고,
  // 제목에 이름이 뜨려면 묶기 전에 들어 있어야 한다(제목은 이 집합을 보고 만든다).
  s.made.add(tabId)
  persist(clientPane.get(client))
  try {
    return await groupInWindow(s, t.windowId, [tabId])
  } catch (e) {
    // 묶기가 실패해도 탭은 이미 열렸다. 탭 생성을 실패로 만들지 않는다.
    note('group-own', e)
    return null
  }
}

// 내가 **직접 연** 탭이 몇 개 남았나. close_tab 이 이 값을 돌려주는 이유: 다 쓴 탭은 그때그때 닫되
// 작업이 끝나도 결과를 보여줄 페이지 하나는 남아야 하는데, 몇 개 남았는지 모르면 마지막 하나까지 닫는다.
// ⚠️그룹 멤버십으로 세면 안 된다 — 방 단위로 그룹을 공유하면서 같은 그룹에 **남의 탭**이 들어왔다.
// 그걸 세면 내 탭을 다 닫고도 remaining 이 남아 있어 「하나는 남겼다」고 잘못 읽는다.
export async function ownTabCount(client) {
  const s = sessionOf(client)
  if (!s) return 0
  let n = 0
  for (const tabId of [...s.made]) {
    const t = await chrome.tabs.get(tabId).catch(() => null)
    if (!t) { forgetTab(tabId); s.made.delete(tabId); continue }
    n++
  }
  return n
}

// --- 에이전트 창 ----------------------------------------------------------

// 우리가 new_window 로 **만든** 창. ⚠️「우리 그룹이 있는 창」으로 판정하면 안 된다 — 사람 창에도
// 우리가 백그라운드로 연 탭과 그 그룹이 섞이므로(실측: 사람 창 하나에 우리 그룹이 하나 있었다)
// 사람 창을 에이전트 창으로 읽고, 그러면 new_window 가 사람이 보던 탭을 갈아치운다.
// storage.session 을 쓰는 이유 둘: 워커가 죽어도 남고, 브라우저를 껐다 켜면 저절로 비워진다
// (창 id 는 재사용되므로 남아 있으면 남의 창을 우리 창이라고 가리킨다).
const AGENT_WINDOWS_KEY = 'agentWindows'
let agentWindows = null

async function agentWindowSet() {
  if (agentWindows) return agentWindows
  try {
    const v = await chrome.storage.session.get(AGENT_WINDOWS_KEY)
    agentWindows = new Set(v[AGENT_WINDOWS_KEY] || [])
  } catch { agentWindows = new Set() }
  return agentWindows
}

function saveAgentWindows() {
  chrome.storage.session.set({ [AGENT_WINDOWS_KEY]: [...(agentWindows || [])] }).catch(() => {})
}

export async function rememberAgentWindow(windowId) {
  const set = await agentWindowSet()
  set.add(Number(windowId))
  saveAgentWindows()
}

export async function forgetAgentWindow(windowId) {
  const set = await agentWindowSet()
  if (set.delete(Number(windowId))) saveAgentWindows()
}

// 지금 쓸 수 있는 에이전트 창. ★세션별이 아니라 브라우저에 하나다 — 예전엔 세션마다 자기 창을
// 찾아서, pane 넷이 각자 확인을 돌리면 사람 화면에 창이 넷 떴다(실측). 창은 화면을 통째로 가리는
// 공유 자원이라 나눠 쓰는 게 맞고, 탭은 여전히 세션별 그룹으로 갈리니 누구 것인지는 그대로 보인다.
export async function agentWindowOf() {
  const set = await agentWindowSet()
  let found = null
  for (const id of [...set]) {
    const win = await chrome.windows.get(id).catch(() => null)
    if (win) { if (found == null) found = id; continue }
    set.delete(id) // 사람이 닫은 창
  }
  saveAgentWindows()
  return found
}

// 에이전트 창을 그룹으로 되짚는다. rememberAgentWindow 기록은 이 버전부터 쌓이므로 그 전에 열린
// 창은 거기 없다 — 이미 흩어져 있는 창을 정리하려면 다른 잣대가 필요하다.
// 판정: 창의 탭이 **전부** 그룹에 들어 있고 그중 하나라도 우리 것이면 에이전트 창. 사람 창에는
// 그룹에 안 든 탭이 반드시 섞인다(주소창으로 연 탭은 아무 그룹에도 안 든다). 세션이 끝난 학생의
// 그룹은 우리 것으로 안 잡히지만, 그 창에 사람 탭이 없다는 사실이 대신 받아 준다.
export async function agentWindowsByGroups() {
  const ours = new Set()
  for (const s of sessions.values()) for (const g of s.groups) ours.add(g)
  const wins = await chrome.windows.getAll({ populate: true }).catch(() => [])
  const out = []
  for (const w of wins) {
    const tabs = w.tabs || []
    if (!tabs.length) continue
    if (tabs.some((t) => t.groupId == null || t.groupId === -1)) continue
    if (!tabs.some((t) => ours.has(t.groupId))) continue
    out.push({ windowId: w.id, tabs: tabs.length })
  }
  return out
}

// 이 탭들에 붙어 있는 **다른** 세션의 이름. 창을 나눠 쓰게 되면서 생긴 위험을 막는다 — 내 확인이
// 끝났다고 창을 닫으면 같은 창에서 일하던 다른 학생의 탭까지 함께 사라진다.
export function otherOwners(client, tabIds) {
  const mine = clientPane.get(client)
  const names = new Set()
  for (const tabId of tabIds) {
    for (const key of tabOwner.get(tabId)?.keys() || []) {
      if (key === mine) continue
      const s = sessions.get(key)
      if (s) names.add(s.identity?.name || key)
    }
  }
  return [...names]
}

// 탭 그룹 현황. ⚠️**닫힌 그룹은 여기 안 나온다** — 크롬이 저장해 탭바에 이름만 남긴 그룹은
// tabGroups.query 가 통째로 빼고 준다(2026-08-10 실측: 그룹을 만들고 마지막 탭을 닫으니 목록에서
// 사라졌고, 그 groupId 로 탭을 넣어 되살리려 하면 `No group with id` 로 거부됐다). 그러니 아래
// `empty` 가 0 이어도 탭바가 깨끗하다는 뜻이 아니다 — 확장이 볼 수 있는 범위에 없다는 뜻뿐이다.
export async function listGroups() {
  const ours = new Set()
  for (const s of sessions.values()) for (const g of s.groups) ours.add(g)
  const groups = await chrome.tabGroups.query({}).catch(() => [])
  const out = []
  for (const g of groups) {
    const tabs = await new Promise((r) => chrome.tabs.query({ groupId: g.id }, r)).catch(() => [])
    out.push({ groupId: g.id, title: g.title, color: g.color, windowId: g.windowId, tabs: tabs.length, ours: ours.has(g.id) })
  }
  return {
    groups: out,
    empty: out.filter((g) => g.tabs === 0).length,
    // 확장 API 에 그룹 삭제가 생겼는지 — 없으면 껍데기는 사람이 우클릭으로 지우는 수밖에 없다.
    canRemove: typeof chrome.tabGroups?.remove === 'function',
    // 「안 보이는 것은 없는 것」이 아니라는 사실을 응답에 실어 둔다. 이 한 줄이 없으면 empty:0 을
    // 보고 「껍데기 없음」으로 보고하게 된다.
    note: '닫힌(저장된) 그룹은 확장 API 로 열거나 지울 수 없고 이 목록에도 안 잡힙니다. 탭바에 남은 것은 사람이 우클릭으로 지워야 합니다.',
  }
}

// ★크롬이 탭 그룹을 저장해 두면, 그룹에 든 채로 마지막 탭이 닫혔을 때 그룹이 사라지는 게 아니라
// **이름만 남은 껍데기가 탭바에 계속 붙어 있다**(2026-08-05: 학생 넷이 만든 그룹 10개가 그렇게 쌓여
// 탭바를 채웠다). ⚠️저장은 크롬 설정·버전에 달렸다 — 2026-08-10 같은 실험에서는 그룹이 그냥 소멸해
// 껍데기가 안 남았다. 그래서 "이 크롬에서는 안 남는다"를 믿고 이 예방을 걷어내면 안 된다.
// 사후 정리는 어느 쪽이든 불가능하다: 확장에 그룹 삭제 API 가 없고(canRemove), 닫힌 그룹은
// tabGroups.query 에도 안 잡히며, groupId 로 탭을 넣어 되살리는 것조차 거부된다(실측). 즉 닫기 전에
// 빼내는 것이 정말로 유일한 방법이다. 우리가 만든 그룹만 건드린다 — 사람 그룹의 탭을 빼면 그 사람이
// 저장해 둔 그룹이 대신 사라진다.
export async function ungroupBeforeClose(tabIds) {
  const ours = new Set()
  for (const s of sessions.values()) for (const g of s.groups) ours.add(g)
  // 방 그룹은 만든 학생이 나가도 남는다. 세션 것만 보면 그 그룹의 탭을 닫을 때 빼내기를 건너뛰어
  // 껍데기가 생긴다 — 방을 도입하면서 새로 생긴 구멍이라 저장소도 함께 본다.
  for (const list of (await roomGroupMap()).values()) for (const g of list) ours.add(g)
  if (!ours.size) return
  const targets = []
  for (const id of tabIds) {
    const t = await chrome.tabs.get(id).catch(() => null)
    if (t && ours.has(t.groupId)) targets.push(id)
  }
  if (targets.length) await chrome.tabs.ungroup(targets).catch((e) => note('ungroup-close', e))
}

export async function groupTabs(key) {
  const s = sessions.get(key)
  if (!s) return { ok: false, error: '세션이 없습니다.' }
  const byWindow = new Map()
  for (const tabId of [...s.tabs]) {
    const t = await chrome.tabs.get(tabId).catch(() => null)
    if (!t) { forgetTab(tabId); continue }
    if (!byWindow.has(t.windowId)) byWindow.set(t.windowId, [])
    byWindow.get(t.windowId).push(tabId)
  }
  if (!byWindow.size) return { ok: false, error: '묶을 탭이 없습니다.' }
  try {
    for (const [windowId, tabIds] of byWindow) await groupInWindow(s, windowId, tabIds)
  } catch (e) {
    note('group', e)
    return { ok: false, error: String(e?.message || e) }
  }
  persist(key)
  return { ok: true, groups: [...s.groups] }
}

export async function ungroupTabs(key) {
  const s = sessions.get(key)
  if (!s) return { ok: false, error: '세션이 없습니다.' }
  const targets = []
  for (const tabId of [...s.tabs]) {
    const t = await chrome.tabs.get(tabId).catch(() => null)
    if (!t) { forgetTab(tabId); continue }
    // 사람이 따로 묶어둔 그룹에 있는 탭이라면 그건 우리 것이 아니므로 그대로 둔다.
    if (s.groups.has(t.groupId)) targets.push(tabId)
  }
  if (targets.length) await chrome.tabs.ungroup(targets).catch((e) => note('ungroup', e))
  s.groups.clear()
  persist(key)
  return { ok: true, ungrouped: targets.length }
}

// --- 팝업용 스냅샷 --------------------------------------------------------

// 확장 아이콘 팝업이 "누가 어느 탭을 잡고 있는지" 를 그리는 데 쓴다. 프사는 툴바 아이콘과 같은
// 합성물을 재활용하므로 캐시에 걸린다.
export async function snapshot(connected) {
  const out = []
  for (const [key, s] of sessions) {
    let avatar = null
    try { avatar = (await compose(s.identity, 'plain', 64)).dataUrl } catch (e) { note('snapshot-avatar', e) }
    const tabs = []
    for (const tabId of [...s.tabs]) {
      const t = await chrome.tabs.get(tabId).catch(() => null)
      if (!t) { forgetTab(tabId); continue }
      tabs.push({
        tabId, windowId: t.windowId,
        title: t.title || t.url || '(제목 없음)',
        url: t.url || '',
        favIconUrl: t.favIconUrl || null,
        busy: s.busy.has(tabId),
        active: !!t.active,
        grouped: s.groups.has(t.groupId),
      })
    }
    out.push({
      key,
      name: s.identity?.name || '이름 없음',
      paneId: s.identity?.paneId || null,
      color: s.identity?.headerColor || '#6BCF7F',
      avatar,
      task: s.task || null,
      busy: s.busy.size > 0,
      // 새로 잡은 탭이 그룹 밖에 있으면 다시 "묶기" 로 돌아간다
      grouped: tabs.length > 0 && tabs.every((t) => t.grouped),
      tabs,
      // 최신이 위로
      log: [...s.log].reverse(),
    })
  }
  // 표시 설정도 함께 싣는다 — 팝업이 따로 물으면 폴링이 두 번이 된다.
  return { connected, display: await getDisplay(), sessions: out, lastError: self.__ccLastError || null }
}

// --- 툴바 아이콘 ----------------------------------------------------------

let actionBusy = false
export async function refreshAction(connected = true) {
  const s = lastActive ? sessions.get(lastActive) : null
  if (!s) {
    // 세션이 없으면 manifest 의 기본 아이콘을 그대로 둔다
    chrome.action.setTitle({ title: `${PRODUCT} — ${connected ? '대기 중' : '브리지 없음'}` })
    chrome.action.setBadgeText({ text: connected ? '' : 'off' })
    return
  }
  const state = !connected ? 'offline' : (anyBusy() ? 'busy' : 'done')
  if (actionBusy) return
  actionBusy = true
  try {
    const { imageData } = await compose(s.identity, state, 64)
    await chrome.action.setIcon({ imageData: { 64: imageData } })
    const who = `${s.identity.name}${s.identity.paneId ? ` (${s.identity.paneId})` : ''}`
    const what = s.task ? ` — ${s.task}` : ''
    chrome.action.setTitle({ title: `${PRODUCT} — ${who}${what}${connected ? '' : ' · 브리지 없음'}` })
    chrome.action.setBadgeText({ text: '' })
  } catch (e) {
    note('refreshAction', e)
  } finally {
    actionBusy = false
  }
}
