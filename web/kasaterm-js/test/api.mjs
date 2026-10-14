// `node test/api.mjs` — the xterm.js-style API around the parser: handler hooks, reset/clear,
// write callbacks, modes and events. Hooks that decline must leave the screen exactly as the app
// shows it, so a recording is replayed with every kind of hook registered.
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { readCast, rowKey } from './oracle.mjs';

const require = createRequire(import.meta.url);
const { Terminal, MODE } = require('../dist/kasaterm.js');
const here = dirname(fileURLToPath(import.meta.url));
const ESC = '\x1b', CSI = '\x1b[', OSC = '\x1b]', ST = '\x1b\\';

// A tab leaves a tab character in the blank cell it crosses (alacritty does, for copying).
const text = (t, y) => { let s = ''; for (let x = 0; x < t.cols; x++) s += t.line(y).text(x); return s.replace(/\t/g, ' ').trimEnd(); };
const tests = [];
const test = (name, fn) => tests.push([name, fn]);
const eq = (got, want, what) => {
  if (JSON.stringify(got) !== JSON.stringify(want)) throw new Error(`${what}: want ${JSON.stringify(want)} got ${JSON.stringify(got)}`);
};

test('csi hook takes the sequence and sees xterm.js params', () => {
  const t = new Terminal({ cols: 10, rows: 2 }), seen = [];
  t.parser.registerCsiHandler({ final: 'm' }, p => { seen.push(p); return true; });
  t.write(`${CSI}1;2:3mA${CSI}mB`);
  eq(seen, [[1, 2, [3]], [0]], 'params');
  eq(t.line(0).flags(0), 0, 'SGR not applied');
});

test('handlers run newest first and fall through on false', () => {
  const t = new Terminal({ cols: 10, rows: 2 }), order = [];
  const a = t.parser.registerCsiHandler({ prefix: '?', final: 'h' }, () => { order.push('a'); return false; });
  t.parser.registerCsiHandler({ prefix: '?', final: 'h' }, () => { order.push('b'); return false; });
  t.write(`${CSI}?2004h`);
  eq(order, ['b', 'a'], 'order');
  eq((t.mode & MODE.BRACKETED_PASTE) !== 0, true, 'built-in still ran');
  a.dispose();
  t.write(`${CSI}?2004h`);
  eq(order, ['b', 'a', 'b'], 'after dispose');
});

test('esc hook can stop a full reset', () => {
  const t = new Terminal({ cols: 10, rows: 2 });
  t.parser.registerEscHandler({ final: 'c' }, () => true);
  t.write(`keep${ESC}c`);
  eq(text(t, 0), 'keep', 'screen');
});

test('osc hook gets the text after the number', () => {
  const t = new Terminal({ cols: 10, rows: 2 }), seen = [];
  t.parser.registerOscHandler(1337, d => { seen.push(d); return true; });
  t.parser.registerOscHandler(2, () => false);
  t.write(`${OSC}1337;File=name=a;inline=1:QUJD\x07${OSC}2;title${ST}`);
  eq(seen, ['File=name=a;inline=1:QUJD'], 'data');
  eq(t.title, 'title', 'declined OSC still handled');
});

test('dcs hook gets payload and params (sixel shape)', () => {
  const t = new Terminal({ cols: 10, rows: 2 }), seen = [];
  t.parser.registerDcsHandler({ final: 'q' }, (d, p) => { seen.push([d, p]); return true; });
  t.write(`${ESC}P0;1;8q"1;1;2;2#0;2;0;0;0~-~${ST}after`);
  t.write(Buffer.from(`${ESC}P1qé\x18x`, 'utf8'));
  eq(seen, [['"1;1;2;2#0;2;0;0;0~-~', [0, 1, 8]]], 'payload; CAN aborts');
  eq(text(t, 0), 'afterx', 'screen');
});

test('apc hook gets the data after its identifier', () => {
  const t = new Terminal({ cols: 10, rows: 2 }), seen = [];
  t.parser.registerApcHandler({ final: 'G' }, d => { seen.push(d); return true; });
  t.write(`${ESC}_Ga=T,f=100;AAAA${ST}${ESC}_Xother${ST}ok`);
  eq(seen, ['a=T,f=100;AAAA'], 'data');
  eq(text(t, 0), 'ok', 'screen');
});

test('declining hooks change nothing in a real recording', () => {
  const cast = readCast(join(here, '..', 'corpus', 'claude-fullscreen.kcast.gz'));
  const plain = new Terminal({ cols: cast.header.cols, rows: cast.header.rows });
  const hooked = new Terminal({ cols: cast.header.cols, rows: cast.header.rows });
  for (const f of 'ABCDEFGHJKLMPSTXZ@abcdfghlmnqrstu') hooked.parser.registerCsiHandler({ final: f }, () => false);
  hooked.parser.registerCsiHandler({ prefix: '?', final: 'h' }, () => false);
  hooked.parser.registerCsiHandler({ prefix: '?', final: 'l' }, () => false);
  for (const n of [0, 2, 4, 8, 10, 11, 52, 133]) hooked.parser.registerOscHandler(n, () => false);
  hooked.parser.registerEscHandler({ final: '7' }, () => false);
  hooked.parser.registerDcsHandler({ final: 'q' }, () => false);
  hooked.parser.registerApcHandler({ final: 'G' }, () => false);
  for (const e of cast.events) {
    for (const t of [plain, hooked]) { if (e.kind === 'r') t.resize(e.cols, e.rows); else t.write(e.data); }
  }
  for (let y = -plain.historySize; y < plain.rows; y++) eq(rowKey(hooked, y), rowKey(plain, y), `line ${y}`);
  eq(hooked.mode, plain.mode, 'mode');
});

test('reset returns to a fresh terminal mid-sequence', () => {
  const t = new Terminal({ cols: 10, rows: 3 });
  t.write(`a\r\nb\r\nc\r\nd${CSI}?2004h${CSI}31m${OSC}2;x\x07${OSC}4;1;#010203\x07${CSI}3`);
  t.reset();
  t.write('X');
  eq([text(t, 0), t.historySize, t.mode, t.title, t.colors[1], t.line(0).fg(0)], ['X', 0, new Terminal().mode, null, null, 256], 'state');
});

test('clear keeps only the cursor line, at the top', () => {
  const t = new Terminal({ cols: 10, rows: 3 });
  t.write('1\r\n2\r\n3\r\n4\r\n$ cmd');
  t.clear();
  eq([text(t, 0), text(t, 1), text(t, 2), t.historySize, t.cursor.line, t.cursor.col], ['$ cmd', '', '', 0, 0, 5], 'state');
});

test('writeln and write callbacks', () => {
  const t = new Terminal({ cols: 10, rows: 3 }), calls = [];
  t.writeln('one', () => calls.push(1));
  t.write('two', () => calls.push(2));
  eq([text(t, 0), text(t, 1), calls], ['one', 'two', [1, 2]], 'state');
});

test('modes getter', () => {
  const t = new Terminal({ cols: 10, rows: 3 });
  t.write(`${CSI}?1h${CSI}?2004h${CSI}?1002h${CSI}?1006h${CSI}?25l${CSI}4h${ESC}=`);
  const m = t.modes;
  eq([m.applicationCursorKeysMode, m.bracketedPasteMode, m.mouseTrackingMode, m.mouseEncoding, m.showCursor, m.insertMode, m.applicationKeypadMode],
    [true, true, 'drag', 'sgr', false, true, true], 'modes');
  const x = new Terminal({ cols: 10, rows: 3, profile: 'xterm' });
  x.write(`${CSI}?9h${CSI}?45h${CSI}>5u`);
  eq([x.modes.mouseTrackingMode, x.modes.reverseWraparoundMode, x.keyboardFlags], ['x10', true, 5], 'xterm modes');
  t.write(`${CSI}>5u`);
  eq(t.keyboardFlags, 0, 'kitty flags stay off by default, as in the app');
});

test('events: linefeed, cursormove, scroll, iconname, pointershape', () => {
  const t = new Terminal({ cols: 10, rows: 2 }), ev = [];
  for (const n of ['linefeed', 'cursormove', 'scroll', 'iconname', 'pointershape']) t.on(n, (...a) => ev.push([n, ...a]));
  t.write('a\nb\nc');
  t.write('');
  t.write(`${OSC}1;icon\x07${OSC}0;both\x07${OSC}22;pointer\x07`);
  eq(ev, [['linefeed'], ['linefeed'], ['scroll', 1], ['cursormove'], ['iconname', 'icon'], ['iconname', 'both'], ['pointershape', 'pointer']], 'events');
  eq(t.iconName, 'both', 'icon name');
});

test('convertEol and tabStopWidth options', () => {
  const t = new Terminal({ cols: 20, rows: 3, convertEol: true, tabStopWidth: 4 });
  t.write('ab\ncd\tx');
  eq([text(t, 1), t.cursor.col], ['cd  x', 5], 'state');
});

test('the default profile ignores what only xterm.js does', () => {
  const t = new Terminal({ cols: 10, rows: 3 }), replies = [];
  t.onData(d => replies.push(d));
  t.write(`abcdef${CSI}1;3H${CSI}2 @${CSI}2'}${CSI}>q${CSI}?6n${ESC}(Ka[${CSI}20h\nz`);
  eq([text(t, 0), text(t, 1), replies], ['aba[ef', '    z', []], 'state');
});

let failed = 0;
for (const [name, fn] of tests) {
  try { fn(); } catch (e) { failed++; console.log(`FAIL ${name}\n  ${e.message}`); }
}
console.log(`${tests.length - failed}/${tests.length} API checks pass`);
process.exit(failed ? 1 : 0);
