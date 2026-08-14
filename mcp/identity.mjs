// 이 크롬을 조작하는 주체가 누구인지 해석한다. **절대 null 을 돌려주지 않는다** —
// 예전엔 호스트 터미널의 캐릭터가 없으면 null 이었고, 그 하나가 브리지의 `if (identity)` 가드에 걸려
// 오버레이·활동로그·툴바 아이콘이 통째로 죽었다(툴은 멀쩡히 도는데 UI 만 고장난 것처럼 보였다).
import { userInfo } from 'node:os'
import { basename } from 'node:path'

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
  return e.KASACHROME_PANE || e.KASATERM_PANE_ID || e.WEZTERM_PANE || e.TMUX_PANE
    || iterm || e.TERM_SESSION_ID || TAG
}

export function resolveIdentity() {
  const character = process.env.KASATERM_CHARACTER
  const name = process.env.KASACHROME_NAME || character || userInfo().username || 'claude'

  // 호스트가 배정한 캐릭터일 때만 그쪽 자산을 찾는다. 이름을 직접 지정했으면 그 이름이 이긴다.
  const entry = (host && character && name === character && host.lookup(name)) || {}

  const raw = String(entry.claudeColor || '').toLowerCase()
  const mapped = CHROME_GROUP_COLORS.has(raw) ? raw : COLOR_ALIAS[raw]
  const [fallbackGroup, fallbackHex] = colorOf(name)

  // 「방」 = 같은 폴더에서 도는 pane 들. 호스트 shim 이 `cwd` 로 서버에 물어 받은 값이라 같은 폴더면
  // 같고 다른 폴더면 다르다(2026-08-15 실측). 탭 그룹을 이 단위로 묶으면 학생 수가 아니라 방 수만큼만
  // 탭바에 뜬다. 이 값이 없으면(kasaterm 밖에서 띄운 경우) 예전처럼 학생마다 자기 그룹을 쓴다.
  const team = process.env.KASATERM_TEAM || null
  // 표시용 이름은 팀 문자열이 아니라 폴더 이름이다 — 팀 값은 `kt-Users-kasa-…-4566` 처럼 생겨서
  // 탭바에 그대로 쓸 수 없다. 하이픈 든 폴더명(mission-control)이 있어 팀 값에서 잘라 쓰지도 못한다.
  const room = team ? basename(process.cwd()) || null : null

  return {
    name,
    slug: entry.slug || null,
    paneId: paneOf(),
    groupColor: mapped || fallbackGroup,
    headerColor: entry.headerColor || fallbackHex,
    profile: entry.profile || null,
    sessionId: process.env.KASATERM_SESSION_ID || null,
    team,
    room,
    // 방 색은 방 이름에서 뽑는다. 그래야 같은 방이 어느 기기에서든 같은 색으로 뜨고, 먼저 연
    // 학생이 누구냐에 따라 색이 바뀌지 않는다.
    roomColor: room ? colorOf(room)[0] : null,
  }
}
