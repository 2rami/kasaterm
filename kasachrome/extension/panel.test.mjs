import test from 'node:test'
import assert from 'node:assert/strict'

// 팝업·사이드 패널은 **확장 페이지라 디버거를 못 붙인다**(`Cannot access a chrome-extension:// URL
// of different extension`) — 스크린샷으로 확인할 수가 없다. 그래서 화면이 열릴 때 터지지 않는지,
// 탭 전환이 실제로 내용을 갈아 끼우는지를 여기서 대신 잡는다. 이 파일이 없으면 panel.js 의 오타
// 하나가 팝업을 통째로 빈 화면으로 만들고, 그걸 알아채는 것은 사람이 아이콘을 눌러 본 뒤다.

class El {
  constructor(tag) {
    this.tagName = tag
    this.children = []
    this.attributes = {}
    this.style = { setProperty() {} }
    this.dataset = {}
    this.handlers = new Map()
    this.className = ''
    this.textContent = ''
    this.hidden = false
    this.disabled = false
  }
  get classList() {
    const self = this
    return {
      add: (c) => { self.className = `${self.className} ${c}`.trim() },
      remove: (c) => { self.className = self.className.split(/\s+/).filter((x) => x && x !== c).join(' ') },
      contains: (c) => self.className.split(/\s+/).includes(c),
    }
  }
  append(...nodes) { for (const n of nodes) if (n != null) this.children.push(n) }
  appendChild(node) { this.children.push(node); return node }
  replaceChildren(...nodes) { this.children = nodes.filter((n) => n != null) }
  setAttribute(name, value) { this.attributes[name] = String(value) }
  getAttribute(name) { return this.attributes[name] ?? null }
  addEventListener(type, fn) { this.handlers.set(type, fn) }
  removeAttribute(name) { delete this.attributes[name] }
  focus() {}
  querySelector() { return null }
  get text() {
    return [this.textContent, ...this.children.map((c) => (c instanceof El ? c.text : String(c ?? '')))].join(' ')
  }
}

function mockDom() {
  const byId = new Map()
  for (const id of ['root', 'conn', 'conn-text', 'nav', 'open-panel']) byId.set(id, new El('div'))
  const body = new El('body')
  body.className = 'popup'
  globalThis.document = {
    getElementById: (id) => byId.get(id) || null,
    createElement: (tag) => new El(tag),
    createTextNode: (t) => String(t),
    querySelector: () => new El('div'),
    body,
  }
  globalThis.setInterval = () => 0
  return byId
}

function mockChrome(state) {
  const store = {}
  globalThis.chrome = {
    runtime: {
      getManifest: () => ({ name: 'Test' }),
      sendMessage: (msg, cb) => {
        if (msg.op === 'state') return cb(state)
        if (msg.op === 'layoutState') return cb({ ok: true, on: false, src: null, tabId: 7 })
        cb({ ok: true })
      },
      lastError: null,
    },
    storage: { local: { get: async (k) => ({ [k]: store[k] }), set: async (v) => Object.assign(store, v) } },
    tabs: { query: async () => [{ id: 7 }] },
    windows: { getCurrent: async () => ({ id: 1 }) },
    sidePanel: { open() {} },
  }
  return store
}

const SESSION = {
  connected: true,
  display: { off: false, frame: true, chip: true, cursor: true, pos: 'tr', dx: 12, dy: 12 },
  sessions: [{
    key: 'p1', name: '사키리', paneId: '%18', color: '#f0c', avatar: null,
    task: '결제 플로', busy: false, grouped: true,
    tabs: [{ tabId: 7, windowId: 1, title: 'Example', url: 'https://example.com', busy: false, active: true }],
    log: [{ at: Date.now(), label: '클릭 — 로그인' }],
  }],
}

const settle = async (n = 8) => { for (let i = 0; i < n; i++) await new Promise((r) => setImmediate(r)) }

test('the popup renders without throwing and splits work from the layout tool', async () => {
  const byId = mockDom()
  const store = mockChrome(SESSION)
  await import(`./panel.js?test=${Date.now()}`)
  await settle()

  const nav = byId.get('nav')
  const root = byId.get('root')
  assert.deepEqual(nav.children.map((b) => b.textContent), ['작업', '레이아웃툴'])
  assert.equal(nav.children[0].getAttribute('aria-selected'), 'true')
  // 작업 화면에는 누가 무슨 작업 중인지가 있고, 레이아웃툴 단추는 섞여 있지 않다.
  assert.match(root.text, /사키리/)
  assert.match(root.text, /결제 플로/)
  assert.doesNotMatch(root.text, /레이아웃툴/)

  nav.children[1].handlers.get('click')()
  await settle()
  assert.equal(nav.children[1].getAttribute('aria-selected'), 'true')
  assert.match(root.text, /레이아웃툴/)
  // 갈라놓은 값어치가 여기다 — 매초 바뀌는 현황이 도구 화면에 딸려 오지 않는다.
  assert.doesNotMatch(root.text, /사키리/)
  // 고른 화면은 남는다. 팝업은 누를 때마다 새로 뜨므로 안 남기면 매번 첫 탭으로 돌아간다.
  assert.equal(store.panelView, 'layout')
})

test('a disconnected bridge is reported on either tab', async () => {
  const byId = mockDom()
  mockChrome({ connected: false, sessions: [] })
  await import(`./panel.js?test=${Date.now()}${Math.random()}`)
  await settle()
  assert.match(byId.get('root').text, /브리지/)
  assert.equal(byId.get('conn').className, 'conn off')
})
