// 호스트 터미널(kasaterm)이 창마다 배정한 캐릭터의 색·프로필 이미지를 읽는다.
// identity.mjs 가 선택적으로 import 하므로 이 파일이 없어도 신원 해석은 그대로 동작한다 —
// 배포 빌드는 이 파일을 통째로 뺀다(남의 맥에는 이 경로가 없고, 캐릭터 자산은 배포 대상도 아니다).
import { readFileSync, existsSync } from 'node:fs'
import { join } from 'node:path'

function root() {
  if (process.env.KASACHROME_KASATERM_DIR) return process.env.KASACHROME_KASATERM_DIR
  // 호스트가 PATH 에 자기 bin 을 꽂아두므로 거기서 레포 위치를 되짚는다.
  const hit = (process.env.PATH || '').split(':').find((p) => p.endsWith('/tmuxify/bin'))
  return hit ? hit.slice(0, -'/bin'.length) : null
}

function readCharacters(dir) {
  const candidates = [
    dir && join(dir, 'app', 'kasaterm', 'collab-hooks', 'characters.json'),
    dir && join(dir, 'dist', 'kasaterm.app', 'Contents', 'Resources', 'collab-hooks', 'characters.json'),
    '/Applications/kasaterm.app/Contents/Resources/collab-hooks/characters.json',
  ].filter(Boolean)
  for (const p of candidates) {
    if (!existsSync(p)) continue
    try {
      const j = JSON.parse(readFileSync(p, 'utf8'))
      return [...(j.leaders || []), ...(j.members || []), ...(Array.isArray(j) ? j : [])]
    } catch { /* 다음 후보 */ }
  }
  return []
}

// 프로필 파일명은 로마자(`midori-profile.png`)인데 characters.json 에는 한글 이름뿐이라
// 이 표가 유일한 다리다. characters.json 에 slug 가 생기면 그쪽이 이긴다.
const SLUGS = {
  아로나: 'arona', 프라나: 'prana', 미도리: 'midori', 모모이: 'momoi', 유즈: 'yuzu',
  아리스: 'arisu', 유우카: 'yuuka', 시로코: 'shiroko', 호시노: 'hoshino',
  코하루: 'koharu', 히마리: 'himari', 아루: 'aru',
}

function slugOf(entry, name) {
  return entry.slug || SLUGS[name] || (/^[a-z0-9_-]+$/i.test(name) ? name.toLowerCase() : null)
}

function profileDataUrl(dir, slug) {
  if (!dir || !slug) return null
  const p = join(dir, 'app', 'kasaterm', 'assets', 'students', `${slug}-profile.png`)
  if (!existsSync(p)) return null
  try {
    return `data:image/png;base64,${readFileSync(p).toString('base64')}`
  } catch {
    return null
  }
}

export function lookup(name) {
  const dir = root()
  if (!dir) return null
  const entry = readCharacters(dir).find((c) => c && c.name === name) || {}
  const slug = slugOf(entry, name)
  return {
    slug,
    claudeColor: entry.claude_color || null,
    headerColor: entry.header_color || null,
    profile: profileDataUrl(dir, slug),
  }
}
