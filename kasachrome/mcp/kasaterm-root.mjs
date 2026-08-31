import { existsSync, realpathSync } from 'node:fs'
import { basename, delimiter, dirname, join, normalize } from 'node:path'

function unquote(entry) {
  const value = entry.trim()
  if (value.length >= 2 && value[0] === '"' && value.at(-1) === '"') return value.slice(1, -1)
  return value
}

function repoMarker(dir) {
  return existsSync(join(dir, 'app', 'kasaterm', 'collab-hooks', 'characters.json'))
}

export function findKasatermRoot(env = process.env) {
  if (env.KASACHROME_KASATERM_DIR) return env.KASACHROME_KASATERM_DIR

  const roots = []
  for (const raw of (env.PATH || '').split(delimiter)) {
    const entry = unquote(raw)
    if (!entry) continue
    let resolved = normalize(entry)
    try { resolved = realpathSync(resolved) } catch { /* PATH에는 아직 만들어지지 않은 항목도 올 수 있다. */ }
    if (basename(resolved).toLowerCase() !== 'bin') continue
    const candidate = dirname(resolved)
    if (repoMarker(candidate)) roots.push(candidate)
  }

  return roots.find((dir) => basename(dir).toLowerCase() === 'kasaterm') || roots[0] || null
}
