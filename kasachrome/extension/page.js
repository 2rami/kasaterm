// content script 와 말하는 유일한 통로. tools 와 students 양쪽이 쓰므로 여기 한 곳에 둔다
// (students 가 tools 를 import 하면 순환이 된다).
const CONTENT_FILE = 'content.js'

export function restricted(url = '') {
  return /^(chrome|edge|about|devtools|chrome-extension|view-source):/i.test(url)
    || url.startsWith('https://chromewebstore.google.com')
    || url.startsWith('https://chrome.google.com/webstore')
}

function ask(tabId, op, args) {
  return new Promise((resolve, reject) => {
    chrome.tabs.sendMessage(tabId, { __cc: true, op, args }, (res) => {
      const err = chrome.runtime.lastError
      if (err) return reject(new Error(err.message))
      if (!res) return reject(new Error('Receiving end does not exist'))
      if (!res.ok) return reject(new Error(res.error))
      resolve(res.result)
    })
  })
}

// content script 는 필요할 때만 주입한다. 새로 연 탭에는 아직 없으므로 이 재시도가 필수다.
export async function page(tabId, op, args = {}) {
  const tab = await chrome.tabs.get(tabId)
  if (restricted(tab.url)) {
    throw new Error(`RESTRICTED_PAGE: ${tab.url} 은 확장이 접근할 수 없는 페이지입니다(크롬 내부·웹스토어). 다른 탭을 쓰세요.`)
  }
  try {
    return await ask(tabId, op, args)
  } catch (e) {
    if (!/Receiving end does not exist|Could not establish connection/i.test(String(e.message))) throw e
    await chrome.scripting.executeScript({ target: { tabId, allFrames: false }, files: [CONTENT_FILE] })
    return await ask(tabId, op, args)
  }
}
