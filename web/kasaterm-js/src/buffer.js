// xterm.js's read-only buffer API (`term.buffer.active.getLine(y).getCell(x)` …) over the grid,
// for code written against xterm.js — addons, search, tests. Lines count from the top of the
// scrollback, as there.
//
// A blank cell reads as ' ', where xterm.js tells a never-written cell ('') from a written
// space; trimming in translateToString drops trailing spaces of either kind.

let markerSerial = 0;

// xterm.js `IMarker`: a line that keeps its identity while the screen scrolls and reflows, and is
// disposed when that line leaves the buffer (trimmed, cleared, deleted, reset).
class Marker {
  constructor(term, grid, line) {
    this.id = ++markerSerial;
    this.term = term;
    this.grid = grid;
    this.row = grid.row(line);
    this.at = line;
    this.isDisposed = false;
    this.disposeListeners = [];
    (this.row.m || (this.row.m = [])).push(this);
    term.markers.add(this);
  }

  // Line from the top of the scrollback, as xterm.js counts; -1 once disposed.
  get line() {
    if (this.isDisposed) return -1;
    const g = this.grid, top = -g.historySize;
    if (this.at < top || this.at >= g.lines || g.row(this.at) !== this.row) {
      // Rows move by a few lines per scroll, so look outward from where it was last seen.
      let found = null;
      for (let d = 1; found === null && (this.at - d >= top || this.at + d < g.lines); d++) {
        if (this.at + d < g.lines && this.at + d >= top && g.row(this.at + d) === this.row) found = this.at + d;
        else if (this.at - d >= top && this.at - d < g.lines && g.row(this.at - d) === this.row) found = this.at - d;
      }
      if (found === null) { this.dispose(); return -1; }
      this.at = found;
    }
    return this.at - top;
  }

  onDispose(fn) {
    this.disposeListeners.push(fn);
    return { dispose: () => { const i = this.disposeListeners.indexOf(fn); if (i >= 0) this.disposeListeners.splice(i, 1); } };
  }

  dispose() {
    if (this.isDisposed) return;
    this.isDisposed = true;
    if (this.row.m) { this.row.m = this.row.m.filter(m => m !== this); if (!this.row.m.length) this.row.m = undefined; }
    this.term.markers.delete(this);
    for (const fn of this.disposeListeners.splice(0)) fn();
  }
}

const CM_P16 = 0x1000000, CM_P256 = 0x2000000, CM_RGB = 0x3000000;

class BufferView {
  constructor(grid, type) { this.grid = grid; this.type = type; }

  // xterm.js parks the cursor one past the last column while a wrap is pending.
  get cursorX() { const c = this.grid.cursor; return c.wrap ? c.col + 1 : c.col; }
  get cursorY() { return this.grid.cursor.line; }
  get baseY() { return this.grid.historySize; }
  get viewportY() { return this.grid.historySize - this.grid.displayOffset; }
  get length() { return this.grid.historySize + this.grid.lines; }

  getLine(y) {
    if (y < 0 || y >= this.length) return undefined;
    return new LineView(this.grid, y - this.grid.historySize);
  }

  getNullCell() { return new CellView(null, 0); }
}

class LineView {
  constructor(grid, line) { this.grid = grid; this.line = line; this.row = grid.row(line); }

  get isWrapped() {
    if (this.line - 1 < -this.grid.historySize) return false;
    const above = this.grid.row(this.line - 1);
    return (above.flags(above.len - 1) & WRAPLINE) !== 0;
  }

  get length() { return this.row.len; }

  getCell(x) { return x < 0 || x >= this.row.len ? undefined : new CellView(this.row, x); }

  translateToString(trimRight = false, start = 0, end = this.row.len) {
    let s = '';
    for (let x = Math.max(0, start); x < Math.min(end, this.row.len); x++) {
      if (this.row.flags(x) & WIDE_CHAR_SPACER) continue;
      s += this.row.cp(x) === 9 ? ' ' : this.row.text(x);
    }
    return trimRight ? s.replace(/ +$/, '') : s;
  }
}

function colorMode(c) {
  if (c === DEFAULT_FG || c === DEFAULT_BG) return 0;
  if (c >= COLOR_RGB) return CM_RGB;
  if (c >= COLOR_INDEXED) return CM_P256;
  return c < 16 ? CM_P16 : 0;
}

function colorValue(c) {
  const mode = colorMode(c);
  if (mode === 0) return -1;
  return mode === CM_P16 ? c : c & (mode === CM_RGB ? 0xffffff : 0xff);
}

class CellView {
  constructor(row, x) { this.row = row; this.x = x; }

  flag(bit) { return this.row ? this.row.flags(this.x) & bit : 0; }

  getWidth() { return this.flag(WIDE_CHAR) ? 2 : this.flag(WIDE_CHAR_SPACER) ? 0 : 1; }
  getChars() {
    if (!this.row || this.flag(WIDE_CHAR_SPACER)) return '';
    return this.row.cp(this.x) === 9 ? ' ' : this.row.text(this.x);
  }
  getCode() { return this.row && !this.flag(WIDE_CHAR_SPACER) ? this.row.cp(this.x) : 0; }

  getFgColorMode() { return this.row ? colorMode(this.row.fg(this.x)) : 0; }
  getBgColorMode() { return this.row ? colorMode(this.row.bg(this.x)) : 0; }
  getFgColor() { return this.row ? colorValue(this.row.fg(this.x)) : -1; }
  getBgColor() { return this.row ? colorValue(this.row.bg(this.x)) : -1; }
  isFgRGB() { return this.getFgColorMode() === CM_RGB; }
  isBgRGB() { return this.getBgColorMode() === CM_RGB; }
  isFgPalette() { const m = this.getFgColorMode(); return m === CM_P16 || m === CM_P256; }
  isBgPalette() { const m = this.getBgColorMode(); return m === CM_P16 || m === CM_P256; }
  isFgDefault() { return this.getFgColorMode() === 0; }
  isBgDefault() { return this.getBgColorMode() === 0; }

  isBold() { return this.flag(BOLD); }
  isDim() { return this.flag(DIM); }
  isItalic() { return this.flag(ITALIC); }
  isUnderline() { return this.flag(ALL_UNDERLINES); }
  isInverse() { return this.flag(INVERSE); }
  isInvisible() { return this.flag(HIDDEN); }
  isStrikethrough() { return this.flag(STRIKEOUT); }
  isBlink() { return this.flag(BLINK); }
  isOverline() { return this.flag(OVERLINE); }
  isAttributeDefault() {
    return this.flag(BOLD | DIM | ITALIC | ALL_UNDERLINES | INVERSE | HIDDEN | STRIKEOUT | BLINK | OVERLINE) === 0
      && this.isFgDefault() && this.isBgDefault();
  }
}
