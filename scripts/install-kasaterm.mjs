#!/usr/bin/env node
// kasaterm 이용자용. 창마다 뜨는 claude 에 이 MCP 가 저절로 붙게 한다.
//   node scripts/install-kasaterm.mjs            등록
//   node scripts/install-kasaterm.mjs --remove   되돌리기
//
// kasaterm 이 pane 안 `claude` 를 감싸면서 이 파일을 `--mcp-config` 로 넘긴다. 그래서 파일 모양이
// 곧 claude 가 읽는 모양이고(중간 변환이 없다), 여기를 갈아 끼우면 다음 `claude` 부터 반영된다.
//
// ⚠ 이 파일이 깨지면 **모든 pane 의 claude 가 부팅을 거부한다**. 그래서 쓰기는 반드시 원자적이다.
//   손으로 망가뜨렸다면 복구는 이 파일을 지우는 것 한 줄이고, claude 에러가 경로를 그대로 찍는다.
import { readFileSync, writeFileSync, renameSync, mkdirSync, existsSync, rmSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import { homedir } from 'node:os'

const ROOT = dirname(dirname(fileURLToPath(import.meta.url)))
const KEY = JSON.parse(readFileSync(join(ROOT, 'package.json'), 'utf8')).name
const CFG_DIR = join(homedir(), '.config', 'kasaterm')
const CFG = join(CFG_DIR, 'claude-mcp.json')
const remove = process.argv.includes('--remove')

const say = (...a) => console.log(...a)

// settings.json 이 아니라 별도 파일인 이유: kasaterm 의 설정 저장이 그 파일을 통째로 다시 써서
// 동시에 건드리면 한쪽 변경이 조용히 사라진다.
function read() {
  if (!existsSync(CFG)) return { mcpServers: {} }
  try {
    const j = JSON.parse(readFileSync(CFG, 'utf8'))
    return j && typeof j === 'object' && j.mcpServers ? j : { mcpServers: {} }
  } catch {
    say(`기존 ${CFG} 를 읽을 수 없어 새로 만듭니다.`)
    return { mcpServers: {} }
  }
}

function write(obj) {
  mkdirSync(CFG_DIR, { recursive: true })
  const tmp = `${CFG}.tmp`
  writeFileSync(tmp, `${JSON.stringify(obj, null, 2)}\n`)
  renameSync(tmp, CFG)
}

const cfg = read()

if (remove) {
  if (!cfg.mcpServers[KEY]) {
    say(`${KEY} 는 등록돼 있지 않습니다.`)
    process.exit(0)
  }
  delete cfg.mcpServers[KEY]
  // 우리가 마지막 항목이었으면 파일째 치운다 — 빈 껍데기를 남겨 두면 다음 사람이 왜 있는지 못 읽는다.
  if (Object.keys(cfg.mcpServers).length === 0) rmSync(CFG, { force: true })
  else write(cfg)
  say(`제거했습니다: ${KEY}`)
  say('이미 떠 있는 claude 는 그대로입니다 — 다음에 새로 뜨는 것부터 빠집니다.')
  process.exit(0)
}

cfg.mcpServers[KEY] = { command: 'node', args: [join(ROOT, 'mcp', 'server.mjs')] }
write(cfg)

say(`등록했습니다: ${KEY}`)
say(`  ${CFG}`)
const others = Object.keys(cfg.mcpServers).filter((k) => k !== KEY)
if (others.length) say(`  (같은 파일의 다른 서버는 그대로 뒀습니다: ${others.join(', ')})`)
say('')
say('kasaterm pane 에서 새로 `claude` 를 띄우면 `/mcp` 목록에 나옵니다.')

// user scope 에도 있으면 같은 서버가 두 경로로 들어온다. 남의 도구가 쓰는 파일이라 읽기만 하고 안내만 한다.
try {
  if (JSON.parse(readFileSync(join(homedir(), '.claude.json'), 'utf8')).mcpServers?.[KEY]) {
    say('')
    say(`참고: ~/.claude.json (user scope) 에도 ${KEY} 가 있습니다. 겹쳐도 동작하지만,`)
    say('정리하려면 아래를 직접 실행하세요 — pane 밖 claude 에서도 빠지는 점만 감안하시고요.')
    say(`  claude mcp remove ${KEY} -s user`)
  }
} catch { /* 파일이 없거나 아직 안 만들어졌다 */ }
