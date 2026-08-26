// 크롬 DevTools 의 Device Toolbar 기기 목록. 이름은 DevTools 표시명을 그대로 정규화한 것이라
// (`iPhone 14 Pro Max` → `iphone-14-pro-max`) 사람이 DevTools 에서 고르던 이름을 그대로 쓴다.
//
// ★크기·dpr 만으로는 기기 흉내가 안 된다. 서버에서 모바일 뷰를 고르는 페이지는 UA 로 가르므로
// 크기만 바꾸면 **데스크톱 HTML 이 폰 폭에 들어간 화면**을 보게 되고, 그건 실제 폰에서 나올 화면이
// 아니다. 그래서 기기마다 UA 와 client hints(Sec-CH-UA-*)까지 함께 둔다.

// UA 템플릿. %v = 이 크롬의 버전, %m = 기기 모델명. DevTools 도 같은 자리표시자 방식을 쓴다 —
// 버전을 문자열에 박아두면 크롬이 올라갈 때마다 조용히 옛 버전을 주장하게 된다.
const UA = {
  ios: 'Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1',
  ipados: 'Mozilla/5.0 (iPad; CPU OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1',
  android: 'Mozilla/5.0 (Linux; Android 13; %m) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/%v Mobile Safari/537.36',
  'android-tablet': 'Mozilla/5.0 (Linux; Android 13; %m) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/%v Safari/537.36',
  windows: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/%v Safari/537.36',
  chromeos: 'Mozilla/5.0 (X11; CrOS aarch64 14541.0.0) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/%v Safari/537.36',
}

// navigator.platform 값. UA 만 바꾸고 이걸 두면 데스크톱 macOS 가 그대로 남아 어긋난다.
const PLATFORM = {
  ios: 'iPhone', ipados: 'iPad',
  android: 'Linux armv8l', 'android-tablet': 'Linux armv8l',
  windows: 'Win32', chromeos: 'Linux aarch64',
}

// Sec-CH-UA-Platform 값. iOS/iPadOS Safari 는 client hints 를 아예 안 보내므로 brands 를 비운다.
const CH_PLATFORM = {
  ios: 'iOS', ipados: 'iOS',
  android: 'Android', 'android-tablet': 'Android',
  windows: 'Windows', chromeos: 'Chrome OS',
}
const CH_VERSION = {
  ios: '17.0', ipados: '17.0',
  android: '13.0.0', 'android-tablet': '13.0.0',
  windows: '10.0.0', chromeos: '14541.0.0',
}

// mobile: CDP setDeviceMetricsOverride 의 mobile 플래그(뷰포트 메타·스크롤바 동작이 갈린다).
// touch: 터치 에뮬레이션. 둘은 별개다 — Surface Pro·Nest Hub 는 데스크톱이지만 손가락이 닿는다.
// os: 위 표의 키. model: Android UA 에 박히는 기기명이자 Sec-CH-UA-Model 값.
const LIST = [
  // ── iPhone ────────────────────────────────────────────────────────────────
  { name: 'iPhone SE', width: 375, height: 667, dsf: 2, os: 'ios', model: 'iPhone', mobile: true, touch: true, aliases: ['iphone-se'] },
  { name: 'iPhone XR', width: 414, height: 896, dsf: 2, os: 'ios', model: 'iPhone', mobile: true, touch: true, aliases: ['iphone-xr', 'iphone-11'] },
  { name: 'iPhone 12 Pro', width: 390, height: 844, dsf: 3, os: 'ios', model: 'iPhone', mobile: true, touch: true, aliases: ['phone', 'iphone', 'iphone-12', 'iphone-13', 'iphone-14'] },
  { name: 'iPhone 14 Pro Max', width: 430, height: 932, dsf: 3, os: 'ios', model: 'iPhone', mobile: true, touch: true, aliases: ['iphone-14-pro-max', 'iphone-pro-max', 'iphone-15-pro-max', 'iphone-16-pro-max'] },
  { name: 'iPhone 15 Pro', width: 393, height: 852, dsf: 3, os: 'ios', model: 'iPhone', mobile: true, touch: true, aliases: ['iphone-15-pro', 'iphone-15', 'iphone-16'] },

  // ── Android 폰 ────────────────────────────────────────────────────────────
  { name: 'Pixel 7', width: 412, height: 915, dsf: 2.625, os: 'android', model: 'Pixel 7', mobile: true, touch: true, aliases: ['pixel', 'pixel-7', 'pixel-8'] },
  { name: 'Samsung Galaxy S8+', width: 360, height: 740, dsf: 4, os: 'android', model: 'SM-G955U', mobile: true, touch: true, aliases: ['galaxy-s8-plus', 's8-plus'] },
  { name: 'Samsung Galaxy S20 Ultra', width: 412, height: 915, dsf: 3.5, os: 'android', model: 'SM-G988B', mobile: true, touch: true, aliases: ['galaxy-s20-ultra', 's20-ultra', 'galaxy-s'] },
  { name: 'Samsung Galaxy A51/71', width: 412, height: 914, dsf: 2.625, os: 'android', model: 'SM-A515F', mobile: true, touch: true, aliases: ['galaxy-a51', 'galaxy-a71'] },
  { name: 'Moto G Power', width: 412, height: 823, dsf: 1.75, os: 'android', model: 'moto g power', mobile: true, touch: true, aliases: ['moto-g-power', 'moto-g'] },

  // ── 폴더블 ────────────────────────────────────────────────────────────────
  // 접힌 상태와 펼친 상태는 아예 다른 레이아웃이 걸린다 — 한쪽만 보고 「폴드 확인」이라 하지 말 것.
  { name: 'Galaxy Z Fold 5', width: 344, height: 882, dsf: 2.625, os: 'android', model: 'SM-F946B', mobile: true, touch: true, aliases: ['galaxy-z-fold-5', 'z-fold', 'z-fold-5', 'fold'] },
  { name: 'Galaxy Z Fold 5 Unfolded', width: 673, height: 841, dsf: 2.625, os: 'android', model: 'SM-F946B', mobile: true, touch: true, aliases: ['galaxy-z-fold-5-unfolded', 'z-fold-unfolded', 'fold-unfolded'] },
  { name: 'Asus Zenbook Fold', width: 853, height: 1280, dsf: 2, os: 'android-tablet', model: 'Zenbook Fold', mobile: true, touch: true, aliases: ['asus-zenbook-fold', 'zenbook-fold'] },
  { name: 'Surface Duo', width: 540, height: 720, dsf: 2.5, os: 'android', model: 'Surface Duo', mobile: true, touch: true, aliases: ['surface-duo'] },

  // ── iPad ──────────────────────────────────────────────────────────────────
  { name: 'iPad Mini', width: 768, height: 1024, dsf: 2, os: 'ipados', model: 'iPad', mobile: true, touch: true, aliases: ['tablet', 'ipad', 'ipad-mini'] },
  { name: 'iPad Air', width: 820, height: 1180, dsf: 2, os: 'ipados', model: 'iPad', mobile: true, touch: true, aliases: ['ipad-air'] },
  { name: 'iPad Pro 11', width: 834, height: 1194, dsf: 2, os: 'ipados', model: 'iPad', mobile: true, touch: true, aliases: ['ipad-pro-11'] },
  { name: 'iPad Pro 12.9', width: 1024, height: 1366, dsf: 2, os: 'ipados', model: 'iPad', mobile: true, touch: true, aliases: ['ipad-pro', 'ipad-pro-12-9'] },

  // ── 터치 데스크톱 ─────────────────────────────────────────────────────────
  // mobile:false 다. 뷰포트 메타를 안 따르고 스크롤바가 자리를 먹는, 진짜 데스크톱 렌더다.
  { name: 'Surface Pro 7', width: 912, height: 1368, dsf: 2, os: 'windows', model: '', mobile: false, touch: true, aliases: ['surface-pro-7', 'surface-pro', 'surface'] },
  { name: 'Nest Hub', width: 1024, height: 600, dsf: 2, os: 'chromeos', model: '', mobile: false, touch: true, aliases: ['nest-hub'] },
  { name: 'Nest Hub Max', width: 1280, height: 800, dsf: 2, os: 'chromeos', model: '', mobile: false, touch: true, aliases: ['nest-hub-max'] },
]

// `iPad Pro 12.9` · `iphone_14_pro_max` · `Galaxy S8+` 가 전부 같은 키로 떨어지게 한다. 사람이
// DevTools 화면에서 읽은 이름을 그대로 복사해 넣어도 걸리는 것이 목적이다.
export function normalizeName(name) {
  return String(name || '').trim().toLowerCase()
    .replace(/\+/g, '-plus')
    .replace(/[\s_./()]+/g, '-')
    .replace(/-+/g, '-')
    .replace(/^-|-$/g, '')
}

const BY_KEY = new Map()
for (const d of LIST) {
  const keys = new Set([normalizeName(d.name), ...(d.aliases || []).map(normalizeName)])
  d.key = normalizeName(d.name)
  for (const k of keys) if (!BY_KEY.has(k)) BY_KEY.set(k, d)
}

// 가로. 목록을 두 배로 늘리는 대신 접미사로 뒤집는다 — 기기가 늘어도 유지보수가 한 벌로 끝난다.
const LANDSCAPE_SUFFIX = /-(landscape|land|가로)$/

// 이름 하나 → 기기 정의. 없으면 null.
export function lookupDevice(name) {
  const raw = normalizeName(name)
  if (!raw) return null
  const landscape = LANDSCAPE_SUFFIX.test(raw)
  const base = landscape ? raw.replace(LANDSCAPE_SUFFIX, '') : raw
  const dev = BY_KEY.get(base)
  if (!dev) return null
  return {
    ...dev,
    landscape,
    width: landscape ? dev.height : dev.width,
    height: landscape ? dev.width : dev.height,
    resolvedName: landscape ? `${dev.name} (landscape)` : dev.name,
    resolvedKey: landscape ? `${dev.key}-landscape` : dev.key,
  }
}

// 오타·비슷한 이름에 후보를 되돌려 준다. 목록이 20개를 넘으면 전부 나열하는 오류는 못 읽는다.
export function suggestDevices(name, limit = 6) {
  const q = normalizeName(name).replace(LANDSCAPE_SUFFIX, '')
  if (!q) return []
  const parts = q.split('-').filter(Boolean)
  const scored = []
  for (const [key, dev] of BY_KEY) {
    if (key !== dev.key) continue
    let score = 0
    if (key.includes(q) || q.includes(key)) score += 10
    for (const p of parts) if (key.includes(p)) score += 2
    for (const a of [dev.key, ...(dev.aliases || []).map(normalizeName)]) {
      for (const p of parts) if (a.includes(p)) score += 1
    }
    if (score > 0) scored.push({ key, score })
  }
  return scored.sort((a, b) => b.score - a.score).slice(0, limit).map((s) => s.key)
}

// list:true 가 돌려주는 표. 가로가 필요하면 이름 뒤에 -landscape 를 붙인다는 것만 알면 된다.
export function deviceTable() {
  return LIST.map((d) => ({
    name: d.key,
    label: d.name,
    viewport: `${d.width}x${d.height}`,
    dpr: d.dsf,
    mobile: d.mobile,
    touch: d.touch,
    os: d.os,
    aliases: (d.aliases || []).map(normalizeName).filter((a) => a !== d.key),
  }))
}

// 이 크롬의 실제 버전. UA 에 버전을 박아두면 크롬이 올라갈 때마다 옛 버전을 주장하게 된다.
function chromeVersion() {
  const m = /Chrome\/([\d.]+)/.exec(navigator.userAgent || '')
  return m ? m[1] : '131.0.0.0'
}

// CDP Emulation.setUserAgentOverride 에 그대로 넘길 인자. userAgentMetadata 까지 함께 주지 않으면
// UA 문자열만 폰이고 Sec-CH-UA-Mobile/Platform 은 데스크톱 크롬으로 남는다 — 헤더로 가르는 서버는
// 그 어긋난 조합을 보게 된다.
export function uaOverrideFor(dev) {
  const version = chromeVersion()
  const major = version.split('.')[0]
  const template = UA[dev.os]
  if (!template) return null
  const userAgent = template.replace(/%v/g, version).replace(/%m/g, dev.model || 'Android')
  const meta = {
    platform: CH_PLATFORM[dev.os] || '',
    platformVersion: CH_VERSION[dev.os] || '',
    architecture: dev.os === 'windows' ? 'x86' : '',
    model: dev.model || '',
    mobile: !!dev.mobile,
    // Safari 는 client hints 를 아예 안 보낸다. brands 를 비워야 iOS 흉내가 실제와 같아진다.
    brands: dev.os === 'ios' || dev.os === 'ipados' ? [] : [
      { brand: 'Not_A Brand', version: '8' },
      { brand: 'Chromium', version: major },
      { brand: 'Google Chrome', version: major },
    ],
    fullVersion: dev.os === 'ios' || dev.os === 'ipados' ? '' : version,
  }
  return { userAgent, platform: PLATFORM[dev.os] || '', userAgentMetadata: meta }
}
