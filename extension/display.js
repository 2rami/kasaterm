// 오버레이를 어디에 어떻게 보일지. 신원별이 아니라 브라우저 전역 한 벌이다 —
// 가리는 건 학생이 아니라 "이 자리"라서, 학생이 바뀔 때마다 다시 치우게 만들면 설정이 아니다.
// service worker 가 유휴 30초에 죽으면서 메모리를 잃으므로 저장소가 정본이고 캐시는 그 사본이다.
const DEFAULTS = { off: false, frame: true, chip: true, cursor: true, pos: 'tr' }
const POSITIONS = new Set(['tl', 'tr', 'bl', 'br'])

let cache = null

export async function getDisplay() {
  if (cache) return cache
  let saved = null
  try { saved = (await chrome.storage.local.get('display')).display } catch { /* 기본값으로 간다 */ }
  cache = { ...DEFAULTS, ...(saved || {}) }
  if (!POSITIONS.has(cache.pos)) cache.pos = DEFAULTS.pos
  return cache
}

async function commit(patch) {
  const cur = await getDisplay()
  const next = { ...cur }
  for (const k of ['off', 'frame', 'chip', 'cursor']) if (k in patch) next[k] = !!patch[k]
  if (patch.pos && POSITIONS.has(patch.pos)) next.pos = patch.pos
  cache = next
  try {
    await chrome.storage.local.set({ display: next })
  } catch (e) {
    // 조용히 삼키면 캐시와 저장이 갈린 채 화면만 맞아 보인다. status 로 꺼내 볼 수 있게 남긴다.
    self.__ccLastError = { where: 'display', at: Date.now(), msg: String((e && e.message) || e) }
  }
  return next
}

// ⚠️저장은 반드시 한 줄로 세운다. 토글을 연달아 누르면 storage.set 이 겹치는데 완료 순서가
// 호출 순서와 같지 않아, 먼저 보낸 요청이 나중에 끝나면 옛 값이 최종본으로 남는다. 캐시는
// 마지막 요청 값이라 화면은 멀쩡하고, 확장이 재시작해 저장소를 다시 읽는 순간에야 어긋남이
// 드러난다 — 무증상이라 실측으로만 잡힌다(2026-08-05, 두 토글이 900ms 안에 겹쳐 재현).
let chain = Promise.resolve()

export function setDisplay(patch = {}) {
  const run = chain.then(() => commit(patch))
  // 앞선 실패에 뒤따르는 요청이 물려 죽지 않게 체인은 삼킨 것으로 잇는다.
  chain = run.catch(() => {})
  return run
}
