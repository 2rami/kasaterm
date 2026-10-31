#!/usr/bin/env node
// 사이트 로그인을 이 맥 키체인에 넣는다 — browser_login 이 꺼내 쓰는 항목(`web:<host>`)이다. 사람이 터미널에서 직접:
//   node scripts/login-add.mjs <주소 또는 host> [사용자명]
// 비밀번호는 화면에 안 찍히는 프롬프트로 받는다. 에이전트 도구에는 쓰기 길이 없고, 이 스크립트도 터미널이 아니면 멈춘다.
import { spawn } from 'node:child_process'
import { existsSync } from 'node:fs'
import { homedir } from 'node:os'
import { join } from 'node:path'
import { createInterface } from 'node:readline'
import { serviceOf, secretRef } from '../mcp/login.mjs'

function fail(message) {
  process.stderr.write(`${message}\n`)
  process.exit(1)
}

// 확장이 탭 주소에서 읽는 값과 같은 꼴이어야 찾힌다 — URL 의 host(소문자·퓨니코드, 기본 아닌 포트 포함).
function hostOf(text) {
  try { return new URL(/^[a-z]+:\/\//i.test(text) ? text : `https://${text}`).host } catch { return null }
}

// 키체인 항목을 읽어도 되는 앱. 이걸 안 주면 만든 도구(`security`)가 믿는 앱이 되어, 같은 사용자의 아무 프로세스나
// `security find-generic-password -w` 로 창 없이 꺼낼 수 있다 — 폰 승인을 건너뛰는 길이 열린다.
function kasatermApp(argv) {
  const at = argv.indexOf('--app')
  const given = at !== -1 ? argv[at + 1] : process.env.KASATERM_APP
  const candidates = given ? [given] : [join(homedir(), 'Applications', 'kasaterm.app'), '/Applications/kasaterm.app']
  return candidates.find((p) => existsSync(p)) || null
}

function ask(question) {
  return new Promise((resolve) => {
    const rl = createInterface({ input: process.stdin, output: process.stdout })
    rl.question(question, (answer) => { rl.close(); resolve(answer.trim()) })
  })
}

function askHidden(question) {
  return new Promise((resolve, reject) => {
    const input = process.stdin
    let value = ''
    const done = (error) => {
      input.setRawMode(false)
      input.pause()
      input.off('data', onData)
      process.stdout.write('\n')
      if (error) reject(error)
      else resolve(value)
    }
    const onData = (chunk) => {
      for (const ch of chunk) {
        if (ch === '\r' || ch === '\n' || ch === '\u0004') return done()
        if (ch === '\u0003') return done(new Error('취소했어요.'))
        if (ch === '\u007f' || ch === '\b') value = [...value].slice(0, -1).join('')
        else value += ch
      }
    }
    process.stdout.write(question)
    input.setRawMode(true)
    input.setEncoding('utf8')
    input.on('data', onData)
    input.resume()
  })
}

// `security -i` 에 표준입력으로 명령을 준다 — 값을 명령줄 인자로 넘기면 그 순간 `ps` 에 보인다. 값은 16진(-X)이라 따옴표가 안 낀다.
function addToKeychain({ service, account, value, app }) {
  const quote = (s) => `"${s}"`
  const line = ['add-generic-password', '-U', '-s', quote(service), '-a', quote(account), '-l', quote(service),
    '-j', quote('kasachrome login'), '-T', quote(app), '-X', Buffer.from(value, 'utf8').toString('hex')].join(' ')
  return new Promise((resolve, reject) => {
    const child = spawn('security', ['-i'], { stdio: ['pipe', 'ignore', 'pipe'] })
    let err = ''
    child.stderr.on('data', (b) => { err += b })
    child.on('error', reject)
    child.on('close', () => (err.trim() ? reject(new Error(err.trim())) : resolve()))
    child.stdin.end(`${line}\n`)
  })
}

const argv = process.argv.slice(2)
const positional = argv.filter((a, i) => !a.startsWith('--') && argv[i - 1] !== '--app')
if (!positional.length || argv.includes('--help')) {
  fail('쓰는 법: node scripts/login-add.mjs <주소 또는 host> [사용자명] [--app <kasaterm.app 경로>]')
}
if (!process.stdin.isTTY || !process.stdout.isTTY) fail('사람이 터미널에서 직접 실행해야 해요(비밀번호를 화면 안 찍히는 프롬프트로 받습니다).')
if (process.platform !== 'darwin') fail('맥 키체인용이에요. Windows 는 cmdkey /generic:web:<host> /user:<사용자명> /pass 로 넣으세요.')

const host = hostOf(positional[0])
if (!host) fail(`주소를 읽지 못했어요: ${positional[0]}`)
const app = kasatermApp(argv)
if (!app) fail('kasaterm.app 을 못 찾았어요 — --app <경로> 로 알려 주세요. 키체인에서 이 항목을 읽을 앱이에요.')

const username = positional[1] || await ask(`${host} 사용자명: `)
// 키체인 서비스·계정에 쓸 수 있는지는 도구가 꺼낼 때와 같은 규칙으로 본다. 따옴표·역슬래시는 `security -i` 가 다르게 읽는다.
try { secretRef(host, username) } catch (e) { fail(e.message) }
if (!username || /["\\]/.test(username)) fail('사용자명이 비었거나 따옴표·역슬래시가 들어 있어요.')

const password = await askHidden(`${host} 비밀번호(화면에 안 보여요): `)
if (!password) fail('비밀번호가 비었어요.')
if (await askHidden('한 번 더: ') !== password) fail('두 번 친 비밀번호가 달라요.')

const service = serviceOf(host)
try {
  await addToKeychain({ service, account: username, value: JSON.stringify({ username, password }), app })
} catch (e) {
  fail(`키체인에 넣지 못했어요: ${e.message}`)
}
console.log(`넣었어요: ${service} (계정 ${username}) — 이 항목을 읽을 앱: ${app}`)
console.log('처음 쓸 때 이 맥에 키체인 허용 창이 뜨면 「항상 허용」을 눌러 주세요. 지우기: security delete-generic-password -s ' + `"${service}" -a "${username}"`)
