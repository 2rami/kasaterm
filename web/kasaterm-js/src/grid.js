// Cells, rows and scrollback storage.
//
// The grid is kept equal to kasaterm's own terminal (alacritty_terminal inside kasa-pty), cell
// for cell — that is what the differential test checks. Storage, row bookkeeping and reflow
// follow alacritty_terminal's grid (Apache-2.0) closely, including its quirks: rows only reset
// the cells they know were written (`occ`), widened rows keep their old background until
// written, and recycled scrollback rows come back without a reset. "Fixing" any of that here
// would make the browser disagree with the app window.

const DEFAULT_FG = 256;   // alacritty NamedColor::Foreground
const DEFAULT_BG = 257;   // NamedColor::Background
const COLOR_INDEXED = 0x1000000;
const COLOR_RGB = 0x2000000;

// alacritty term::cell::Flags bit values.
const INVERSE = 1, BOLD = 2, ITALIC = 4, UNDERLINE = 8, WRAPLINE = 16, WIDE_CHAR = 32,
  WIDE_CHAR_SPACER = 64, DIM = 128, HIDDEN = 256, STRIKEOUT = 512, LEADING_WIDE_CHAR_SPACER = 1024,
  DOUBLE_UNDERLINE = 2048, UNDERCURL = 4096, DOTTED_UNDERLINE = 8192, DASHED_UNDERLINE = 16384;
const ALL_UNDERLINES = UNDERLINE | DOUBLE_UNDERLINE | UNDERCURL | DOTTED_UNDERLINE | DASHED_UNDERLINE;
// Only `profile: 'xterm'` sets these (alacritty has no such flags), well clear of its bits.
const BLINK = 1 << 20, OVERLINE = 1 << 21, PROTECTED = 1 << 22;
const NOT_EMPTY_FLAGS = INVERSE | ALL_UNDERLINES | STRIKEOUT | WRAPLINE | WIDE_CHAR_SPACER | LEADING_WIDE_CHAR_SPACER;

const MAX_CACHE_SIZE = 1000;

// A row is four u32 per cell — code point, fg, bg, flags — plus a sparse array of extras
// ({zw, ul, link}: combining marks, underline colour, hyperlink) that most cells never need.
// `occ` bounds the cells written since the last reset; every writer must raise it.
class Row {
  constructor(cols) {
    this.d = new Uint32Array(cols * 4);
    this.len = cols;
    this.x = null;
    this.occ = 0;
    for (let i = 0; i < cols; i++) { const o = i * 4; this.d[o] = 32; this.d[o + 1] = DEFAULT_FG; this.d[o + 2] = DEFAULT_BG; }
  }

  static from(d, x, len, occ) {
    const r = Object.create(Row.prototype);
    r.d = d; r.x = x; r.len = len; r.occ = occ;
    return r;
  }

  cp(i) { return this.d[i * 4]; }
  fg(i) { return this.d[i * 4 + 1]; }
  bg(i) { return this.d[i * 4 + 2]; }
  flags(i) { return this.d[i * 4 + 3]; }
  extra(i) { return this.x ? this.x[i] : undefined; }

  text(i) {
    const zw = this.x && this.x[i] && this.x[i].zw;
    const base = String.fromCodePoint(this.d[i * 4]);
    return zw ? (base + zw).normalize('NFC') : base;
  }

  touch(i) { if (i + 1 > this.occ) this.occ = i + 1; }
  touchTo(end) { if (end > this.occ) this.occ = end; }
  touchAll() { this.occ = this.len; }

  setExtra(i, e) {
    if (e) { if (!this.x) this.x = new Array(this.len); this.x[i] = e; }
    else if (this.x) this.x[i] = undefined;
  }

  // Cell::from(bg): a blank cell that keeps only the background (erase with "bce").
  blank(i, bg) {
    const o = i * 4;
    this.d[o] = 32; this.d[o + 1] = DEFAULT_FG; this.d[o + 2] = bg; this.d[o + 3] = 0;
    if (this.x) this.x[i] = undefined;
  }

  blankRange(from, to, bg) { for (let i = from; i < to; i++) this.blank(i, bg); }

  // Row::reset: only cells up to `occ` are cleared unless the last cell's background differs
  // from the template — the cells after `occ` are assumed to still match it.
  reset(bg) {
    if (this.d[(this.len - 1) * 4 + 2] !== bg) this.occ = this.len;
    this.blankRange(0, this.occ, bg);
    this.occ = 0;
  }

  swap(a, b) {
    const d = this.d, oa = a * 4, ob = b * 4;
    for (let k = 0; k < 4; k++) { const t = d[oa + k]; d[oa + k] = d[ob + k]; d[ob + k] = t; }
    if (this.x) { const t = this.x[a]; this.x[a] = this.x[b]; this.x[b] = t; }
  }

  isEmpty(i) {
    const o = i * 4, cp = this.d[o];
    return (cp === 32 || cp === 9) && this.d[o + 1] === DEFAULT_FG && this.d[o + 2] === DEFAULT_BG
      && (this.d[o + 3] & NOT_EMPTY_FLAGS) === 0 && !(this.x && this.x[i] && this.x[i].zw);
  }

  isClear() {
    for (let i = 0; i < this.len; i++) if (!this.isEmpty(i)) return false;
    return true;
  }

  // ── variable-length helpers used only by reflow ──

  slice(from, to) {
    return Row.from(this.d.slice(from * 4, to * 4), this.x ? this.x.slice(from, to) : null, to - from, 0);
  }

  // Row::grow pads with default cells and leaves `occ` alone; shrinking clamps it.
  setLen(n) {
    if (n === this.len) return;
    if (n < this.len) {
      this.d = this.d.slice(0, n * 4);
      if (this.x) this.x.length = n;
      this.occ = Math.min(this.occ, n);
    } else {
      const d = new Uint32Array(n * 4);
      d.set(this.d);
      for (let i = this.len; i < n; i++) { const o = i * 4; d[o] = 32; d[o + 1] = DEFAULT_FG; d[o + 2] = DEFAULT_BG; }
      this.d = d;
      if (this.x) this.x.length = n;
    }
    this.len = n;
  }

  joined(other) {
    const d = new Uint32Array((this.len + other.len) * 4);
    d.set(this.d); d.set(other.d, this.len * 4);
    let x = null;
    if (this.x || other.x) {
      x = new Array(this.len + other.len);
      if (this.x) for (let i = 0; i < this.len; i++) x[i] = this.x[i];
      if (other.x) for (let i = 0; i < other.len; i++) x[this.len + i] = other.x[i];
    }
    return { d, x };
  }

  // Row::shrink: cut at `cols`, return the cut-off cells without their empty tail (or null).
  shrink(cols) {
    if (this.len <= cols) return null;
    const cut = this.slice(cols, this.len);
    this.setLen(cols);
    let keep = 0;
    for (let i = cut.len - 1; i >= 0; i--) if (!cut.isEmpty(i)) { keep = i + 1; break; }
    if (keep === 0) return null;
    cut.setLen(keep);
    return cut;
  }

  frontSplitOff(at) {
    const head = this.slice(0, at), rest = this.slice(at, this.len);
    this.occ = Math.max(0, this.occ - at);
    this.d = rest.d; this.x = rest.x; this.len = rest.len;
    return head;
  }

  append(cells) {
    const j = this.joined(cells);
    this.occ += cells.len;
    this.d = j.d; this.x = j.x; this.len += cells.len;
  }

  appendFront(cells) {
    const j = cells.joined(this);
    this.occ += cells.len;
    this.d = j.d; this.x = j.x; this.len += cells.len;
  }

  setFlag(i, bit, on) {
    if (on) this.d[i * 4 + 3] |= bit; else this.d[i * 4 + 3] &= ~bit;
  }
}

function spacerCell() {
  const r = new Row(1);
  r.d[3] = LEADING_WIDE_CHAR_SPACER;
  return r;
}

// alacritty grid::storage::Storage: a ring of rows whose `zero` moves instead of the rows, with
// up to MAX_CACHE_SIZE rows past `len` kept for reuse. Line 0 is the top visible line; negative
// lines are scrollback.
class Storage {
  constructor(visibleLines, columns) {
    this.inner = [];
    for (let i = 0; i < visibleLines; i++) this.inner.push(new Row(columns));
    this.zero = 0;
    this.visibleLines = visibleLines;
    this.len = visibleLines;
  }

  index(line) {
    const z = this.zero + (this.visibleLines - line - 1);
    return z >= this.inner.length ? z - this.inner.length : z;
  }

  get(line) { return this.inner[this.index(line)]; }

  swap(a, b) {
    const i = this.index(a), j = this.index(b);
    const t = this.inner[i]; this.inner[i] = this.inner[j]; this.inner[j] = t;
  }

  growVisibleLines(next) {
    this.initialize(next - this.visibleLines, this.get(0).len);
    this.visibleLines = next;
  }

  shrinkVisibleLines(next) {
    this.shrinkLines(this.visibleLines - next);
    this.visibleLines = next;
  }

  shrinkLines(n) {
    this.len -= n;
    if (this.inner.length > this.len + MAX_CACHE_SIZE) this.truncate();
  }

  truncate() { this.rezero(); this.inner.length = this.len; }

  initialize(rows, columns) {
    if (this.len + rows > this.inner.length) {
      this.rezero();
      const size = this.inner.length + Math.max(rows, MAX_CACHE_SIZE);
      while (this.inner.length < size) this.inner.push(new Row(columns));
    }
    this.len += rows;
  }

  rotate(count) {
    const n = this.inner.length;
    this.zero = ((this.zero + count) % n + n) % n;
  }

  rotateDown(count) { this.zero = (this.zero + count) % this.inner.length; }

  replaceInner(rows) { this.len = rows.length; this.inner = rows; this.zero = 0; }

  takeAll() {
    this.truncate();
    const rows = this.inner;
    this.inner = [];
    this.len = 0;
    return rows;
  }

  rezero() {
    if (this.zero === 0) return;
    this.inner = this.inner.slice(this.zero).concat(this.inner.slice(0, this.zero));
    this.zero = 0;
  }
}

class Cursor {
  constructor() {
    this.line = 0;
    this.col = 0;
    this.wrap = false;           // alacritty input_needs_wrap
    this.fg = DEFAULT_FG;
    this.bg = DEFAULT_BG;
    this.fl = 0;
    this.ext = null;             // template extras {ul, link}; null when neither is set
    this.charsets = [0, 0, 0, 0];
  }

  clone() {
    const c = new Cursor();
    c.line = this.line; c.col = this.col; c.wrap = this.wrap;
    c.fg = this.fg; c.bg = this.bg; c.fl = this.fl; c.ext = this.ext;
    c.charsets = this.charsets.slice();
    return c;
  }
}

class Grid {
  constructor(lines, cols, maxHistory) {
    this.lines = lines;
    this.cols = cols;
    this.maxHistory = maxHistory;
    this.raw = new Storage(lines, cols);
    this.cursor = new Cursor();
    this.saved = new Cursor();
    this.displayOffset = 0;
    this.scrolled = 0;           // lines pushed off the top so far — `scroll` events count them
  }

  get historySize() { return Math.max(0, this.raw.len - this.lines); }

  row(y) { return this.raw.get(y); }

  resetRegion(start, end, bg) { for (let y = start; y < end; y++) this.raw.get(y).reset(bg); }

  updateHistory(max) {
    if (this.historySize > max) this.raw.shrinkLines(this.historySize - max);
    this.displayOffset = Math.min(this.displayOffset, max);
    this.maxHistory = max;
  }

  increaseScrollLimit(count) {
    count = Math.min(count, this.maxHistory - this.historySize);
    if (count !== 0) this.raw.initialize(count, this.cols);
  }

  decreaseScrollLimit(count) {
    count = Math.min(count, this.historySize);
    if (count !== 0) {
      this.raw.shrinkLines(count);
      this.displayOffset = Math.min(this.displayOffset, this.historySize);
    }
  }

  scrollDown(start, end, n, bg) {
    if (end - start <= n) { this.resetRegion(start, end, bg); return; }
    if (this.maxHistory === 0) {
      for (let y = end; y < this.lines; y++) this.raw.swap(y, y - n);
      this.raw.rotateDown(n);
      for (let y = 0; y < n; y++) this.raw.get(y).reset(bg);
      for (let y = 0; y < start; y++) this.raw.swap(y, y + n);
    } else {
      for (let y = end - 1; y >= start + n; y--) this.raw.swap(y, y - n);
      for (let y = start + n - 1; y >= start; y--) this.raw.get(y).reset(bg);
    }
  }

  // Grid::scroll_up. A region starting at the top line pushes its lines into scrollback even
  // when it has a bottom margin; the lines below the margin are swapped back into place.
  scrollUp(start, end, n, bg) {
    if (end - start <= n && start !== 0) { this.resetRegion(start, end, bg); return; }
    if (this.displayOffset !== 0) this.displayOffset = Math.min(this.displayOffset + n, this.maxHistory);
    if (start === 0) {
      this.scrolled += n;
      this.increaseScrollLimit(n);
      this.raw.rotate(-n);
      for (let y = this.lines - 1; y >= end; y--) this.raw.swap(y, y - n);
    } else {
      for (let y = start; y < end - n; y++) this.raw.swap(y, y + n);
    }
    for (let y = end - n; y < end; y++) this.raw.get(y).reset(bg);
  }

  clearHistory() {
    this.raw.shrinkLines(this.historySize);
    this.displayOffset = 0;
  }

  // Grid::clear_viewport: scroll everything down to the last non-empty cell into scrollback.
  clearViewport(bg) {
    let positions = 1;
    found: for (let y = this.lines - 1; y >= 0; y--) {
      const r = this.row(y);
      for (let x = this.cols - 1; x >= 0; x--) {
        if (!r.isEmpty(x)) { positions = y + 1; break found; }
      }
      if (y === 0 && this.historySize > 0) positions = 0;
    }
    this.scrollUp(0, this.lines, positions, bg);
    this.resetRegion(0, this.lines - positions, bg);
  }

  reset() {
    this.clearHistory();
    this.saved = new Cursor();
    this.cursor = new Cursor();
    this.displayOffset = 0;
    this.resetRegion(-this.historySize, this.lines, DEFAULT_BG);
  }

  // ── resize (alacritty grid/resize.rs) ──

  resize(reflow, lines, cols) {
    const template = { fg: this.cursor.fg, bg: this.cursor.bg, fl: this.cursor.fl, ext: this.cursor.ext };
    this.cursor.fg = DEFAULT_FG; this.cursor.bg = DEFAULT_BG; this.cursor.fl = 0; this.cursor.ext = null;
    if (this.lines < lines) this.growLines(lines);
    else if (this.lines > lines) this.shrinkLines(lines);
    if (this.cols < cols) this.growColumns(reflow, cols);
    else if (this.cols > cols) this.shrinkColumns(reflow, cols);
    Object.assign(this.cursor, template);
  }

  growLines(target) {
    const added = target - this.lines;
    this.raw.growVisibleLines(target);
    this.lines = target;
    const fromHistory = Math.min(this.historySize, added);
    if (fromHistory !== added) this.scrollUp(0, target, added - fromHistory, DEFAULT_BG);
    this.saved.line += fromHistory;
    this.cursor.line += fromHistory;
    this.displayOffset = Math.max(0, this.displayOffset - added);
    this.decreaseScrollLimit(added);
  }

  shrinkLines(target) {
    const required = Math.max(0, this.cursor.line + 1 - target);
    if (required > 0) {
      this.scrollUp(0, this.lines, required, DEFAULT_BG);
      this.cursor.line = Math.min(this.cursor.line, target - 1);
    }
    this.saved.line = Math.min(this.saved.line, target - 1);
    this.raw.rotate(this.lines - target);
    this.raw.shrinkVisibleLines(target);
    this.lines = target;
  }

  // Point::sub with Boundary::Cursor on a grid of `this.lines` x `cols`.
  pointSub(line, col, cols, rhs) {
    const changes = Math.floor(Math.max(0, rhs + cols - 1 - col) / cols);
    line -= changes;
    col = (cols + col - (rhs % cols)) % cols;
    col = Math.min(col, cols - 1);
    if (line < 0) return [0, 0];
    if (line > this.lines - 1) return [this.lines - 1, cols - 1];
    return [line, col];
  }

  growColumns(reflow, columns) {
    const shouldReflow = row => reflow && row.len > 0 && row.len < columns && (row.flags(row.len - 1) & WRAPLINE) !== 0;
    this.cols = columns;
    const cursor = this.cursor;
    const reversed = [];   // top first
    let cursorLineDelta = 0;
    if (cursor.wrap && reflow) { cursor.wrap = false; cursor.col += 1; }
    const rows = this.raw.takeAll();   // bottom first: index i counts up from the bottom line
    for (let i = rows.length - 1; i >= 0; i--) {
      const row = rows[i];
      const lastRow = reversed[reversed.length - 1];
      if (!lastRow || !shouldReflow(lastRow)) { reversed.push(row); continue; }
      lastRow.touchAll();
      lastRow.setFlag(lastRow.len - 1, WRAPLINE, false);
      let lastLen = lastRow.len;
      if (lastLen >= 1 && (lastRow.flags(lastLen - 1) & LEADING_WIDE_CHAR_SPACER)) {
        lastRow.shrink(lastLen - 1);
        lastLen -= 1;
      }
      let numWrapped = columns - lastLen;
      const len = Math.min(row.len, numWrapped);
      let cells;
      if (row.flags(len - 1) & WIDE_CHAR) {
        numWrapped -= 1;
        cells = row.frontSplitOff(len - 1);
        const j = cells.joined(spacerCell());
        cells = Row.from(j.d, j.x, cells.len + 1, 0);
      } else {
        cells = row.frontSplitOff(len);
      }
      lastRow.append(cells);
      const cursorBufferLine = this.lines - cursor.line - 1;
      if (i === cursorBufferLine && reflow) {
        let target = this.pointSub(cursor.line, cursor.col, columns, numWrapped);
        if (target[1] === 0 && row.isClear()) {
          cursor.wrap = true;
          target = this.pointSub(target[0], target[1], columns, 1);
        }
        cursor.col = target[1];
        const lineDelta = cursor.line - target[0];
        if (lineDelta !== 0 && row.isClear()) continue;
        cursorLineDelta += lineDelta;
      } else if (row.isClear()) {
        if (i < this.displayOffset) this.displayOffset = Math.max(0, this.displayOffset - 1);
        if (i < cursorBufferLine) cursor.line += 1;
        continue;
      }
      if (lastRow.len > 0) { lastRow.touchAll(); lastRow.setFlag(lastRow.len - 1, WRAPLINE, true); }
      reversed.push(row);
    }
    if (reversed.length < this.lines) {
      const delta = this.lines - reversed.length;
      cursor.line = Math.max(cursor.line - delta, 0);
      while (reversed.length < this.lines) reversed.push(new Row(columns));
    }
    if (cursorLineDelta !== 0) {
      const cursorBufferLine = this.lines - cursor.line - 1;
      const available = Math.min(cursorBufferLine, reversed.length - this.lines);
      const overflow = Math.max(0, cursorLineDelta - available);
      reversed.length = reversed.length + overflow - cursorLineDelta;
      cursor.line = Math.max(cursor.line - overflow, 0);
    }
    for (const row of reversed) if (row.len < columns) row.setLen(columns);
    this.raw.replaceInner(reversed.reverse());
    this.displayOffset = Math.min(this.displayOffset, this.historySize);
  }

  shrinkColumns(reflow, columns) {
    this.cols = columns;
    const cursor = this.cursor;
    if (cursor.wrap && reflow) { cursor.wrap = false; cursor.col += 1; }
    const newRaw = [];   // top first
    let buffered = null;
    const rows = this.raw.takeAll();
    for (let i = rows.length - 1; i >= 0; i--) {
      let row = rows[i];
      if (buffered) {
        if (i === this.lines - cursor.line - 1) cursor.col += buffered.len;
        row.appendFront(buffered);
        buffered = null;
      }
      for (;;) {
        let wrapped = row.shrink(columns);
        if (!(wrapped && reflow)) {
          if (reflow && i === this.lines - cursor.line - 1 && cursor.col > columns) {
            wrapped = Row.from(new Uint32Array(0), null, 0, 0);
          } else {
            newRaw.push(row);
            break;
          }
        }
        if (row.len >= columns && (row.flags(columns - 1) & WIDE_CHAR)) {
          const wide = row.slice(columns - 1, columns);
          row.touch(columns - 1);
          row.blank(columns - 1, DEFAULT_BG);
          row.setFlag(columns - 1, LEADING_WIDE_CHAR_SPACER, true);
          const j = wide.joined(wrapped);
          wrapped = Row.from(j.d, j.x, wrapped.len + 1, 0);
        }
        const len = wrapped.len;
        if (len > 0 && (wrapped.flags(len - 1) & LEADING_WIDE_CHAR_SPACER)) {
          if (len === 1) {
            row.touch(columns - 1);
            row.setFlag(columns - 1, WRAPLINE, true);
            newRaw.push(row);
            break;
          }
          wrapped.setFlag(len - 2, WRAPLINE, true);
          wrapped.setLen(len - 1);
        }
        newRaw.push(row);
        if (row.len > 0) { row.touchAll(); row.setFlag(row.len - 1, WRAPLINE, true); }
        if (wrapped.len > 0 && (wrapped.flags(wrapped.len - 1) & WRAPLINE) && i >= 1 && wrapped.len < columns) {
          wrapped.setFlag(wrapped.len - 1, WRAPLINE, false);
          buffered = wrapped;
          break;
        }
        const cursorBufferLine = this.lines - cursor.line - 1;
        if ((i === cursorBufferLine && cursor.col < columns) || i < cursorBufferLine) {
          cursor.line = Math.max(cursor.line - 1, 0);
        }
        if (i === cursorBufferLine && cursor.col >= columns) cursor.col -= columns;
        const occ = wrapped.len;
        if (occ < columns) wrapped.setLen(columns);
        wrapped.occ = occ;
        row = wrapped;
        if (i < this.displayOffset) this.displayOffset += 1;
      }
    }
    newRaw.reverse();
    if (newRaw.length > this.maxHistory + this.lines) newRaw.length = this.maxHistory + this.lines;
    this.raw.replaceInner(newRaw);
    this.displayOffset = Math.min(this.displayOffset, this.historySize);
    if (!reflow) {
      cursor.col = Math.min(cursor.col, columns - 1);
    } else if (cursor.col === columns && !(this.row(cursor.line).flags(columns - 1) & WRAPLINE)) {
      cursor.wrap = true;
      cursor.col -= 1;
    } else {
      cursor.col = Math.min(cursor.col, columns - 1);
      if (cursor.line < 0) { cursor.line = 0; cursor.col = 0; }
      else if (cursor.line > this.lines - 1) { cursor.line = this.lines - 1; cursor.col = columns - 1; }
    }
    this.saved.col = Math.min(this.saved.col, columns - 1);
  }
}
