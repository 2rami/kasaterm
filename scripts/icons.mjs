#!/usr/bin/env node
// 툴바 아이콘 png 4종을 만든다. 유지보수자만 돌리고 결과는 레포에 커밋되므로,
// 확장을 쓰는 사람은 이 스크립트를 실행할 일이 없다.
//
// SVG 를 거치지 않고 직접 래스터화하는 이유: 이 맥에는 cairosvg 가 설치돼 있어도 libcairo 가 없어
// 실행 순간에만 깨졌고, 유일하게 남은 qlmanage 는 썸네일이라 여백을 넣어 그림이 좌상단에 몰렸다.
// 도형이 셋뿐이라 zlib(내장)만으로 정확히 그리는 편이 남의 맥에서도 확실하다.
import { deflateSync } from 'node:zlib'
import { writeFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'

const DIR = join(dirname(dirname(fileURLToPath(import.meta.url))), 'extension', 'icons')
const SIZES = [16, 32, 48, 128]

// 사람이 화면에서 보는 것의 미니어처 — 페이지 뷰포트 테두리와 우상단 칩. 16px 에서 살아남게 도형 셋만.
// 자체 배경을 까는 이유: MV3 action 아이콘은 테마별 변형을 못 받아서 투명 선화는 다크 툴바에서 사라진다.
const BG = [0x2b, 0x32, 0x42]
const LINE = [0xe8, 0xec, 0xf2]
const CHIP = [0x4c, 0x8d, 0xff]

const clamp01 = (v) => (v < 0 ? 0 : v > 1 ? 1 : v)

function sdRoundRect(px, py, cx, cy, hw, hh, r) {
  const qx = Math.abs(px - cx) - (hw - r)
  const qy = Math.abs(py - cy) - (hh - r)
  return Math.min(Math.max(qx, qy), 0) + Math.hypot(Math.max(qx, 0), Math.max(qy, 0)) - r
}

const sdCircle = (px, py, cx, cy, r) => Math.hypot(px - cx, py - cy) - r

// sdf 한 픽셀 폭으로 부드럽게. 커버리지를 알파로 쓰면 별도 슈퍼샘플링이 필요 없다.
const cover = (d, k) => clamp01(0.5 - d / k)

function over(dst, i, rgb, a) {
  if (a <= 0) return
  for (let c = 0; c < 3; c++) dst[i + c] = Math.round(dst[i + c] * (1 - a) + rgb[c] * a)
  dst[i + 3] = Math.round(dst[i + 3] * (1 - a) + 255 * a)
}

function draw(size) {
  const s = size / 128
  const px = new Uint8Array(size * size * 4)
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      const i = (y * size + x) * 4
      const u = (x + 0.5) / s
      const v = (y + 0.5) / s
      const k = 1 / s // 픽셀 하나가 원본 좌표계에서 차지하는 폭

      over(px, i, BG, cover(sdRoundRect(u, v, 64, 64, 64, 64, 28), k))
      // 링은 라운드 사각형 윤곽의 절대값으로 낸다 — 획 굵기 8
      over(px, i, LINE, cover(Math.abs(sdRoundRect(u, v, 64, 64, 40, 34, 12)) - 4, k))
      // 칩이 링 위에 겹치므로 배경색으로 한 번 뚫고 그 안에 파란 점
      over(px, i, BG, cover(sdCircle(u, v, 100, 32, 20.5), k))
      over(px, i, CHIP, cover(sdCircle(u, v, 100, 32, 17), k))
    }
  }
  return px
}

const CRC = (() => {
  const t = new Int32Array(256)
  for (let n = 0; n < 256; n++) {
    let c = n
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1
    t[n] = c
  }
  return t
})()

function crc32(buf) {
  let c = -1
  for (const b of buf) c = CRC[(c ^ b) & 0xff] ^ (c >>> 8)
  return (c ^ -1) >>> 0
}

function chunk(type, data) {
  const len = Buffer.alloc(4)
  len.writeUInt32BE(data.length)
  const body = Buffer.concat([Buffer.from(type, 'ascii'), data])
  const crc = Buffer.alloc(4)
  crc.writeUInt32BE(crc32(body))
  return Buffer.concat([len, body, crc])
}

function png(size, rgba) {
  const ihdr = Buffer.alloc(13)
  ihdr.writeUInt32BE(size, 0)
  ihdr.writeUInt32BE(size, 4)
  ihdr[8] = 8 // bit depth
  ihdr[9] = 6 // truecolor + alpha
  // 각 행 앞에 filter byte(0 = None)가 붙는다
  const raw = Buffer.alloc(size * (size * 4 + 1))
  for (let y = 0; y < size; y++) {
    raw[y * (size * 4 + 1)] = 0
    Buffer.from(rgba.buffer, y * size * 4, size * 4).copy(raw, y * (size * 4 + 1) + 1)
  }
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr),
    chunk('IDAT', deflateSync(raw, { level: 9 })),
    chunk('IEND', Buffer.alloc(0)),
  ])
}

for (const size of SIZES) {
  writeFileSync(join(DIR, `icon-${size}.png`), png(size, draw(size)))
  console.log(`icon-${size}.png`)
}
