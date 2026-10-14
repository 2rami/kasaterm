// Drawing included: the same streams through kasaterm.js (canvas), xterm.js 5.5 (the DOM renderer
// the old /term used), xterm.js 6.0 DOM and xterm.js 6.0 + WebGL, in headless Chromium.
// Reports wall time, main-thread time (CDP TaskDuration) and frame gaps while the data flows.
//   node bench/browser.mjs            (needs Playwright installed globally and `npm i` in bench/)
import { createRequire } from 'node:module';
import { execSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { workloads } from './workloads.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
const { chromium } = require(join(execSync('npm root -g').toString().trim(), 'playwright'));
const repo = join(here, '..', '..', '..');
// The xterm.js 5.5 + fit build /term shipped until 2026-10, from git history.
const xterm55 = execSync('git show 579cd414:crates/kasa-mcp/assets/term/xterm.js', { cwd: repo, maxBuffer: 1 << 26 });
const xterm55css = execSync('git show 579cd414:crates/kasa-mcp/assets/term/xterm.css', { cwd: repo });
const files = {
  '/page.html': [readFileSync(join(here, 'page.html')), 'text/html'],
  '/kasaterm.js': [readFileSync(join(here, '..', 'dist', 'kasaterm.js')), 'text/javascript'],
  '/xterm55.js': [xterm55, 'text/javascript'],
  '/xterm55.css': [xterm55css, 'text/css'],
  '/xterm6.js': [readFileSync(join(here, 'node_modules/@xterm/xterm/lib/xterm.js')), 'text/javascript'],
  '/xterm6.css': [readFileSync(join(here, 'node_modules/@xterm/xterm/css/xterm.css')), 'text/css'],
  '/xterm6-webgl.js': [readFileSync(join(here, 'node_modules/@xterm/addon-webgl/lib/addon-webgl.js')), 'text/javascript'],
  '/font.woff2': [readFileSync(join(repo, 'crates/kasa-mcp/assets/term/font.woff2')), 'font/woff2'],
};
const scale = Number(process.env.KTJS_BENCH_SCALE || 0.25);
for (const [name, buf] of Object.entries(workloads(scale))) files[`/w/${name}`] = [buf, 'application/octet-stream'];
const loads = Object.keys(files).filter(k => k.startsWith('/w/')).map(k => k.slice(3));
files['/w/warmup'] = [Buffer.concat(Object.values(workloads(0.02))), 'application/octet-stream'];
const kinds = (process.env.KTJS_BENCH_TERMS || 'kasa,xterm55,xterm6,xterm6-webgl').split(',');

const pct = (a, p) => { const s = a.slice().sort((x, y) => x - y); return s[Math.min(s.length - 1, Math.floor(s.length * p))] || 0; };
const browser = await chromium.launch({ args: ['--enable-gpu', '--ignore-gpu-blocklist'] });

async function open(kind) {
  const page = await browser.newPage({ viewport: { width: 1100, height: 720 }, deviceScaleFactor: 2 });
  await page.route('http://bench.local/**', route => {
    const f = files[new URL(route.request().url()).pathname];
    return f ? route.fulfill({ body: f[0], contentType: f[1] }) : route.fulfill({ status: 404 });
  });
  page.on('pageerror', e => console.error(kind, e.message));
  await page.goto(`http://bench.local/page.html?term=${kind}`);
  await page.evaluate(() => setup());
  const cdp = await page.context().newCDPSession(page);
  await cdp.send('Performance.enable');
  const busy = async () => (await cdp.send('Performance.getMetrics')).metrics.find(m => m.name === 'TaskDuration').value * 1000;
  return { page, busy };
}

const page0 = (await open('kasa')).page;
console.log('GPU:', await page0.evaluate(() => gpu()));
await page0.close();
const median = a => a.slice().sort((x, y) => x - y)[Math.floor(a.length / 2)];
const rows = [];
for (const load of loads) {
  for (const kind of kinds) {
    const totals = [], cpus = [], gaps = [];
    for (let run = 0; run < 3; run++) {
      const { page, busy } = await open(kind);
      // A first pass warms the JIT, the glyph caches and the GPU so the timed pass measures steady state.
      await page.evaluate(u => flood(u), 'http://bench.local/w/warmup');
      const b0 = await busy();
      const r = await page.evaluate(u => flood(u), `http://bench.local/w/${load}`);
      cpus.push(await busy() - b0);
      totals.push(r.total);
      gaps.push(...r.gaps);
      await page.close();
    }
    const mb = files[`/w/${load}`][0].length / (1 << 20);
    rows.push({ workload: load, terminal: kind, 'MB/s': (mb / median(totals) * 1000).toFixed(1), 'main thread ms': median(cpus).toFixed(0),
      'gap p50 ms': pct(gaps, 0.5).toFixed(0), 'gap p95 ms': pct(gaps, 0.95).toFixed(0), 'gap max ms': Math.max(...gaps).toFixed(0) });
  }
}
console.table(rows);
const scroll = [];
for (const kind of kinds) {
  const { page, busy } = await open(kind);
  await page.evaluate(u => flood(u), 'http://bench.local/w/warmup');
  await page.evaluate(u => flood(u), 'http://bench.local/w/sgr');
  const b0 = await busy();
  const r = await page.evaluate(() => scrollback(300));
  const cpu = await busy() - b0;
  scroll.push({ terminal: kind, 'ms per step': (r.total / 300).toFixed(1), 'main thread ms per step': (cpu / 300).toFixed(1),
    'gap p95 ms': pct(r.gaps, 0.95).toFixed(0), 'gap max ms': Math.max(...r.gaps).toFixed(0) });
  await page.close();
}
console.log('scrolling back through 10,000 lines, 3 lines per step:');
console.table(scroll);
await browser.close();
