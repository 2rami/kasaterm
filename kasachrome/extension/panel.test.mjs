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
    // SVG 아이콘은 createElementNS 로 만든다 — namespaceURI 까지 흉내내야 path 도 같은 길로 간다.
    createElementNS: (ns, tag) => Object.assign(new El(tag), { namespaceURI: ns }),
    createTextNode: (t) => String(t),
    querySelector: () => new El('div'),
    body,
  }
  globalThis.setInterval = () => 0
  return byId
}

function mockChrome(state, emulation = { on: false }) {
  const store = {}
  const calls = []
  globalThis.chrome = {
    runtime: {
      getManifest: () => ({ name: 'Test' }),
      sendMessage: (msg, cb) => {
        if (msg.op === 'state') return cb(state)
        if (msg.op === 'layoutState') return cb({ ok: true, on: false, src: null, tabId: 7 })
        if (msg.op === 'emulateState') return cb(emulation)
        if (msg.op === 'emulate') { calls.push(msg.args); return cb({ ok: true, result: { emulating: true } }) }
        cb({ ok: true })
      },
      lastError: null,
    },
    storage: { local: { get: async (k) => ({ [k]: store[k] }), set: async (v) => Object.assign(store, v) } },
    tabs: { query: async () => [{ id: 7 }] },
    windows: { getCurrent: async () => ({ id: 1 }) },
    sidePanel: { open() {} },
  }
  return { store, calls }
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

test('the popup renders without throwing and keeps each tool on its own tab', async () => {
  const byId = mockDom()
  const { store } = mockChrome(SESSION)
  await import(`./panel.js?test=${Date.now()}`)
  await settle()

  const nav = byId.get('nav')
  const root = byId.get('root')
  assert.deepEqual(nav.children.map((b) => b.textContent), ['작업', '기기', '레이아웃툴'])
  assert.equal(nav.children[0].getAttribute('aria-selected'), 'true')
  // 작업 화면에는 누가 무슨 작업 중인지가 있고, 도구 단추는 섞여 있지 않다.
  assert.match(root.text, /사키리/)
  assert.match(root.text, /결제 플로/)
  assert.doesNotMatch(root.text, /레이아웃툴/)

  nav.children[2].handlers.get('click')()
  await settle()
  assert.equal(nav.children[2].getAttribute('aria-selected'), 'true')
  assert.match(root.text, /레이아웃툴/)
  // 갈라놓은 값어치가 여기다 — 매초 바뀌는 현황이 도구 화면에 딸려 오지 않는다.
  assert.doesNotMatch(root.text, /사키리/)
  // 고른 화면은 남는다. 팝업은 누를 때마다 새로 뜨므로 안 남기면 매번 첫 탭으로 돌아간다.
  assert.equal(store.panelView, 'layout')
})

test('device buttons apply to the tab the human is looking at, and go back', async () => {
  const byId = mockDom()
  const { calls } = mockChrome(SESSION)
  await import(`./panel.js?test=${Date.now()}${Math.random()}`)
  await settle()

  byId.get('nav').children[1].handlers.get('click')()
  await settle()
  const root = byId.get('root')
  assert.match(root.text, /폰/)
  assert.match(root.text, /4K/)
  // 크기를 함께 낸다 — 이름만으로는 「노트북」과 「FHD」가 무엇이 다른지 안 보인다.
  assert.match(root.text, /3840×2160/)

  const grid = root.children[0].children.find((c) => c.className === 'dev-grid')
  const labelOf = (b) => b.children.find((c) => c.className === 'dev-l')?.textContent
  grid.children.find((b) => labelOf(b) === '폰').handlers.get('click')()
  await settle()
  // 활성 탭에만 건다. 창을 건드리면 이 브라우저를 함께 쓰는 사람들의 설정이 통째로 날아간다.
  assert.deepEqual(calls.at(-1), { tabId: 7, device: 'iphone-15-pro' })

  grid.children.find((b) => labelOf(b) === '원래대로').handlers.get('click')()
  await settle()
  assert.deepEqual(calls.at(-1), { tabId: 7, off: true })
})

test('rotating needs a device first, and flips the one already applied', async () => {
  const byId = mockDom()
  const { calls } = mockChrome(SESSION)
  await import(`./panel.js?test=${Date.now()}${Math.random()}`)
  await settle()
  byId.get('nav').children[1].handlers.get('click')()
  await settle()

  const flipOf = (root) => root.children[0].children.find((c) => c.className?.startsWith('dev-flip'))
  // 아무것도 안 걸렸으면 뒤집을 것이 없다. 눌리는 단추로 두면 「왜 안 되지」가 되고 답이 화면에 없다.
  assert.equal(flipOf(byId.get('root')).disabled, true)

  const byId2 = mockDom()
  const m2 = mockChrome(SESSION, { on: true, width: 393, height: 852, scale: 1 })
  await import(`./panel.js?test=${Date.now()}${Math.random()}`)
  await settle()
  byId2.get('nav').children[1].handlers.get('click')()
  await settle()
  const flip = flipOf(byId2.get('root'))
  assert.equal(flip.disabled, false)
  flip.handlers.get('click')()
  await settle()
  assert.deepEqual(m2.calls.at(-1), { tabId: 7, device: 'iphone-15-pro', landscape: true })
})

test('a device lying on its side still shows its button pressed', async () => {
  const byId = mockDom()
  // 뒤집힌 크기를 같은 기기로 못 읽으면 눕히는 순간 눌린 버튼이 사라져, 기기 뷰가 꺼진 것처럼 보인다.
  mockChrome(SESSION, { on: true, width: 852, height: 393, scale: 0.9 })
  await import(`./panel.js?test=${Date.now()}${Math.random()}`)
  await settle()
  byId.get('nav').children[1].handlers.get('click')()
  await settle()
  const root = byId.get('root')
  const grid = root.children[0].children.find((c) => c.className === 'dev-grid')
  const pressed = grid.children.filter((b) => b.getAttribute('aria-pressed') === 'true')
  assert.equal(pressed.length, 1)
  assert.equal(pressed[0].children.find((c) => c.className === 'dev-l').textContent, '폰')
  assert.match(root.text, /세로로 세우기/)
})

test('a device already applied shows which button is pressed, and says it was scaled down', async () => {
  const byId = mockDom()
  mockChrome(SESSION, { on: true, width: 3840, height: 2160, scale: 0.39 })
  await import(`./panel.js?test=${Date.now()}${Math.random()}`)
  await settle()

  byId.get('nav').children[1].handlers.get('click')()
  await settle()
  const root = byId.get('root')
  const grid = root.children[0].children.find((c) => c.className === 'dev-grid')
  const pressed = grid.children.filter((b) => b.getAttribute('aria-pressed') === 'true')
  assert.equal(pressed.length, 1)
  assert.equal(pressed[0].children.find((c) => c.className === 'dev-l').textContent, '4K')
  // 줄여서 보여준다는 사실을 안 밝히면 「왜 글자가 작지」가 페이지 탓으로 읽힌다.
  assert.match(root.text, /39% 로 줄여/)
})

test('a disconnected bridge is reported on every tab', async () => {
  const byId = mockDom()
  mockChrome({ connected: false, sessions: [] })
  await import(`./panel.js?test=${Date.now()}${Math.random()}`)
  await settle()
  assert.match(byId.get('root').text, /브리지/)
  assert.equal(byId.get('conn').className, 'conn off')
})
