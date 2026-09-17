// 사람이 지금 어디를 보고 있나 — 카사텀 설정(하단바·설정·폰 허브의 「브라우저 기기」)의
// `open_url_target`. 「폰」이면 이 크롬의 탭·창은 사람 눈앞에 없고, 보여 줄 페이지는
// 카사텀 `/open-url` 을 거쳐 폰 쪽지로 간다(localhost 는 카사텀이 임시 터널로 바깥 주소를
// 만들어 넣는다). 학생이 이 사실을 모르면 「탭을 열어 뒀어요」로 끝나므로, 도구 응답마다
// 알려 주고 링크를 돌려준다(2026-09-17 지시).
import { sourceBase } from './localhost-route.mjs'

export const PHONE_NOTE = '사람은 지금 폰을 보고 있어 이 크롬의 탭·창이 눈에 안 보입니다. 보여 줄 페이지는 browser_show_human 으로 보내고, 돌려받은 링크를 답장에 그대로 적으세요.'

export function humanOnPhone(settings) {
  return typeof settings?.open_url_target === 'string' && settings.open_url_target.trim() === 'phone'
}

// 탭·창을 여는 도구의 응답에 붙인다 — 배열·문자열 응답도 잃지 않고 감싼다.
export function withHumanNote(result, onPhone) {
  if (!onPhone) return result
  const base = result && typeof result === 'object' && !Array.isArray(result) ? result : { result }
  return { ...base, humanOnPhone: true, note: PHONE_NOTE }
}

// 카사텀의 `open` 과 같은 길(`GET /open-url`). 폰이면 쪽지+푸시로 가고 바깥 주소가 돌아온다.
// 아니면 설정에서 고른 기기의 브라우저에서 연다. 터널을 새로 세우면 수십 초라 넉넉히 기다린다.
export async function showHuman(url, options = {}) {
  if (typeof url !== 'string' || !/^https?:\/\//.test(url)) throw new Error('SHOW_HUMAN_BAD_URL: http(s) 주소만 보낼 수 있습니다.')
  const env = options.env || process.env
  const base = options.base || sourceBase(env)
  const request = options.fetch || globalThis.fetch
  const endpoint = new URL('/open-url', base)
  endpoint.searchParams.set('url', url)
  endpoint.searchParams.set('pane', env.KASATERM_PANE_ID || '')
  const response = await request(endpoint, { signal: AbortSignal.timeout(70000) })
  const value = await response.json()
  if (!response.ok || value.ok !== true) {
    throw new Error(`SHOW_HUMAN_FAILED: ${value?.error || '카사텀이 페이지를 열지 못했습니다.'}`)
  }
  const target = typeof value.target === 'string' ? value.target : null
  const shown = typeof value.url === 'string' && value.url ? value.url : url
  const phone = target === 'phone'
  return {
    ok: true,
    target,
    url: shown,
    note: phone
      ? `폰 쪽지로 보냈습니다. 답장에 이 링크를 그대로 적으세요: ${shown}`
      : `${target ? `${target} 기기` : '이 기기'} 브라우저에서 열었습니다.`,
    ...(value.tunnel_error ? { tunnelError: value.tunnel_error } : {}),
  }
}
