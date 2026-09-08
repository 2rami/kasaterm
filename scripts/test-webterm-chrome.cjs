const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

(async () => {
  const elements = new Map();
  function element(id) {
    if (!elements.has(id)) elements.set(id, {
      style: { values: {}, setProperty(key, value) { this.values[key] = value; } },
      dataset: {}, textContent: '', src: '', getAttribute(key) { return this[key]; },
    });
    return elements.get(id);
  }
  const events = {}, intervals = new Set();
  let nextInterval = 0, slow = true, aborted = 0, calls = 0;
  const window = { addEventListener(name, listener) { events[name] = listener; } };
  const document = {
    visibilityState: 'visible', documentElement: element('root'),
    getElementById: element, addEventListener() {},
  };
  const context = {
    window, document, CSS: { supports: () => true }, AbortController,
    setTimeout, clearTimeout,
    setInterval() { const id = ++nextInterval; intervals.add(id); return id; },
    clearInterval: id => intervals.delete(id),
    fetch: (url, { signal }) => {
      calls++;
      if (url.endsWith('/mobile/me') && slow) return new Promise((_, reject) => {
        signal.addEventListener('abort', () => { aborted++; reject(new Error('aborted')); });
      });
      const value = url.endsWith('/design-tokens')
        ? { theme: 'native', palette: { bg: '#252c35', fg: '#ffffff' }, ansi: Array(16).fill('#ffffff') }
        : url.endsWith('/term/panes')
          ? [{ id: '%1', name: '캐릭터', slug: 'fixture', harness: 'claude', status: 'working', title: '작업 제목' }]
          : { machine: '검증 기기' };
      return Promise.resolve({ ok: true, json: async () => value });
    },
  };
  vm.runInNewContext(fs.readFileSync('crates/kasa-mcp/assets/term/chrome.js', 'utf8'), context);
  const chrome = window.KasaTermChrome.create({ root: '', pane: '%1', timeoutMs: 20 });
  await new Promise(resolve => setTimeout(resolve, 5));
  assert.equal(element('root').style.values['--kt-bg'], '#252c35', 'one slow endpoint blocked available theme tokens');
  assert.equal(element('pane-state').textContent, '작업 중', 'native working state was omitted');
  assert.equal(element('whot').textContent, '작업 제목');
  element('avatar').onerror();
  chrome.updatePane({ id: '%1', slug: 'fixture', harness: 'claude' });
  assert.equal(element('avatar').style.display, 'none', 'polling restored a known broken avatar');
  assert.equal(element('ahole').style.display, 'flex');
  await new Promise(resolve => setTimeout(resolve, 25));
  assert.equal(aborted, 1, 'slow metadata fetch was not bounded');
  slow = false;
  await chrome.refresh();
  assert.equal(element('machine-name').textContent, '검증 기기');
  events.pagehide();
  assert.equal(intervals.size, 0);
  const before = calls;
  events.pageshow();
  await new Promise(resolve => setTimeout(resolve, 5));
  assert.equal(intervals.size, 1, 'bfcache return did not restart refresh');
  assert.ok(calls > before);
  events.pagehide();
  console.log('webterm native theme, status, avatar and metadata lifecycle checks passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
