// `node test/xterm.mjs [filter]` — what xterm.js does and kasaterm's own terminal does not.
//
// kasaterm.js runs these only with `profile: 'xterm'`; the default profile must keep ignoring
// them so the browser shows what the app window shows. The answer is xterm.js itself
// (@xterm/headless, a devDependency) cell by cell; where xterm.js answers with a placeholder or
// leaves the reply to its renderer, the case states the reply the spec asks for instead. Kitty
// keyboard flags are a feature kasaterm's terminal has but keeps off, so those cases go to the
// alacritty oracle with the protocol switched on.
import { createRequire } from 'node:module';
import { compareCast } from './oracle.mjs';
import { cast } from './synth.mjs';

const require = createRequire(import.meta.url);
const KasaTerm = require('../dist/kasaterm.js');
let Xterm;
try { ({ Terminal: Xterm } = require('@xterm/headless')); } catch {
  console.error('@xterm/headless missing: npm install (in web/kasaterm-js)');
  process.exit(2);
}
// writeSync is the only synchronous way in; it warns once per terminal.
console.warn = () => {};

const ESC = '\x1b', CSI = '\x1b[', OSC = '\x1b]', DCS = '\x1bP', ST = '\x1b\\';

// Every row different and every column different, so a shifted cell shows.
const fill = (cols, rows) => Array.from({ length: rows }, (_, y) =>
  Array.from({ length: cols }, (_, x) => String.fromCharCode(97 + ((x + 3 * y) % 26))).join('')).join('\r\n');

function xtermColor(cell, fg) {
  const mode = fg ? cell.getFgColorMode() : cell.getBgColorMode();
  const value = fg ? cell.getFgColor() : cell.getBgColor();
  if (mode === 0) return 'd';
  if (mode === 0x3000000) return '#' + value.toString(16).padStart(6, '0');
  return 'p' + value;
}

function ktColor(c) {
  if (c === KasaTerm.DEFAULT_FG || c === KasaTerm.DEFAULT_BG) return 'd';
  if (c >= KasaTerm.COLOR_RGB) return '#' + (c & 0xffffff).toString(16).padStart(6, '0');
  if (c >= KasaTerm.COLOR_INDEXED) return 'p' + (c & 0xff);
  return 'p' + c;
}

const ATTRS = ['bold', 'dim', 'italic', 'underline', 'inverse', 'invisible', 'strike', 'blink', 'overline'];

function xtermSnapshot(t) {
  const buf = t.buffer.active, rows = [];
  for (let y = 0; y < t.rows; y++) {
    const line = buf.getLine(buf.baseY + y), cells = [];
    for (let x = 0; x < t.cols; x++) {
      const c = line.getCell(x);
      const on = [c.isBold(), c.isDim(), c.isItalic(), c.isUnderline(), c.isInverse(), c.isInvisible(),
        c.isStrikethrough(), c.isBlink(), c.isOverline()];
      cells.push({ ch: c.getWidth() === 0 ? '' : (c.getChars() || ' '), fg: xtermColor(c, true), bg: xtermColor(c, false),
        a: ATTRS.filter((_, i) => on[i]).join(' ') });
    }
    rows.push(cells);
  }
  return { rows, cursor: [buf.cursorY, buf.cursorX], alt: buf.type === 'alternate' };
}

function ktSnapshot(t) {
  const rows = [];
  for (let y = 0; y < t.rows; y++) {
    const row = t.line(y), cells = [];
    for (let x = 0; x < t.cols; x++) {
      const f = row.flags(x);
      if (f & KasaTerm.WIDE_CHAR_SPACER) { cells.push({ ch: '', fg: ktColor(row.fg(x)), bg: ktColor(row.bg(x)), a: '' }); continue; }
      const on = [f & KasaTerm.BOLD, f & KasaTerm.DIM, f & KasaTerm.ITALIC, f & KasaTerm.ALL_UNDERLINES, f & KasaTerm.INVERSE,
        f & KasaTerm.HIDDEN, f & KasaTerm.STRIKEOUT, f & KasaTerm.BLINK, f & KasaTerm.OVERLINE];
      cells.push({ ch: row.cp(x) === 9 ? ' ' : row.text(x), fg: ktColor(row.fg(x)), bg: ktColor(row.bg(x)), a: ATTRS.filter((_, i) => on[i]).join(' ') });
    }
    rows.push(cells);
  }
  const cur = t.cursor;
  return { rows, cursor: [cur.line, cur.wrap ? cur.col + 1 : cur.col], alt: t.isAltScreen };
}

// The two terminals describe the blank cell after a wide glyph differently; only what is drawn
// is compared.
function render(rows) { return rows.map(r => r.map(c => c.ch).join('')); }

function diff(want, got) {
  const out = [];
  if (JSON.stringify(want.cursor) !== JSON.stringify(got.cursor)) out.push(`  cursor: xterm ${want.cursor} kasaterm.js ${got.cursor}`);
  if (want.alt !== got.alt) out.push(`  alt screen: xterm ${want.alt} kasaterm.js ${got.alt}`);
  for (let y = 0; y < want.rows.length; y++) {
    for (let x = 0; x < want.rows[y].length; x++) {
      const a = want.rows[y][x], b = got.rows[y][x];
      if (JSON.stringify(a) === JSON.stringify(b)) continue;
      out.push(`  line ${y} col ${x}: xterm ${JSON.stringify(a)} kasaterm.js ${JSON.stringify(b)}`
        + `\n    xterm:       ${JSON.stringify(render(want.rows)[y])}\n    kasaterm.js: ${JSON.stringify(render(got.rows)[y])}`);
      break;
    }
    if (out.length > 6) break;
  }
  return out.join('\n');
}

const normalizeVersion = s => s.replace(/\x1bP>\|[^\x1b]*\x1b\\/g, '\x1bP>|NAME\x1b\\');

// One case: `steps` are strings, Buffers, [cols, rows] or functions run on kasaterm.js alone.
// `xtermSteps` feeds xterm.js an equivalent stream where the published @xterm/headless predates
// a feature its main branch has; `replies` overrides what xterm.js would answer (null: take
// xterm.js's). `check(kt, xt)` adds assertions of its own.
function runCase({ cols = 10, rows = 6, steps, xtermSteps, replies = null, xtermOptions = {}, options = {}, check }) {
  const xt = new Xterm({ cols, rows, allowProposedApi: true, scrollback: 100, ...xtermOptions });
  const kt = new KasaTerm.Terminal({ cols, rows, profile: 'xterm', ...options });
  const xr = [], kr = [];
  xt.onData(d => xr.push(d));
  kt.onData(d => kr.push(d));
  for (const s of xtermSteps || steps) {
    if (typeof s === 'function') continue;
    if (Array.isArray(s)) xt.resize(s[0], s[1]);
    else xt._core.writeSync(typeof s === 'string' ? s : new Uint8Array(s));
  }
  for (const s of steps) {
    if (typeof s === 'function') s(kt);
    else if (Array.isArray(s)) kt.resize(s[0], s[1]);
    else kt.write(s);
  }
  const problems = [];
  const d = diff(xtermSnapshot(xt), ktSnapshot(kt));
  if (d) problems.push(d);
  const want = replies ?? xr.map(normalizeVersion), got = kr.map(normalizeVersion);
  if (JSON.stringify(want) !== JSON.stringify(got)) problems.push(`  replies:\n    want:        ${JSON.stringify(want)}\n    kasaterm.js: ${JSON.stringify(got)}`);
  if (check) { const c = check(kt, xt); if (c) problems.push('  ' + c); }
  return problems.join('\n');
}

const fill6 = fill(10, 6);
const MODE_KEYS = ['applicationCursorKeysMode', 'applicationKeypadMode', 'bracketedPasteMode', 'insertMode', 'mouseTrackingMode',
  'originMode', 'reverseWraparoundMode', 'sendFocusMode', 'showCursor', 'synchronizedOutputMode', 'win32InputMode', 'wraparoundMode'];
const sameModes = (kt, xt) => {
  const bad = MODE_KEYS.filter(k => xt.modes[k] !== undefined && kt.modes[k] !== xt.modes[k]);
  return bad.length ? 'modes differ: ' + bad.map(k => `${k} xterm ${xt.modes[k]} kasaterm.js ${kt.modes[k]}`).join(', ') : '';
};
const XT_ALL = { vtExtensions: { kittyKeyboard: false, win32InputMode: true },
  windowOptions: { pushTitle: true, popTitle: true, getWinSizeChars: true, getCellSizePixels: true } };

export const CASES = {
  'scroll-left-right': { steps: [fill6, `${CSI}2;5r${CSI}3;1H${CSI}44m${CSI}2 @`, `${CSI} A${CSI}0m${CSI}6;1H${CSI} @`, `${CSI}4;1H${CSI}20 A`] },
  'insert-delete-columns': { steps: [fill6, `${CSI}2;4r${CSI}3;4H${CSI}42m${CSI}2'}`, `${CSI}0m${CSI}3'~${CSI}1;9H${CSI}4'}`, `${CSI}6;3H${CSI}'~`, `${CSI}r${CSI}1;1H${CSI}99'~`] },
  'insert-delete-columns-wide': { steps: ['ab한글cd\r\n가나다라마', `${CSI}1;3H${CSI}1'}`, `${CSI}1;6H${CSI}'~`] },
  // `CSI Ps ^` is SD (ECMA-48's corrected final); xterm.js main has it, 6.0.0 not yet.
  'scroll-down-caret': { steps: [fill6, `${CSI}2^`, `${CSI}2;4r${CSI}^`], xtermSteps: [fill6, `${CSI}2T`, `${CSI}2;4r${CSI}T`] },
  'protected-erase': {
    steps: [fill6, `${CSI}1;3H${CSI}1"qPROT${CSI}0"qfree`, `${CSI}1;1H${CSI}?K${CSI}2;3H${CSI}1"qPP${CSI}2"q${CSI}2;5H${CSI}?1K`,
      `${CSI}3;3H${CSI}1"qQQ${CSI}"q${CSI}3;1H${CSI}?2K`, `${CSI}4;2H${CSI}1"qRR${CSI}0"q${CSI}5;5H${CSI}?1J`, `${CSI}2;6H${CSI}?J`,
      `${CSI}6;1H${CSI}1"qSS${CSI}0"q${CSI}?2J`, `${CSI}1;3H${CSI}K${CSI}3;1H${CSI}5X`],
  },
  'soft-reset': {
    steps: [fill6, `${CSI}4h${CSI}?6h${CSI}2;4r${CSI}1;31;44m${ESC}(0${CSI}?25l${CSI}?7l${CSI}2;2Hxq`, `${ESC}7${CSI}!p`, 'qq', `${CSI}5;1Hz${ESC}8w`],
    check: (kt, xt) => sameModes(kt, xt),
  },
  'alt-screen-47-1047-1048': {
    steps: ['main line', `${CSI}2;3H${CSI}?1048h${CSI}4;4Hm${CSI}?1048lr`, `${CSI}?47halt47${CSI}?47l`, 'back', `${CSI}?1047h${CSI}3;1Halt1047`,
      `${CSI}?1047l!`, `${CSI}?47h`, 'again', `${CSI}?1049h${CSI}5;5H${CSI}?47lX`],
  },
  'reverse-wraparound': { steps: [`${CSI}?45habcdefghijKLM`, '\r\b\bX', `${CSI}3;1H\b\bY`, `${CSI}?45l${CSI}2;1H\b\bZ`] },
  'line-feed-new-line': { steps: [`${CSI}20ha\nb\x0bc\x0cd${ESC}De${CSI}20l\nf`] },
  'c1-controls': {
    steps: [Buffer.from('ab\u0084c\u0085d\u0088\r\n\t!\u009b31mR\u009b0m\u009d2;t1\u0007\u0090$qm\u009c', 'utf8'), Buffer.from('\u009c\u0098sos\u009cS\u009e\u009cP\u009f\u009cA\u008dU', 'utf8')],
  },
  'national-charsets': {
    // Dutch `[` is the ĳ ligature; xterm.js keeps "ij" and draws only the i, so it is checked apart.
    rows: 12, cols: 40,
    steps: [['A', '4', 'C', '5', 'R', 'f', 'Q', '9', 'K', 'Y', 'E', '6', 'Z', 'H', '7', '=', '`', '%5', '%6']
      .map(f => `${ESC}(${f}#$@${f === '4' ? '' : '['}\\]^_\`{|}~ `).join(''),
      `\r\n${ESC}%G#${ESC}(A#${ESC}%@#${ESC})K\x0e[\x0f[${ESC}*A${ESC}n#${ESC}+0${ESC}oq${ESC}~[${ESC}}#${ESC}|q${ESC}(B\x0f`,
      `\r\n${ESC}(K${CSI}?2h[${ESC}-A#${ESC}.A#${ESC}/A#`],
    check: kt => { const t = new KasaTerm.Terminal({ cols: 4, rows: 1, profile: 'xterm' }); t.write(`${ESC}(4[`); return t.line(0).text(0) === 'ĳ' ? '' : 'Dutch [ is not ĳ'; },
  },
  // 221/222 (kitty's bold-only and faint-only resets) are in xterm.js main, not 6.0.0.
  'sgr-blink-overline': {
    cols: 20, steps: [`${CSI}5mb${CSI}25mn${CSI}53mo${CSI}55mp${CSI}1;2mq${CSI}221mr${CSI}1;2m${CSI}222ms${CSI}0;6mt`],
    xtermSteps: [`${CSI}5mb${CSI}25mn${CSI}53mo${CSI}55mp${CSI}1;2mq${CSI}22;2mr${CSI}1;2m${CSI}22;1ms${CSI}0;6mt`],
  },
  'replies': {
    steps: [`${CSI}>q${CSI}>1q${CSI}3;4H${CSI}?6n`, `${CSI}0m${DCS}$qm${ST}${CSI}2;5r${DCS}$qr${ST}${DCS}$q"q${ST}${CSI}1"q${DCS}$q"q${ST}`,
      `${DCS}$q"p${ST}${CSI}3 q${DCS}$q q${ST}${DCS}$qx${ST}`],
    // xterm.js reports the cursor style from its options, not from DECSCUSR; DECRQSS ` q`
    // answers with the style in force. DECSCL: xterm.js says VT100 (61); kasaterm's DA1 claims
    // VT220, so 62.
    replies: ['\x1bP>|NAME\x1b\\', '\x1b[?3;4R', '\x1bP1$r0m\x1b\\', '\x1bP1$r2;5r\x1b\\', '\x1bP1$r0"q\x1b\\', '\x1bP1$r1"q\x1b\\',
      '\x1bP1$r62;1"p\x1b\\', '\x1bP1$r3 q\x1b\\', '\x1bP0$r\x1b\\'],
  },
  'decrqss-sgr': {
    // xterm.js always answers `0m`; the spec (and xterm) report the attributes in force.
    steps: [`${CSI}1;3;4:3;7;38;5;100;48;2;1;2;3m${DCS}$qm${ST}${CSI}0;2;9;93;105m${DCS}$qm${ST}`],
    replies: ['\x1bP1$r0;1;3;4:3;7;38;5;100;48;2;1;2;3m\x1b\\', '\x1bP1$r0;2;9;93;105m\x1b\\'],
  },
  'mode-reports': {
    xtermOptions: XT_ALL,
    steps: [`${CSI}2$p${CSI}12$p${CSI}?8$p${CSI}?9$p${CSI}?45$p${CSI}?66$p${CSI}?67$p${CSI}?1015$p${CSI}?1016$p`,
      `${CSI}?1048$p${CSI}?47$p${CSI}?1047$p${CSI}?9001$p${CSI}?9h${CSI}?45h${CSI}?66h${CSI}?9001h${CSI}?1016h`,
      `${CSI}?9$p${CSI}?45$p${CSI}?66$p${CSI}?9001$p${CSI}?1016$p${CSI}?47h${CSI}?47$p${CSI}?1047$p${CSI}20h${CSI}20$p`],
    // win32-input-mode (9001) is in xterm.js main behind vtExtensions, not in 6.0.0.
    replies: ['2;4', '12;3', '?8;3', '?9;2', '?45;2', '?66;2', '?67;4', '?1015;4', '?1016;2', '?1048;1', '?47;2', '?1047;2', '?9001;2',
      '?9;1', '?45;1', '?66;1', '?9001;1', '?1016;1', '?47;1', '?1047;1', '20;1'].map(r => `\x1b[${r}$y`),
    check: (kt, xt) => sameModes(kt, xt),
  },
  'window-ops': {
    xtermOptions: XT_ALL, options: { cellPixels: { width: 7, height: 15 } },
    steps: [`${OSC}2;one${ST}${OSC}1;icon1${ST}${CSI}22;1t${CSI}22;2t${OSC}2;two${ST}${OSC}1;icon2${ST}`, `${CSI}23;2t${CSI}18t${CSI}16t`, `${CSI}23;1t`],
    // xterm.js leaves the cell size to its renderer; the spec answers CSI 6 ; height ; width t.
    replies: ['\x1b[8;6;10t', '\x1b[6;15;7t'],
    check: (kt, xt) => kt.title !== xt._core._inputHandler._windowTitle || kt.iconName !== xt._core._inputHandler._iconName
      ? `title/icon: xterm ${xt._core._inputHandler._windowTitle}/${xt._core._inputHandler._iconName} kasaterm.js ${kt.title}/${kt.iconName}` : '',
  },
  'color-scheme': {
    // xterm.js hands the query to its theme service; the answer is the color-palette-update
    // extension's: CSI ? 997 ; 1 n dark, 2 light, unasked while 2031 is set.
    steps: [`${CSI}?996n${CSI}?2031h`, kt => kt.setColorScheme('light'), `${CSI}?2031l`, kt => kt.setColorScheme('dark'), `${CSI}?996n`],
    options: { colorScheme: 'dark' },
    replies: ['\x1b[?997;1n', '\x1b[?997;2n', '\x1b[?997;1n'],
  },
};

// xterm.js's buffer API read back from both, line by line.
function sameBuffer(kt, xt) {
  const a = xt.buffer.active, b = kt.buffer.active, out = [];
  for (const k of ['type', 'cursorX', 'cursorY', 'baseY', 'viewportY', 'length']) if (a[k] !== b[k]) out.push(`${k}: xterm ${a[k]} kasaterm.js ${b[k]}`);
  for (let y = 0; y < a.length; y++) {
    const la = a.getLine(y), lb = b.getLine(y);
    if (la.translateToString(true) !== lb.translateToString(true)) out.push(`line ${y}: xterm ${JSON.stringify(la.translateToString(true))} kasaterm.js ${JSON.stringify(lb.translateToString(true))}`);
    if (la.isWrapped !== lb.isWrapped) out.push(`line ${y} isWrapped: xterm ${la.isWrapped} kasaterm.js ${lb.isWrapped}`);
    if (la.translateToString(false, 2, 7) !== lb.translateToString(false, 2, 7)) out.push(`line ${y} cols 2-7 differ`);
    for (let x = 0; x < la.length; x++) {
      const ca = la.getCell(x), cb = lb.getCell(x);
      const key = c => [c.getWidth(), c.getFgColorMode(), c.getFgColor(), c.getBgColorMode(), c.getBgColor(), !!c.isBold(), !!c.isUnderline(),
        c.isAttributeDefault(), c.getWidth() ? c.getChars() || ' ' : ''].join();
      if (key(ca) !== key(cb)) { out.push(`line ${y} col ${x}: xterm ${key(ca)} kasaterm.js ${key(cb)}`); break; }
    }
    if (out.length > 5) break;
  }
  return out.join('\n  ');
}

export const BUFFER_CASES = {
  'buffer-api': {
    rows: 3, steps: ['one\r\n', `${CSI}1;31mtwo${CSI}0m ${CSI}38;5;200;48;2;1;2;3mx${CSI}m\r\n`, 'abcdefghijklm\r\n', '한글이다가나\r\n', `${CSI}4mu${CSI}24mend`],
    check: sameBuffer,
  },
  'buffer-api-alt': { rows: 3, steps: ['main\r\nmore', `${CSI}?1049hALT${CSI}2;2H`], check: (kt, xt) => sameBuffer(kt, xt) || (xt.buffer.normal.getLine(0).translateToString(true) === kt.buffer.normal.getLine(0).translateToString(true) ? '' : 'normal buffer differs') },
};

// Kitty keyboard flags, against kasaterm's terminal with the protocol on.
export const KITTY_CASES = {
  'kitty-flags': cast(20, 5, [`${CSI}>1u${CSI}?u${CSI}>3u${CSI}?u${CSI}=5;2u${CSI}?u${CSI}=1;3u${CSI}?u`, `${CSI}=31u${CSI}?u${CSI}=2;1u`,
    `${CSI}<u${CSI}?u${CSI}<5u${CSI}?u${CSI}>u${CSI}?u`, `${CSI}>1u${CSI}?1049h${CSI}?u${CSI}>2u${CSI}?u${CSI}?1049l${CSI}?u`,
    `${CSI}?1049h${CSI}?u${CSI}?1049l${ESC}c${CSI}?u${CSI}>4u${CSI}=8;2u${CSI}?u`]),
};

const filter = process.argv[2] || '';
let failed = 0, total = 0;
for (const [name, c] of Object.entries(CASES)) {
  if (filter && !name.includes(filter)) continue;
  total++;
  let res;
  try { res = runCase(c); } catch (e) { res = '  threw: ' + (e.stack || e); }
  if (res) { failed++; console.log(`FAIL ${name}\n${res}`); }
}
for (const [name, c] of Object.entries(BUFFER_CASES)) {
  if (filter && !name.includes(filter)) continue;
  total++;
  let res;
  try { res = runCase({ ...c, options: { profile: 'alacritty' } }); } catch (e) { res = '  threw: ' + (e.stack || e); }
  if (res) { failed++; console.log(`FAIL ${name}\n${res}`); }
}
for (const [name, c] of Object.entries(KITTY_CASES)) {
  if (filter && !name.includes(filter)) continue;
  total++;
  const res = compareCast(c, { profile: 'xterm', env: { KTJS_KITTY: '1' } });
  if (res.mismatch) { failed++; console.log(`FAIL ${name} at chunk ${res.mismatch.index} (${res.mismatch.what})\n${res.mismatch.detail}`); }
}
console.log(`${total - failed}/${total} xterm-only cases match`);
process.exit(failed ? 1 : 0);
