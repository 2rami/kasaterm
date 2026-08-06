#!/usr/bin/env node
// 툴바 아이콘 png 4종을 만든다. 유지보수자만 돌리고 결과는 레포에 커밋되므로,
// 확장을 쓰는 사람은 이 스크립트를 실행할 일이 없다.
//
// 원본을 레포 안에 두는 이유: kasaterm 앱 레포(tmuxify) 경로를 참조하면 그 레포가 없는 맥에서 깨진다.
// 리사이즈를 sips 로 하는 이유: 픽셀아트 원본이라 도형으로 다시 그릴 수 없는데, 이 맥에는
// libcairo 가 없어 cairosvg 계열이 실행 순간에만 깨졌다. sips 는 macOS 기본 탑재라 의존이 없다.
import { execFileSync } from 'node:child_process'
import { existsSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'

const ROOT = dirname(dirname(fileURLToPath(import.meta.url)))
const SRC = join(ROOT, 'assets', 'appicon.png')
const DIR = join(ROOT, 'extension', 'icons')
const SIZES = [16, 32, 48, 128]

if (!existsSync(SRC)) throw new Error(`${SRC} 가 없습니다`)

for (const size of SIZES) {
  execFileSync('sips', ['-Z', String(size), SRC, '--out', join(DIR, `icon-${size}.png`)], { stdio: 'ignore' })
  console.log(`icon-${size}.png`)
}
