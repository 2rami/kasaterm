// VT parser and terminal state.
//
// The parser is the DEC/ANSI state machine from vt100.net (Paul Williams), with the deviations
// kasaterm's own parser (vte 0.15) makes: UTF-8 in the ground state, OSC ended by BEL, 7-bit
// controls only, at most 32 numbers and 2 intermediates per sequence. Byte-for-byte agreement
// with it is the point — the same PTY stream must leave the same grid in the app and in here.

const MODE = {
  SHOW_CURSOR: 1, APP_CURSOR: 2, APP_KEYPAD: 4, MOUSE_REPORT_CLICK: 8, BRACKETED_PASTE: 16,
  SGR_MOUSE: 32, MOUSE_MOTION: 64, LINE_WRAP: 128, LINE_FEED_NEW_LINE: 256, ORIGIN: 512,
  INSERT: 1024, FOCUS_IN_OUT: 2048, ALT_SCREEN: 4096, MOUSE_DRAG: 8192, UTF8_MOUSE: 16384,
  ALTERNATE_SCROLL: 32768, URGENCY_HINTS: 131072,
};
const MOUSE_MODE = MODE.MOUSE_REPORT_CLICK | MODE.MOUSE_MOTION | MODE.MOUSE_DRAG;
const DEFAULT_MODE = MODE.SHOW_CURSOR | MODE.LINE_WRAP | MODE.ALTERNATE_SCROLL | MODE.URGENCY_HINTS;
// Kitty keyboard flags sit in the mode word from this bit up, as alacritty keeps them.
const KITTY_SHIFT = 18;
const KITTY_MODE = 0x1f << KITTY_SHIFT;
const KEYBOARD_STACK_MAX = 4096;

// Modes only `profile: 'xterm'` knows; kept apart so `mode` stays alacritty's word.
const XMODE = { REVERSE_WRAP: 1, X10_MOUSE: 2, SGR_PIXELS_MOUSE: 4, WIN32_INPUT: 8, COLOR_SCHEME_REPORTS: 16, SIXEL_DISPLAY: 32 };

const VERSION = '0.1.0';

const G_INDEX = { 0x28: 0, 0x29: 1, 0x2a: 2, 0x2b: 3 };
const XTERM_G_INDEX = { 0x2d: 1, 0x2e: 2 };
// ESC n, o, |, }, ~: LS2, LS3, LS3R, LS2R, LS1R.
const XTERM_SHIFTS = { 0x6e: 2, 0x6f: 3, 0x7c: 3, 0x7d: 2, 0x7e: 1 };

const DEC_SPECIAL = {
  0x5f: 0x20, 0x60: 0x25c6, 0x61: 0x2592, 0x62: 0x2409, 0x63: 0x240c, 0x64: 0x240d, 0x65: 0x240a,
  0x66: 0xb0, 0x67: 0xb1, 0x68: 0x2424, 0x69: 0x240b, 0x6a: 0x2518, 0x6b: 0x2510, 0x6c: 0x250c,
  0x6d: 0x2514, 0x6e: 0x253c, 0x6f: 0x23ba, 0x70: 0x23bb, 0x71: 0x2500, 0x72: 0x23bc, 0x73: 0x23bd,
  0x74: 0x251c, 0x75: 0x2524, 0x76: 0x2534, 0x77: 0x252c, 0x78: 0x2502, 0x79: 0x2264, 0x7a: 0x2265,
  0x7b: 0x3c0, 0x7c: 0x2260, 0x7d: 0xa3, 0x7e: 0xb7,
};

// VT220 national replacement sets (`ESC ( F` with these finals, xterm profile): each string
// gives the replacements for `#@[\]^_`{|}~` in order, a space where the ASCII glyph stays.
const NRCS_SLOTS = '#@[\\]^_`{|}~';
const NRCS = (() => {
  const sets = {
    A: '£           ', 4: '£¾ĳ½|   ¨f¼´', C: '  ÄÖÅÜ éäöåü', R: '£à°ç§   éùè¨', Q: ' àâçêî ôéùèû',
    K: ' §ÄÖÜ   äöüß', Y: '£§°çé  ùàòèì', E: ' ÄÆØÅÜ äæøåü', Z: '£§¡Ñ¿   °ñç ', H: ' ÉÄÖÅÜ éäöåü',
    '=': 'ùàéçêîèôäöüû',
  };
  sets[5] = sets.C; sets[6] = sets.E; sets[7] = sets.H;
  const out = {};
  for (const [k, s] of Object.entries(sets)) {
    const map = {};
    [...s].forEach((ch, i) => { if (ch !== ' ') map[NRCS_SLOTS.charCodeAt(i)] = ch.codePointAt(0); });
    out[k] = map;
  }
  return out;
})();

const CURSOR_SHAPES = ['block', 'underline', 'beam'];
const DEFAULT_CURSOR_STYLE = { shape: 'block', blinking: false };

const S_GROUND = 0, S_ESC = 1, S_ESC_INT = 2, S_CSI_ENTRY = 3, S_CSI_PARAM = 4, S_CSI_INT = 5,
  S_CSI_IGNORE = 6, S_DCS_ENTRY = 7, S_DCS_PARAM = 8, S_DCS_INT = 9, S_DCS_PASS = 10, S_DCS_IGNORE = 11,
  S_OSC = 12, S_SOS = 13;

const MAX_PARAMS = 32;
const SYNC_BUFFER_SIZE = 0x200000;
const MAX_OSC_PARAMS = 16;
const TITLE_STACK_MAX = 4096;
// Alacritty reports itself as version 0.26.0 in secondary DA.
const DA2_REPLY = '\x1b[>0;2600;1c';
const DA1_REPLY = '\x1b[?62;22c';
// With sixel on (xterm profile), as xterm.js's image addon: sixel (4) and national charsets (9).
const DA1_XTERM_REPLY = '\x1b[?62;4;9;22c';
const STRING_LIMIT = 33554432;   // xterm.js image addon sixelSizeLimit; also caps DCS and APC

const textDecoder = typeof TextDecoder !== 'undefined' ? new TextDecoder() : null;
const textEncoder = typeof TextEncoder !== 'undefined' ? new TextEncoder() : null;

function utf8(bytes) { return textDecoder.decode(bytes); }

const strictDecoder = typeof TextDecoder !== 'undefined' ? new TextDecoder('utf-8', { fatal: true }) : null;

function strictUtf8(bytes) {
  try { return strictDecoder.decode(bytes); } catch { return null; }
}

function hex2(n) { return n.toString(16).padStart(2, '0'); }

// String payloads (OSC, DCS, APC) can run to megabytes — inline pictures — so they grow in place.
class Bytes {
  constructor() { this.buf = new Uint8Array(64); this.len = 0; }
  push(b) {
    if (this.len === this.buf.length) { const g = new Uint8Array(this.buf.length * 2); g.set(this.buf); this.buf = g; }
    this.buf[this.len++] = b;
  }
  last() { return this.len ? this.buf[this.len - 1] : -1; }
  pop() { if (this.len) this.len--; }
  view() { return this.buf.subarray(0, this.len); }
  clear() { this.len = 0; if (this.buf.length > 65536) this.buf = new Uint8Array(64); }
}

// Parameter groups in xterm.js's public form: `1;2:3` is [1, 2, [3]].
function paramsArray(groups) {
  const out = [];
  for (const g of groups) { out.push(g[0]); if (g.length > 1) out.push(g.slice(1)); }
  return out;
}

// XParseColor: `#rgb`…`#rrrrggggbbbb` or `rgb:r/g/b` with 1–4 hex digits per channel.
function parseXColor(s) {
  if (s.startsWith('#')) {
    const body = s.slice(1), n = Math.floor(body.length / 3);
    const part = p => {
      if (!/^[0-9a-fA-F]+$/.test(p)) return null;
      return (parseInt(p, 16) << 4) >> (4 * Math.max(0, p.length - 1)) & 0xff;
    };
    const r = part(body.slice(0, n)), g = part(body.slice(n, 2 * n)), b = part(body.slice(2 * n));
    return r === null || g === null || b === null ? null : [r, g, b];
  }
  if (s.startsWith('rgb:')) {
    const parts = s.slice(4).split('/');
    if (parts.length !== 3) return null;
    const out = parts.map(p => {
      if (p.length === 0 || p.length > 4 || !/^[0-9a-fA-F]+$/.test(p)) return null;
      return Math.floor(255 * parseInt(p, 16) / (16 ** p.length - 1));
    });
    return out.includes(null) ? null : out;
  }
  return null;
}

function parseU8(s) { return /^[0-9]+$/.test(s) && Number(s) <= 255 ? Number(s) : null; }

function base64Decode(s) {
  if (!/^[A-Za-z0-9+/]*={0,2}$/.test(s) || s.length % 4 !== 0) return null;
  try {
    const bin = atob(s), out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  } catch { return null; }
}

class Terminal {
  constructor(options = {}) {
    const cols = Math.max(2, options.cols || 80), rows = Math.max(1, options.rows || 24);
    this.scrollback = options.scrollback ?? 10000;
    this.listeners = new Map();
    this.grid = new Grid(rows, cols, this.scrollback);
    this.inactive = new Grid(rows, cols, 0);
    this.mode = DEFAULT_MODE;
    this.top = 0;
    this.bottom = rows;
    this.activeCharset = 0;
    this.cursorStyle = null;
    this.title = null;
    this.titleStack = [];
    this.colors = new Array(269).fill(null);
    this.precedingChar = null;
    this.hyperlinkSerial = 0;
    this.syncSince = 0;
    this.syncPending = false;
    this.syncBuf = new Uint8Array(4096);
    this.syncLen = 0;
    this.terminated = false;
    this.normalize = options.normalize ?? true;
    // 'alacritty' (default) interprets exactly as the app window does. 'xterm' adds what xterm.js
    // understands and the app ignores — the mirror would then disagree with the app.
    this.xterm = options.profile === 'xterm';
    this.convertEol = !!options.convertEol;
    this.tabStopWidth = options.tabStopWidth || 8;
    this.tabs = this.makeTabs(cols);
    this.cellPixels = options.cellPixels || null;
    this.colorScheme = options.colorScheme || null;
    this.iconName = null;
    this.iconStack = [];
    this.xmode = 0;
    this.keyboardStack = [];
    this.inactiveKeyboardStack = [];
    this.handlers = null;
    this.parser = this.parserApi();
    this.addons = [];
    this.markers = new Set();
    this.queue = [];             // writes waiting behind a paused parser or an async drain
    this.carry = [];
    this.paused = false;
    this.pauseToken = 0;
    this.asyncWrite = !!options.asyncWrite;
    this.widthOf = charWidth;
    this.unicodeVersion = 'alacritty';
    this.unicode = this.unicodeApi();
    this.windowsPty = null;
    this.setWindowsPty(options.windowsPty);
    this.oscRaw = new Bytes();
    this.oscBounds = [];
    this.kittyGraphics = new KittyGraphics();
    this.inlineImages = [];      // iTerm and sixel pictures: {uid, key, marker, pixels}
    this.imageSerial = 0;
    this.sixelPalette = SIXEL_PALETTE_LIMIT;
    this.options = this.liveOptions();
    this.heldUtf8 = new Uint8Array(0);
    this.resetParser();
    this.u8need = 0; this.u8cp = 0; this.u8lo = 0x80; this.u8hi = 0xbf;
  }

  makeTabs(cols, old) {
    const t = new Uint8Array(cols), every = this.tabStopWidth;
    for (let i = 0; i < cols; i++) t[i] = old && i < old.length ? old[i] : (i % every === 0 ? 1 : 0);
    return t;
  }

  get cols() { return this.grid.cols; }
  get rows() { return this.grid.lines; }
  get cursor() { return this.grid.cursor; }
  get isAltScreen() { return (this.mode & MODE.ALT_SCREEN) !== 0; }
  get historySize() { return this.grid.historySize; }
  line(y) { return this.grid.row(y); }

  // ── events ──

  on(name, fn) {
    if (!this.listeners.has(name)) this.listeners.set(name, new Set());
    this.listeners.get(name).add(fn);
    return { dispose: () => this.listeners.get(name).delete(fn) };
  }

  onData(fn) { return this.on('data', fn); }

  emit(name, ...args) {
    const set = this.listeners.get(name);
    if (set) for (const fn of set) fn(...args);
  }

  reply(s) { this.emit('reply', s); this.emit('data', s); }

  // ── input ──

  // Bytes from the PTY. Between `CSI ? 2026 h` and `CSI ? 2026 l` (synchronized output) they are
  // held and applied in one go, as kasaterm does — so a resize arriving mid-update reflows the
  // screen as it was before the update, and no half-drawn frame is ever visible.
  //
  // Normally a write is applied before it returns. It waits its turn instead while a parser hook
  // is settling a promise, while another write is being applied (a handler wrote), or always
  // with `asyncWrite` — then the queue drains in slices of about a frame, as xterm.js does.
  write(data, callback) {
    if (this.paused || this.queue.length || this.writing || this.asyncWrite) {
      this.queue.push({ data, callback, raw: false });
      if (!this.paused && !this.writing) { if (this.asyncWrite) this.scheduleDrain(); else this.drain(); }
      return;
    }
    this.apply(data, callback, false);
  }

  apply(data, callback, raw) {
    this.writing = true;
    const bytes = raw ? data : typeof data === 'string' ? textEncoder.encode(this.normalize ? data.normalize('NFC') : data)
      : this.normalize ? this.composeBatch(data) : data;
    const cur = this.grid.cursor, line = cur.line, col = cur.col, scrolled = this.grid.scrolled;
    let i = 0;
    while (i < bytes.length && !this.paused) {
      i += this.syncPending ? this.advanceSync(bytes.subarray(i)) : this.parse(bytes, i, true);
    }
    this.writing = false;
    if (this.paused) {
      // The rest, already composed, goes first when the hook settles; the callback waits for it.
      this.queue.unshift(...this.carry.map(b => ({ data: b, callback: null, raw: true })), { data: bytes.subarray(i), callback, raw: true });
      this.carry = [];
      callback = null;
    }
    if (this.inlineImages.length + this.kittyGraphics.placements.length && this.historySize >= this.scrollback) this.dropImages();
    if (this.grid.scrolled !== scrolled && this.listeners.has('scroll')) this.emit('scroll', this.grid.scrolled - scrolled);
    const now = this.grid.cursor;
    if ((now !== cur || now.line !== line || now.col !== col) && this.listeners.has('cursormove')) this.emit('cursormove');
    this.emit('write');
    if (callback) callback();
    if (this.queue.length && !this.paused && !this.draining && !this.asyncWrite) this.drain();
  }

  drain() {
    if (this.draining) return;
    this.draining = true;
    const start = Date.now();
    while (!this.paused && this.queue.length) {
      const w = this.queue.shift();
      this.apply(w.data, w.callback, w.raw);
      if (this.asyncWrite && this.queue.length && Date.now() - start >= 12) { this.scheduleDrain(); break; }
    }
    this.draining = false;
  }

  scheduleDrain() {
    if (this.drainTimer) return;
    this.drainTimer = setTimeout(() => { this.drainTimer = null; this.drain(); }, 0);
  }

  // A hook returned a promise: stop parsing until it settles.
  pause(promise) {
    this.paused = true;
    const token = ++this.pauseToken;
    promise.then(() => {
      if (token !== this.pauseToken) return;
      this.paused = false;
      this.drain();
    });
  }

  writeln(data, callback) { this.write(data); this.write('\r\n', callback); }

  // Back to a freshly made terminal: RIS plus whatever the parser was in the middle of.
  reset() {
    this.queue = [];
    this.carry = [];
    this.paused = false;
    this.pauseToken++;
    this.state = S_GROUND;
    this.resetParser();
    this.u8need = 0; this.u8lo = 0x80; this.u8hi = 0xbf;
    this.heldUtf8 = new Uint8Array(0);
    this.syncPending = false;
    this.syncLen = 0;
    this.resetState();
    this.colors.fill(null);
    this.precedingChar = null;
    this.iconName = null;
    this.iconStack = [];
    this.emit('reset');
  }

  // Drop everything but the cursor's line, which becomes the top line.
  clear() {
    const grid = this.grid, cur = grid.cursor;
    if (cur.line > 0) grid.raw.swap(0, cur.line);
    grid.resetRegion(1, grid.lines, DEFAULT_BG);
    grid.clearHistory();
    cur.line = 0;
    this.emit('scroll', 0);
  }

  get modes() {
    const m = this.mode, x = this.xmode, on = bit => (m & bit) !== 0;
    let mouseTrackingMode = 'none';
    if (x & XMODE.X10_MOUSE) mouseTrackingMode = 'x10';
    else if (on(MODE.MOUSE_MOTION)) mouseTrackingMode = 'any';
    else if (on(MODE.MOUSE_DRAG)) mouseTrackingMode = 'drag';
    else if (on(MODE.MOUSE_REPORT_CLICK)) mouseTrackingMode = 'vt200';
    return {
      applicationCursorKeysMode: on(MODE.APP_CURSOR), applicationKeypadMode: on(MODE.APP_KEYPAD),
      bracketedPasteMode: on(MODE.BRACKETED_PASTE), insertMode: on(MODE.INSERT), mouseTrackingMode,
      mouseEncoding: x & XMODE.SGR_PIXELS_MOUSE ? 'sgr-pixels' : on(MODE.SGR_MOUSE) ? 'sgr' : on(MODE.UTF8_MOUSE) ? 'utf8' : 'default',
      originMode: on(MODE.ORIGIN), reverseWraparoundMode: (x & XMODE.REVERSE_WRAP) !== 0, sendFocusMode: on(MODE.FOCUS_IN_OUT),
      showCursor: on(MODE.SHOW_CURSOR), synchronizedOutputMode: this.syncPending, win32InputMode: (x & XMODE.WIN32_INPUT) !== 0,
      wraparoundMode: on(MODE.LINE_WRAP), alternateScrollMode: on(MODE.ALTERNATE_SCROLL),
      colorSchemeReports: (x & XMODE.COLOR_SCHEME_REPORTS) !== 0,
    };
  }

  // Kitty keyboard progressive-enhancement flags in force (0 when the protocol is off).
  get keyboardFlags() { return (this.mode & KITTY_MODE) >>> KITTY_SHIFT; }

  // ── xterm.js-style surroundings: addons, live options, viewport, buffer ──

  loadAddon(addon) {
    this.addons.push(addon);
    addon.activate(this);
  }

  dispose() {
    for (const addon of this.addons.splice(0)) addon.dispose();
    this.emit('dispose');
    this.listeners.clear();
  }

  // `term.options.x = y` takes effect from the next byte, as in xterm.js.
  liveOptions() {
    const term = this;
    return {
      get profile() { return term.xterm ? 'xterm' : 'alacritty'; },
      set profile(v) { term.xterm = v === 'xterm'; },
      get convertEol() { return term.convertEol; },
      set convertEol(v) { term.convertEol = !!v; },
      get tabStopWidth() { return term.tabStopWidth; },
      set tabStopWidth(v) { term.tabStopWidth = v || 8; term.tabs = term.makeTabs(term.cols); },
      get scrollback() { return term.scrollback; },
      set scrollback(v) { term.scrollback = v; (term.isAltScreen ? term.inactive : term.grid).updateHistory(v); },
      get cellPixels() { return term.cellPixels; },
      set cellPixels(v) { term.cellPixels = v; },
      get windowsPty() { return term.windowsPty; },
      set windowsPty(v) { term.setWindowsPty(v); },
      get asyncWrite() { return term.asyncWrite; },
      set asyncWrite(v) { term.asyncWrite = !!v; },
      get colorScheme() { return term.colorScheme; },
      set colorScheme(v) { term.setColorScheme(v); },
    };
  }

  // A marker on the cursor's line (plus `offset`) of the screen in use; undefined off the buffer.
  registerMarker(offset = 0) {
    const line = this.grid.cursor.line + offset;
    if (line < -this.historySize || line >= this.rows) return undefined;
    return new Marker(this, this.grid, line);
  }

  // The viewport moves over the scrollback; 'viewport' tells the view to redraw.
  scrollLines(n) { this.scrollToOffset(this.grid.displayOffset - n); }
  scrollPages(n) { this.scrollLines(n * (this.rows - 1)); }
  scrollToTop() { this.scrollToOffset(this.historySize); }
  scrollToBottom() { this.scrollToOffset(0); }
  scrollToLine(line) { this.scrollToOffset(this.historySize - line); }

  scrollToOffset(offset) {
    const next = Math.max(0, Math.min(this.historySize, offset));
    if (next === this.grid.displayOffset) return;
    this.grid.displayOffset = next;
    this.emit('viewport', next);
  }

  get buffer() {
    const alt = this.isAltScreen;
    return {
      active: new BufferView(this.grid, alt ? 'alternate' : 'normal'),
      normal: new BufferView(alt ? this.inactive : this.grid, 'normal'),
      alternate: new BufferView(alt ? this.grid : this.inactive, 'alternate'),
      onBufferChange: fn => this.on('screen', () => fn(this.buffer.active)),
    };
  }

  // The host's light/dark scheme: answers `CSI ? 996 n`, and is reported unasked while a program
  // keeps mode 2031 set (xterm profile).
  setColorScheme(scheme) {
    this.colorScheme = scheme;
    if (this.xmode & XMODE.COLOR_SCHEME_REPORTS) this.reportColorScheme();
  }

  reportColorScheme() {
    if (this.colorScheme) this.reply(`\x1b[?997;${this.colorScheme === 'light' ? 2 : 1}n`);
  }

  // ── parser hooks (xterm.js `term.parser`) ──
  //
  // Handlers run newest first before the built-in one; returning true ends the sequence there.
  // An identifier is {prefix, intermediates, final} as in xterm.js (OSC takes the number).
  // Only synchronous handlers: a returned promise counts as not handled.

  parserApi() {
    const add = (kind, key, fn) => {
      if (!this.handlers) this.handlers = { csi: new Map(), esc: new Map(), osc: new Map(), dcs: new Map(), apc: new Map() };
      const map = this.handlers[kind];
      if (!map.has(key)) map.set(key, []);
      const list = map.get(key);
      list.unshift(fn);
      return { dispose: () => { const i = list.indexOf(fn); if (i >= 0) list.splice(i, 1); } };
    };
    const ident = id => (id.prefix || '') + (id.intermediates || '') + id.final;
    return {
      registerCsiHandler: (id, fn) => add('csi', ident(id), fn),
      registerEscHandler: (id, fn) => add('esc', ident(id), fn),
      registerDcsHandler: (id, fn) => add('dcs', ident(id), fn),
      registerApcHandler: (id, fn) => add('apc', ident(id), fn),
      registerOscHandler: (number, fn) => add('osc', String(number), fn),
    };
  }

  // Hooks for one sequence, newest first, then `builtin` unless one returned true. A hook that
  // returns a promise pauses parsing until it settles (as in xterm.js); the rest runs then.
  runHooks(kind, key, args, builtin) {
    const list = this.handlers && this.handlers[kind].get(key);
    if (!list || !list.length) { builtin(); return; }
    const hooks = list.slice();
    const from = i => {
      for (; i < hooks.length; i++) {
        const r = hooks[i](...args);
        if (r === true) return;
        if (r && typeof r.then === 'function') {
          this.pause(Promise.resolve(r).then(ok => { if (ok !== true) from(i + 1); }, () => from(i + 1)));
          return;
        }
      }
      builtin();
    };
    from(0);
  }

  hasHandler(kind, key) {
    const list = this.handlers && this.handlers[kind].get(key);
    return !!(list && list.length);
  }

  // kasaterm composes each PTY read to NFC before parsing (so decomposed Hangul from macOS file
  // names shows as syllables). Doing the same per chunk keeps cells, REP and DEC line-drawing
  // mapping identical. A code point split across chunks is held back until it is complete;
  // a chunk that is not valid UTF-8 is parsed untouched.
  composeBatch(data) {
    let all = data;
    if (this.heldUtf8.length) { all = new Uint8Array(this.heldUtf8.length + data.length); all.set(this.heldUtf8); all.set(data, this.heldUtf8.length); }
    const n = all.length;
    let cut = n;
    for (let back = 1; back <= Math.min(3, n); back++) {
      const i = n - back, b = all[i];
      if ((b & 0xc0) === 0x80) continue;
      const width = (b & 0xe0) === 0xc0 ? 2 : (b & 0xf0) === 0xe0 ? 3 : (b & 0xf8) === 0xf0 ? 4 : 1;
      if (width > 1 && i + width > n) cut = i;
      break;
    }
    this.heldUtf8 = all.slice(cut);
    const batch = cut === n ? all : all.subarray(0, cut);
    let ascii = true;
    for (let i = 0; i < batch.length; i++) if (batch[i] >= 0x80) { ascii = false; break; }
    if (ascii || !mayChangeUnderNfc(batch)) return batch;
    const text = strictUtf8(batch);
    return text === null ? batch : textEncoder.encode(text.normalize('NFC'));
  }

  // Runs the state machine from `start`; with `stopAtSync` it returns right after a
  // synchronized-update start so the rest can be held. Returns the number of bytes consumed.
  parse(bytes, start, stopAtSync) {
    this.terminated = false;
    let i = start;
    for (; i < bytes.length; i++) {
      if (this.state === S_GROUND) i = this.ground(bytes, i);
      else { this.step(bytes[i]); if (this.paused) { i++; break; } }
      if (stopAtSync && this.terminated) { i++; break; }
    }
    return i - start;
  }

  advanceSync(bytes) {
    if (this.syncLen + bytes.length >= SYNC_BUFFER_SIZE - 1) {
      this.stopSync(null);
      return this.parse(bytes, 0, true);
    }
    if (this.syncLen + bytes.length > this.syncBuf.length) {
      const grown = new Uint8Array(Math.max(this.syncBuf.length * 2, this.syncLen + bytes.length));
      grown.set(this.syncBuf.subarray(0, this.syncLen));
      this.syncBuf = grown;
    }
    this.syncBuf.set(bytes, this.syncLen);
    this.syncLen += bytes.length;
    // Only the exact `ESC[?2026h` / `ESC[?2026l` byte strings are looked for, newest first.
    const buf = this.syncBuf, len = this.syncLen;
    const from = Math.max(0, len - bytes.length - 7), to = Math.max(0, len - 7);
    let bsu = null;
    for (let p = to - 1; p >= from; p--) {
      if (buf[p] !== 0x1b || buf[p + 1] !== 0x5b || buf[p + 2] !== 0x3f || buf[p + 3] !== 0x32
        || buf[p + 4] !== 0x30 || buf[p + 5] !== 0x32 || buf[p + 6] !== 0x36) continue;
      if (buf[p + 7] === 0x68) { bsu = p; this.syncSince = Date.now(); }
      else if (buf[p + 7] === 0x6c) { this.stopSync(bsu); break; }
    }
    return bytes.length;
  }

  // Apply the held bytes. With `bsu` set, a new update began inside them: apply up to it and
  // keep holding the rest.
  stopSync(bsu) {
    const held = this.syncBuf.slice(0, this.syncLen);
    const used = this.parse(held.subarray(0, bsu === null ? held.length : bsu), 0, false);
    if (this.paused) {
      // A hook paused inside the update: everything after it is parsed again on resuming,
      // including any update that starts later.
      this.carry.push(held.subarray(used));
      this.syncPending = false;
      this.syncLen = 0;
      return;
    }
    if (bsu !== null) {
      this.syncBuf.set(held.subarray(bsu), 0);
      this.syncLen = held.length - bsu;
    } else {
      this.syncPending = false;
      this.syncLen = 0;
      this.emit('sync-end');
    }
  }

  // Give up waiting for the end of a synchronized update (kasaterm itself never does).
  flushSync() {
    if (!this.syncPending) return;
    this.stopSync(null);
    if (this.carry.length) { this.queue.unshift(...this.carry.map(b => ({ data: b, callback: null, raw: true }))); this.carry = []; }
  }

  // Ground state: decode UTF-8 straight to print/execute. Returns the index of the last byte
  // consumed so the caller's loop can continue after it.
  ground(bytes, i) {
    const n = bytes.length;
    for (; i < n; i++) {
      const b = bytes[i];
      if (this.u8need > 0) {
        if (b >= this.u8lo && b <= this.u8hi) {
          this.u8cp = (this.u8cp << 6) | (b & 0x3f);
          this.u8lo = 0x80; this.u8hi = 0xbf;
          if (--this.u8need === 0) {
            if (this.u8cp >= 0xa0) this.print(this.u8cp);
            else if (this.xterm && this.c1(this.u8cp)) return i;
          }
          continue;
        }
        this.u8need = 0;
        this.print(0xfffd);
      }
      if (b < 0x80) {
        if (b >= 0x20) {
          if (b === 0x7f) { this.print(b); continue; }
          let j = i + 1;
          while (j < n && bytes[j] >= 0x20 && bytes[j] < 0x7f) j++;
          this.printAscii(bytes, i, j);
          i = j - 1;
          continue;
        }
        if (b === 0x1b) { this.resetParser(); this.state = S_ESC; return i; }
        this.execute(b);
      } else if (b < 0xa0) {
        // A stray continuation byte that could be a C1 control is dropped, not replaced.
      } else if (b < 0xc2 || b > 0xf4) {
        this.print(0xfffd);
      } else if (b < 0xe0) {
        this.u8need = 1; this.u8cp = b & 0x1f;
      } else if (b < 0xf0) {
        this.u8need = 2; this.u8cp = b & 0x0f;
        if (b === 0xe0) this.u8lo = 0xa0;
        else if (b === 0xed) this.u8hi = 0x9f;
      } else {
        this.u8need = 3; this.u8cp = b & 0x07;
        if (b === 0xf0) this.u8lo = 0x90;
        else if (b === 0xf4) this.u8hi = 0x8f;
      }
    }
    return n - 1;
  }

  resetParser() {
    this.state = this.state === undefined ? S_GROUND : this.state;
    this.inter = [];
    this.ignoring = false;
    this.param = 0;
    this.pvals = this.pvals || new Uint16Array(MAX_PARAMS);
    this.psub = this.psub || new Uint8Array(MAX_PARAMS);
    this.plen = 0;
    this.pcur = 0;
  }

  collect(b) {
    if (this.inter.length === 2) this.ignoring = true;
    else this.inter.push(b);
  }

  paramDigit(b) {
    if (this.plen === MAX_PARAMS) { this.ignoring = true; return; }
    this.param = Math.min(0xffff, this.param * 10 + (b - 0x30));
  }

  paramPush(sub) {
    if (this.plen === MAX_PARAMS) { this.ignoring = true; return false; }
    this.psub[this.plen - this.pcur] = this.pcur + 1;
    this.pvals[this.plen] = this.param;
    this.pcur = sub ? this.pcur + 1 : 0;
    this.plen++;
    this.param = 0;
    return true;
  }

  // Parameter groups: [[n, sub, ...], ...] — colon-separated numbers share a group.
  groups() {
    const out = [];
    for (let i = 0; i < this.plen;) {
      const k = this.psub[i];
      if (k === 1) out.push([this.pvals[i]]);
      else { const g = []; for (let j = i; j < i + k; j++) g.push(this.pvals[j]); out.push(g); }
      i += k;
    }
    return out;
  }

  interString() {
    const v = this.inter;
    return v.length === 0 ? '' : v.length === 1 ? String.fromCharCode(v[0]) : String.fromCharCode(v[0], v[1]);
  }

  isExecute(b) { return b <= 0x17 || b === 0x19 || (b >= 0x1c && b <= 0x1f); }

  anywhere(b) {
    if (b === 0x18 || b === 0x1a) { this.execute(b); this.state = S_GROUND; }
    else if (b === 0x1b) { this.resetParser(); this.state = S_ESC; }
  }

  step(b) {
    switch (this.state) {
      case S_ESC:
        if (this.isExecute(b)) this.execute(b);
        else if (b <= 0x2f && b >= 0x20) { this.collect(b); this.state = S_ESC_INT; }
        else if (b === 0x50) { this.resetParser(); this.state = S_DCS_ENTRY; }
        else if (b === 0x58 || b === 0x5e || b === 0x5f) this.stringStart(b);
        else if (b === 0x5b) { this.resetParser(); this.state = S_CSI_ENTRY; }
        else if (b === 0x5d) this.oscStart();
        else if (b >= 0x30 && b <= 0x7e) { this.escDispatch(b); this.state = S_GROUND; }
        else if (b === 0x18 || b === 0x1a) { this.execute(b); this.state = S_GROUND; }
        return;
      case S_ESC_INT:
        if (this.isExecute(b)) this.execute(b);
        else if (b >= 0x20 && b <= 0x2f) this.collect(b);
        else if (b >= 0x30 && b <= 0x7e) { this.escDispatch(b); this.state = S_GROUND; }
        else if (b !== 0x7f) this.anywhere(b);
        return;
      case S_CSI_ENTRY:
        if (this.isExecute(b)) this.execute(b);
        else if (b >= 0x20 && b <= 0x2f) { this.collect(b); this.state = S_CSI_INT; }
        else if (b >= 0x30 && b <= 0x39) { this.paramDigit(b); this.state = S_CSI_PARAM; }
        else if (b === 0x3a) { this.paramPush(true); this.state = S_CSI_PARAM; }
        else if (b === 0x3b) { this.paramPush(false); this.state = S_CSI_PARAM; }
        else if (b >= 0x3c && b <= 0x3f) { this.collect(b); this.state = S_CSI_PARAM; }
        else if (b >= 0x40 && b <= 0x7e) this.csiFinal(b);
        else this.anywhere(b);
        return;
      case S_CSI_PARAM:
        if (this.isExecute(b)) this.execute(b);
        else if (b >= 0x20 && b <= 0x2f) { this.collect(b); this.state = S_CSI_INT; }
        else if (b >= 0x30 && b <= 0x39) this.paramDigit(b);
        else if (b === 0x3a) this.paramPush(true);
        else if (b === 0x3b) this.paramPush(false);
        else if (b >= 0x3c && b <= 0x3f) this.state = S_CSI_IGNORE;
        else if (b >= 0x40 && b <= 0x7e) this.csiFinal(b);
        else if (b !== 0x7f) this.anywhere(b);
        return;
      case S_CSI_INT:
        if (this.isExecute(b)) this.execute(b);
        else if (b >= 0x20 && b <= 0x2f) this.collect(b);
        else if (b >= 0x30 && b <= 0x3f) this.state = S_CSI_IGNORE;
        else if (b >= 0x40 && b <= 0x7e) this.csiFinal(b);
        else this.anywhere(b);
        return;
      case S_CSI_IGNORE:
        if (this.isExecute(b)) this.execute(b);
        else if (b >= 0x40 && b <= 0x7e) this.state = S_GROUND;
        else if (b >= 0x80 || b === 0x18 || b === 0x1a || b === 0x1b) this.anywhere(b);
        return;
      case S_DCS_ENTRY:
      case S_DCS_PARAM:
      case S_DCS_INT:
        this.dcsStep(b);
        return;
      case S_DCS_PASS:
        if (b === 0x18 || b === 0x1a) { this.dcs = null; this.execute(b); this.state = S_GROUND; }
        else if (b === 0x1b) { this.dcsEnd(false); this.resetParser(); this.state = S_ESC; }
        // A raw 0x9C ends it, even inside a UTF-8 character, as in the app; xterm.js decodes
        // first, so there only C2 9C (U+009C) does.
        else if (b === 0x9c && (!this.xterm || this.dcsLast === 0xc2)) { this.dcsEnd(true); this.state = S_GROUND; }
        else { this.dcsLast = b; if (this.dcs && b !== 0x7f) this.collectString(this.dcs, b); }
        return;
      case S_DCS_IGNORE:
        this.anywhere(b);
        return;
      case S_SOS:
        if (b === 0x18 || b === 0x1a || b === 0x1b) { if (b === 0x1b) this.apcEnd(); this.apc = null; this.anywhere(b); }
        else if (b === 0x9c && this.xterm && this.sosLast === 0xc2) {
          if (this.apc) this.apc.buf.pop();
          this.apcEnd(); this.apc = null; this.state = S_GROUND;
        } else { this.sosLast = b; if (this.apc) this.collectString(this.apc, b); }
        return;
      case S_OSC:
        if (b === 0x07) { this.oscEnd(true); this.state = S_GROUND; }
        else if (b === 0x9c && this.xterm && this.oscRaw.last() === 0xc2) { this.oscRaw.pop(); this.oscEnd(false); this.state = S_GROUND; }
        else if (b === 0x18 || b === 0x1a) { this.oscEnd(false); this.execute(b); this.state = S_GROUND; }
        else if (b === 0x1b) { this.oscEnd(false); this.resetParser(); this.state = S_ESC; }
        else if (b === 0x3b) this.oscParam();
        else if (!(b <= 0x06 || (b >= 0x08 && b <= 0x17) || b === 0x19 || (b >= 0x1c && b <= 0x1f))) this.oscRaw.push(b);
        return;
    }
  }

  dcsStep(b) {
    const s = this.state;
    if (this.isExecute(b) || b === 0x7f) return;
    if (s === S_DCS_ENTRY) {
      if (b >= 0x20 && b <= 0x2f) { this.collect(b); this.state = S_DCS_INT; }
      else if (b >= 0x30 && b <= 0x39) { this.paramDigit(b); this.state = S_DCS_PARAM; }
      else if (b === 0x3a) { this.paramPush(true); this.state = S_DCS_PARAM; }
      else if (b === 0x3b) { this.paramPush(false); this.state = S_DCS_PARAM; }
      else if (b >= 0x3c && b <= 0x3f) { this.collect(b); this.state = S_DCS_PARAM; }
      else if (b >= 0x40 && b <= 0x7e) this.dcsHook(b);
      else this.anywhere(b);
    } else if (s === S_DCS_PARAM) {
      if (b >= 0x20 && b <= 0x2f) { this.collect(b); this.state = S_DCS_INT; }
      else if (b >= 0x30 && b <= 0x39) this.paramDigit(b);
      else if (b === 0x3a) this.paramPush(true);
      else if (b === 0x3b) this.paramPush(false);
      else if (b >= 0x3c && b <= 0x3f) this.state = S_DCS_IGNORE;
      else if (b >= 0x40 && b <= 0x7e) this.dcsHook(b);
      else this.anywhere(b);
    } else {
      if (b >= 0x20 && b <= 0x2f) this.collect(b);
      else if (b >= 0x30 && b <= 0x3f) this.state = S_DCS_IGNORE;
      else if (b >= 0x40 && b <= 0x7e) this.dcsHook(b);
      else this.anywhere(b);
    }
  }

  dcsHook(final) {
    if (this.plen === MAX_PARAMS) this.ignoring = true; else this.paramPush(false);
    this.state = S_DCS_PASS;
    this.dcsLast = 0;
    const key = this.interString() + String.fromCharCode(final);
    const builtin = this.xterm && (key === '$q' || key === '+q' || key === 'q');
    this.dcs = !this.ignoring && (builtin || this.hasHandler('dcs', key))
      ? { key, params: this.groups(), buf: new Bytes(), over: false } : null;
  }

  // DCS and APC payloads are only kept when someone will read them.
  collectString(s, b) {
    if (s.buf.len >= STRING_LIMIT) s.over = true; else s.buf.push(b);
  }

  dcsEnd(c1) {
    const d = this.dcs;
    this.dcs = null;
    if (!d || d.over) return;
    // A C1 ST arriving as UTF-8 (C2 9C) ends the string; its first byte is already collected.
    if (c1 && this.xterm && d.buf.last() === 0xc2) d.buf.pop();
    const bytes = d.buf.view();
    this.runHooks('dcs', d.key, [utf8(bytes), paramsArray(d.params)], () => {
      if (!this.xterm) return;
      if (d.key === '$q') this.requestStatusString(utf8(bytes));
      else if (d.key === '+q') for (const r of termcapReplies(utf8(bytes), this.cols, this.rows)) this.reply(r);
      else if (d.key === 'q') this.sixel(bytes, d.params);
    });
  }

  // APC is kept whole: kitty graphics (`G`) are the app's, the rest may have a hook.
  stringStart(kind) {
    this.state = S_SOS;
    this.sosLast = 0;
    this.apc = kind === 0x5f ? { buf: new Bytes(), over: false } : null;
  }

  // The first character names the handler (`G` for kitty graphics), the rest is data.
  apcEnd() {
    const a = this.apc;
    if (!a || a.over || a.buf.len === 0) return;
    const bytes = a.buf.view(), ident = String.fromCharCode(bytes[0]), body = utf8(bytes.subarray(1));
    this.runHooks('apc', ident, [body], () => { if (ident === 'G') this.kitty(body); });
  }

  // The buffer is cleared at the next OSC, not at this one's end: a paused hook's builtin still
  // reads these bytes when it runs.
  oscStart() { this.oscRaw.clear(); this.oscBounds = []; this.state = S_OSC; }

  oscParam() {
    if (this.oscBounds.length === MAX_OSC_PARAMS) return;
    this.oscBounds.push(this.oscRaw.len);
  }

  oscEnd(bell) {
    this.oscParam();
    const raw = this.oscRaw.view(), params = [];
    let start = 0;
    for (const end of this.oscBounds) { params.push(raw.subarray(start, end)); start = end; }
    const terminator = bell ? '\x07' : '\x1b\\', builtin = () => this.oscDispatch(params, terminator);
    const head = this.handlers && params.length ? utf8(params[0]) : null;
    if (head === null || !this.hasHandler('osc', head)) { builtin(); return; }
    this.runHooks('osc', head, [params.slice(1).map(p => utf8(p)).join(';')], builtin);
  }

  // ── C0 ──

  execute(b) {
    switch (b) {
      case 0x09: this.putTab(1); break;
      case 0x08: this.backspace(); break;
      case 0x0d: this.carriageReturn(); break;
      case 0x0a: case 0x0b: case 0x0c:
        this.linefeed();
        // The app ignores LNM; xterm.js treats it as its convertEol option.
        if (this.convertEol || (this.xterm && (this.mode & MODE.LINE_FEED_NEW_LINE))) this.carriageReturn();
        if (this.windowsWrapping) this.guessWindowsWrap();
        if (this.listeners.has('linefeed')) this.emit('linefeed');
        break;
      case 0x07: this.emit('bell'); break;
      case 0x0f: this.activeCharset = 0; break;
      case 0x0e: this.activeCharset = 1; break;
    }
  }

  // A C1 control that arrived as a UTF-8 code point (xterm profile; the app drops them).
  // Returns true when it opened a sequence, so the ground loop hands over to the state machine.
  c1(cp) {
    switch (cp) {
      case 0x84: this.linefeed(); return false;
      case 0x85: this.linefeed(); this.carriageReturn(); return false;
      case 0x88: this.tabs[this.grid.cursor.col] = 1; return false;
      case 0x90: this.resetParser(); this.state = S_DCS_ENTRY; return true;
      case 0x9b: this.resetParser(); this.state = S_CSI_ENTRY; return true;
      case 0x9d: this.oscStart(); return true;
      case 0x98: case 0x9e: case 0x9f: this.stringStart(cp === 0x9f ? 0x5f : 0x58); return true;
    }
    return false;
  }

  // ── printing ──

  print(cp) {
    this.precedingChar = cp;
    this.input(cp);
  }

  // A run of printable ASCII: the same cells input() would write, without its per-character
  // dispatch. Anything unusual (insert mode, a non-ASCII charset, another width table, a wide
  // cell being overwritten) takes the general path.
  printAscii(bytes, from, to) {
    const grid = this.grid, cur = grid.cursor;
    if ((this.mode & MODE.INSERT) || cur.charsets[this.activeCharset] !== 0 || this.widthOf !== charWidth) {
      for (let k = from; k < to; k++) this.print(bytes[k]);
      return;
    }
    this.precedingChar = bytes[to - 1];
    const cols = grid.cols;
    let k = from;
    while (k < to) {
      if (cur.wrap) this.wrapline();
      const row = grid.row(cur.line), d = row.d, fg = cur.fg, bg = cur.bg, fl = cur.fl, ext = cur.ext;
      let col = cur.col;
      for (;;) {
        const o = col * 4;
        if (d[o + 3] & (WIDE_CHAR | WIDE_CHAR_SPACER)) {
          cur.col = col;
          this.writeAtCursor(bytes[k]);
        } else {
          d[o] = bytes[k]; d[o + 1] = fg; d[o + 2] = bg; d[o + 3] = fl;
          if (ext) row.setExtra(col, { zw: '', ul: ext.ul, link: ext.link });
          else if (row.x) row.x[col] = undefined;
        }
        k++;
        if (col + 1 >= cols) { cur.wrap = true; break; }
        col++;
        if (k === to) break;
      }
      // The run wrote up to col, or up to col-1 when it ended before the margin.
      row.touchTo(cur.wrap ? col + 1 : col);
      cur.col = col;
    }
  }

  input(cp) {
    const width = this.widthOf(cp);
    if (width < 0) return;
    const grid = this.grid, cur = grid.cursor;
    if (width === 0) {
      let col = cur.col;
      if (!cur.wrap) col = Math.max(0, col - 1);
      const row = grid.row(cur.line);
      if (row.flags(col) & WIDE_CHAR_SPACER) col = Math.max(0, col - 1);
      row.touch(col);
      const e = row.extra(col);
      const ch = String.fromCodePoint(cp);
      row.setExtra(col, e ? { zw: (e.zw || '') + ch, ul: e.ul, link: e.link } : { zw: ch, ul: -1, link: null });
      return;
    }
    if (cur.wrap) this.wrapline();
    const cols = grid.cols;
    if ((this.mode & MODE.INSERT) && cur.col + width < cols) {
      const row = grid.row(cur.line);
      row.touchAll();
      for (let c = cols - width - 1; c >= cur.col; c--) row.swap(c + width, c);
    }
    if (width === 1) {
      this.writeAtCursor(cp);
    } else {
      if (cur.col + 1 >= cols) {
        if (this.mode & MODE.LINE_WRAP) {
          cur.fl |= LEADING_WIDE_CHAR_SPACER;
          this.writeAtCursor(32);
          cur.fl &= ~LEADING_WIDE_CHAR_SPACER;
          this.wrapline();
        } else {
          cur.wrap = true;
          return;
        }
      }
      cur.fl |= WIDE_CHAR;
      this.writeAtCursor(cp);
      cur.fl &= ~WIDE_CHAR;
      cur.col += 1;
      cur.fl |= WIDE_CHAR_SPACER;
      this.writeAtCursor(32);
      cur.fl &= ~WIDE_CHAR_SPACER;
    }
    if (cur.col + 1 < cols) cur.col += 1;
    else cur.wrap = true;
  }

  mapCharset(cp) {
    const set = this.grid.cursor.charsets[this.activeCharset];
    if (set === 1) return DEC_SPECIAL[cp] ?? cp;
    if (set !== 0) return NRCS[set][cp] ?? cp;
    return cp;
  }

  writeAtCursor(cp) {
    const grid = this.grid, cur = grid.cursor;
    cp = this.mapCharset(cp);
    const line = cur.line, col = cur.col;
    const row = grid.row(line);
    row.touch(col);
    const fl = row.flags(col);
    if (fl & (WIDE_CHAR | WIDE_CHAR_SPACER)) {
      const last = grid.cols - 1;
      if ((fl & WIDE_CHAR) && col < last) {
        row.touch(col + 1);
        row.d[(col + 1) * 4 + 3] &= ~WIDE_CHAR_SPACER;
      } else if (col > 0) {
        const o = (col - 1) * 4;
        row.touch(col - 1);
        row.d[o + 3] &= ~WIDE_CHAR;
        const e = row.extra(col - 1);
        if (e && e.zw) row.setExtra(col - 1, { zw: '', ul: e.ul, link: e.link });
        row.d[o] = 32;
      }
      if (col <= 1 && line !== -grid.historySize) {
        const above = grid.row(line - 1);
        above.touch(last);
        above.d[last * 4 + 3] &= ~LEADING_WIDE_CHAR_SPACER;
      }
    }
    const o = col * 4;
    row.d[o] = cp; row.d[o + 1] = cur.fg; row.d[o + 2] = cur.bg; row.d[o + 3] = cur.fl;
    if (cur.ext) row.setExtra(col, { zw: '', ul: cur.ext.ul, link: cur.ext.link });
    else if (row.x) row.x[col] = undefined;
  }

  wrapline() {
    if (!(this.mode & MODE.LINE_WRAP)) return;
    const cur = this.grid.cursor;
    const row = this.grid.row(cur.line);
    row.touch(cur.col);
    row.d[cur.col * 4 + 3] |= WRAPLINE;
    if (cur.line + 1 >= this.bottom) this.linefeed();
    else cur.line += 1;
    cur.col = 0;
    cur.wrap = false;
  }

  // ── cursor motion ──

  goto(line, col) {
    const cur = this.grid.cursor;
    let off = 0, maxY = this.rows - 1;
    if (this.mode & MODE.ORIGIN) { off = this.top; maxY = this.bottom - 1; }
    cur.line = Math.max(Math.min(line + off, maxY), 0);
    cur.col = Math.min(col, this.cols - 1);
    cur.wrap = false;
  }

  linefeed() {
    const cur = this.grid.cursor, next = cur.line + 1;
    if (next === this.bottom) this.scrollUpRelative(this.top, 1);
    else if (next < this.rows) cur.line += 1;
  }

  carriageReturn() { const c = this.grid.cursor; c.col = 0; c.wrap = false; }

  backspace() {
    const c = this.grid.cursor;
    if (this.xmode & XMODE.REVERSE_WRAP) { this.reverseBackspace(); return; }
    if (c.col > 0) { c.col -= 1; c.wrap = false; }
  }

  // Reverse wraparound (mode 45, xterm profile): a pending wrap is undone in place, and at the
  // left edge of a line that continues the one above, back to that line's last cell.
  reverseBackspace() {
    const grid = this.grid, c = grid.cursor, last = grid.cols - 1;
    if (c.wrap) { c.wrap = false; return; }
    if (c.col > 0) { c.col -= 1; return; }
    if (c.line <= this.top || c.line >= this.bottom) return;
    const above = grid.row(c.line - 1);
    if (!(above.flags(last) & WRAPLINE)) return;
    above.d[last * 4 + 3] &= ~WRAPLINE;
    c.line -= 1;
    c.col = above.flags(last) & LEADING_WIDE_CHAR_SPACER ? last - 1 : last;
  }

  putTab(count) {
    const grid = this.grid, cur = grid.cursor;
    if (cur.wrap) { this.wrapline(); return; }
    while (cur.col < grid.cols && count !== 0) {
      count--;
      const row = grid.row(cur.line);
      row.touch(cur.col);
      if (row.cp(cur.col) === 32) row.d[cur.col * 4] = this.mapCharset(9);
      for (;;) {
        if (cur.col + 1 === grid.cols) break;
        cur.col += 1;
        if (this.tabs[cur.col]) break;
      }
    }
  }

  reverseIndex() {
    const cur = this.grid.cursor;
    if (cur.line === this.top) this.scrollDownRelative(this.top, 1);
    else cur.line = Math.max(cur.line - 1, 0);
  }

  scrollUpRelative(origin, lines) {
    lines = Math.min(lines, this.bottom - this.top);
    this.grid.scrollUp(origin, this.bottom, lines, this.grid.cursor.bg);
  }

  scrollDownRelative(origin, lines) {
    lines = Math.min(lines, this.bottom - this.top);
    lines = Math.min(lines, this.bottom - origin);
    this.grid.scrollDown(origin, this.bottom, lines, this.grid.cursor.bg);
  }

  // ── screens ──

  swapAlt() {
    if (!(this.mode & MODE.ALT_SCREEN)) {
      this.inactive.cursor = this.grid.cursor.clone();
      this.grid.saved = this.grid.cursor.clone();
      this.inactive.resetRegion(0, this.inactive.lines, this.inactive.cursor.bg);
    }
    this.swapKeyboardStacks();
    const g = this.grid; this.grid = this.inactive; this.inactive = g;
    this.mode ^= MODE.ALT_SCREEN;
    this.dropImages();
    this.emit('screen', this.isAltScreen);
  }

  resize(cols, rows) {
    cols = Math.max(2, cols | 0); rows = Math.max(1, rows | 0);
    if (cols === this.cols && rows === this.rows) return;
    const oldCols = this.cols;
    const alt = this.isAltScreen, reflow = this.reflowAllowed();
    this.grid.resize(!alt && reflow, rows, cols);
    this.inactive.resize(alt && reflow, rows, cols);
    // Without reflow a wider row keeps its continuation mark on the old last cell; the guessed
    // Windows wraps are line properties in xterm.js, so they move to the new last cell.
    if (this.windowsPty && !reflow && cols > oldCols) {
      for (const g of [this.grid, this.inactive]) {
        for (let y = -g.historySize; y < g.lines; y++) {
          const row = g.row(y);
          if (!(row.flags(oldCols - 1) & WRAPLINE)) continue;
          row.setFlag(oldCols - 1, WRAPLINE, false);
          row.touch(cols - 1);
          row.setFlag(cols - 1, WRAPLINE, true);
        }
      }
    }
    if (oldCols !== cols) {
      this.dropImages();
      this.tabs = this.makeTabs(cols, this.tabs);
      // kasa-pty re-applies the scrollback cap to whichever screen is active, so a width change
      // during an alternate-screen program gives that screen scrollback too. Mirrored for parity.
      this.grid.updateHistory(this.scrollback);
    }
    this.top = 0;
    this.bottom = rows;
    this.emit('resize', cols, rows);
  }

  resetState() {
    if (this.mode & MODE.ALT_SCREEN) { const g = this.grid; this.grid = this.inactive; this.inactive = g; }
    this.activeCharset = 0;
    this.cursorStyle = null;
    this.grid.reset();
    this.inactive.reset();
    this.top = 0;
    this.bottom = this.rows;
    this.tabs = this.makeTabs(this.cols);
    this.titleStack = [];
    this.title = null;
    this.mode = DEFAULT_MODE;
    this.xmode = 0;
    this.keyboardStack = [];
    this.inactiveKeyboardStack = [];
    this.dropImages();
    this.kittyGraphics = new KittyGraphics();
    for (const marker of [...this.markers]) marker.dispose();
  }

  // ── ESC ──

  escDispatch(b) {
    if (!this.handlers) { this.escBuiltin(b); return; }
    this.runHooks('esc', this.interString() + String.fromCharCode(b), [], () => this.escBuiltin(b));
  }

  escBuiltin(b) {
    const inter = this.inter;
    // National sets share their finals with plain ESC sequences (ESC 7, E, H, =, Z…), so they
    // only count after an intermediate.
    const nrcs = this.xterm && inter.length === 1 && NRCS[String.fromCharCode(b)];
    if (b === 0x42 || b === 0x30 || nrcs) {
      if (inter.length !== 1) return;
      let index = G_INDEX[inter[0]];
      if (index === undefined && this.xterm) index = XTERM_G_INDEX[inter[0]];
      if (index !== undefined) this.grid.cursor.charsets[index] = b === 0x30 ? 1 : b === 0x42 ? 0 : String.fromCharCode(b);
      return;
    }
    if (b === 0x38 && inter.length === 1 && inter[0] === 0x23) { this.decaln(); return; }
    if (this.xterm && inter.length === 1 && inter[0] === 0x25 && (b === 0x40 || b === 0x47)) {
      this.activeCharset = 0;
      this.grid.cursor.charsets[0] = 0;
      return;
    }
    if (inter.length) return;
    // Locking shifts to G2/G3 (and their right-half forms, which xterm.js also applies to GL).
    if (this.xterm && XTERM_SHIFTS[b] !== undefined) { this.activeCharset = XTERM_SHIFTS[b]; return; }
    switch (b) {
      case 0x44: this.linefeed(); break;
      case 0x45: this.linefeed(); this.carriageReturn(); break;
      case 0x48: this.tabs[this.grid.cursor.col] = 1; break;
      case 0x4d: this.reverseIndex(); break;
      case 0x5a: this.reply(this.xterm ? DA1_XTERM_REPLY : DA1_REPLY); break;
      case 0x63: this.resetState(); break;
      case 0x37: this.grid.saved = this.grid.cursor.clone(); break;
      case 0x38: this.grid.cursor = this.grid.saved.clone(); break;
      case 0x3d: this.mode |= MODE.APP_KEYPAD; break;
      case 0x3e: this.mode &= ~MODE.APP_KEYPAD; break;
    }
  }

  decaln() {
    for (let y = 0; y < this.rows; y++) {
      const row = this.grid.row(y);
      row.touchAll();
      row.blankRange(0, this.cols, DEFAULT_BG);
      for (let x = 0; x < this.cols; x++) row.d[x * 4] = 0x45;
    }
  }

  // ── CSI ──

  csiFinal(b) {
    if (this.plen === MAX_PARAMS) this.ignoring = true; else this.paramPush(false);
    this.state = S_GROUND;
    if (this.ignoring || this.inter.length > 2) return;
    const groups = this.groups(), inter = this.interString();
    if (!this.handlers) { this.csiDispatch(b, groups, inter); return; }
    this.runHooks('csi', inter + String.fromCharCode(b), [paramsArray(groups)], () => this.csiDispatch(b, groups, inter));
  }

  csiDispatch(final, groups, inter) {
    let k = 0;
    const next = def => { const g = groups[k++]; return g && g[0] !== 0 ? g[0] : def; };
    const cur = this.grid.cursor;
    const f = String.fromCharCode(final);
    const key = inter + f;
    switch (key) {
      case '@': this.insertBlank(next(1)); break;
      case 'A': this.goto(cur.line - next(1), cur.col); break;
      case 'B': case 'e': this.goto(cur.line + next(1), cur.col); break;
      case 'b': {
        const n = next(1);
        if (this.precedingChar !== null) for (let i = 0; i < n; i++) this.input(this.precedingChar);
        break;
      }
      case 'C': case 'a': cur.col = Math.min(cur.col + next(1), this.cols - 1); cur.wrap = false; break;
      case 'D': cur.col = Math.max(0, cur.col - next(1)); cur.wrap = false; break;
      case 'd': this.goto(next(1) - 1, cur.col); break;
      case 'E': this.goto(cur.line + next(1), 0); break;
      case 'F': this.goto(cur.line - next(1), 0); break;
      case 'G': case '`': this.goto(cur.line, next(1) - 1); break;
      case 'g': {
        const m = next(0);
        if (m === 0) this.tabs[cur.col] = 0; else if (m === 3) this.tabs.fill(0);
        break;
      }
      case 'H': case 'f': {
        if (this.windowsWrapping && f === 'H') this.guessWindowsWrap();
        const y = next(1), x = next(1);
        this.goto(y - 1, x - 1);
        break;
      }
      case 'h': for (const g of groups) this.setMode(g[0], true); break;
      case '?h':
        for (const g of groups) {
          if (g[0] === 2026) { this.syncPending = true; this.syncSince = Date.now(); this.terminated = true; }
          this.setPrivateMode(g[0], true);
        }
        break;
      case 'l': for (const g of groups) this.setMode(g[0], false); break;
      case '?l': for (const g of groups) this.setPrivateMode(g[0], false); break;
      case 'I': this.moveForwardTabs(next(1)); break;
      case 'J': this.clearScreen(next(0)); break;
      case 'K': this.clearLine(next(0)); break;
      case 'L': if (cur.line >= this.top && cur.line < this.bottom) this.scrollDownRelative(cur.line, next(1)); break;
      case 'M': {
        const n = Math.min(this.rows - cur.line, next(1));
        if (n > 0 && cur.line >= this.top && cur.line < this.bottom) this.scrollUpRelative(cur.line, n);
        break;
      }
      case 'm': this.sgr(groups); break;
      case 'n': {
        const m = next(0);
        if (m === 5) this.reply('\x1b[0n');
        else if (m === 6) this.reply(`\x1b[${cur.line + 1};${cur.col + 1}R`);
        break;
      }
      case 'P': this.deleteChars(next(1)); break;
      case '$p': { const m = next(0); this.reply(`\x1b[${m};${this.modeState(m)}$y`); break; }
      case '?$p': { const m = next(0); this.reply(`\x1b[?${m};${this.privateModeState(m)}$y`); break; }
      case ' q': {
        const id = next(0);
        if (id === 0) this.cursorStyle = null;
        else if (id <= 6) this.cursorStyle = { shape: CURSOR_SHAPES[(id - 1) >> 1], blinking: id % 2 === 1 };
        this.emit('cursorstyle');
        break;
      }
      case 'r': {
        const t = next(1);
        const g = groups[k++];
        this.setScrollingRegion(t, g && g[0] !== 0 ? g[0] : this.rows);
        break;
      }
      case 'S': this.scrollUpRelative(this.top, next(1)); break;
      case 's': this.grid.saved = cur.clone(); break;
      case 'T': this.scrollDownRelative(this.top, next(1)); break;
      case 't': {
        const m = next(1);
        if (m === 14) { this.emit('pxsize'); this.emit('data', this.pixelSizeReply()); }
        else if (m === 18) this.reply(`\x1b[8;${this.rows};${this.cols}t`);
        else if (m === 22 || m === 23) {
          // The second parameter picks title (2), icon name (1) or both (0); the app only has titles.
          const which = this.xterm ? next(0) : 2;
          if (which === 0 || which === 2) {
            if (m === 23) { if (this.titleStack.length) this.setTitle(this.titleStack.pop()); }
            else { if (this.titleStack.length >= TITLE_STACK_MAX) this.titleStack.shift(); this.titleStack.push(this.title); }
          }
          if (this.xterm && (which === 0 || which === 1)) {
            if (m === 23) { if (this.iconStack.length) this.setIconName(this.iconStack.pop()); }
            else { if (this.iconStack.length >= TITLE_STACK_MAX) this.iconStack.shift(); this.iconStack.push(this.iconName); }
          }
        } else if (m === 16 && this.xterm && this.cellPixels) {
          this.reply(`\x1b[6;${this.cellPixels.height};${this.cellPixels.width}t`);
        }
        break;
      }
      case 'u': this.grid.cursor = this.grid.saved.clone(); break;
      case 'X': {
        const start = cur.col, end = Math.min(start + next(1), this.cols);
        const row = this.grid.row(cur.line);
        row.touchTo(end);
        row.blankRange(start, end, cur.bg);
        break;
      }
      case 'Z': this.moveBackwardTabs(next(1)); break;
      default:
        if (this.xterm && this.xtermCsi(key, groups)) break;
        // DA asks with no parameter or 0; the `>` form is secondary DA.
        if (f === 'c' && next(0) === 0) {
          if (inter === '') this.reply(this.xterm ? DA1_XTERM_REPLY : DA1_REPLY);
          else if (inter[0] === '>') this.reply(DA2_REPLY);
        }
    }
  }

  // Sequences only the xterm profile understands; false for anything else.
  xtermCsi(key, groups) {
    const p = (k, def) => { const g = groups[k]; return g && g[0] !== 0 ? g[0] : def; };
    const cur = this.grid.cursor;
    switch (key) {
      case ' @': this.shiftColumns(0, -p(0, 1)); return true;
      case ' A': this.shiftColumns(0, p(0, 1)); return true;
      case "'}": this.shiftColumns(cur.col, p(0, 1)); return true;
      case "'~": this.shiftColumns(cur.col, -p(0, 1)); return true;
      case '^': this.scrollDownRelative(this.top, p(0, 1)); return true;
      case '?J': this.selectiveEraseDisplay(p(0, 0)); return true;
      case '?K': this.selectiveEraseLine(p(0, 0)); return true;
      case '"q': {
        const m = p(0, 0);
        if (m === 1) cur.fl |= PROTECTED; else if (m === 0 || m === 2) cur.fl &= ~PROTECTED;
        return true;
      }
      case '!p': this.softReset(); return true;
      case '>q': if (p(0, 0) === 0) this.reply(`\x1bP>|kasaterm.js(${VERSION})\x1b\\`); return true;
      case '?n': {
        const m = p(0, 0);
        if (m === 6) this.reply(`\x1b[?${cur.line + 1};${cur.col + (cur.wrap ? 2 : 1)}R`);
        else if (m === 996) this.reportColorScheme();
        return true;
      }
      case '?u': {
        const top = this.keyboardStack.length ? this.keyboardStack[this.keyboardStack.length - 1] : 0;
        this.reply(`\x1b[?${top}u`);
        return true;
      }
      case '=u': this.setKeyboardMode(p(0, 0) & 0x1f, p(1, 1)); return true;
      case '>u': this.pushKeyboardMode(p(0, 0) & 0x1f); return true;
      case '<u': this.popKeyboardModes(p(0, 1)); return true;
      case '?S': if (groups.length >= 2) this.graphicsAttributes(groups[0][0], groups[1][0], groups[2] ? groups[2][0] : 0, groups.length); return true;
    }
    return false;
  }

  // XTSMGRAPHICS (xterm profile): sixel colour registers (item 1) and the largest sixel picture
  // (item 2); read (1), reset (2), set (3), read the maximum (4). Status 0 ok, 1 bad item, 2 bad action.
  graphicsAttributes(item, action, value, count) {
    const say = (status, ...v) => this.reply(`\x1b[?${item};${status}${v.map(x => ';' + x).join('')}S`);
    if (item === 1) {
      if (action === 1) say(0, this.sixelPalette);
      else if (action === 2) { this.sixelPalette = SIXEL_PALETTE_LIMIT; say(0, this.sixelPalette); }
      else if (action === 3 && count > 2 && value >= 1 && value <= SIXEL_PALETTE_LIMIT) { this.sixelPalette = value; say(0, value); }
      else if (action === 4) say(0, SIXEL_PALETTE_LIMIT);
      else say(2);
    } else if (item === 2) {
      const side = Math.floor(Math.sqrt(SIXEL_PIXEL_LIMIT)), cell = this.cellPixels || DEFAULT_CELL;
      const w = this.cols * cell.width, h = this.rows * cell.height;
      if (action === 1) { if (w * h < SIXEL_PIXEL_LIMIT) say(0, w, h); else say(0, side, side); }
      else if (action === 4) say(0, side, side);
      else say(2);
    } else say(1);
  }

  // SL/SR (from column 0) and DECIC/DECDC (from the cursor) on every line of the scroll region:
  // n > 0 inserts blank columns, n < 0 deletes. Nothing happens with the cursor outside it.
  shiftColumns(at, n) {
    const grid = this.grid, cur = grid.cursor, cols = grid.cols, bg = cur.bg;
    if (cur.line < this.top || cur.line >= this.bottom) return;
    const k = Math.min(Math.abs(n), cols - at);
    for (let y = this.top; y < this.bottom; y++) {
      const row = grid.row(y);
      row.touchAll();
      if (n > 0) {
        for (let x = cols - 1; x >= at + k; x--) row.swap(x, x - k);
        row.blankRange(at, at + k, bg);
      } else {
        for (let x = at; x < cols - k; x++) row.swap(x, x + k);
        row.blankRange(cols - k, cols, bg);
      }
      // Moved columns no longer continue onto the next line.
      for (let x = 0; x < cols; x++) row.d[x * 4 + 3] &= ~(WRAPLINE | LEADING_WIDE_CHAR_SPACER);
      this.repairWide(row, bg);
    }
  }

  // A wide glyph cut from its spacer, or a spacer cut from its glyph, becomes a blank.
  repairWide(row, bg) {
    for (let x = 0; x < row.len; x++) {
      const f = row.flags(x);
      if ((f & WIDE_CHAR) && (x + 1 >= row.len || !(row.flags(x + 1) & WIDE_CHAR_SPACER))) row.blank(x, bg);
      else if ((f & WIDE_CHAR_SPACER) && (x === 0 || !(row.flags(x - 1) & WIDE_CHAR))) row.blank(x, bg);
    }
  }

  // DECSEL/DECSED: EL and ED that leave cells written under DECSCA protection alone.
  selectiveErase(y, from, to) {
    const row = this.grid.row(y), bg = this.grid.cursor.bg;
    row.touchTo(to);
    for (let x = from; x < to; x++) if (!(row.flags(x) & PROTECTED)) row.blank(x, bg);
  }

  selectiveEraseLine(mode) {
    const cur = this.grid.cursor, cols = this.cols;
    if (mode === 0) { if (!cur.wrap) this.selectiveErase(cur.line, cur.col, cols); }
    else if (mode === 1) this.selectiveErase(cur.line, 0, Math.min(cur.col + 1, cols));
    else if (mode === 2) this.selectiveErase(cur.line, 0, cols);
  }

  selectiveEraseDisplay(mode) {
    const cur = this.grid.cursor, cols = this.cols;
    if (mode === 0) {
      this.selectiveEraseLine(0);
      for (let y = cur.line + 1; y < this.rows; y++) this.selectiveErase(y, 0, cols);
    } else if (mode === 1) {
      for (let y = 0; y < cur.line; y++) this.selectiveErase(y, 0, cols);
      this.selectiveEraseLine(1);
    } else if (mode === 2) {
      for (let y = 0; y < this.rows; y++) this.selectiveErase(y, 0, cols);
    } else if (mode === 3) {
      this.grid.clearHistory();
      this.dropImages();
    }
  }

  // DECSTR: modes, attributes, charsets, margins and the saved cursor go back to their defaults;
  // the screen and the cursor position stay.
  softReset() {
    const cur = this.grid.cursor;
    this.mode = (this.mode & ~(MODE.APP_CURSOR | MODE.APP_KEYPAD | MODE.BRACKETED_PASTE | MODE.ORIGIN | MODE.INSERT
      | MODE.FOCUS_IN_OUT)) | MODE.LINE_WRAP | MODE.SHOW_CURSOR;
    this.xmode &= ~(XMODE.REVERSE_WRAP | XMODE.WIN32_INPUT | XMODE.COLOR_SCHEME_REPORTS);
    this.top = 0;
    this.bottom = this.rows;
    cur.fg = DEFAULT_FG; cur.bg = DEFAULT_BG; cur.fl = 0; cur.ext = null;
    cur.charsets = [0, 0, 0, 0];
    this.activeCharset = 0;
    this.grid.saved = new Cursor();
  }

  // ── kitty keyboard flags (alacritty's handling, which the app has switched off) ──

  setKeyboardMode(flags, behavior) {
    const active = (this.mode & KITTY_MODE) >>> KITTY_SHIFT;
    const next = behavior === 3 ? active & ~flags : behavior === 2 ? active | flags : flags;
    this.mode = (this.mode & ~KITTY_MODE) | (next << KITTY_SHIFT);
  }

  // alacritty drops the bottom of its *title* stack when this one is full (and panics if that
  // is empty); the oldest keyboard entry is dropped here instead.
  pushKeyboardMode(flags) {
    if (this.keyboardStack.length >= KEYBOARD_STACK_MAX) this.keyboardStack.shift();
    this.keyboardStack.push(flags);
    this.setKeyboardMode(flags, 1);
  }

  popKeyboardModes(n) {
    this.keyboardStack.length = Math.max(0, this.keyboardStack.length - n);
    this.setKeyboardMode(this.keyboardStack.length ? this.keyboardStack[this.keyboardStack.length - 1] : 0, 1);
  }

  swapKeyboardStacks() {
    const t = this.keyboardStack;
    this.keyboardStack = this.inactiveKeyboardStack;
    this.inactiveKeyboardStack = t;
    this.setKeyboardMode(this.keyboardStack.length ? this.keyboardStack[this.keyboardStack.length - 1] : 0, 1);
  }

  // ── inline pictures (images.js decodes; where they sit and where the cursor ends is here) ──

  // A marker on a buffer line (counted from the top of the scrollback).
  markLine(line) { return new Marker(this, this.grid, line - this.historySize); }

  // kitty graphics, as the app's kitty.rs: replies go back to the program, a placement may move
  // the cursor down and right like `ESC D`… `CSI G`.
  kitty(body) {
    const cur = this.grid.cursor;
    const env = {
      cols: this.cols, history: this.historySize, cell: this.cellPixels || DEFAULT_CELL,
      mark: line => this.markLine(line), nextUid: () => ++this.imageSerial,
    };
    const out = this.kittyGraphics.handle(body, { line: this.historySize + cur.line, col: cur.col }, env);
    if (out.removed.length) this.forgetPlacements(out.removed);
    if (out.reply) this.reply(out.reply);
    if (out.placed) {
      const { placement: p, image: im } = out.placed;
      this.emit('image', {
        id: p.uid, protocol: 'kitty', marker: p.marker, col: p.col, cols: p.cols, rows: p.rows, z: p.z, virtual: p.virtual,
        imageId: im.id, placementId: p.id, width: im.width, height: im.height, format: im.format, data: im.data.slice(),
        compressed: im.compressed,
      });
      this.evictImages();
    }
    if (out.cursor) {
      for (let k = 0; k < out.cursor[0]; k++) this.linefeed();
      this.goto(this.grid.cursor.line, out.cursor[1]);
    }
  }

  // OSC 1337 `File=…:base64` as the app lays it out: anchored at the cursor, which stays; the
  // width in cells from `width=N` or 60% of the line (20 to 100), the height from the PNG's
  // shape at a 2.1 cell aspect. The same name and size again is the same picture moved.
  itermImage(body) {
    const colon = body.indexOf(':');
    if (colon < 0) return;
    const bytes = base64Bytes(body.slice(colon + 1));
    if (bytes.length < 16 || this.historySize >= this.scrollback) return;
    let name = '', size = bytes.length, width = null;
    // The app splits the part before ':' at ';' and '=' as it comes, so a `name` right after
    // `File=` is not seen as one — kept, the moving-picture key depends on it.
    for (const kv of body.slice(0, colon).split(';')) {
      const eq = kv.indexOf('=');
      if (eq < 0) continue;
      const k = kv.slice(0, eq), v = kv.slice(eq + 1);
      if (k === 'name') name = utf8(base64Bytes(v));
      else if (k === 'size' && /^[0-9]+$/.test(v)) size = Number(v);
      else if (k === 'width') width = /^[0-9]+$/.test(v) && Number(v) <= 0xffff ? Number(v) : null;
    }
    const cur = this.grid.cursor, gridCols = this.cols;
    const cols = Math.max(1, Math.min(width ?? Math.max(20, Math.min(100, Math.floor(gridCols * 6 / 10))), Math.max(1, gridCols - cur.col)));
    const png = pngSize(bytes);
    const rows = Math.max(1, png && png.width > 0 ? Math.ceil(cols * png.height / png.width / 2.1) : 12);
    const key = name + '\0' + size;
    let rec = this.inlineImages.find(r => r.key === key);
    if (rec) rec.marker.dispose();
    else { rec = { uid: ++this.imageSerial, key, pixels: png ? png.width * png.height : 0 }; this.inlineImages.push(rec); }
    rec.marker = this.markLine(this.historySize + cur.line);
    this.emit('image', {
      id: rec.uid, protocol: 'iterm', marker: rec.marker, col: cur.col, cols, rows, name, mime: imageMime(bytes),
      width: png ? png.width : 0, height: png ? png.height : 0, data: bytes.slice(),
    });
    this.evictImages();
  }

  // Sixel (xterm profile), placed as xterm.js's image addon does: from the cursor, which moves
  // down to the picture's last line and back to its column — or, with DECSDM (80), at the top
  // left of the screen with the cursor left alone.
  sixel(data, params) {
    const pic = decodeSixel(data, this.sixelPalette);
    if (!pic) return;
    const cell = this.cellPixels || DEFAULT_CELL, cur = this.grid.cursor;
    const scrolling = !(this.xmode & XMODE.SIXEL_DISPLAY);
    const col = scrolling ? cur.col : 0, rows = Math.ceil(pic.height / cell.height);
    const cols = Math.min(Math.ceil(pic.width / cell.width), this.cols - col);
    const marker = this.markLine(this.historySize + (scrolling ? cur.line : 0));
    if (scrolling) {
      for (let k = 0; k < rows - 1; k++) this.linefeed();
      cur.col = col;
      cur.wrap = false;
    }
    const rec = { uid: ++this.imageSerial, key: null, marker, pixels: pic.width * pic.height };
    this.inlineImages.push(rec);
    this.emit('image', {
      id: rec.uid, protocol: 'sixel', marker, col, cols, rows, width: pic.width, height: pic.height, format: 'rgba',
      data: pic.pixels, transparent: (params[1] ? params[1][0] : 0) === 1, background: cur.bg,
    });
    this.evictImages();
  }

  forgetPlacements(placements) {
    for (const p of placements) if (p.marker) p.marker.dispose();
    this.emit('imagedelete', placements.map(p => p.uid));
  }

  // The app forgets anchored pictures when the lines they hang on renumber: a width change
  // (reflow), a screen switch, a cleared or full scrollback.
  dropImages() {
    const gone = this.kittyGraphics.dropAnchored();
    for (const r of this.inlineImages.splice(0)) { r.marker.dispose(); gone.push({ uid: r.uid, marker: null }); }
    if (gone.length) this.forgetPlacements(gone);
  }

  // At most IMAGE_SLOTS pictures and IMAGE_PIXEL_BUDGET pixels between them; the oldest go
  // first, the one just shown stays.
  evictImages() {
    const all = () => [
      ...this.inlineImages.map(r => ({ uid: r.uid, pixels: r.pixels, kitty: false })),
      ...this.kittyGraphics.images.map(im => ({ uid: im.uid, pixels: im.width * im.height, kitty: true })),
    ].sort((a, b) => a.uid - b.uid);
    for (let list = all(); list.length > 1; list = all()) {
      const pixels = list.reduce((n, x) => n + x.pixels, 0);
      if (list.length <= IMAGE_SLOTS && pixels <= IMAGE_PIXEL_BUDGET) break;
      const oldest = list[0];
      if (oldest.kitty) this.forgetPlacements(this.kittyGraphics.evict(oldest.uid));
      else {
        const i = this.inlineImages.findIndex(r => r.uid === oldest.uid), [r] = this.inlineImages.splice(i, 1);
        r.marker.dispose();
        this.emit('imagedelete', [r.uid]);
      }
    }
  }

  // ── Unicode widths and Windows PTYs (xterm.js `term.unicode`, `windowsPty`) ──

  // Width providers beside the app's own table ('alacritty', the default). Another one changes
  // where characters land, so the mirror would no longer match the app.
  unicodeApi() {
    const term = this, providers = new Map([['alacritty', { version: 'alacritty', wcwidth: cp => Math.max(0, charWidth(cp)) }]]);
    return {
      get versions() { return [...providers.keys()]; },
      get activeVersion() { return term.unicodeVersion; },
      set activeVersion(v) {
        const provider = providers.get(v);
        if (!provider) throw new Error(`unknown Unicode version "${v}"`);
        term.unicodeVersion = v;
        term.widthOf = v === 'alacritty' ? charWidth : cp => (cp === 0x7f ? -1 : provider.wcwidth(cp));
      },
      register(provider) { providers.set(provider.version, provider); },
    };
  }

  // xterm.js's `windowsPty`: ConPTY before build 21376 (and winpty) reprints instead of
  // wrapping, so reflow is off and a line is guessed to continue when the one above ends in a
  // character; on any Windows PTY, growing the screen adds blank lines instead of pulling
  // scrollback down.
  setWindowsPty(v) {
    this.windowsPty = v && (v.backend !== undefined || v.buildNumber !== undefined) ? { ...v } : null;
    const w = this.windowsPty;
    this.windowsWrapping = !!(w && w.backend !== undefined && w.buildNumber !== undefined && w.backend === 'conpty' && w.buildNumber < 21376);
    for (const g of [this.grid, this.inactive]) g.noHistoryPull = !!w;
  }

  reflowAllowed() {
    const w = this.windowsPty;
    return !(w && w.buildNumber) || (w.backend === 'conpty' && w.buildNumber >= 21376);
  }

  guessWindowsWrap() {
    const grid = this.grid, line = grid.cursor.line, last = grid.cols - 1;
    if (line - 1 < -grid.historySize) return;
    const above = grid.row(line - 1);
    above.touch(last);
    above.setFlag(last, WRAPLINE, above.cp(last) !== 32);
  }

  // DECRQSS (xterm profile). DECSCL answers the VT220 level DA1 claims.
  requestStatusString(data) {
    const cur = this.grid.cursor, ok = s => this.reply(`\x1bP1$r${s}\x1b\\`);
    if (data === 'm') ok(this.sgrString() + 'm');
    else if (data === 'r') ok(`${this.top + 1};${this.bottom}r`);
    else if (data === '"q') ok(`${cur.fl & PROTECTED ? 1 : 0}"q`);
    else if (data === '"p') ok('62;1"p');
    else if (data === ' q') {
      const st = this.resolvedCursorStyle;
      ok(`${{ block: 2, underline: 4, beam: 6 }[st.shape] - (st.blinking ? 1 : 0)} q`);
    } else this.reply('\x1bP0$r\x1b\\');
  }

  sgrString() {
    const cur = this.grid.cursor, f = cur.fl, out = ['0'];
    const color = (c, base, bright, ext) => {
      if (c === DEFAULT_FG || c === DEFAULT_BG) return;
      if (c >= COLOR_RGB) out.push(`${ext};2;${(c >> 16) & 0xff};${(c >> 8) & 0xff};${c & 0xff}`);
      else if (c >= COLOR_INDEXED) out.push(`${ext};5;${c & 0xff}`);
      else if (c < 8) out.push(String(base + c));
      else if (c < 16) out.push(String(bright + c - 8));
    };
    if (f & BOLD) out.push('1');
    if (f & DIM) out.push('2');
    if (f & ITALIC) out.push('3');
    const ul = f & ALL_UNDERLINES;
    if (ul) out.push(ul === UNDERLINE ? '4' : '4:' + (ul === DOUBLE_UNDERLINE ? 2 : ul === UNDERCURL ? 3 : ul === DOTTED_UNDERLINE ? 4 : 5));
    if (f & BLINK) out.push('5');
    if (f & INVERSE) out.push('7');
    if (f & HIDDEN) out.push('8');
    if (f & STRIKEOUT) out.push('9');
    if (f & OVERLINE) out.push('53');
    color(cur.fg, 30, 90, 38);
    color(cur.bg, 40, 100, 48);
    if (cur.ext && cur.ext.ul >= 0) color(cur.ext.ul, -1, -1, 58);
    return out.join(';');
  }

  pixelSizeReply() {
    const cell = this.cellPixels || { width: 0, height: 0 };
    return `\x1b[4;${this.rows * cell.height};${this.cols * cell.width}t`;
  }

  insertBlank(count) {
    const cur = this.grid.cursor, cols = this.cols;
    count = Math.min(count, cols - cur.col);
    const src = cur.col, dst = cur.col + count, num = cols - dst;
    const row = this.grid.row(cur.line);
    row.touchAll();
    for (let off = num - 1; off >= 0; off--) row.swap(dst + off, src + off);
    row.blankRange(src, dst, cur.bg);
  }

  deleteChars(count) {
    const cur = this.grid.cursor, cols = this.cols;
    count = Math.min(count, cols);
    const start = cur.col, end = Math.min(start + count, cols - 1), num = cols - end;
    const row = this.grid.row(cur.line);
    row.touchAll();
    for (let off = 0; off < num; off++) row.swap(start + off, end + off);
    row.blankRange(cols - count, cols, cur.bg);
  }

  moveForwardTabs(count) {
    const cur = this.grid.cursor, cols = this.cols;
    for (let n = 0; n < count; n++) {
      let col = cur.col;
      if (col === cols - 1) break;
      for (let i = col + 1; i < cols; i++) { col = i; if (this.tabs[i]) break; }
      cur.col = col;
    }
  }

  moveBackwardTabs(count) {
    const cur = this.grid.cursor;
    for (let n = 0; n < count; n++) {
      let col = cur.col;
      if (col === 0) break;
      for (let i = col - 1; i >= 0; i--) if (this.tabs[i]) { col = i; break; }
      cur.col = col;
    }
  }

  clearLine(mode) {
    const cur = this.grid.cursor, row = this.grid.row(cur.line);
    let from, to;
    if (mode === 0) { if (cur.wrap) return; from = cur.col; to = this.cols; }
    else if (mode === 1) { from = 0; to = cur.col + 1; }
    else if (mode === 2) { from = 0; to = this.cols; }
    else return;
    row.touchTo(to);
    row.blankRange(from, to, cur.bg);
  }

  clearScreen(mode) {
    const grid = this.grid, cur = grid.cursor, bg = cur.bg;
    if (mode === 1) {
      // Lines above the cursor are only cleared once the cursor is below line 1 (as alacritty).
      if (cur.line > 1) grid.resetRegion(0, cur.line, bg);
      const end = Math.min(cur.col + 1, this.cols), row = grid.row(cur.line);
      row.touchTo(end);
      row.blankRange(0, end, bg);
    } else if (mode === 0) {
      const row = grid.row(cur.line);
      row.touchAll();
      row.blankRange(cur.col, this.cols, bg);
      if (cur.line < this.rows - 1) grid.resetRegion(cur.line + 1, this.rows, bg);
    } else if (mode === 2) {
      if (this.isAltScreen) grid.resetRegion(0, this.rows, bg);
      else grid.clearViewport(bg);
    } else if (mode === 3) {
      if (grid.historySize > 0) { grid.clearHistory(); this.dropImages(); }
    }
  }

  // ── modes ──

  setMode(m, on) {
    const bit = m === 4 ? MODE.INSERT : m === 20 ? MODE.LINE_FEED_NEW_LINE : 0;
    if (bit) this.mode = on ? this.mode | bit : this.mode & ~bit;
  }

  setPrivateMode(m, on) {
    const M = MODE;
    const flag = bit => { this.mode = on ? this.mode | bit : this.mode & ~bit; };
    switch (m) {
      case 1: flag(M.APP_CURSOR); break;
      case 3:
        this.setScrollingRegion(1, this.rows);
        this.grid.resetRegion(0, this.rows, this.grid.cursor.bg);
        break;
      case 6: flag(M.ORIGIN); if (on) this.goto(0, 0); break;
      case 7: flag(M.LINE_WRAP); break;
      case 12:
        this.cursorStyle = { ...(this.cursorStyle || DEFAULT_CURSOR_STYLE), blinking: on };
        this.emit('cursorstyle');
        break;
      case 25: flag(M.SHOW_CURSOR); break;
      case 1000: if (on) this.mode &= ~MOUSE_MODE; flag(M.MOUSE_REPORT_CLICK); this.xmode &= ~XMODE.X10_MOUSE; break;
      case 1002: if (on) this.mode &= ~MOUSE_MODE; flag(M.MOUSE_DRAG); this.xmode &= ~XMODE.X10_MOUSE; break;
      case 1003: if (on) this.mode &= ~MOUSE_MODE; flag(M.MOUSE_MOTION); this.xmode &= ~XMODE.X10_MOUSE; break;
      case 1004: flag(M.FOCUS_IN_OUT); break;
      case 1005: if (on) this.mode &= ~M.SGR_MOUSE; flag(M.UTF8_MOUSE); break;
      case 1006: if (on) this.mode &= ~M.UTF8_MOUSE; flag(M.SGR_MOUSE); if (on) this.xmode &= ~XMODE.SGR_PIXELS_MOUSE; break;
      case 1007: flag(M.ALTERNATE_SCROLL); break;
      case 1042: flag(M.URGENCY_HINTS); break;
      case 1049: if (on !== this.isAltScreen) this.swapAlt(); break;
      case 2004: flag(M.BRACKETED_PASTE); break;
      default: if (this.xterm) this.setXtermMode(m, on);
    }
  }

  setXtermMode(m, on) {
    const xflag = bit => { this.xmode = on ? this.xmode | bit : this.xmode & ~bit; };
    switch (m) {
      case 2: if (on) { this.grid.cursor.charsets = [0, 0, 0, 0]; } break;
      case 9: if (on) this.mode &= ~MOUSE_MODE; xflag(XMODE.X10_MOUSE); break;
      case 45: xflag(XMODE.REVERSE_WRAP); break;
      case 66: this.mode = on ? this.mode | MODE.APP_KEYPAD : this.mode & ~MODE.APP_KEYPAD; break;
      case 1016: xflag(XMODE.SGR_PIXELS_MOUSE); break;
      case 1048: if (on) this.grid.saved = this.grid.cursor.clone(); else this.grid.cursor = this.grid.saved.clone(); break;
      case 47: case 1047: if (on !== this.isAltScreen) this.swapScreenPlain(on); break;
      case 2031: xflag(XMODE.COLOR_SCHEME_REPORTS); break;
      case 9001: xflag(XMODE.WIN32_INPUT); break;
      case 80: xflag(XMODE.SIXEL_DISPLAY); break;
    }
  }

  // xterm's 47/1047: the alternate screen without alacritty's cursor save — the cursor carries
  // over both ways, and the alternate screen is cleared on the way in.
  swapScreenPlain(on) {
    const cur = this.grid.cursor.clone();
    if (on) this.inactive.resetRegion(0, this.inactive.lines, cur.bg);
    const g = this.grid; this.grid = this.inactive; this.inactive = g;
    this.grid.cursor = cur;
    this.mode ^= MODE.ALT_SCREEN;
    this.swapKeyboardStacks();
    this.dropImages();
    this.emit('screen', this.isAltScreen);
  }

  setScrollingRegion(top, bottom) {
    if (top >= bottom) return;
    this.top = Math.min(top - 1, this.rows);
    this.bottom = Math.min(bottom, this.rows);
    this.goto(0, 0);
  }

  modeState(m) {
    if (this.xterm && m === 2) return 4;
    if (this.xterm && m === 12) return 3;
    if (m === 4) return this.mode & MODE.INSERT ? 1 : 2;
    if (m === 20) return this.mode & MODE.LINE_FEED_NEW_LINE ? 1 : 2;
    return 0;
  }

  privateModeState(m) {
    const has = bit => (this.mode & bit ? 1 : 2);
    switch (m) {
      case 1: return has(MODE.APP_CURSOR);
      case 6: return has(MODE.ORIGIN);
      case 7: return has(MODE.LINE_WRAP);
      case 12:
        this.cursorStyle = this.cursorStyle || { ...DEFAULT_CURSOR_STYLE };
        return this.cursorStyle.blinking ? 1 : 2;
      case 25: return has(MODE.SHOW_CURSOR);
      case 1000: return has(MODE.MOUSE_REPORT_CLICK);
      case 1002: return has(MODE.MOUSE_DRAG);
      case 1003: return has(MODE.MOUSE_MOTION);
      case 1004: return has(MODE.FOCUS_IN_OUT);
      case 1005: return has(MODE.UTF8_MOUSE);
      case 1006: return has(MODE.SGR_MOUSE);
      case 1007: return has(MODE.ALTERNATE_SCROLL);
      case 1042: return has(MODE.URGENCY_HINTS);
      case 1049: return has(MODE.ALT_SCREEN);
      case 2004: return has(MODE.BRACKETED_PASTE);
      case 2026: return 2;
      default: return this.xterm ? this.xtermModeState(m) : 0;
    }
  }

  // DECRQM for the modes only xterm.js knows: 1/2 set/reset, 3/4 permanently set/reset.
  xtermModeState(m) {
    const has = bit => (this.xmode & bit ? 1 : 2);
    switch (m) {
      case 8: return 3;
      case 9: return has(XMODE.X10_MOUSE);
      case 45: return has(XMODE.REVERSE_WRAP);
      case 66: return this.mode & MODE.APP_KEYPAD ? 1 : 2;
      case 67: case 1015: return 4;
      case 1016: return has(XMODE.SGR_PIXELS_MOUSE);
      case 1048: return 1;
      case 47: case 1047: return this.isAltScreen ? 1 : 2;
      case 9001: return has(XMODE.WIN32_INPUT);
      case 80: return has(XMODE.SIXEL_DISPLAY);
      default: return 0;
    }
  }

  get resolvedCursorStyle() { return this.cursorStyle || DEFAULT_CURSOR_STYLE; }

  // ── SGR ──

  sgr(groups) {
    const cur = this.grid.cursor;
    // `values` yields numbers until it runs out; a colour stops reading at the first bad one.
    const color = next => {
      const kind = next();
      if (kind === 2) {
        const r = next(); if (r === undefined || r > 255) return null;
        const g = next(); if (g === undefined || g > 255) return null;
        const b = next(); if (b === undefined || b > 255) return null;
        return COLOR_RGB | (r << 16) | (g << 8) | b;
      }
      if (kind === 5) {
        const i = next();
        return i === undefined || i > 255 ? null : COLOR_INDEXED | i;
      }
      return null;
    };
    const setUnderline = bit => { cur.fl = (cur.fl & ~ALL_UNDERLINES) | bit; };
    let k = 0;
    while (k < groups.length) {
      const g = groups[k++], p = g[0];
      if (p === 4) {
        const sub = g.length === 2 ? g[1] : -1;
        if (sub === 0) cur.fl &= ~ALL_UNDERLINES;
        else if (sub === 2) setUnderline(DOUBLE_UNDERLINE);
        else if (sub === 3) setUnderline(UNDERCURL);
        else if (sub === 4) setUnderline(DOTTED_UNDERLINE);
        else if (sub === 5) setUnderline(DASHED_UNDERLINE);
        else setUnderline(UNDERLINE);
        continue;
      }
      if (p === 38 || p === 48 || p === 58) {
        let next;
        if (g.length === 1) {
          // `38;5;n` reads the following parameters themselves.
          next = () => (k < groups.length ? groups[k++][0] : undefined);
        } else {
          const s = g.slice(1), list = [s[0], ...s.slice(s.length > 4 ? 2 : 1)];
          let i = 0;
          next = () => list[i++];
        }
        const c = color(next);
        if (c === null) continue;
        if (p === 38) cur.fg = c; else if (p === 48) cur.bg = c; else this.setTemplateUnderline(c);
        continue;
      }
      if (g.length > 1) continue;
      if (p === 0) { cur.fg = DEFAULT_FG; cur.bg = DEFAULT_BG; cur.fl = 0; this.setTemplateUnderline(-1); }
      else if (p === 1) cur.fl |= BOLD;
      else if (p === 2) cur.fl |= DIM;
      else if (p === 3) cur.fl |= ITALIC;
      else if (p === 7) cur.fl |= INVERSE;
      else if (p === 8) cur.fl |= HIDDEN;
      else if (p === 9) cur.fl |= STRIKEOUT;
      else if (p === 21) cur.fl &= ~BOLD;
      else if (p === 22) cur.fl &= ~(BOLD | DIM);
      else if (p === 23) cur.fl &= ~ITALIC;
      else if (p === 24) cur.fl &= ~ALL_UNDERLINES;
      else if (p === 27) cur.fl &= ~INVERSE;
      else if (p === 28) cur.fl &= ~HIDDEN;
      else if (p === 29) cur.fl &= ~STRIKEOUT;
      else if (p >= 30 && p <= 37) cur.fg = p - 30;
      else if (p === 39) cur.fg = DEFAULT_FG;
      else if (p >= 40 && p <= 47) cur.bg = p - 40;
      else if (p === 49) cur.bg = DEFAULT_BG;
      else if (p === 59) this.setTemplateUnderline(-1);
      else if (p >= 90 && p <= 97) cur.fg = p - 90 + 8;
      else if (p >= 100 && p <= 107) cur.bg = p - 100 + 8;
      else if (this.xterm) {
        if (p === 5) cur.fl |= BLINK;
        else if (p === 25) cur.fl &= ~BLINK;
        else if (p === 53) cur.fl |= OVERLINE;
        else if (p === 55) cur.fl &= ~OVERLINE;
        else if (p === 221) cur.fl &= ~BOLD;
        else if (p === 222) cur.fl &= ~DIM;
      }
    }
  }

  setTemplateUnderline(c) {
    const cur = this.grid.cursor, link = cur.ext ? cur.ext.link : null;
    cur.ext = c < 0 && !link ? null : { ul: c, link };
  }

  setTemplateLink(link) {
    const cur = this.grid.cursor, ul = cur.ext ? cur.ext.ul : -1;
    cur.ext = !link && ul < 0 ? null : { ul, link };
  }

  // ── OSC ──

  setTitle(title) {
    this.title = title;
    this.emit('title', title);
  }

  // The app has no icon names; tracking one changes nothing on screen.
  setIconName(name) {
    this.iconName = name;
    this.emit('iconname', name);
  }

  oscDispatch(params, terminator) {
    if (params.length === 0 || params[0].length === 0) return;
    const head = utf8(params[0]);
    const str = i => strictUtf8(params[i]);
    switch (head) {
      case '0': case '2': {
        if (params.length < 2) return;
        const parts = [];
        for (let i = 1; i < params.length; i++) { const s = str(i); if (s !== null) parts.push(s); }
        this.setTitle(parts.join(';').trim());
        if (head === '0') this.setIconName(parts.join(';'));
        return;
      }
      case '1': {
        if (params.length >= 2) this.setIconName(params.slice(1).map(p => utf8(p)).join(';'));
        return;
      }
      case '22': {
        // Mouse pointer shape (a CSS cursor name); alacritty only passes it on as well.
        if (params.length === 2) this.emit('pointershape', utf8(params[1]));
        return;
      }
      case '4': {
        if (params.length <= 1 || params.length % 2 === 0) return;
        for (let i = 1; i + 1 < params.length; i += 2) {
          const index = parseU8(utf8(params[i]));
          if (index === null) continue;
          const spec = utf8(params[i + 1]), rgb = parseXColor(spec);
          if (rgb) { this.colors[index] = rgb; this.emit('colors'); }
          else if (spec === '?') this.colorQuery(`4;${index}`, index, terminator);
        }
        return;
      }
      case '7': {
        const uri = str(1);
        if (uri !== null) this.emit('cwd', uri);
        return;
      }
      case '8': {
        if (params.length <= 2) return;
        let uri = str(2) ?? '';
        for (let i = 3; i < params.length; i++) uri += ';' + (str(i) ?? '');
        if (uri === '') { this.setTemplateLink(null); return; }
        let id = null;
        for (const kv of utf8(params[1]).split(':')) if (kv.startsWith('id=')) { id = kv.slice(3); break; }
        this.setTemplateLink({ id, uri, serial: ++this.hyperlinkSerial });
        return;
      }
      case '10': case '11': case '12': {
        if (params.length < 2) return;
        let code = Number(head);
        for (let i = 1; i < params.length; i++, code++) {
          const index = 256 + code - 10;
          if (index > 258) break;
          const spec = utf8(params[i]), rgb = parseXColor(spec);
          if (rgb) { this.colors[index] = rgb; this.emit('colors'); }
          else if (spec === '?') this.colorQuery(String(code), index, terminator);
        }
        return;
      }
      case '50': {
        const v = params.length >= 2 ? utf8(params[1]) : '';
        const m = /^CursorShape=([012])/.exec(v);
        if (m) {
          this.cursorStyle = { ...(this.cursorStyle || DEFAULT_CURSOR_STYLE), shape: ['block', 'beam', 'underline'][+m[1]] };
          this.emit('cursorstyle');
        }
        return;
      }
      case '52': {
        if (params.length < 3) return;
        const sel = params[1].length ? params[1][0] : 0x63;
        const kind = sel === 0x63 ? 'Clipboard' : (sel === 0x70 || sel === 0x73) ? 'Selection' : null;
        if (!kind) return;
        if (utf8(params[2]) === '?') return;   // reading the clipboard back is not allowed
        const bytes = base64Decode(utf8(params[2]));
        const text = bytes && strictUtf8(bytes);
        if (text !== null && text !== undefined) this.emit('clipboard', kind, text);
        return;
      }
      case '104': {
        if (params.length === 1 || params[1].length === 0) { for (let i = 0; i < 256; i++) this.colors[i] = null; }
        else for (let i = 1; i < params.length; i++) { const n = parseU8(utf8(params[i])); if (n !== null) this.colors[n] = null; }
        this.emit('colors');
        return;
      }
      case '110': this.colors[256] = null; this.emit('colors'); return;
      case '111': this.colors[257] = null; this.emit('colors'); return;
      case '112': this.colors[258] = null; this.emit('colors'); return;
      case '133': this.emit('prompt', params.slice(1).map(p => utf8(p))); return;
      case '1337': this.itermImage(params.slice(1).map(p => utf8(p)).join(';')); return;
    }
  }

  colorQuery(prefix, index, terminator) {
    this.emit('colorquery', index);
    const rgb = this.queryColor ? this.queryColor(index) : null;
    if (!rgb) return;
    const [r, g, b] = rgb;
    this.emit('data', `\x1b]${prefix};rgb:${hex2(r)}${hex2(r)}/${hex2(g)}${hex2(g)}/${hex2(b)}${hex2(b)}${terminator}`);
  }

  get syncActive() { return this.syncPending; }
}
