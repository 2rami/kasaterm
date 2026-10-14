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

// Private modes the reference terminal knows; anything else is ignored (and reported as 0).
const PRIVATE_MODES = new Set([1, 3, 6, 7, 12, 25, 1000, 1002, 1003, 1004, 1005, 1006, 1007, 1042, 1049, 2004, 2026]);

const DEC_SPECIAL = {
  0x5f: 0x20, 0x60: 0x25c6, 0x61: 0x2592, 0x62: 0x2409, 0x63: 0x240c, 0x64: 0x240d, 0x65: 0x240a,
  0x66: 0xb0, 0x67: 0xb1, 0x68: 0x2424, 0x69: 0x240b, 0x6a: 0x2518, 0x6b: 0x2510, 0x6c: 0x250c,
  0x6d: 0x2514, 0x6e: 0x253c, 0x6f: 0x23ba, 0x70: 0x23bb, 0x71: 0x2500, 0x72: 0x23bc, 0x73: 0x23bd,
  0x74: 0x251c, 0x75: 0x2524, 0x76: 0x2534, 0x77: 0x252c, 0x78: 0x2502, 0x79: 0x2264, 0x7a: 0x2265,
  0x7b: 0x3c0, 0x7c: 0x2260, 0x7d: 0xa3, 0x7e: 0xb7,
};

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

const textDecoder = typeof TextDecoder !== 'undefined' ? new TextDecoder() : null;
const textEncoder = typeof TextEncoder !== 'undefined' ? new TextEncoder() : null;

function utf8(bytes) { return textDecoder.decode(bytes); }

const strictDecoder = typeof TextDecoder !== 'undefined' ? new TextDecoder('utf-8', { fatal: true }) : null;

function strictUtf8(bytes) {
  try { return strictDecoder.decode(bytes); } catch { return null; }
}

function hex2(n) { return n.toString(16).padStart(2, '0'); }

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
    this.tabs = Terminal.makeTabs(cols);
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
    this.heldUtf8 = new Uint8Array(0);
    this.resetParser();
    this.u8need = 0; this.u8cp = 0; this.u8lo = 0x80; this.u8hi = 0xbf;
  }

  static makeTabs(cols, old) {
    const t = new Uint8Array(cols);
    for (let i = 0; i < cols; i++) t[i] = old && i < old.length ? old[i] : (i % 8 === 0 ? 1 : 0);
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
  write(data) {
    const bytes = typeof data === 'string' ? textEncoder.encode(this.normalize ? data.normalize('NFC') : data)
      : this.normalize ? this.composeBatch(data) : data;
    let i = 0;
    while (i < bytes.length) {
      i += this.syncPending ? this.advanceSync(bytes.subarray(i)) : this.parse(bytes, i, true);
    }
    this.emit('write');
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
    if (ascii) return batch;
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
      else this.step(bytes[i]);
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
    this.parse(held.subarray(0, bsu === null ? held.length : bsu), 0, false);
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
  flushSync() { if (this.syncPending) this.stopSync(null); }

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
          if (--this.u8need === 0 && this.u8cp >= 0xa0) this.print(this.u8cp);
          continue;
        }
        this.u8need = 0;
        this.print(0xfffd);
      }
      if (b < 0x80) {
        if (b >= 0x20) { this.print(b); continue; }
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
      out.push(Array.from(this.pvals.subarray(i, i + k)));
      i += k;
    }
    return out;
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
        else if (b === 0x58 || b === 0x5e || b === 0x5f) this.state = S_SOS;
        else if (b === 0x5b) { this.resetParser(); this.state = S_CSI_ENTRY; }
        else if (b === 0x5d) { this.oscRaw = []; this.oscBounds = []; this.state = S_OSC; }
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
        if (b === 0x18 || b === 0x1a) { this.execute(b); this.state = S_GROUND; }
        else if (b === 0x1b) { this.resetParser(); this.state = S_ESC; }
        else if (b === 0x9c) this.state = S_GROUND;
        return;
      case S_DCS_IGNORE:
      case S_SOS:
        this.anywhere(b);
        return;
      case S_OSC:
        if (b === 0x07) { this.oscEnd(true); this.state = S_GROUND; }
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
      else if (b >= 0x40 && b <= 0x7e) this.dcsHook();
      else this.anywhere(b);
    } else if (s === S_DCS_PARAM) {
      if (b >= 0x20 && b <= 0x2f) { this.collect(b); this.state = S_DCS_INT; }
      else if (b >= 0x30 && b <= 0x39) this.paramDigit(b);
      else if (b === 0x3a) this.paramPush(true);
      else if (b === 0x3b) this.paramPush(false);
      else if (b >= 0x3c && b <= 0x3f) this.state = S_DCS_IGNORE;
      else if (b >= 0x40 && b <= 0x7e) this.dcsHook();
      else this.anywhere(b);
    } else {
      if (b >= 0x20 && b <= 0x2f) this.collect(b);
      else if (b >= 0x30 && b <= 0x3f) this.state = S_DCS_IGNORE;
      else if (b >= 0x40 && b <= 0x7e) this.dcsHook();
      else this.anywhere(b);
    }
  }

  dcsHook() {
    if (this.plen === MAX_PARAMS) this.ignoring = true; else this.paramPush(false);
    this.state = S_DCS_PASS;
  }

  oscParam() {
    if (this.oscBounds.length === MAX_OSC_PARAMS) return;
    this.oscBounds.push(this.oscRaw.length);
  }

  oscEnd(bell) {
    this.oscParam();
    const raw = Uint8Array.from(this.oscRaw), params = [];
    let start = 0;
    for (const end of this.oscBounds) { params.push(raw.subarray(start, end)); start = end; }
    this.oscRaw = [];
    this.oscBounds = [];
    this.oscDispatch(params, bell ? '\x07' : '\x1b\\');
  }

  // ── C0 ──

  execute(b) {
    switch (b) {
      case 0x09: this.putTab(1); break;
      case 0x08: this.backspace(); break;
      case 0x0d: this.carriageReturn(); break;
      case 0x0a: case 0x0b: case 0x0c: this.linefeed(); break;
      case 0x07: this.emit('bell'); break;
      case 0x0f: this.activeCharset = 0; break;
      case 0x0e: this.activeCharset = 1; break;
    }
  }

  // ── printing ──

  print(cp) {
    this.precedingChar = cp;
    this.input(cp);
  }

  input(cp) {
    const width = charWidth(cp);
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
    if (this.grid.cursor.charsets[this.activeCharset] === 1) return DEC_SPECIAL[cp] ?? cp;
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
    if (c.col > 0) { c.col -= 1; c.wrap = false; }
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
    const g = this.grid; this.grid = this.inactive; this.inactive = g;
    this.mode ^= MODE.ALT_SCREEN;
    this.emit('screen', this.isAltScreen);
  }

  resize(cols, rows) {
    cols = Math.max(2, cols | 0); rows = Math.max(1, rows | 0);
    if (cols === this.cols && rows === this.rows) return;
    const oldCols = this.cols;
    const alt = this.isAltScreen;
    this.grid.resize(!alt, rows, cols);
    this.inactive.resize(alt, rows, cols);
    if (oldCols !== cols) {
      this.tabs = Terminal.makeTabs(cols, this.tabs);
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
    this.tabs = Terminal.makeTabs(this.cols);
    this.titleStack = [];
    this.title = null;
    this.mode = DEFAULT_MODE;
  }

  // ── ESC ──

  escDispatch(b) {
    const inter = this.inter;
    const charsetIndex = () => ({ 0x28: 0, 0x29: 1, 0x2a: 2, 0x2b: 3 })[inter.length === 1 ? inter[0] : -1];
    if (b === 0x42 || b === 0x30) {
      const index = charsetIndex();
      if (index !== undefined) this.grid.cursor.charsets[index] = b === 0x30 ? 1 : 0;
      return;
    }
    if (b === 0x38 && inter.length === 1 && inter[0] === 0x23) { this.decaln(); return; }
    if (inter.length) return;
    switch (b) {
      case 0x44: this.linefeed(); break;
      case 0x45: this.linefeed(); this.carriageReturn(); break;
      case 0x48: this.tabs[this.grid.cursor.col] = 1; break;
      case 0x4d: this.reverseIndex(); break;
      case 0x5a: this.reply(DA1_REPLY); break;
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
    this.csiDispatch(b, this.groups(), this.inter.map(c => String.fromCharCode(c)).join(''));
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
      case 'H': case 'f': { const y = next(1), x = next(1); this.goto(y - 1, x - 1); break; }
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
        else if (m === 22) {
          if (this.titleStack.length >= TITLE_STACK_MAX) this.titleStack.shift();
          this.titleStack.push(this.title);
        } else if (m === 23) {
          if (this.titleStack.length) this.setTitle(this.titleStack.pop());
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
        // DA asks with no parameter or 0; the `>` form is secondary DA.
        if (f === 'c' && next(0) === 0) {
          if (inter === '') this.reply(DA1_REPLY);
          else if (inter[0] === '>') this.reply(DA2_REPLY);
        }
    }
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
      if (grid.historySize > 0) grid.clearHistory();
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
      case 1000: if (on) this.mode &= ~MOUSE_MODE; flag(M.MOUSE_REPORT_CLICK); break;
      case 1002: if (on) this.mode &= ~MOUSE_MODE; flag(M.MOUSE_DRAG); break;
      case 1003: if (on) this.mode &= ~MOUSE_MODE; flag(M.MOUSE_MOTION); break;
      case 1004: flag(M.FOCUS_IN_OUT); break;
      case 1005: if (on) this.mode &= ~M.SGR_MOUSE; flag(M.UTF8_MOUSE); break;
      case 1006: if (on) this.mode &= ~M.UTF8_MOUSE; flag(M.SGR_MOUSE); break;
      case 1007: flag(M.ALTERNATE_SCROLL); break;
      case 1042: flag(M.URGENCY_HINTS); break;
      case 1049: if (on !== this.isAltScreen) this.swapAlt(); break;
      case 2004: flag(M.BRACKETED_PASTE); break;
    }
  }

  setScrollingRegion(top, bottom) {
    if (top >= bottom) return;
    this.top = Math.min(top - 1, this.rows);
    this.bottom = Math.min(bottom, this.rows);
    this.goto(0, 0);
  }

  modeState(m) {
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
