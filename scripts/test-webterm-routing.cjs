const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

const index = fs.readFileSync('crates/kasa-mcp/assets/term/index.html', 'utf8');
const script = [...index.matchAll(/<script(?:\s[^>]*)?>([\s\S]*?)<\/script>/g)].at(-1)[1];
async function route(search, panes = []) {
  const navigations = [], historyWrites = [];
  const location = { pathname: '/u/fixture/m/device/term', search, hash: '#tail', replace: url => navigations.push(url) };
  const element = { style: {}, classList: { add() {} }, addEventListener() {} };
  const context = {
    location, encodeURIComponent, URLSearchParams,
    history: { replaceState(_state, _title, url) { historyWrites.push(url); location.search = new URL(url, 'https://fixture').search; } },
    document: { getElementById: () => element, body: element },
    matchMedia: () => ({ matches: false }),
    KasaTerm: { Terminal: class { constructor() { throw new Error('TERM_ENTRY'); } } },
    fetch: async url => ({ json: async () => url.endsWith('/sessions') ? { active: 2 } : panes }),
  };
  let terminal = false;
  try { await vm.runInNewContext(script, context); }
  catch (error) { if (error.message === 'TERM_ENTRY') terminal = true; else throw error; }
  return { navigations, historyWrites, terminal };
}

(async () => {
  // Default: kasaterm.js on the picked pane, no redirect.
  const exact = await route('?pane=%251&t=opaque%2Bvalue&pic=0');
  assert.deepEqual(exact.navigations, []);
  assert.equal(exact.terminal, true);
  const picked = await route('?t=opaque%2Bvalue&option=keep', [{ id: '%9', window: 2 }]);
  assert.equal(picked.terminal, true);
  assert.ok(picked.historyWrites[0].includes('t=opaque%2Bvalue&option=keep&pane=%259'));
  const fresh = await route('?t=opaque', [{ id: 'web-existing' }]);
  assert.equal(fresh.terminal, true, 'no native pane must retain new-shell behavior');
  assert.deepEqual(fresh.historyWrites, []);
  const legacy = await route('?renderer=ansi&t=opaque', [{ id: '%9', window: 2 }]);
  assert.equal(legacy.terminal, true);
  assert.ok(legacy.historyWrites[0].includes('renderer=ansi&t=opaque&pane=%259'));
  // The server-parsed grid stays one option away, entered before any socket opens.
  const gridExact = await route('?renderer=grid&pane=%251&t=opaque%2Bvalue');
  assert.deepEqual(gridExact.navigations, ['/u/fixture/m/device/term/grid?renderer=grid&pane=%251&t=opaque%2Bvalue#tail']);
  assert.equal(gridExact.terminal, false, 'grid routing opened a terminal before redirect');
  const gridPicked = await route('?renderer=grid&t=opaque', [{ id: '%9', window: 2 }]);
  assert.deepEqual(gridPicked.navigations, ['/u/fixture/m/device/term/grid?renderer=grid&t=opaque&pane=%259#tail']);

  const grid = fs.readFileSync('crates/kasa-mcp/assets/term/grid.html', 'utf8');
  const helpers = ['paneQuery', 'socketURL'].map(name => grid.match(new RegExp(`function ${name}\\([^]*?\\n}`))[0]).join('\n');
  const context = { connectedPane: 'web-owned', proto: 'wss', ROOT: '/u/fixture/m/device', encodeURIComponent,
    location: { host: 'fixture.test', search: '?pane=%251&t=opaque%2Bvalue&pic=1' } };
  vm.runInNewContext(`${helpers}\nresult=socketURL();`, context);
  assert.equal(context.result, 'wss://fixture.test/u/fixture/m/device/term/ws?t=opaque%2Bvalue&pic=1&pane=web-owned&grid=1&glyphs=1');
  context.connectedPane = null;
  context.location.search = '?pane=%151&glyphs=0&grid=0&t=opaque%2Bvalue';
  vm.runInNewContext('result=socketURL();', context);
  assert.equal(context.result, 'wss://fixture.test/u/fixture/m/device/term/ws?pane=%151&t=opaque%2Bvalue&grid=1&glyphs=1');
  console.log('webterm kasaterm.js default, grid opt-in, opaque query and owned-session reconnect checks passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
