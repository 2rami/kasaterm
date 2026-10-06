// 전사본에 이름은 두 갈래로 남는다 — 사람·kasaterm 이 붙인 것(`/rename`, tell --title, summon --name)은
// custom-title, claude 가 스스로 지은 요약은 ai-title. kasaterm 입력박스 배지는 custom-title 만 띄운다.
export type Titles = { custom: string; ai: string }

export const NAME_MAX = 24

export function titlesIn(text: string): Titles {
  const found: Titles = { custom: '', ai: '' }
  for (const line of text.split('\n')) {
    if (!line.includes('-title"')) continue
    let v: { type?: unknown; customTitle?: unknown; aiTitle?: unknown }
    try {
      v = JSON.parse(line)
    } catch {
      continue
    }
    if (v.type === 'custom-title' && typeof v.customTitle === 'string') found.custom = v.customTitle
    if (v.type === 'ai-title' && typeof v.aiTitle === 'string') found.ai = v.aiTitle
  }
  return found
}

// 모델은 시켜도 따옴표·「이름:」·마침표를 붙여 오곤 한다.
export function tidy(raw: string): string {
  const line = raw.split('\n').map(s => s.trim()).find(s => s.length > 0) ?? ''
  const bare = line
    .replace(/^[#>*\-\s]+/, '')
    .replace(/\*\*/g, '')
    .replace(/^(이름|제목|세션 이름)\s*[:：]\s*/, '')
    .replace(/^["'`「『]+|["'`」』.。]+$/g, '')
    .trim()
  return [...bare].slice(0, NAME_MAX).join('')
}

// 모델이 이름 대신 자기 얘기(「저는 …」)·사과·시킨 글의 베낌(「작업 세션 이름」)을 내면 이름이 아니다 —
// 버리고 다음 말에서 다시 짓는다.
export function usable(name: string): boolean {
  if (!name) return false
  return !/세션 이름|작업 이름|이름 한 줄|에이전트|\bAI\b|저는|제가|죄송|할 수 없|모르겠/.test(name)
}
