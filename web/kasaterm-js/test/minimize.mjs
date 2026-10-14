// `node test/minimize.mjs fuzz:<seed>` — shrink a failing fuzz stream to the fewest chunks that
// still disagree with the reference, then print it as a test case.
import { compareCast } from './oracle.mjs';
import { fuzzCast } from './synth.mjs';

const seed = Number((process.argv[2] || '').replace('fuzz:', ''));
const full = fuzzCast(seed);
const fails = events => compareCast({ header: full.header, events }).mismatch !== null;
let events = full.events;
const first = compareCast(full).mismatch;
if (!first) { console.log('passes'); process.exit(0); }
if (first.what !== 'scrollback') events = events.slice(0, first.index + 1);
for (let size = Math.ceil(events.length / 2); size >= 1; size = Math.floor(size / 2)) {
  for (let i = 0; i < events.length;) {
    const trial = events.slice(0, i).concat(events.slice(i + size));
    if (trial.length && fails(trial)) events = trial; else i += size;
  }
}
// Merge adjacent output chunks when that still fails, so the case reads as one string.
for (let i = 0; i + 1 < events.length;) {
  const a = events[i], b = events[i + 1];
  if (a.kind === 'o' && b.kind === 'o') {
    const trial = events.slice(0, i).concat([{ kind: 'o', data: Buffer.concat([a.data, b.data]) }], events.slice(i + 2));
    if (fails(trial)) { events = trial; continue; }
  }
  i++;
}
console.log(JSON.stringify(full.header));
for (const e of events) console.log(e.kind === 'r' ? JSON.stringify([e.cols, e.rows]) : JSON.stringify(e.data.toString('latin1') === e.data.toString('utf8') ? e.data.toString() : e.data.toString('utf8')));
console.log(compareCast({ header: full.header, events }).mismatch.detail);
