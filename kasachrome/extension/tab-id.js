// 비었다 = 활성 탭. 크롬 탭 id 는 양의 정수라 0 은 빈 값이 숫자로 잘못 바뀐 것이다(`Number("")`) —
// 그걸 「없음」으로 보면 사람의 활성 탭을 찍고 닫는다(2026-10-01·10-12 실제로 닫혔다).
export function explicitTabId(tabId) {
  if (tabId === undefined || tabId === null) return null
  const id = Number(tabId)
  if (tabId === '' || typeof tabId === 'boolean' || !Number.isInteger(id) || id <= 0) {
    throw new Error(`BAD_TAB_ID: tabId ${JSON.stringify(tabId)} 는 탭 번호가 아닙니다. list_tabs 로 확인한 양의 정수를 주세요(활성 탭을 쓰려면 tabId 를 빼세요).`)
  }
  return id
}
