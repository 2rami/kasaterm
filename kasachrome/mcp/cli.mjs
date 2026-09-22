// CLI 진입점. MCP 서버와 같은 프로세스·같은 도구 레지스트리를 쓴다 — 기기 라우팅·폴백·
// 스크린샷 저장 같은 로직을 여기에 베끼면 두 벌이 갈라져 조용히 어긋나므로, 이 파일은
// 인자 해석과 출력만 맡는다. 도구 목록·인자·설명은 전부 zod 스키마에서 뽑으므로
// 도구가 늘어도 여기는 안 고친다.
import { z } from 'zod'

const strip = (name) => name.replace(/^browser_/, '')

// 한글·한자는 터미널에서 두 칸을 먹는다. padEnd 는 글자 수로 세므로 그대로 쓰면
// 한글이 섞인 줄만 설명이 왼쪽으로 밀려 표가 어긋난다.
const WIDE = /[\u1100-\u115F\u2E80-\uA4CF\uAC00-\uD7A3\uF900-\uFAFF\uFE30-\uFE6F\uFF00-\uFF60\uFFE0-\uFFE6]/
const width = (text) => [...text].reduce((n, ch) => n + (WIDE.test(ch) ? 2 : 1), 0)
const padTo = (text, n) => text + ' '.repeat(Math.max(0, n - width(text)))

function fields(schema) {
  return Object.entries(schema || {}).map(([name, zs]) => {
    let j = {}
    try { j = z.toJSONSchema(zs, { io: 'input', unrepresentable: 'any' }) } catch {}
    return {
      name,
      type: j.type || 'any',
      item: j.items?.type,
      choices: j.enum,
      desc: j.description || '',
      required: !zs.safeParse(undefined).success,
    }
  })
}

// 목록에서는 한 줄만 보여 준다. 도구 설명은 모델용이라 길게는 1000자가 넘는데,
// 그걸 45개 늘어놓으면 목록이 제 구실을 못 한다. 전문은 `kc <도구> --help` 에 있다.
function summary(description) {
  const first = description.split('\n')[0]
  const stop = first.match(/^(.{0,104}?[.?!])(\s|$)/)
  return stop ? stop[1].trim() : first.slice(0, 104).trim() + '…'
}

function usage(name, flds, { brief = false } = {}) {
  const req = flds.filter((f) => f.required).map((f) => `<${f.name}>`)
  const optFields = flds.filter((f) => !f.required)
  const opt = optFields.map((f) => (f.type === 'boolean' ? `[--${f.name}]` : `[--${f.name} ${f.type === 'integer' || f.type === 'number' ? 'N' : f.name.toUpperCase()}]`))
  // 목록에서는 선택 인자를 개수로 접는다 — 여덟 개를 늘어놓으면 그 줄만 화면을 넘어가
  // 옆에 붙은 설명이 통째로 밀린다. 펼친 것은 `kc <도구> --help` 에 있다.
  if (brief && optFields.length > 2) return [strip(name), ...req, `[선택 ${optFields.length}]`].join(' ')
  return [strip(name), ...req, ...opt].join(' ')
}

function cast(value, f) {
  if (f.type === 'integer' || f.type === 'number') {
    const n = Number(value)
    if (Number.isNaN(n)) throw new Error(`--${f.name} 은 숫자여야 합니다 (받은 값: ${value})`)
    return n
  }
  if (f.type === 'boolean') return value !== 'false'
  if (f.type === 'array') {
    try { return JSON.parse(value) } catch {}
    return value.split(',').map((v) => (f.item === 'integer' || f.item === 'number' ? Number(v) : v))
  }
  if (f.type === 'object') return JSON.parse(value)
  if (value === 'null') return null
  return value
}

function parseArgs(argv, flds) {
  const args = {}
  const loose = []
  for (let i = 0; i < argv.length; i++) {
    const token = argv[i]
    if (!token.startsWith('--')) { loose.push(token); continue }
    const [key, inline] = token.slice(2).split(/=(.*)/s)
    const f = flds.find((x) => x.name === key)
    if (!f) throw new Error(`모르는 옵션 --${key} — 쓸 수 있는 것: ${flds.map((x) => '--' + x.name).join(', ') || '없음'}`)
    if (f.type === 'boolean' && inline === undefined) { args[key] = true; continue }
    const raw = inline !== undefined ? inline : argv[++i]
    if (raw === undefined) throw new Error(`--${key} 에 값이 필요합니다`)
    args[key] = cast(raw, f)
  }
  // 통째 JSON 을 받는 길 — MCP 도구 호출을 그대로 옮겨 적을 때 이게 제일 짧다.
  if (loose.length === 1 && /^\s*\{/.test(loose[0])) return { ...JSON.parse(loose[0]), ...args }
  // 자리 인자는 필수 항목에만 순서대로 붙인다. 선택 항목까지 받으면 무엇이 어디로 갔는지 안 보인다.
  const req = flds.filter((f) => f.required && args[f.name] === undefined)
  if (loose.length > req.length) throw new Error(`인자가 ${loose.length - req.length}개 많습니다 — 선택 항목은 --이름 으로 주세요`)
  loose.forEach((v, i) => { args[req[i].name] = cast(v, req[i]) })
  return args
}

function render(result) {
  const out = []
  for (const part of result?.content || []) {
    if (part.type === 'text') out.push(part.text)
    // CLI 로 base64 를 뱉으면 터미널만 망가지고 쓸 데가 없다. 파일로 받는 길을 알려 준다.
    else if (part.type === 'image') out.push(`[${part.mimeType} ${Math.round(part.data.length * 0.75 / 1024).toLocaleString()}KB] — --path /절대/경로.png 로 저장하세요`)
  }
  return out.join('\n')
}

function resolve(name, tools) {
  const want = strip(name)
  const names = [...tools.keys()]
  const exact = names.find((n) => strip(n) === want)
  if (exact) return exact
  const prefix = names.filter((n) => strip(n).startsWith(want))
  if (prefix.length === 1) return prefix[0]
  if (prefix.length > 1) throw new Error(`"${name}" 은 여럿에 걸립니다: ${prefix.map(strip).join(', ')}`)
  throw new Error(`모르는 도구 "${name}" — kc --help 로 목록을 보세요`)
}

function listAll(tools) {
  const rows = [...tools.entries()].map(([name, t]) => [usage(name, fields(t.schema), { brief: true }), summary(t.description)])
  const pad = Math.max(...rows.map((r) => width(r[0])))
  return [
    'kc — 카사크롬 도구를 터미널에서 직접. MCP 서버를 거치지 않는다.',
    '',
    '  kc <도구> [인자…]        실행',
    '  kc <도구> --help         그 도구의 전문',
    "  kc <도구> '{\"k\":1}'      인자를 JSON 통째로",
    '',
    ...rows.map(([u, s]) => `  ${padTo(u, pad)}  ${s}`),
  ].join('\n')
}

function detail(name, tool) {
  const flds = fields(tool.schema)
  const lines = [`kc ${usage(name, flds)}`, '', tool.description]
  if (flds.length) {
    lines.push('', '인자:')
    const pad = Math.max(...flds.map((f) => f.name.length))

    for (const f of flds) {
      const type = f.choices ? f.choices.join('|') : f.type + (f.type === 'array' ? `<${f.item || 'any'}>` : '')
      lines.push(`  --${f.name.padEnd(pad)}  ${type}${f.required ? ' (필수)' : ''}${f.desc ? ` — ${f.desc}` : ''}`)
    }
  }
  return lines.join('\n')
}

export async function runCli(argv, tools) {
  const [first, ...rest] = argv
  if (!first || first === '--help' || first === '-h') { console.log(listAll(tools)); return 0 }
  const name = resolve(first, tools)
  const tool = tools.get(name)
  if (rest.includes('--help') || rest.includes('-h')) { console.log(detail(name, tool)); return 0 }
  const result = await tool.run(parseArgs(rest, fields(tool.schema)))
  const text = render(result)
  if (result?.isError) { console.error(text); return 1 }
  console.log(text)
  return 0
}
