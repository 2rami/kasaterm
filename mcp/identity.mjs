// 이 크롬을 조작하는 주체가 누구인지 해석한다. **절대 null 을 돌려주지 않는다** —
// 예전엔 호스트 터미널의 캐릭터가 없으면 null 이었고, 그 하나가 브리지의 `if (identity)` 가드에 걸려
// 오버레이·활동로그·툴바 아이콘이 통째로 죽었다(툴은 멀쩡히 도는데 UI 만 고장난 것처럼 보였다).
import { userInfo } from 'node:os'

// 호스트 터미널이 캐릭터(색·프로필 이미지)를 주는 경우에만 쓰는 어댑터. 없으면 없는 대로 간다.
let host = null
try { host = await import('./kasaterm.mjs') } catch { /* 배포판엔 이 파일이 없다 */ }

// 크롬 탭 그룹은 이 8색만 받는다. 호스트가 주는 색을 여기에 맞춘다.
const CHROME_GROUP_COLORS = new Set(['grey', 'blue', 'red', 'yellow', 'green', 'pink', 'purple', 'cyan'])
const COLOR_ALIAS = {
  orange: 'yellow', magenta: 'pink', violet: 'purple', teal: 'cyan',
  gray: 'grey', white: 'grey', black: 'grey',
}

// 신원에 색이 없을 때 이름에서 뽑는 팔레트. 크롬 그룹 8색과 오버레이용 hex 를 짝지어 둔다.
const PALETTE = [
  ['blue', '#4C8DFF'], ['green', '#3FB950'], ['purple', '#A371F7'], ['cyan', '#39C5CF'],
  ['pink', '#F778BA'], ['yellow', '#D9A441'], ['red', '#F26D6D'], ['grey', '#8B949E'],
]

// 같은 이름이면 어느 기기에서든 같은 색이어야 화면공유·스크린샷에서 사람이 알아본다.
function colorOf(name) {
  let h = 5381
  for (const ch of name) h = ((h * 33) ^ ch.codePointAt(0)) >>> 0
  return PALETTE[h % PALETTE.length]
}

// 프로세스마다 다른 4자. 창 id 를 주는 터미널이 없을 때 세션 키로 쓴다 — 키가 겹치면
// 저장소에 남은 이전 세션의 탭·작업명이 되살아난다.
const TAG = `#${(((process.pid * 0x9e3779b1) ^ Date.now()) >>> 0).toString(36).slice(-4)}`

function paneOf() {
  const e = process.env
  const iterm = (e.ITERM_SESSION_ID || '').split(':')[0]
  return e.CHROMECLAUDE_PANE || e.KASATERM_PANE_ID || e.WEZTERM_PANE || e.TMUX_PANE
    || iterm || e.TERM_SESSION_ID || TAG
}

export function resolveIdentity() {
  const character = process.env.KASATERM_CHARACTER
  const name = process.env.CHROMECLAUDE_NAME || character || userInfo().username || 'claude'

  // 호스트가 배정한 캐릭터일 때만 그쪽 자산을 찾는다. 이름을 직접 지정했으면 그 이름이 이긴다.
  const entry = (host && character && name === character && host.lookup(name)) || {}

  const raw = String(entry.claudeColor || '').toLowerCase()
  const mapped = CHROME_GROUP_COLORS.has(raw) ? raw : COLOR_ALIAS[raw]
  const [fallbackGroup, fallbackHex] = colorOf(name)

  return {
    name,
    slug: entry.slug || null,
    paneId: paneOf(),
    groupColor: mapped || fallbackGroup,
    headerColor: entry.headerColor || fallbackHex,
    profile: entry.profile || null,
    sessionId: process.env.KASATERM_SESSION_ID || null,
  }
}
