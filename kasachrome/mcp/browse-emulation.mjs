// 하단바 「모바일」에서 폰을 골랐으면(docs/browse-target.md) 학생이 여는 새 탭도
// 그 폰 크기로 — 매번 「아이폰으로 봐 줘」라고 말하지 않아도 되게. 설정 파일의
// `browse_device`(`phone:<이름>`) 와 `browse_viewport`({width,height,dpr}) 를 읽는다.
// 사람이 볼 페이지의 목적지(browse_open)는 여기서 안 본다 — 이건 학생 확인용
// 크롬의 모양일 뿐, 페이지가 어디로 가는지는 앱이 정한다.
export function browseEmulation(settings) {
  const device = typeof settings?.browse_device === 'string' ? settings.browse_device : ''
  if (!device.startsWith('phone:')) return null
  const vp = settings?.browse_viewport
  const width = Number(vp?.width), height = Number(vp?.height)
  if (!(width > 0) || !(height > 0)) {
    // 폰이 아직 화면을 안 알렸으면 흔한 아이폰 크기.
    return { width: 393, height: 852, deviceScaleFactor: 3, mobile: true, touch: true, fit: true }
  }
  const dpr = Number(vp?.dpr)
  return { width: Math.round(width), height: Math.round(height),
    deviceScaleFactor: dpr > 0 ? dpr : 2, mobile: vp?.mobile !== false, touch: true, fit: true }
}
