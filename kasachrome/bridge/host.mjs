// 이 브리지가 도는 기계를 사람이 알아볼 이름으로. 확장은 샌드박스라 호스트명을 못 읽고,
// 확장이 붙을 수 있는 브리지는 **자기 기계의 것뿐**이므로(extension/background.js 의
// BRIDGE_URL 이 127.0.0.1 하드코딩) 브리지의 기계 = 거기 붙은 모든 크롬의 기계다.
// MCP 는 KASACHROME_BRIDGE_URLS 로 터널 너머 남의 브리지에도 붙으니, 이 값이 없으면
// 「지금 조작하는 크롬이 어느 기계 것인지」를 알 길이 아예 없다.
import { hostname } from 'node:os'
import { execFileSync } from 'node:child_process'
import { readFileSync } from 'node:fs'
import { createHash } from 'node:crypto'

function sh(cmd, args) {
  try {
    return execFileSync(cmd, args, {
      encoding: 'utf8', timeout: 2000, stdio: ['ignore', 'pipe', 'ignore'],
    }).trim()
  } catch { return '' }
}

// 사람이 시스템 설정에서 붙인 이름을 먼저 본다. `hostname()` 은 같은 이름에서 파생되지만
// 공백이 하이픈으로 바뀌고 DHCP·네트워크에 따라 `-2` 가 붙는 등 흔들린다 — 화면에 뜨는
// 이름이 그날그날 달라지면 「어느 기계」를 알아보려는 목적이 무너진다.
function readableHost() {
  const forced = (process.env.KASACHROME_HOST || '').trim()
  if (forced) return forced
  if (process.platform === 'darwin') {
    const pretty = sh('scutil', ['--get', 'ComputerName'])
    if (pretty) return pretty
  }
  return hostname().replace(/\.local$/, '') || 'unknown'
}

// 이름이 겹치는 기계가 둘 있을 때 갈라 보는 값. 원본(하드웨어 UUID·machine-id)은 기계 지문이라
// 그대로 싣지 않고 해시해서 앞 8자만 쓴다 — 같으냐 다르냐만 판정하면 되는 값이다.
function machineId() {
  let raw = ''
  if (process.platform === 'darwin') {
    const out = sh('ioreg', ['-rd1', '-c', 'IOPlatformExpertDevice'])
    raw = (out.match(/"IOPlatformUUID"\s*=\s*"([^"]+)"/) || [])[1] || ''
  } else if (process.platform === 'linux') {
    for (const p of ['/etc/machine-id', '/var/lib/dbus/machine-id']) {
      try { raw = readFileSync(p, 'utf8').trim(); if (raw) break } catch {}
    }
  } else if (process.platform === 'win32') {
    const out = sh('reg', ['query', 'HKLM\\SOFTWARE\\Microsoft\\Cryptography', '/v', 'MachineGuid'])
    raw = (out.match(/MachineGuid\s+REG_SZ\s+(\S+)/) || [])[1] || ''
  }
  // 기계 고유값을 못 얻는 자리에서도 값은 있어야 한다. 이름이 겹치면서 이 길로 떨어진
  // 두 기계는 구분이 안 되지만, 그때는 KASACHROME_HOST 로 사람이 이름을 갈라 주면 된다.
  return createHash('sha256').update(raw || hostname()).digest('hex').slice(0, 8)
}

// 부팅 때 한 번만. 매 목록 조회마다 프로세스를 띄울 값이 아니다.
export const HOST = readableHost()
export const HOST_ID = machineId()
