// 호스트 터미널(kasaterm)이 창마다 배정한 캐릭터의 색·프로필 이미지를 읽는다.
// identity.mjs 가 선택적으로 import 하므로 이 파일이 없어도 신원 해석은 그대로 동작한다 —
// 배포 빌드는 이 파일을 통째로 뺀다(남의 맥에는 이 경로가 없고, 캐릭터 자산은 배포 대상도 아니다).
import { readFileSync, existsSync, readdirSync } from 'node:fs'
import { join } from 'node:path'
import { homedir } from 'node:os'
import { findKasatermRoot } from './kasaterm-root.mjs'

function root() {
  return findKasatermRoot()
}

// 사용자 설정 뿌리. 테마 팩(명부 + 그림)이 전부 이 아래 산다.
const CFG = join(homedir(), '.config', 'kasaterm')

function readJson(p) {
  try {
    return JSON.parse(readFileSync(p, 'utf8'))
  } catch {
    return null
  }
}

function settings() {
  return readJson(join(CFG, 'settings.json')) || {}
}

// 한 명부(JSON)에서 캐릭터 배열을 뽑는다. 번들 `characters.json` 과 테마 팩의
// `theme.json` 이 같은 모양(leader/leaders/members)이라 한 함수로 읽힌다.
function entriesOf(j) {
  if (!j) return []
  const one = j.leader ? [j.leader] : []
  return [...one, ...(j.leaders || []), ...(j.members || []), ...(Array.isArray(j) ? j : [])]
}

function bundleEntries(dir) {
  const candidates = [
    dir && join(dir, 'app', 'kasaterm', 'collab-hooks', 'characters.json'),
    dir && join(dir, 'dist', 'kasaterm.app', 'Contents', 'Resources', 'collab-hooks', 'characters.json'),
    '/Applications/kasaterm.app/Contents/Resources/collab-hooks/characters.json',
  ].filter(Boolean)
  for (const p of candidates) {
    if (existsSync(p)) return entriesOf(readJson(p))
  }
  return []
}

function themeIds() {
  try {
    return readdirSync(join(CFG, 'themes'), { withFileTypes: true })
      .filter((e) => e.isDirectory() && !e.name.startsWith('_'))
      .map((e) => e.name)
      .sort()
  } catch {
    return []
  }
}

function themeEntries(id) {
  return entriesOf(readJson(join(CFG, 'themes', id, 'theme.json')))
}

/// 이름 하나를 어느 명부에서 읽을지 정하는 **순서**가 이 파일의 핵심이다.
///
/// 이름이 두 테마에 겹치면(실측: 「리오」가 번들과 eternalreturn 양쪽에 있다) 순서가
/// 곧 정체다 — 순서가 없으면 사용자가 켠 적도 없는 쪽 얼굴이 브라우저 오버레이에
/// 뜬다. 그래서 **사용자가 그 이름을 고른 테마**를 맨 앞에 세운다. 그다음이 활성
/// 테마, 그다음 번들, 마지막이 나머지 설치본이다. 앱 쪽 배정 규칙과 같은 줄을 서야
/// 화면 두 곳이 서로 다른 사람을 가리키지 않는다.
function rosters(name) {
  const s = settings()
  const picks = s.character_picks || {}
  const out = []
  const seen = new Set()
  const push = (id, entries) => {
    if (seen.has(id)) return
    seen.add(id)
    out.push({ id, entries })
  }
  for (const [theme, names] of Object.entries(picks)) {
    if (!Array.isArray(names) || !names.includes(name)) continue
    if (theme === '__base') push('__base', bundleEntries(root()))
    else push(theme, themeEntries(theme))
  }
  const active = s.character_theme
  if (active) push(active, themeEntries(active))
  push('__base', bundleEntries(root()))
  for (const id of themeIds()) push(id, themeEntries(id))
  return out
}

// 프로필 파일명은 로마자(`midori-profile.png`)인데 옛 명부에는 한글 이름뿐이라
// 이 표가 다리였다. 지금은 명부에 slug 가 있어 그쪽이 이긴다 — 명부를 못 읽은
// 경우의 마지막 그물로만 남긴다.
const SLUGS = {
  아로나: 'arona', 프라나: 'prana', 미도리: 'midori', 모모이: 'momoi', 유즈: 'yuzu',
  아리스: 'arisu', 유우카: 'yuuka', 시로코: 'shiroko', 호시노: 'hoshino',
  코하루: 'koharu', 히마리: 'himari', 아루: 'aru',
}

function slugOf(entry, name) {
  return entry.slug || SLUGS[name] || (/^[a-z0-9_-]+$/i.test(name) ? name.toLowerCase() : null)
}

/// 그림도 **그 캐릭터가 온 명부와 같은 자리**에서 찾는다. 테마 팩은 명부와 그림이
/// 한 벌이라, 이름은 테마에서 왔는데 그림은 번들에서 찾으면 영영 못 찾는다 — 그게
/// 테마 학생이 전부 민색 원으로 떨어지던 이유였다(2026-08-29 실측: 에무·하치와레·
/// 진천우 모두 슬러그부터 null 이었다).
function profileDataUrl(dir, slug, themeId) {
  if (!slug) return null
  // 실제 구조는 `profile/<slug>.png` 다. 예전엔 `<slug>-profile.png` 를 봤는데 그런
  // 파일은 트리에 하나도 없어 프사가 통째로 비었다(2026-08-29). 옛 이름도 후보로
  // 남긴다 — 배포 빌드가 평평하게 펴는 경우가 있다.
  const themeDirs = themeId && themeId !== '__base'
    ? [join(CFG, 'themes', themeId, 'sprites')]
    : []
  const roots = [
    ...themeDirs,
    join(CFG, 'students'),
    dir && join(dir, 'app', 'kasaterm', 'assets', 'students'),
    dir && join(dir, 'dist', 'kasaterm.app', 'Contents', 'Resources', 'assets', 'students'),
    '/Applications/kasaterm.app/Contents/Resources/assets/students',
  ].filter(Boolean)
  for (const r of roots) {
    for (const p of [join(r, 'profile', `${slug}.png`), join(r, `${slug}-profile.png`)]) {
      if (!existsSync(p)) continue
      try {
        return `data:image/png;base64,${readFileSync(p).toString('base64')}`
      } catch { /* 다음 후보 */ }
    }
  }
  return null
}

export function lookup(name) {
  const dir = root()
  // 뿌리를 못 찾아도 포기하지 않는다 — 테마 팩은 사용자 설정 아래 있어서 레포
  // 위치와 무관하다. 번들만 못 읽을 뿐이다.
  let entry = {}
  let themeId = null
  for (const r of rosters(name)) {
    const hit = r.entries.find((c) => c && c.name === name)
    if (hit) {
      entry = hit
      themeId = r.id
      break
    }
  }
  const slug = slugOf(entry, name)
  return {
    slug,
    claudeColor: entry.claude_color || null,
    headerColor: entry.header_color || null,
    profile: profileDataUrl(dir, slug, themeId),
  }
}
