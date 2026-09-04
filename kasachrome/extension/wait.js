// 답이 영영 오지 않는 비동기 하나가 도구 전체를 침묵시키지 않게 하는 최소 장치.
// 침묵은 재시도도 우회도 못 하게 만든다 — 부르는 쪽에는 「안 된다」는 말조차 안 간다.
// ⚠️먼저 끝난 쪽에서 타이머를 반드시 걷는다. 안 걷으면 service worker 가 그만큼 더 깨어 있다.
export function withTimeout(promise, ms, message) {
  let timer
  return Promise.race([
    Promise.resolve(promise).finally(() => clearTimeout(timer)),
    new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(message)), ms) }),
  ])
}
