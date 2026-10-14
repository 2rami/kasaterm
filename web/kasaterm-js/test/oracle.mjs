// Differential check: replay a recording through kasaterm's own terminal (alacritty_terminal via
// `vt_oracle`) and through kasaterm.js, and compare the grid after every chunk — every cell's
// text, width flags, colours, attributes, underline colour and link, plus cursor, modes,
// scrollback and the replies sent back to the program.
import { spawnSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { gunzipSync } from 'node:zlib';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
export const KasaTerm = require(join(here, '..', 'dist', 'kasaterm.js'));

const REPO = resolve(here, '..', '..', '..');
export function oraclePath() {
  const candidates = [process.env.KTJS_ORACLE,
    join(process.env.CARGO_TARGET_DIR || join(REPO, 'target'), 'release', 'examples', 'vt_oracle')];
  const found = candidates.find(p => p && existsSync(p));
  if (!found) throw new Error('vt_oracle not built: cargo build --release -p kasa-pty --example vt_oracle');
  return found;
}

export function readCast(path) {
  let text = readFileSync(path);
  if (path.endsWith('.gz')) text = gunzipSync(text);
  const lines = text.toString('utf8').split('\n').filter(Boolean);
  const header = JSON.parse(lines[0]);
  const events = [];
  for (const line of lines.slice(1)) {
    const e = JSON.parse(line);
    if (e[1] === 'o') events.push({ kind: 'o', data: Buffer.from(e[2], 'base64') });
    else if (e[1] === 'r') events.push({ kind: 'r', cols: e[2], rows: e[3] });
  }
  return { header, events };
}

function frames(cast, upto = Infinity, tail = 'e') {
  const parts = [];
  const resize = (c, r) => { const b = Buffer.alloc(5); b[0] = 0x72; b.writeUInt16LE(c, 1); b.writeUInt16LE(r, 3); parts.push(b); };
  resize(cast.header.cols, cast.header.rows);
  cast.events.slice(0, upto).forEach(e => {
    if (e.kind === 'r') resize(e.cols, e.rows);
    else { const h = Buffer.alloc(5); h[0] = 0x6f; h.writeUInt32LE(e.data.length, 1); parts.push(h, e.data); }
  });
  parts.push(Buffer.from(tail));
  return Buffer.concat(parts);
}

function runOracle(input, env) {
  const r = spawnSync(oraclePath(), [], { input, maxBuffer: 1 << 30, env: env ? { ...process.env, ...env } : undefined });
  if (r.status !== 0) throw new Error(`vt_oracle failed: ${r.stderr}`);
  return r.stdout.toString('utf8').split('\n').filter(Boolean).map(l => JSON.parse(l));
}

// ── the same serialisation as vt_oracle.rs ──

function colorKey(c) {
  if (c >= KasaTerm.COLOR_RGB) return '#' + (c & 0xffffff).toString(16).padStart(6, '0');
  if (c >= KasaTerm.COLOR_INDEXED) return 'i' + (c & 0xff);
  return 'n' + c;
}

export function cellKey(row, x) {
  const e = row.extra(x);
  let link = '-';
  if (e && e.link) link = e.link.uri + (e.link.id !== null ? '\u0003' + e.link.id : '');
  return row.text(x) + '\u0001' + row.flags(x) + '\u0001' + colorKey(row.fg(x)) + '\u0001'
    + colorKey(row.bg(x)) + '\u0001' + (e && e.ul >= 0 ? colorKey(e.ul) : '-') + '\u0001' + link + '\u0002';
}

export function rowKey(term, y) {
  const row = term.line(y);
  let s = '';
  for (let x = 0; x < term.cols; x++) s += cellKey(row, x);
  return s;
}

function fnv(s) {
  let h = 0x811c9dc5;
  for (const ch of s) { h ^= ch.codePointAt(0); h = Math.imul(h, 16777619) >>> 0; }
  return h;
}

function attach(term) {
  const events = [];
  term.on('reply', s => events.push('w:' + s));
  term.on('colorquery', i => events.push('color:' + i));
  term.on('pxsize', () => events.push('pxsize'));
  term.on('clipboard', (kind, text) => events.push(`clip:${kind}:${text}`));
  term.on('title', t => events.push(t === null ? 'title-reset' : 'title:' + t));
  term.on('bell', () => events.push('bell'));
  return events;
}

function state(term) {
  const cur = term.cursor, style = term.resolvedCursorStyle;
  const rows = [];
  for (let y = 0; y < term.rows; y++) rows.push(fnv(rowKey(term, y)));
  const recent = [];
  for (let y = -Math.min(3, term.historySize); y < 0; y++) recent.push(fnv(rowKey(term, y)));
  return {
    cols: term.cols, lines: term.rows, cur: [cur.line, cur.col, cur.wrap], mode: term.mode,
    style: [style.shape, style.blinking], hist: term.historySize, rows, recent, sync: term.syncPending,
  };
}

function same(a, b) { return JSON.stringify(a) === JSON.stringify(b); }

function describeCell(key) {
  const [text, flags, fg, bg, ul, link] = key.split('\u0001');
  return JSON.stringify({ text, flags: +flags, fg, bg, ul, link });
}

function diffRows(want, got, top) {
  const out = [];
  for (let i = 0; i < Math.max(want.length, got.length); i++) {
    if (want[i] === got[i]) continue;
    const a = (want[i] || '').split('\u0002'), b = (got[i] || '').split('\u0002');
    const cols = [];
    for (let x = 0; x < Math.max(a.length, b.length); x++) if (a[x] !== b[x]) cols.push(x);
    const text = cells => cells.map(c => c.split('\u0001')[0]).join('');
    out.push(`  line ${top + i}: ${cols.length} cells differ, first at col ${cols[0]}\n`
      + `    alacritty:   ${JSON.stringify(text(a))}\n    kasaterm.js: ${JSON.stringify(text(b))}\n`
      + `    alacritty   cell: ${describeCell(a[cols[0]] || '')}\n    kasaterm.js cell: ${describeCell(b[cols[0]] || '')}`);
    if (out.length >= 6) break;
  }
  return out.join('\n');
}

// Returns {checked, mismatch} — mismatch carries a readable diff of the first failing chunk.
export function compareCast(cast, { scrollback = 100000, profile, env } = {}) {
  // The first line answers the opening resize that creates the terminal.
  const ref = runOracle(frames(cast), env).slice(1);
  const term = new KasaTerm.Terminal({ cols: cast.header.cols, rows: cast.header.rows, scrollback, profile });
  const events = attach(term);
  const refEvents = [];
  let checked = 0;
  const fail = (index, what, detail) => ({ checked, mismatch: { index, what, detail } });
  for (let i = 0; i < cast.events.length; i++) {
    const e = cast.events[i];
    if (e.kind === 'r') term.resize(e.cols, e.rows); else term.write(e.data);
    const want = ref[i];
    refEvents.push(...want.events);
    const got = state(term);
    const { i: _i, events: _e, ...wantState } = want;
    if (Object.keys(wantState).some(k => !same(wantState[k], got[k]))) {
      const dump = runOracle(frames(cast, i + 1, 'd'), env);
      const wantRows = dump[dump.length - 1].dump;
      const gotRows = []; for (let y = 0; y < term.rows; y++) gotRows.push(rowKey(term, y));
      const fields = Object.keys(wantState).filter(k => !same(wantState[k], got[k]) && k !== 'rows');
      const head = fields.map(k => `  ${k}: alacritty ${JSON.stringify(wantState[k])} kasaterm.js ${JSON.stringify(got[k])}`).join('\n');
      return fail(i, fields.join(',') || 'rows', [head, diffRows(wantRows, gotRows, 0)].filter(Boolean).join('\n'));
    }
    if (!same(refEvents, events)) {
      return fail(i, 'events', `  alacritty:   ${JSON.stringify(refEvents.slice(-5))}\n  kasaterm.js: ${JSON.stringify(events.slice(-5))}`);
    }
    checked++;
  }
  term.flushSync();
  const end = ref[ref.length - 1];
  const gotAll = [];
  for (let y = -term.historySize; y < term.rows; y++) gotAll.push(rowKey(term, y));
  if (!same(end.dump, gotAll)) {
    return fail(cast.events.length, 'scrollback', diffRows(end.dump, gotAll, end.top));
  }
  return { checked, mismatch: null };
}
