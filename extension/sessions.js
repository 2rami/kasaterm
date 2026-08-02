// 이 크롬을 조작하는 신원(터미널의 에이전트)을 브라우저 쪽에 그려주는 층.
// 표시는 전부 페이지 위 오버레이로 한다 — 테두리 글로우(신원 색), 우상단 칩(아바타 + 작업명),
// 조작 지점의 아바타 커서. 크롬 탭 그룹은 글자와 8색뿐이라 이미지를 못 넣어 쓰지 않는다.
// 글로우와 커서는 조작하는 동안만, 칩은 세션이 끝날 때까지 남는다 — 담당 신원은 상시 보여야 한다.
// 이미지 합성은 background 에서 — content script 에서 canvas 를 쓰면 페이지 CSP 에 막힌다.
import { page } from './page.js'

const sessions = new Map() // paneKey -> {identity, task, tabs:Set, busy:Set}
const clientPane = new Map() // clientKey -> paneKey
const tabOwner = new Map() // tabId -> paneKey
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
    .set({ [`pane:${key}`]: { at: Date.now(), task: s.task, tabs: [...s.tabs], groups: [...s.groups], log: s.log } })
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
  for (const tabId of s.tabs) {
    tabOwner.delete(tabId)
    clearTimeout(offTimers.get(tabId))
    offTimers.delete(tabId)
    page(tabId, 'overlay', { state: 'off', lingerMs: 0 }).catch(() => {})
  }
  sessions.delete(key)
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

// 한 탭의 주인은 하나뿐이다. 다른 세션이 이어받으면 이전 주인 목록에서 뺀다 —
// tabOwner 만 덮으면 팝업에 같은 탭이 두 세션 밑에 동시에 남는다.
function claimTab(tabId, key) {
  const prevKey = tabOwner.get(tabId)
  if (prevKey === key) return
  const prev = prevKey && sessions.get(prevKey)
  if (prev) {
    prev.tabs.delete(tabId)
    prev.busy.delete(tabId)
    persist(prevKey)
  }
  tabOwner.set(tabId, key)
}

export function forgetTab(tabId) {
  const key = tabOwner.get(tabId)
  tabOwner.delete(tabId)
  clearTimeout(offTimers.get(tabId))
  offTimers.delete(tabId)
  const s = key && sessions.get(key)
  if (s) { s.tabs.delete(tabId); s.busy.delete(tabId) }
}

function anyBusy() {
  for (const s of sessions.values()) if (s.busy.size) return true
  return false
}

// --- 오버레이 -------------------------------------------------------------

// mode: 'on'(조작 중) · 'idle'(대기 — 칩만) · 'off'(세션 종료)
async function setOverlay(tabId, s, mode) {
  if (mode === 'off') {
    await page(tabId, 'overlay', { state: 'off' }).catch(() => {})
    return
  }
  const { dataUrl } = await compose(s.identity, 'plain', 64)
  await page(tabId, 'overlay', {
    state: mode,
    color: s.identity.headerColor || '#6BCF7F',
    avatar: dataUrl,
    name: s.identity.name,
    task: s.task,
  }).catch((e) => {
    // 크롬 내부 페이지와 그새 닫힌 탭은 정상적인 실패다. 나머지만 남겨 status 로 볼 수 있게 한다.
    if (!/RESTRICTED_PAGE|No tab with id/.test(String(e.message))) note('overlay', e)
  })
}

// 담당 탭 하나를 지금 상태에 맞게 다시 칠한다. 조작 중이면 글로우까지, 아니면 칩만.
async function paintTab(tabId, s) {
  await setOverlay(tabId, s, s.busy.has(tabId) ? 'on' : 'idle')
}

// 세션이 맡은 탭 전부를 다시 칠한다. 죽은 탭은 여기서 정리한다.
async function paintSession(key) {
  const s = sessions.get(key)
  if (!s) return
  for (const tabId of [...s.tabs]) {
    const alive = await chrome.tabs.get(tabId).catch(() => null)
    if (!alive) { forgetTab(tabId); continue }
    await paintTab(tabId, s).catch(() => {})
  }
}

// 페이지가 새로 뜨면 오버레이가 통째로 날아간다. 담당 세션이 있는 탭이면 다시 그린다.
export async function restoreOverlay(tabId) {
  const key = tabOwner.get(tabId)
  const s = key && sessions.get(key)
  if (!s) return
  await paintTab(tabId, s).catch(() => {})
}

// 조작 지점에 아바타 커서를 찍는다. 사람이 "지금 어디를 누르는지" 눈으로 따라갈 수 있게.
export async function showCursor(tabId, x, y, click = false) {
  // 세션 없는 탭에 커서만 보내면 content script 가 오버레이 호스트를 만들어 둔다 — 화면엔 안 보이지만
  // (호스트가 opacity:0) 방문한 모든 페이지에 빈 div 가 남는다. 형제 함수들과 같은 자리에서 막는다.
  if (!tabId || x == null || y == null || !tabOwner.has(tabId)) return
  await page(tabId, 'cursor', { x, y, click }).catch(() => {})
}

export async function markBusy(client, tabId) {
  const s = sessionOf(client)
  if (!s) return
  const key = clientPane.get(client)
  lastActive = key
  if (tabId) {
    // 조작한 탭도 세션이 맡은 탭이다. 여기서 등록해야 세션이 끝날 때 칩을 걷어내고,
    // 그 전까지는 대기 중에도 누가 이 탭을 잡고 있는지 계속 보인다.
    const fresh = !s.tabs.has(tabId) || tabOwner.get(tabId) !== key
    claimTab(tabId, key)
    s.tabs.add(tabId)
    if (fresh) persist(key)
    s.busy.add(tabId)
    clearTimeout(offTimers.get(tabId))
    offTimers.delete(tabId)
    await setOverlay(tabId, s, 'on')
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
      setOverlay(tabId, s, 'idle').catch(() => {})
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

// 자동으로는 절대 묶지 않는다 — 사람이 열어둔 탭이 제멋대로 옮겨 다니면 탭바가 어지럽다.
// 팝업에서 명시적으로 눌렀을 때만 이 두 함수가 돈다.

// 창을 넘나드는 그룹은 없다. 창마다 따로 묶고, 그 창에 이미 우리 그룹이 있으면 거기 합친다.
async function groupInWindow(s, windowId, tabIds) {
  let target = null
  for (const gid of [...s.groups]) {
    const g = await chrome.tabGroups.get(gid).catch(() => null)
    if (!g) { s.groups.delete(gid); continue }
    if (g.windowId === windowId) { target = gid; break }
  }
  const groupId = await chrome.tabs.group(
    target ? { tabIds, groupId: target } : { tabIds, createProperties: { windowId } },
  )
  s.groups.add(groupId)
  // 탭 그룹은 이미지를 못 받는다(글자 + 8색뿐). 아바타는 페이지 칩에 있으니 여기선 이름과 색만.
  await chrome.tabGroups
    .update(groupId, { title: s.identity?.name || PRODUCT, color: s.identity?.groupColor || 'grey' })
    .catch((e) => note('group-title', e))
  return groupId
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
  return { connected, sessions: out, lastError: self.__ccLastError || null }
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
