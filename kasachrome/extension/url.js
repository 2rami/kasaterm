// 주소를 사람이 읽는 한 조각으로 줄인다.
// 활동 로그(background)와 탭 목록(팝업·패널)이 같은 표기를 쓰도록 여기 하나만 둔다 —
// 두 벌로 두었더니 한쪽만 고쳐져 화면에 확장 ID 가 그대로 샜다.
export function hostOf(url) {
  try {
    const u = new URL(url)
    // 확장·로컬 파일은 host 가 사람이 못 읽는 ID 이거나(확장) 비어 있다(파일) — 파일명이 낫다
    if (u.protocol === 'chrome-extension:' || u.protocol === 'file:') {
      return decodeURIComponent(u.pathname.split('/').pop() || '') || u.pathname
    }
    return u.host || u.href
  } catch {
    return String(url || '').slice(0, 40)
  }
}

// 같은 주소를 두 번 열지 않기 위한 비교 키. 끝 슬래시만 무시하고 쿼리·해시는 그대로 본다 —
// SPA 에서 해시는 화면을 가르는 라우트라 지워버리면 다른 페이지를 같은 것으로 읽는다.
export function sameUrlKey(url) {
  return String(url || '').replace(/\/+$/, '')
}

// 이미 열려 있는 같은 주소의 탭 하나. **내가 연 탭을 먼저** 고른다 — 사람 탭은 돌려주더라도 내
// 몫으로 삼지 않으므로(그룹에 넣지 않고 닫을 몫으로도 안 센다), 고를 수 있으면 내 것이 언제나 낫다.
// 빈 새 탭은 주소가 아니라 상태라 재사용 대상이 아니다.
export function pickExistingTab(tabs, url, mine = new Set()) {
  const key = sameUrlKey(url)
  if (!key || key === 'about:blank') return null
  const hits = (tabs || []).filter((t) => sameUrlKey(t.url) === key)
  return hits.find((t) => mine.has(t.id)) || hits[0] || null
}
