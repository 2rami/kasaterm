// `node test/run.mjs [filter]` — differential test against kasaterm's terminal. Corpus recordings
// in corpus/, the hand-written cases and a fuzz sweep (`KTJS_FUZZ=<count>`, default 200).
import { readdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { compareCast, readCast } from './oracle.mjs';
import { CASES, fuzzCast } from './synth.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const filter = process.argv[2] || '';
const fuzzCount = Number(process.env.KTJS_FUZZ ?? 200);
let failed = 0, total = 0, checkpoints = 0;

function report(name, res) {
  total++;
  checkpoints += res.checked;
  if (res.mismatch) {
    failed++;
    console.log(`FAIL ${name} at chunk ${res.mismatch.index} (${res.mismatch.what}) after ${res.checked} matching chunks\n${res.mismatch.detail}`);
  } else if (process.env.KTJS_VERBOSE) {
    console.log(`ok   ${name}: ${res.checked} chunks`);
  }
}

const corpus = join(here, '..', 'corpus');
for (const f of readdirSync(corpus).filter(f => /\.kcast(\.gz)?$/.test(f)).sort()) {
  if (filter && !f.includes(filter)) continue;
  report(`corpus/${f}`, compareCast(readCast(join(corpus, f))));
}
for (const [name, c] of Object.entries(CASES)) {
  if (filter && !name.includes(filter)) continue;
  report(`case ${name}`, compareCast(c));
}
if (!filter || filter.startsWith('fuzz')) {
  const range = /^fuzz:(\d+)(?:-(\d+))?$/.exec(filter);
  const first = range ? Number(range[1]) : 1, last = range ? Number(range[2] || range[1]) : fuzzCount;
  const only = Array.from({ length: Math.max(0, last - first + 1) }, (_, i) => first + i);
  for (const seed of only) report(`fuzz:${seed}`, compareCast(fuzzCast(seed)));
}
console.log(`${total - failed}/${total} streams identical, ${checkpoints} chunk states compared cell by cell`);
process.exit(failed ? 1 : 0);
