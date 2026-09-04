// content script 와 말하는 유일한 통로. tools 와 students 양쪽이 쓰므로 여기 한 곳에 둔다
// (students 가 tools 를 import 하면 순환이 된다).
import { withTimeout } from './wait.js'

const CONTENT_FILE = 'content.js'

// ★이 왕복은 페이지의 **메인 스레드**가 돌아야 답이 온다. 그 스레드가 막히면
// `chrome.tabs.sendMessage` 는 콜백을 영영 부르지 않는다 — 오류도 안 나고 그냥 조용하다.
// 2026-09-05 실측: 메인 스레드를 600초 점유하는 페이지에서 close_tab 이 상위 타임아웃(30초)까지
// 침묵했다. close_tab 은 `chrome.tabs.*` 만 쓰는데도 그랬는데, 조작 전 오버레이를 그리느라
// 이 통로를 먼저 타기 때문이다(background.js 의 markBusy). 같은 이유로 WebRTC 영상을 물고 있는
// 탭에서 navigate·eval_js·screenshot 이 전부 먹통이었다 — 영상은 컴포지터가 계속 그리므로
// 페이지는 멀쩡해 보이고, 막힌 것은 메인 스레드뿐이라 겉으로는 구분이 안 된다.
// 그러므로 상한은 선택이 아니다. 멈춘 곳의 이름을 달아 빠르게 실패시켜야 우회할 수 있다.
const ASK_TIMEOUT_MS = 10000

export function restricted(url = '') {
  return /^(chrome|edge|about|devtools|chrome-extension|view-source):/i.test(url)
    || url.startsWith('https://chromewebstore.google.com')
    || url.startsWith('https://chrome.google.com/webstore')
}

function ask(tabId, op, args, timeoutMs) {
  return new Promise((resolve, reject) => {
    // 상한에 걸린 뒤 콜백이 늦게 오면 이미 정산된 약속을 또 건드린다. 한 번만 정산한다.
    let settled = false
    const finish = (fn, arg) => {
      if (settled) return
      settled = true
      clearTimeout(timer)
      fn(arg)
    }
    const timer = setTimeout(
      () => finish(reject, new Error(`PAGE_TIMEOUT: ${op} 이 ${timeoutMs}ms 안에 응답하지 않았습니다 — 페이지 메인 스레드가 막혀 있습니다.`)),
      timeoutMs,
    )
    chrome.tabs.sendMessage(tabId, { __cc: true, op, args }, (res) => {
      const err = chrome.runtime.lastError
      if (err) return finish(reject, new Error(err.message))
      if (!res) return finish(reject, new Error('Receiving end does not exist'))
      if (!res.ok) return finish(reject, new Error(res.error))
      finish(resolve, res.result)
    })
  })
}

// content script 는 필요할 때만 주입한다. 새로 연 탭에는 아직 없으므로 이 재시도가 필수다.
export async function page(tabId, op, args = {}, { timeoutMs = ASK_TIMEOUT_MS } = {}) {
  const tab = await chrome.tabs.get(tabId)
  if (restricted(tab.url)) {
    throw new Error(`RESTRICTED_PAGE: ${tab.url} 은 확장이 접근할 수 없는 페이지입니다(크롬 내부·웹스토어). 다른 탭을 쓰세요.`)
  }
  try {
    return await ask(tabId, op, args, timeoutMs)
  } catch (e) {
    if (!/Receiving end does not exist|Could not establish connection/i.test(String(e.message))) throw e
    // ⚠️주입도 렌더러가 돌아야 끝난다. 여기에 상한이 없으면 「content script 가 없는 막힌 탭」이
    // 위 상한을 빠져나와 다시 무한정 기다리는 자리가 된다.
    await withTimeout(
      chrome.scripting.executeScript({ target: { tabId, allFrames: false }, files: [CONTENT_FILE] }),
      timeoutMs,
      `PAGE_TIMEOUT: content script 주입이 ${timeoutMs}ms 안에 끝나지 않았습니다 — 페이지 메인 스레드가 막혀 있습니다.`,
    )
    return await ask(tabId, op, args, timeoutMs)
  }
}
