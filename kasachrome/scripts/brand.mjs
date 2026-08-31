#!/usr/bin/env node
// 배포판을 만든다. 소스는 개인판 이름 그대로 두고 여기서만 갈아입힌다.
//   node scripts/brand.mjs                          → dist/<key>/
//   node scripts/brand.mjs --out ../sionic/storm-browser
//
// 갈리는 것이 이름 문자열 몇 개뿐이라 레포는 한 벌로 유지한다 — 포크를 뜨면 같은 로직 두 벌이 되고,
// 이 프로젝트는 그 사고를 이미 겪었다(hostOf 가 두 곳에 있어 한쪽만 고쳐졌다).
// 치환 지점이 적은 이유는 화면 문구가 manifest.name 과 extension/port.js 를 읽어 쓰기 때문이다.
import { cpSync, readFileSync, writeFileSync, rmSync, mkdirSync, existsSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join, resolve, isAbsolute } from 'node:path'

const ROOT = dirname(dirname(fileURLToPath(import.meta.url)))

const BRAND = {
  name: 'Storm Browser',
  key: 'storm-browser', // MCP 등록 키이자 툴 접두사(mcp__storm-browser__browser_*)
}

const SRC_NAME = 'kasaterm Browser'
const SRC_KEY = 'kasachrome'

const outArg = process.argv[process.argv.indexOf('--out') + 1]
const OUT = process.argv.includes('--out') && outArg
  ? (isAbsolute(outArg) ? outArg : resolve(ROOT, outArg))
  : join(ROOT, 'dist', BRAND.key)

// node_modules 는 받는 쪽에서 npm install 로 만든다.
const COPY = ['extension', 'bridge', 'mcp', 'scripts', 'package.json', 'README.md']

// --out 으로 실물 폴더에 쓸 때 그 안의 .git 은 남긴다 — 배포판이 자체 이력을 갖기 때문이다.
// 그 외 우리가 심는 파일은 매번 새로 짓는다(옛 빌드의 잔재가 섞이면 추적이 안 된다).
for (const item of [...COPY, 'node_modules']) rmSync(join(OUT, item), { recursive: true, force: true })
mkdirSync(OUT, { recursive: true })
for (const item of COPY) cpSync(join(ROOT, item), join(OUT, item), { recursive: true })

// 브랜드 아이콘으로 갈아끼운다. 개인판 아이콘은 도형 생성물이고 이쪽은 로고 합성물이라
// 만드는 경로가 아예 다르다(scripts/icons.mjs vs scripts/icons-brand.mjs).
const brandIcons = join(ROOT, 'brand', BRAND.key, 'icons')
if (!existsSync(brandIcons)) throw new Error(`${brandIcons} 가 없습니다 — scripts/icons-brand.mjs 로 먼저 구우세요`)
cpSync(brandIcons, join(OUT, 'extension', 'icons'), { recursive: true })
// 배포판을 다시 브랜딩할 일은 없다. 아이콘 굽는 스크립트도 개인판 레포에서만 돌린다.
rmSync(join(OUT, 'scripts', 'brand.mjs'), { force: true })
rmSync(join(OUT, 'scripts', 'icons-brand.mjs'), { force: true })
// 호스트 터미널 어댑터는 남의 맥에서 100% 죽은 코드다. identity.mjs 가 optional import 라 빠져도 돈다.
rmSync(join(OUT, 'mcp', 'kasaterm.mjs'), { force: true })
rmSync(join(OUT, 'mcp', 'kasaterm-root.mjs'), { force: true })
rmSync(join(OUT, 'mcp', 'kasaterm-root.test.mjs'), { force: true })
rmSync(join(OUT, 'scripts', 'install-kasaterm.mjs'), { force: true })

function swap(rel, pairs) {
  const p = join(OUT, rel)
  let t = readFileSync(p, 'utf8')
  for (const [from, to] of pairs) {
    if (!t.includes(from)) throw new Error(`${rel} 에 "${from}" 이 없습니다 — 치환 대상이 옮겨졌습니다`)
    t = t.split(from).join(to)
  }
  writeFileSync(p, t)
}

// 호스트 터미널을 안 쓰는 사람에게는 읽을 이유가 없는 절. 마커째 들어낸다.
function dropSection(rel, marker) {
  const p = join(OUT, rel)
  const t = readFileSync(p, 'utf8')
  const re = new RegExp(`\\n?<!-- ${marker} -->[\\s\\S]*?<!-- /${marker} -->\\n`, 'g')
  if (!re.test(t)) throw new Error(`${rel} 에 <!-- ${marker} --> 절이 없습니다`)
  writeFileSync(p, t.replace(re, ''))
}

// 개인판에만 두는 장식. 회사 화면에 섞이면 튀므로 통째로 뺀다.
function dropBlock(rel, marker) {
  const p = join(OUT, rel)
  const t = readFileSync(p, 'utf8')
  const re = new RegExp(`\\n?[ \\t]*/\\* ${marker}[\\s\\S]*?/\\* /${marker} \\*/`, 'g')
  if (!t.match(re)) throw new Error(`${rel} 에 /* ${marker} */ 블록이 없습니다`)
  writeFileSync(p, t.replace(re, ''))
}

swap('extension/manifest.json', [[SRC_NAME, BRAND.name]])
swap('extension/popup.html', [[SRC_NAME, BRAND.name]])
swap('extension/sidepanel.html', [[SRC_NAME, BRAND.name]])
swap('mcp/server.mjs', [[`const NAME = '${SRC_KEY}'`, `const NAME = '${BRAND.key}'`]])
swap('package.json', [[`"name": "${SRC_KEY}"`, `"name": "${BRAND.key}"`]])
swap('README.md', [[`# ${SRC_NAME}`, `# ${BRAND.name}`]])
dropSection('README.md', 'host-only')
dropBlock('extension/content.js', 'brand-skip')

console.log(`${OUT}\n확장 로드 경로: ${join(OUT, 'extension')}`)
