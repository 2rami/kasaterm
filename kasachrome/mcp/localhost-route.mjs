import { readFileSync } from 'node:fs'
import { dirname, basename, extname, join } from 'node:path'

export function isLoopbackUrl(value) {
  try {
    const host = new URL(value).hostname.toLowerCase()
    return ['localhost', 'localhost.', '[::1]'].includes(host) || /^127\.\d+\.\d+\.\d+$/.test(host)
  }
  catch { return false }
}

export function sourceBase(env = process.env, read = readFileSync) {
  const socket = env.KASATERM_SOCKET_PATH || env.CMUX_SOCKET_PATH
  if (!socket) throw new Error('SOURCE_APP_UNKNOWN: 로컬 개발 서버를 전달할 카사텀 연결이 없습니다.')
  const stem = basename(socket, extname(socket))
  const port = Number(String(read(join(dirname(socket), `${stem}.mcp_port`), 'utf8')).trim())
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error('SOURCE_APP_PORT_INVALID')
  return `http://127.0.0.1:${port}`
}

export async function resolveBrowserArgs(tool, args, route, options = {}) {
  if (!route.selected || !['new_tab', 'new_window', 'navigate'].includes(tool) || !args.url) return args
  const urls = Array.isArray(args.url) ? args.url : [args.url]
  if (!urls.some(isLoopbackUrl)) return args
  const base = options.base || sourceBase()
  const request = options.fetch || globalThis.fetch
  const resolved = []
  for (const url of urls) {
    if (!isLoopbackUrl(url)) { resolved.push(url); continue }
    const endpoint = new URL('/browser/resolve-url', base)
    endpoint.searchParams.set('url', url)
    endpoint.searchParams.set('machine', route.selected)
    const response = await request(endpoint, { signal: AbortSignal.timeout(25000), redirect: 'error' })
    const value = await response.json()
    if (!response.ok || value.ok !== true || typeof value.url !== 'string'
        || value.machine !== route.selected) {
      throw new Error(`LOCALHOST_FORWARD_FAILED: ${value.error || '선택한 기기에 개발 서버를 연결하지 못했습니다.'}`)
    }
    resolved.push(value.url)
  }
  return { ...args, url: Array.isArray(args.url) ? resolved : resolved[0] }
}
