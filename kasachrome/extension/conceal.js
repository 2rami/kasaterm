// 로그인으로 채운 비밀번호가 다른 도구의 결과로 새어 나가지 않게 한다. 칸에 들어간 값은 eval_js·cdp_raw 로
// 읽히고, 제출하면 네트워크 요청 본문에도 실린다 — 그 결과가 그대로 모델에게 가면 채우기를 따로 둔 의미가 없다.
// 실수로 새는 것을 막을 뿐, 값을 비틀어 꺼내는 스크립트(글자 사이 띄우기 따위)까지는 못 막는다.
// 값은 이 워커 메모리에만 있고 워커가 다시 뜨면 잊는다.
export const MASK = '<concealed>'
const HOLD_MS = 30 * 60 * 1000
// 짧은 값은 평범한 글자와 겹쳐 엉뚱한 글을 지운다.
const MIN_LENGTH = 4
// 스크린샷 같은 base64 덩어리는 건너뛴다 — 우연히 겹친 곳을 지우면 그림이 깨진다.
const BLOB_MIN = 20000
const BLOB = /^[A-Za-z0-9+/=\r\n]+$/

const held = new Map()

export function remember(secret, now = Date.now()) {
  if (typeof secret === 'string' && secret.length >= MIN_LENGTH) held.set(secret, now + HOLD_MS)
}

// 페이지·요청 본문에 실리는 꼴: 그대로, JSON 문자열 안, URL·폼 인코딩.
function formsOf(secret) {
  const url = encodeURIComponent(secret)
  return [secret, JSON.stringify(secret).slice(1, -1), url, url.replace(/%20/g, '+')]
}

function activeForms(now) {
  const forms = new Set()
  for (const [secret, until] of held) {
    if (until <= now) held.delete(secret)
    else for (const form of formsOf(secret)) forms.add(form)
  }
  return [...forms].sort((a, b) => b.length - a.length)
}

export function conceal(value, now = Date.now()) {
  if (!held.size) return value
  const forms = activeForms(now)
  if (!forms.length) return value
  const scrub = (text) => {
    if (text.length >= BLOB_MIN && BLOB.test(text)) return text
    for (const form of forms) if (text.includes(form)) text = text.split(form).join(MASK)
    return text
  }
  const walk = (v) => {
    if (typeof v === 'string') return scrub(v)
    if (Array.isArray(v)) return v.map(walk)
    if (v && typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, walk(x)]))
    return v
  }
  return walk(value)
}
