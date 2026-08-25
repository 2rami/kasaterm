#!/usr/bin/env node
// 배포판 아이콘을 다시 굽는다 (로고나 배경색을 바꿀 때만 — 결과 PNG 는 레포에 커밋된다).
//   TAB=<탭id> node scripts/icons-brand.mjs brand/storm-browser/logo.png brand/storm-browser/icons '#0EA5E9'
//
// 개인판 아이콘(scripts/icons.mjs)과 달리 **브라우저가 필요하다** — 로고 원본이 래스터라
// 합성하려면 PNG 디코더가 있어야 하는데, 확장이 이미 쓰는 canvas 경로를 빌리는 편이
// 디코더를 직접 구현하는 것보다 훨씬 싸다.
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs'
import { WebSocket } from 'ws'
import { PORT } from '../extension/port.js'

const [logoPath, outDir, sky] = process.argv.slice(2)
if (!logoPath || !outDir || !sky) {
  console.error('usage: TAB=<탭id> node scripts/icons-brand.mjs <로고.png> <출력폴더> <배경색>')
  process.exit(1)
}
const logo = `data:image/png;base64,${readFileSync(logoPath).toString('base64')}`

// 로고를 흰색으로 칠하지 않는다 — 이 로고는 알파가 나선이 아니라 원판 전체라 마스크로 쓰면
// 흰 원 하나가 된다(실측). 원본 색 그대로가 하늘색 위에서 가장 잘 읽힌다.
const code = `(async () => {
  const img = new Image()
  img.src = ${JSON.stringify(logo)}
  await img.decode()
  const out = {}
  for (const S of [16, 32, 48, 128]) {
    const lw = Math.round(S * 0.66), lh = Math.round(lw * img.height / img.width)
    const lc = new OffscreenCanvas(lw, lh), lx = lc.getContext('2d')
    lx.drawImage(img, 0, 0, lw, lh)

    const c = new OffscreenCanvas(S, S), x = c.getContext('2d')
    x.fillStyle = ${JSON.stringify(sky)}
    x.beginPath(); x.roundRect(0, 0, S, S, S * 0.22); x.fill()
    x.drawImage(lc, Math.round((S - lw) / 2), Math.round((S - lh) / 2))

    const buf = await (await c.convertToBlob({ type: 'image/png' })).arrayBuffer()
    let s = ''
    for (const b of new Uint8Array(buf)) s += String.fromCharCode(b)
    out[S] = btoa(s)
  }
  return out
})()`

const sock = new WebSocket(`ws://127.0.0.1:${PORT}`)
sock.on('open', () => {
  sock.send(JSON.stringify({ type: 'hello', role: 'client', identity: { name: 'icons', paneId: '#icons' } }))
  sock.send(JSON.stringify({ type: 'call', id: 1, tool: 'eval_js', args: { tabId: Number(process.env.TAB), code } }))
})
sock.on('message', (raw) => {
  const m = JSON.parse(raw.toString())
  if (m.type !== 'result') return
  if (!m.ok) { console.error(m.error); process.exit(1) }
  const png = m.result?.value
  if (!png || !Object.keys(png).length) { console.error('빈 결과 — 탭이 살아 있는지 확인하세요'); process.exit(1) }
  mkdirSync(outDir, { recursive: true })
  for (const [size, b64] of Object.entries(png)) {
    writeFileSync(`${outDir}/icon-${size}.png`, Buffer.from(b64, 'base64'))
    console.log(`icon-${size}.png`)
  }
  process.exit(0)
})
