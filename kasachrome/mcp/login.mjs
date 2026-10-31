// browser_login — 사이트 로그인을 비밀번호가 모델 맥락에 한 글자도 안 남게 채운다. 값은 카사텀 secret
// (이 맥 키체인 + 폰 Face ID 승인)에서 이 프로세스의 메모리로만 오고, 브리지를 거쳐 확장이 페이지 칸에 넣는다.
// 디스크에는 값 없는 사용 기록 한 줄만 남는다.
import { spawn } from 'node:child_process'
import { appendFileSync, mkdirSync, readFileSync, renameSync, statSync } from 'node:fs'
import { homedir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

// 브리지 로그와 같은 폴더 — 배포판은 패키지 이름을 따라 갈린다.
const PKG = JSON.parse(readFileSync(join(dirname(dirname(fileURLToPath(import.meta.url))), 'package.json'), 'utf8'))
export const AUDIT_FILE = join(homedir(), `.${PKG.name}`, 'credential-access.jsonl')
const AUDIT_MAX = 2 * 1024 * 1024
// 폰 승인은 2분에 만료된다. 그보다 조금 더 기다려 카사텀이 내는 만료 사유를 받는다.
const SECRET_TIMEOUT_MS = 150000
const MASK = '<concealed>'

const REASONS = {
  denied: ['LOGIN_DENIED', '폰에서 거절했습니다.'],
  expired: ['LOGIN_TIMEOUT', '2분 안에 폰 허락이 오지 않았습니다.'],
  cancelled: ['LOGIN_CANCELLED', '승인 요청이 취소됐습니다.'],
  keychain_denied: ['LOGIN_KEYCHAIN_DENIED', '맥의 키체인 접근 허용 창에서 거절됐습니다.'],
  no_trusted_key: ['LOGIN_NO_APPROVER', '이 맥이 믿는 폰 열쇠가 없습니다 — 폰 설정 → 계정 → 비밀 승인에서 연결해야 합니다.'],
  not_in_pane: ['LOGIN_NOT_IN_PANE', 'kasaterm 칸 안에서 띄운 세션만 쓸 수 있습니다.'],
  requester_unknown: ['LOGIN_NOT_IN_PANE', 'kasaterm 칸 안에서 띄운 세션만 쓸 수 있습니다.'],
  not_text: ['LOGIN_BAD_SECRET', '키체인 값이 글자가 아닙니다.'],
}

export function serviceOf(host) {
  return `web:${host}`
}

// 카사텀 secret 참조(`서비스[#계정]`). 계정은 마지막 `#` 뒤라 계정에는 `#` 을 못 쓴다. 앱은 256자까지 받는다.
export function secretRef(host, account) {
  if (typeof host !== 'string' || !/^[a-z0-9.\-[\]:]+$/i.test(host)) throw new Error('LOGIN_BAD_HOST: 탭 주소에서 host 를 읽지 못했습니다.')
  if (account != null && (typeof account !== 'string' || !account || /[#\p{Cc}]/u.test(account))) {
    throw new Error('LOGIN_BAD_ACCOUNT: account 에는 # 이나 제어 문자를 넣을 수 없습니다.')
  }
  const ref = account ? `${serviceOf(host)}#${account}` : serviceOf(host)
  if (`keychain://${ref}`.length > 256) throw new Error('LOGIN_BAD_ACCOUNT: host 와 account 를 합쳐 너무 깁니다.')
  return ref
}

// kasaterm-cli 는 값을 stdout 에만, 사유는 stderr 마지막 줄 `… (코드)` 로 낸다. 오류에는 그 사유만 싣는다.
export function secretError(stderr, ref) {
  const last = String(stderr || '').trim().split('\n').pop() || ''
  const code = last.match(/\(([a-z_]+)\)\s*$/)?.[1]
  if (code === 'not_found') {
    const host = ref.replace(/^web:/, '').replace(/#.*$/, '')
    return new Error(`LOGIN_NOT_REGISTERED: 이 맥 키체인에 ${ref} 로그인이 없습니다. 사람에게 등록을 부탁하세요 — node kasachrome/scripts/login-add.mjs ${host}`)
  }
  const known = REASONS[code]
  if (known) return new Error(`${known[0]}: ${known[1]}`)
  return new Error(`LOGIN_SECRET_FAILED: ${last.replace(/^kasaterm-cli:\s*/, '').slice(0, 300) || '카사텀 secret 이 값을 주지 않았습니다.'}`)
}

export function readSecret(ref, { cli = process.env.KASACHROME_SECRET_CLI || 'kasaterm-cli', timeoutMs = SECRET_TIMEOUT_MS, spawnImpl = spawn } = {}) {
  return new Promise((resolve, reject) => {
    let settled = false
    let timer = null
    const out = []
    const err = []
    const finish = (error, value) => {
      if (settled) return
      settled = true
      clearTimeout(timer)
      for (const chunk of out) chunk.fill(0)
      if (error) reject(error)
      else resolve(value)
    }
    let child
    try {
      child = spawnImpl(cli, ['secret', 'read', ref], { stdio: ['ignore', 'pipe', 'pipe'] })
    } catch {
      reject(new Error('LOGIN_NO_KASATERM: kasaterm-cli 를 실행하지 못했습니다.'))
      return
    }
    timer = setTimeout(() => {
      try { child.kill('SIGTERM') } catch {}
      finish(new Error('LOGIN_TIMEOUT: 폰 허락을 기다리다 그만뒀습니다.'))
    }, timeoutMs)
    child.stdout.on('data', (chunk) => out.push(chunk))
    child.stderr.on('data', (chunk) => { if (err.length < 64) err.push(chunk) })
    child.on('error', (e) => finish(new Error(e.code === 'ENOENT'
      ? 'LOGIN_NO_KASATERM: kasaterm-cli 를 찾지 못했습니다 — kasaterm 칸 안에서 띄운 세션이어야 합니다.'
      : 'LOGIN_NO_KASATERM: kasaterm-cli 를 실행하지 못했습니다.')))
    child.on('close', (code) => {
      if (code === 0) finish(null, Buffer.concat(out).toString('utf8'))
      else finish(secretError(Buffer.concat(err).toString('utf8'), ref))
    })
  })
}

// 등록 스크립트는 `{"username","password"}` 를 넣는다. 그 밖의 값은 통째로 비밀번호이고, 사용자명은 계정 라벨이다
// (`security add-generic-password -s web:<host> -a <사용자명> -w` 로 손수 넣은 항목).
export function parseCredential(value, account) {
  if (value.startsWith('{')) {
    try {
      const v = JSON.parse(value)
      if (v && typeof v.password === 'string') return { username: typeof v.username === 'string' && v.username ? v.username : account || null, password: v.password }
    } catch { /* JSON.parse 의 오류문은 입력 앞부분을 싣는다 — 버리고 통째로 비밀번호로 본다 */ }
  }
  return { username: account || null, password: value }
}

export function maskName(name) {
  return name ? `${[...name][0]}•••` : null
}

function scrub(text, secrets) {
  let out = text
  for (const s of secrets) if (s) out = out.split(s).join(MASK)
  return out
}

export function appendAudit(entry, file = AUDIT_FILE) {
  try {
    mkdirSync(dirname(file), { recursive: true, mode: 0o700 })
    try { if (statSync(file).size > AUDIT_MAX) renameSync(file, `${file}.1`) } catch { /* 아직 없다 */ }
    appendFileSync(file, `${JSON.stringify({ at: new Date().toISOString(), ...entry })}\n`, { mode: 0o600 })
  } catch (e) {
    process.stderr.write(`[login] 사용 기록을 못 남겼습니다: ${e.code || e.message}\n`)
  }
}

export async function login({ tabId, account, showUsername = false }, { call, read = readSecret, audit = appendAudit, identity = {} }) {
  if (!Number.isInteger(tabId) || tabId <= 0) throw new Error('BAD_TAB_ID: login 은 list_tabs 로 확인한 tabId 가 꼭 있어야 합니다.')
  const entry = {
    who: { name: identity.name || null, pane: identity.paneId || null, session: identity.sessionId || null },
    tabId, host: null, service: null, account: account || null,
  }
  const secrets = []
  try {
    const target = await call('login_target', { tabId }, 15000)
    entry.host = target.host
    entry.service = secretRef(target.host, account)
    const cred = parseCredential(await read(entry.service), account)
    secrets.push(cred.password)
    if (!cred.password) throw new Error('LOGIN_BAD_SECRET: 키체인 값이 비어 있습니다.')
    const username = target.fields.username ? cred.username : null
    const password = target.fields.password ? cred.password : null
    if (!username && !password) throw new Error('LOGIN_NOTHING_TO_FILL: 이 화면은 사용자명 칸만 있는데 저장된 사용자명이 없습니다 — account 로 사용자명을 주세요.')
    const done = await call('login_fill', { tabId, host: target.host, username, password }, 15000)
    if (done.missed?.length) entry.missed = done.missed
    if (!done.filled.length) throw new Error(`LOGIN_NOT_APPLIED: 값을 넣었지만 페이지가 받아들이지 않았습니다(${done.missed.join(', ')}).`)
    entry.result = 'filled'
    entry.filled = done.filled
    return {
      ok: true,
      host: done.host,
      service: entry.service,
      account: account || null,
      filled: done.filled,
      ...(done.missed?.length ? { missed: done.missed, missedNote: '값을 넣었지만 페이지가 받아들이지 않았습니다(입력 가로채는 칸).' } : {}),
      username: done.filled.includes('username') ? (showUsername ? cred.username : maskName(cred.username)) : null,
      note: '채우기만 했습니다 — 제출은 browser_click 으로 누르세요. 비밀번호는 이 결과에 없고, 다른 도구 결과에 그 값이 나오면 <concealed> 로 가려집니다.'
        + (target.fields.password ? '' : ' 이 화면에는 비밀번호 칸이 없어 사용자명만 넣었습니다(두 단계 로그인이면 다음 화면에서 login 을 다시 부르세요).'),
    }
  } catch (e) {
    const message = scrub(String(e?.message || e), secrets)
    entry.result = message.match(/^([A-Z_]+):/)?.[1] || 'error'
    throw new Error(message)
  } finally {
    audit(entry)
  }
}
