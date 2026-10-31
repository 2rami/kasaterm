// 로그인 칸 채우기. 비밀번호는 MCP 프로세스가 카사텀 secret(키체인 + 폰 승인)에서 받아 브리지를 거쳐 여기로만 온다 —
// 저장하지 않고, 결과·오류·활동 기록에는 칸 종류와 host 만 남긴다.
import { withTimeout } from './wait.js'
import { remember } from './conceal.js'

const PAGE_TIMEOUT_MS = 10000

// host 는 부르는 쪽이 아니라 탭의 실제 주소에서 읽는다 — 이 값이 곧 키체인 서비스 이름(`web:<host>`)이라
// 다른 사이트의 비밀번호가 엉뚱한 페이지로 가지 않는다. 평문 http 는 같은 이름을 가로채기 쉬워 loopback 만 받는다.
export function loginHostOf(url) {
  let u
  try { u = new URL(url) } catch { throw new Error('LOGIN_BAD_PAGE: 이 탭은 웹 페이지가 아닙니다.') }
  const loopback = u.hostname === 'localhost' || u.hostname === '[::1]' || /^127\.\d+\.\d+\.\d+$/.test(u.hostname)
  if (u.protocol === 'https:' || (u.protocol === 'http:' && loopback)) return u.host
  throw new Error(`LOGIN_INSECURE_PAGE: ${u.protocol}//${u.host} 에는 채우지 않습니다 — https(또는 localhost)만 받습니다.`)
}

// 페이지 안(isolated world)에서 돈다. executeScript 는 함수 원문만 옮기므로 바깥 이름을 쓰지 않는다.
// 확인과 채우기가 같은 동기 실행 안에 있어, 확인한 문서와 값을 넣는 문서가 갈릴 틈이 없다.
function loginPage(mode, expectedHost, username, password) {
  const here = location.host
  if (expectedHost && here !== expectedHost) return { ok: false, code: 'LOGIN_HOST_CHANGED', host: here }

  const inputs = []
  const walk = (root) => {
    for (const el of root.querySelectorAll('input')) inputs.push(el)
    for (const el of root.querySelectorAll('*')) if (el.shadowRoot) walk(el.shadowRoot)
  }
  walk(document)
  const usable = inputs.filter((el) => {
    if (el.disabled || el.readOnly) return false
    const box = el.getBoundingClientRect()
    return box.width > 0 && box.height > 0 && getComputedStyle(el).visibility !== 'hidden'
  })
  const tokens = (el) => (el.getAttribute('autocomplete') || '').toLowerCase().split(/\s+/)

  // 가입·비밀번호 바꾸기 폼(new-password 만 있는 곳)에는 넣지 않는다.
  const passwords = usable.filter((el) => el.type === 'password')
  const pass = passwords.find((el) => tokens(el).includes('current-password'))
    || passwords.find((el) => !tokens(el).includes('new-password')) || null

  const texty = usable.filter((el) => ['text', 'email', 'tel'].includes(el.type))
  let user = texty.find((el) => tokens(el).some((t) => t === 'username' || t === 'email')) || null
  if (!user && pass) {
    const before = texty.filter((el) => (!pass.form || el.form === pass.form)
      && (el.compareDocumentPosition(pass) & Node.DOCUMENT_POSITION_FOLLOWING))
    user = before[before.length - 1] || null
  }
  // 아이디를 먼저 받고 다음 화면에서 비밀번호를 받는 사이트(두 단계 로그인)의 첫 화면.
  if (!user) {
    user = texty.find((el) => el.type === 'email')
      || texty.find((el) => /user|email|login|account|identifier/i.test(`${el.name} ${el.id}`)) || null
  }
  if (!user && !pass) return { ok: false, code: 'LOGIN_NO_FORM', host: here }
  if (mode === 'probe') return { ok: true, host: here, fields: { username: !!user, password: !!pass } }

  // 네이티브 setter 로 넣어야 React·Vue 의 값 추적이 바뀐 것을 본다.
  const put = (el, value) => {
    el.focus()
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(el, value)
    el.dispatchEvent(new InputEvent('input', { bubbles: true, composed: true, inputType: 'insertReplacementText' }))
    el.dispatchEvent(new Event('change', { bubbles: true }))
    return el.value === value
  }
  const filled = []
  const missed = []
  if (user && typeof username === 'string') (put(user, username) ? filled : missed).push('username')
  if (pass && typeof password === 'string') (put(pass, password) ? filled : missed).push('password')
  return { ok: true, host: here, filled, missed }
}

async function inTopFrame(tabId, mode, host, username = null, password = null) {
  const [run] = await withTimeout(
    chrome.scripting.executeScript({ target: { tabId, frameIds: [0] }, func: loginPage, args: [mode, host, username, password] }),
    PAGE_TIMEOUT_MS,
    `PAGE_TIMEOUT: 로그인 칸 확인이 ${PAGE_TIMEOUT_MS}ms 안에 끝나지 않았습니다 — 페이지 메인 스레드가 막혀 있습니다.`,
  )
  const r = run?.result
  if (!r) throw new Error('LOGIN_PAGE_FAILED: 페이지에서 로그인 칸을 확인하지 못했습니다.')
  if (r.ok) return r
  if (r.code === 'LOGIN_HOST_CHANGED') throw new Error(`LOGIN_HOST_CHANGED: 탭 주소가 ${host} 에서 ${r.host} 로 바뀌어 넣지 않았습니다.`)
  throw new Error(`LOGIN_NO_FORM: ${r.host} 에서 사용자명·비밀번호 칸을 찾지 못했습니다(보이는 칸만 봅니다).`)
}

export async function loginTarget(tabId) {
  const tab = await chrome.tabs.get(tabId)
  const r = await inTopFrame(tabId, 'probe', loginHostOf(tab.url))
  return { host: r.host, fields: r.fields }
}

export async function loginFill(tabId, { host, username, password }) {
  if (typeof host !== 'string' || !host) throw new Error('LOGIN_BAD_ARGS: host 가 없습니다.')
  // 값을 받은 순간부터 가린다 — 채우다 실패해도 그 값이 다른 도구의 결과로 나오지 않게.
  if (typeof password === 'string') remember(password)
  const tab = await chrome.tabs.get(tabId)
  const now = loginHostOf(tab.url)
  if (now !== host) throw new Error(`LOGIN_HOST_CHANGED: 탭 주소가 ${host} 에서 ${now} 로 바뀌어 넣지 않았습니다.`)
  const r = await inTopFrame(tabId, 'fill', host, username ?? null, password ?? null)
  return { host: r.host, filled: r.filled, missed: r.missed }
}
