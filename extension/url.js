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
