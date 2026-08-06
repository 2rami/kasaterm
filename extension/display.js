// 오버레이를 어디에 어떻게 보일지. 신원별이 아니라 브라우저 전역 한 벌이다 —
// 가리는 건 학생이 아니라 "이 자리"라서, 학생이 바뀔 때마다 다시 치우게 만들면 설정이 아니다.
// service worker 가 유휴 30초에 죽으면서 메모리를 잃으므로 저장소가 정본이고 캐시는 그 사본이다.
// dx·dy 는 고른 모서리에서 칩이 떨어진 거리(px)다. 사람이 칩을 끌어 옮기면 여기가 바뀐다 —
// 절대 좌표로 두지 않는 이유는 창 크기가 바뀔 때 칩이 화면 밖으로 밀려나기 때문이다.
// 기본값 12 는 content.js 의 CHIP_EDGE 와 같은 값이다(모듈을 공유하지 않아 각자 둔다).
const DEFAULTS = { off: false, frame: true, chip: true, cursor: true, pos: 'tr', dx: 12, dy: 12 }
const POSITIONS = new Set(['tl', 'tr', 'bl', 'br'])
// 창을 줄였다 키우는 사이 칩이 아득히 밖으로 나가 있지 않게 하는 상한.
const MAX_OFFSET = 4000

let cache = null

export async function getDisplay() {
  if (cache) return cache
  let saved = null
  try { saved = (await chrome.storage.local.get('display')).display } catch { /* 기본값으로 간다 */ }
  cache = { ...DEFAULTS, ...(saved || {}) }
  if (!POSITIONS.has(cache.pos)) cache.pos = DEFAULTS.pos
  for (const k of ['dx', 'dy']) if (!Number.isFinite(cache[k])) cache[k] = DEFAULTS[k]
  return cache
}

async function commit(patch) {
  const cur = await getDisplay()
  const next = { ...cur }
  for (const k of ['off', 'frame', 'chip', 'cursor']) if (k in patch) next[k] = !!patch[k]
  if (patch.pos && POSITIONS.has(patch.pos)) next.pos = patch.pos
  // 값은 페이지 안에서 도는 content script 가 보낸다 — 숫자가 아니면 버린다.
  for (const k of ['dx', 'dy']) {
    if (!(k in patch)) continue
    const n = Number(patch[k])
    if (Number.isFinite(n)) next[k] = Math.max(0, Math.min(MAX_OFFSET, Math.round(n)))
  }
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
