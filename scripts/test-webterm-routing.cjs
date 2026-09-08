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
    Terminal: class { constructor() { throw new Error('ANSI_ENTRY'); } },
    fetch: async url => ({ json: async () => url.endsWith('/sessions') ? { active: 2 } : panes }),
  };
  let ansi = false;
  try { await vm.runInNewContext(script, context); }
  catch (error) { if (error.message === 'ANSI_ENTRY') ansi = true; else throw error; }
  return { navigations, historyWrites, ansi };
}

(async () => {
  const exact = await route('?pane=%251&t=opaque%2Bvalue&pic=0');
  assert.deepEqual(exact.navigations, ['/u/fixture/m/device/term/grid?pane=%251&t=opaque%2Bvalue&pic=0#tail']);
  assert.equal(exact.ansi, false, 'default routing opened an ANSI terminal before redirect');
  const picked = await route('?t=opaque%2Bvalue&option=keep', [{ id: '%9', window: 2 }]);
  assert.deepEqual(picked.navigations, ['/u/fixture/m/device/term/grid?t=opaque%2Bvalue&option=keep&pane=%259#tail']);
  const fresh = await route('?t=opaque', [{ id: 'web-existing' }]);
  assert.deepEqual(fresh.navigations, ['/u/fixture/m/device/term/grid?t=opaque#tail'], 'no native pane must retain new-shell behavior');
  const ansi = await route('?renderer=ansi&t=opaque', [{ id: '%9', window: 2 }]);
  assert.equal(ansi.ansi, true);
  assert.equal(ansi.navigations.length, 0);
  assert.ok(ansi.historyWrites[0].includes('renderer=ansi&t=opaque&pane=%259'));

  const grid = fs.readFileSync('crates/kasa-mcp/assets/term/grid.html', 'utf8');
  const helpers = ['paneQuery', 'socketURL'].map(name => grid.match(new RegExp(`function ${name}\\([^]*?\\n}`))[0]).join('\n');
  const context = { connectedPane: 'web-owned', proto: 'wss', ROOT: '/u/fixture/m/device', encodeURIComponent,
    location: { host: 'fixture.test', search: '?pane=%251&t=opaque%2Bvalue&pic=1' } };
  vm.runInNewContext(`${helpers}\nresult=socketURL();`, context);
  assert.equal(context.result, 'wss://fixture.test/u/fixture/m/device/term/ws?t=opaque%2Bvalue&pic=1&pane=web-owned&grid=1');
  console.log('webterm default view, ANSI opt-in, opaque query and owned-session reconnect checks passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
