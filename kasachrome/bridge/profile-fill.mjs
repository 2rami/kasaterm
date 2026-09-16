// ★프사는 MCP 가 **도는 기계**의 파일에서 읽어 실려 온다. 그래서 다른 기기의 학생이 터널 너머로
// 이 크롬을 조작하면 그 기계에는 이 테마의 그림이 없어 빈 채로 오고, 페이지 위 칩에 얼굴 대신
// 첫 글자 모노그램만 뜬다 — 화면을 보는 사람이 누가 만지는지 얼굴로 못 알아본다(2026-09-17 지시).
// 브리지는 **크롬과 같은 기계**에서 도니 여기엔 그림이 있다. 그러니 브리지가 채운다.
//
// 비어 있을 때만 채우는 것이 요점이다 — 보낸 쪽이 자기 그림을 실어 보냈으면 그게 정본이고,
// 덮어쓰면 그 기계에서 고른 테마가 무시된다. 이름으로 찾으므로 두 기계가 같은 이름을 다른 테마로
// 쓰면 이쪽 얼굴이 뜨는데, 그건 받아들인다: 이 크롬을 보는 사람의 명부가 이 기계 것이고,
// 남의 테마 얼굴보다 아무 얼굴도 없는 쪽이 더 나쁘다.
export function withLocalProfile(identity, lookup) {
  if (!identity || identity.profile || typeof lookup !== 'function' || !identity.name) return identity
  let found = null
  // 명부를 못 읽어도 신원 자체는 절대 죽이지 않는다 — 신원이 null 이 되면 오버레이·활동 로그·
  // 툴바 아이콘이 통째로 죽고, 툴은 멀쩡히 도는데 UI 만 고장난 것처럼 보인다.
  try { found = lookup(identity.name) } catch { return identity }
  if (!found?.profile) return identity
  return { ...identity, profile: found.profile, slug: identity.slug || found.slug || null }
}
