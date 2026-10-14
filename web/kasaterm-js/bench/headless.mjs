// Parser throughput without drawing: kasaterm.js against @xterm/headless 6.0, the same bytes fed
// in 64 KB reads at 120x40 with 10,000 lines of scrollback. `node bench/headless.mjs`
import { createRequire } from 'node:module';
import { workloads, chunks } from './workloads.mjs';

const require = createRequire(import.meta.url);
const Kasa = require('../dist/kasaterm.js');
const { Terminal: XHeadless } = require('@xterm/headless');

async function runKasa(parts) {
  const t = new Kasa.Terminal({ cols: 120, rows: 40, scrollback: 10000 });
  const start = performance.now();
  for (const p of parts) t.write(p);
  return performance.now() - start;
}

function runXterm(parts) {
  const t = new XHeadless({ cols: 120, rows: 40, scrollback: 10000, allowProposedApi: true });
  const start = performance.now();
  return new Promise(resolve => {
    parts.forEach((p, i) => t.write(p, i === parts.length - 1 ? () => resolve(performance.now() - start) : undefined));
  });
}

const median = a => a.slice().sort((x, y) => x - y)[Math.floor(a.length / 2)];
const loads = workloads(Number(process.env.KTJS_BENCH_SCALE || 1));
const rows = [];
for (const [name, buf] of Object.entries(loads)) {
  const parts = chunks(buf);
  const k = [], x = [];
  for (let i = 0; i < 3; i++) { k.push(await runKasa(parts)); x.push(await runXterm(parts)); }
  const mb = buf.length / (1 << 20);
  rows.push({ workload: name, MB: mb.toFixed(1), 'kasaterm.js MB/s': (mb / median(k) * 1000).toFixed(1),
    'xterm 6.0 MB/s': (mb / median(x) * 1000).toFixed(1), ratio: (median(x) / median(k)).toFixed(2) });
}
console.table(rows);
