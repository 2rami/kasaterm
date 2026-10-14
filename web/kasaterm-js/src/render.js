// Canvas renderer: paints a Terminal's visible lines onto one 2D canvas.
//
// Only lines whose cells, cursor, selection or link hover changed since the last frame are
// repainted, each clipped to its own band so an overflowing glyph cannot leave remnants on its
// neighbours. Box-drawing and block characters are drawn as shapes so lines join across cells
// whatever the font and line height.

const DEFAULT_THEME = {
  background: '#12161c', foreground: '#c8d0d9', cursor: '#7aa2f7', cursorAccent: '#12161c',
  selectionBackground: '#2c3543', selectionForeground: null,
  ansi: ['#12161c', '#f7768e', '#9ece6a', '#e0af68', '#7aa2f7', '#bb9af7', '#7dcfff', '#a9b1d6',
    '#414868', '#ff7a93', '#b9f27c', '#ff9e64', '#7da6ff', '#c0a3ff', '#0db9d7', '#c0caf5'],
};
const ANSI_NAMES = ['black', 'red', 'green', 'yellow', 'blue', 'magenta', 'cyan', 'white', 'brightBlack',
  'brightRed', 'brightGreen', 'brightYellow', 'brightBlue', 'brightMagenta', 'brightCyan', 'brightWhite'];
const DIM_FACTOR = 0.66;
const DEFAULT_FONT = '"KasaTerm", "D2Coding", ui-monospace, SFMono-Regular, Menlo, "Apple SD Gothic Neo", "Noto Sans Mono CJK KR", monospace';

function parseCssColor(css) {
  const m = /^#([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(css || '');
  if (m) {
    const h = m[1].length === 3 ? m[1].replace(/./g, c => c + c) : m[1];
    return [parseInt(h.slice(0, 2), 16), parseInt(h.slice(2, 4), 16), parseInt(h.slice(4, 6), 16)];
  }
  const r = /^rgba?\((\d+)[ ,]+(\d+)[ ,]+(\d+)/i.exec(css || '');
  return r ? [+r[1], +r[2], +r[3]] : null;
}

function normalizeTheme(theme = {}) {
  const t = { ...DEFAULT_THEME, ...theme };
  const ansi = Array.isArray(theme.ansi) && theme.ansi.length === 16 ? theme.ansi.slice() : DEFAULT_THEME.ansi.slice();
  ANSI_NAMES.forEach((name, i) => { if (theme[name]) ansi[i] = theme[name]; });
  t.ansi = ansi;
  return t;
}

// A theme from kasaterm's design tokens (`/design-tokens`, the same source KasaTermChrome uses).
function themeFromTokens(tokens) {
  const p = (tokens && tokens.palette) || {};
  const theme = {};
  if (p.bg) { theme.background = p.bg; theme.cursorAccent = p.bg; }
  if (p.fg) theme.foreground = p.fg;
  if (p.accent) theme.cursor = p.accent;
  if (p.surface_active) theme.selectionBackground = p.surface_active;
  if (tokens && Array.isArray(tokens.ansi) && tokens.ansi.length === 16) theme.ansi = tokens.ansi.slice();
  return theme;
}

function cube(i) {
  if (i < 232) { const n = i - 16, s = [0, 95, 135, 175, 215, 255]; return [s[Math.floor(n / 36)], s[Math.floor(n / 6) % 6], s[n % 6]]; }
  const v = 8 + (i - 232) * 10;
  return [v, v, v];
}

const isBoxChar = cp => (cp >= 0x2500 && cp <= 0x259f);

class Renderer {
  constructor(term, canvas, options = {}) {
    this.term = term;
    this.canvas = canvas;
    this.ctx = canvas.getContext('2d', { alpha: false });
    this.fontFamily = options.fontFamily || DEFAULT_FONT;
    this.fontSize = options.fontSize || 13;
    this.lineHeight = options.lineHeight || 1.2;
    this.setTheme(options.theme);
    this.lines = [];          // per visible row: signature of what was painted
    this.widthCache = new Map();
    this.measure();
  }

  setTheme(theme) {
    this.theme = normalizeTheme(theme);
    this.rgb = new Map();
    this.invalidate();
  }

  setFont(fontSize, fontFamily) {
    if (fontSize) this.fontSize = fontSize;
    if (fontFamily) this.fontFamily = fontFamily;
    this.widthCache.clear();
    this.measure();
  }

  invalidate() { this.lines = []; }

  font(bold, italic) { return `${italic ? 'italic ' : ''}${bold ? '700 ' : ''}${this.fontSize}px ${this.fontFamily}`; }

  measure() {
    const ctx = this.ctx;
    ctx.font = this.font(false, false);
    const m = ctx.measureText('M');
    const ascent = m.fontBoundingBoxAscent ?? m.actualBoundingBoxAscent ?? this.fontSize * 0.8;
    const descent = m.fontBoundingBoxDescent ?? m.actualBoundingBoxDescent ?? this.fontSize * 0.2;
    this.dpr = (typeof window !== 'undefined' && window.devicePixelRatio) || 1;
    this.cellW = m.width;
    this.cellH = Math.ceil((ascent + descent) * this.lineHeight * this.dpr) / this.dpr;
    this.baseline = Math.round(((this.cellH - (ascent + descent)) / 2 + ascent) * this.dpr) / this.dpr;
    this.invalidate();
  }

  // Canvas backing store sized to the grid in device pixels.
  resize() {
    const w = this.term.cols * this.cellW, h = this.term.rows * this.cellH;
    const pw = Math.ceil(w * this.dpr), ph = Math.ceil(h * this.dpr);
    if (this.canvas.width !== pw || this.canvas.height !== ph) {
      this.canvas.width = pw; this.canvas.height = ph;
    }
    this.canvas.style.width = `${pw / this.dpr}px`;
    this.canvas.style.height = `${ph / this.dpr}px`;
    this.invalidate();
  }

  snap(v) { return Math.round(v * this.dpr) / this.dpr; }

  // ── colours ──

  baseRgb(c) {
    const colors = this.term.colors, t = this.theme;
    if (c >= COLOR_RGB) return [(c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff];
    if (c >= COLOR_INDEXED) {
      const i = c & 0xff;
      if (colors[i]) return colors[i];
      return i < 16 ? parseCssColor(t.ansi[i]) : cube(i);
    }
    if (c < 16) return colors[c] || parseCssColor(t.ansi[c]);
    if (c === DEFAULT_FG || c === 267 || c === 268) return colors[256] || parseCssColor(t.foreground);
    if (c === DEFAULT_BG) return colors[257] || parseCssColor(t.background);
    if (c === 258) return colors[258] || parseCssColor(t.cursor);
    if (c >= 259 && c <= 266) return (colors[c - 259] || parseCssColor(t.ansi[c - 259])).map(v => Math.round(v * DIM_FACTOR));
    return parseCssColor(t.foreground);
  }

  css(c, dim) {
    const key = dim ? -1 - c : c;
    let v = this.rgb.get(key);
    if (!v) {
      let rgb = this.baseRgb(c) || [255, 0, 255];
      if (dim) rgb = rgb.map(x => Math.round(x * DIM_FACTOR));
      v = `rgb(${rgb[0]},${rgb[1]},${rgb[2]})`;
      this.rgb.set(key, v);
    }
    return v;
  }

  // Foreground and background CSS for a cell after inverse, hidden and selection.
  cellColors(fg, bg, fl, selected) {
    let f = this.css(fg, (fl & DIM) !== 0), b = this.css(bg, false);
    if (fl & INVERSE) { const t = f; f = b; b = t; }
    if (selected) {
      b = this.theme.selectionBackground;
      if (this.theme.selectionForeground) f = this.theme.selectionForeground;
    }
    if (fl & HIDDEN) f = b;
    return [f, b];
  }

  // ── painting ──

  // `view` supplies what the grid alone does not know: scroll offset, selection, cursor state
  // and the hovered link.
  paint(view) {
    const term = this.term, ctx = this.ctx, rows = term.rows, cols = term.cols;
    ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    ctx.textBaseline = 'alphabetic';
    const offset = term.grid.displayOffset;
    const cur = term.cursor;
    const cursorRow = cur.line + offset;
    for (let y = 0; y < rows; y++) {
      const line = y - offset;
      const row = term.line(line);
      const sel = view.selectionSpan(line, cols);
      const cursorHere = y === cursorRow ? view.cursorState() : '';
      const hover = view.hoverSpan(line);
      const below = view.images ? view.images.belowKey(y) : '';
      const prev = this.lines[y];
      if (prev && prev.row === row && prev.sel === sel && prev.cursor === cursorHere && prev.hover === hover
        && prev.below === below && sameCells(prev.d, row.d) && prev.x === extrasKey(row)) continue;
      this.paintRow(y, row, sel, cursorHere ? cur : null, cursorHere, hover, below ? view.images : null);
      this.lines[y] = { row, d: row.d.slice(), x: extrasKey(row), sel, cursor: cursorHere, hover, below };
    }
  }

  // `images` paints kitty z<0 pictures between the default background and the cells, as the app.
  paintRow(y, row, sel, cur, cursorState, hover, images) {
    const ctx = this.ctx, cols = this.term.cols, cw = this.cellW, top = y * this.cellH, h = this.cellH;
    const [selFrom, selTo] = sel ? sel.split(':').map(Number) : [-1, -1];
    // The band spans the whole backing store: its last device column is only partly covered by
    // the cells and would otherwise stay the canvas's initial black.
    const full = this.canvas.width / this.dpr;
    ctx.save();
    ctx.beginPath();
    ctx.rect(0, top, full, h);
    ctx.clip();
    ctx.fillStyle = this.css(DEFAULT_BG, false);
    ctx.fillRect(0, top, full, h);
    if (images) images.paintBelow(ctx, y);

    // Backgrounds, merged into runs.
    let runStart = 0, runColor = null;
    const flushBg = end => {
      if (runColor && runColor !== this.css(DEFAULT_BG, false)) {
        const x0 = this.snap(runStart * cw), x1 = this.snap(end * cw);
        ctx.fillStyle = runColor;
        ctx.fillRect(x0, top, x1 - x0, h);
      }
    };
    const n = Math.min(cols, row.len);
    for (let x = 0; x < n; x++) {
      const [, b] = this.cellColors(row.fg(x), row.bg(x), row.flags(x), x >= selFrom && x < selTo);
      if (b !== runColor) { flushBg(x); runStart = x; runColor = b; }
    }
    flushBg(n);

    // Text: plain ASCII of one style goes out as one string, everything else cell by cell.
    const base = top + this.baseline;
    let text = '', textX = 0, textEnd = -1, textStyle = null, textFont = null;
    const flushText = () => {
      if (!text) return;
      ctx.font = textFont; ctx.fillStyle = textStyle;
      ctx.fillText(text, textX, base);
      text = '';
    };
    for (let x = 0; x < n; x++) {
      const fl = row.flags(x);
      if (fl & (WIDE_CHAR_SPACER | LEADING_WIDE_CHAR_SPACER)) { flushText(); continue; }
      const cp = row.cp(x);
      const e = row.extra(x);
      const [f] = this.cellColors(row.fg(x), row.bg(x), fl, x >= selFrom && x < selTo);
      const isCursorCell = cur && cur.col === x && cursorState.startsWith('block');
      const fillColor = isCursorCell ? this.theme.cursorAccent : f;
      const fnt = this.font((fl & BOLD) !== 0, (fl & ITALIC) !== 0);
      if (cp === 32 || cp === 9 || (fl & HIDDEN)) { flushText(); continue; }
      const simple = cp > 32 && cp < 127 && !(e && e.zw);
      if (simple && !isCursorCell) {
        if (text && (fnt !== textFont || fillColor !== textStyle || textEnd !== x)) flushText();
        if (!text) { textX = x * cw; textFont = fnt; textStyle = fillColor; }
        text += String.fromCharCode(cp);
        textEnd = x + 1;
        continue;
      }
      flushText();
      if (isCursorCell) this.paintCursorBlock(x, top);
      const wide = (fl & WIDE_CHAR) !== 0;
      if (isBoxChar(cp) && !(e && e.zw)) { this.paintBox(cp, x * cw, top, wide ? 2 * cw : cw, h, fillColor); continue; }
      this.paintGlyph(row.text(x), x * cw, base, wide ? 2 : 1, fnt, fillColor);
    }
    flushText();

    // Decorations: underlines in their style and colour, strike-through, hovered link.
    for (let x = 0; x < n; x++) {
      const fl = row.flags(x);
      const e = row.extra(x);
      const linked = hover && (hover[0] === 'L' ? !!(e && e.link && hover === 'L' + linkKey(e.link))
        : x >= +hover.slice(1).split(':')[0] && x < +hover.split(':')[1]);
      if (!(fl & (ALL_UNDERLINES | STRIKEOUT)) && !linked) continue;
      const [f] = this.cellColors(row.fg(x), row.bg(x), fl, x >= selFrom && x < selTo);
      const ulColor = e && e.ul >= 0 ? this.css(e.ul, false) : f;
      const x0 = this.snap(x * cw), x1 = this.snap((x + 1) * cw);
      const thick = Math.max(1, Math.round(this.fontSize / 13 * this.dpr)) / this.dpr;
      // Below the baseline by half the descent: Hangul fallback glyphs sit lower than Latin ones.
      const uy = this.snap(top + Math.min(h - thick, this.baseline + Math.max(2, (h - this.baseline) * 0.45)));
      ctx.fillStyle = ulColor;
      if (fl & UNDERLINE || (linked && !(fl & ALL_UNDERLINES))) ctx.fillRect(x0, uy, x1 - x0, thick);
      else if (fl & DOUBLE_UNDERLINE) { ctx.fillRect(x0, uy - thick, x1 - x0, thick); ctx.fillRect(x0, uy + thick, x1 - x0, thick); }
      else if (fl & DOTTED_UNDERLINE) { for (let px = x0; px < x1; px += thick * 2) ctx.fillRect(px, uy, thick, thick); }
      else if (fl & DASHED_UNDERLINE) { const d = Math.max(2, cw / 3); for (let px = x0; px < x1; px += d * 1.5) ctx.fillRect(px, uy, Math.min(d, x1 - px), thick); }
      else if (fl & UNDERCURL) {
        ctx.strokeStyle = ulColor; ctx.lineWidth = thick;
        ctx.beginPath();
        const amp = Math.max(1, h / 16), cy = Math.min(uy, top + h - amp - thick);
        for (let px = 0; px <= cw; px += 1) ctx.lineTo(x0 + px, cy + Math.sin((x0 + px) / cw * Math.PI * 2) * amp);
        ctx.stroke();
      }
      if (fl & STRIKEOUT) { ctx.fillStyle = f; ctx.fillRect(x0, this.snap(top + h / 2), x1 - x0, thick); }
    }

    if (cur && !cursorState.startsWith('block')) this.paintCursorShape(cur.col, top, cursorState);
    ctx.restore();
  }

  paintGlyph(text, x, base, cells, font, color) {
    const ctx = this.ctx;
    ctx.font = font;
    ctx.fillStyle = color;
    const key = font + '\u0000' + text;
    let w = this.widthCache.get(key);
    if (w === undefined) { w = ctx.measureText(text).width; this.widthCache.set(key, w); }
    const room = cells * this.cellW;
    if (w > room * 1.05) {
      // A fallback glyph wider than its cells is squeezed rather than spilled over the next.
      ctx.save();
      ctx.translate(x, base);
      ctx.scale(room / w, 1);
      ctx.fillText(text, 0, 0);
      ctx.restore();
    } else {
      ctx.fillText(text, x + (cells > 1 ? (room - w) / 2 : 0), base);
    }
  }

  paintCursorBlock(x, top) {
    const ctx = this.ctx, x0 = this.snap(x * this.cellW), x1 = this.snap((x + 1) * this.cellW);
    ctx.fillStyle = this.css(258, false);
    ctx.fillRect(x0, top, x1 - x0, this.cellH);
  }

  paintCursorShape(x, top, state) {
    const ctx = this.ctx, cw = this.cellW, h = this.cellH;
    const x0 = this.snap(x * cw), x1 = this.snap((x + 1) * cw);
    const px = Math.max(1, Math.round(this.dpr)) / this.dpr;
    ctx.fillStyle = this.css(258, false);
    if (state.startsWith('hollow')) {
      ctx.strokeStyle = this.css(258, false);
      ctx.lineWidth = px;
      ctx.strokeRect(x0 + px / 2, top + px / 2, x1 - x0 - px, h - px);
    } else if (state.startsWith('underline')) {
      ctx.fillRect(x0, top + h - 2 * px, x1 - x0, 2 * px);
    } else if (state.startsWith('beam')) {
      ctx.fillRect(x0, top, 2 * px, h);
    }
  }

  // Lines from the cell centre to its edges, each reaching past the centre so joins close;
  // double lines are two light strokes.
  paintBox(cp, x, top, w, h, color) {
    const ctx = this.ctx;
    ctx.fillStyle = color;
    ctx.strokeStyle = color;
    if (cp >= 0x2580) { this.paintBlock(cp, x, top, w, h); return; }
    const light = Math.max(1, Math.round(this.fontSize / 13 * this.dpr)) / this.dpr;
    const heavy = light * 2;
    const cxm = x + w / 2, cym = top + h / 2;
    const spec = BOX_SEGMENTS[cp];
    if (!spec) {
      ctx.lineWidth = light;
      ctx.beginPath();
      if (cp === 0x2571 || cp === 0x2573) { ctx.moveTo(x + w, top); ctx.lineTo(x, top + h); }
      if (cp === 0x2572 || cp === 0x2573) { ctx.moveTo(x, top); ctx.lineTo(x + w, top + h); }
      ctx.stroke();
      return;
    }
    const [up, right, down, left] = [0, 1, 2, 3].map(i => +spec[i]);
    if (spec[4] === 'a') {
      ctx.lineWidth = light;
      ctx.beginPath();
      const r = Math.min(w, h) / 2, mx = this.snap(cxm), my = this.snap(cym);
      if (down && right) { ctx.moveTo(mx, top + h); ctx.arcTo(mx, my, x + w, my, r); ctx.lineTo(x + w, my); }
      if (down && left) { ctx.moveTo(mx, top + h); ctx.arcTo(mx, my, x, my, r); ctx.lineTo(x, my); }
      if (up && left) { ctx.moveTo(mx, top); ctx.arcTo(mx, my, x, my, r); ctx.lineTo(x, my); }
      if (up && right) { ctx.moveTo(mx, top); ctx.arcTo(mx, my, x + w, my, r); ctx.lineTo(x + w, my); }
      ctx.stroke();
      return;
    }
    const dashes = spec.length > 4 ? +spec[4] : 0;
    const reach = heavy / 2;
    const bands = weight => (weight === 3 ? [[-1.5 * light, light], [0.5 * light, light]]
      : [[-(weight === 2 ? heavy : light) / 2, weight === 2 ? heavy : light]]);
    const vertical = (y0, y1, weight) => {
      if (!weight) return;
      for (const [off, t] of bands(weight)) {
        const lx = this.snap(cxm + off);
        if (dashes) this.dashed(lx, y0, y1, t, dashes, true); else ctx.fillRect(lx, y0, t, y1 - y0);
      }
    };
    const horizontal = (x0, x1, weight) => {
      if (!weight) return;
      for (const [off, t] of bands(weight)) {
        const ly = this.snap(cym + off);
        if (dashes) this.dashed(x0, ly, x1, t, dashes, false); else ctx.fillRect(x0, ly, x1 - x0, t);
      }
    };
    vertical(top, cym + reach, up);
    vertical(cym - reach, top + h, down);
    horizontal(x, cxm + reach, left);
    horizontal(cxm - reach, x + w, right);
  }

  dashed(a, b, c, t, n, vertical) {
    const ctx = this.ctx;
    if (vertical) {
      const len = (c - b) / n;
      for (let i = 0; i < n; i++) ctx.fillRect(a, b + i * len, t, len * 0.6);
    } else {
      const len = (c - a) / n;
      for (let i = 0; i < n; i++) ctx.fillRect(a + i * len, b, len * 0.6, t);
    }
  }

  paintBlock(cp, x, top, w, h) {
    const ctx = this.ctx;
    const rect = (fx, fy, fw, fh) => {
      const x0 = this.snap(x + fx * w), y0 = this.snap(top + fy * h);
      ctx.fillRect(x0, y0, this.snap(x + (fx + fw) * w) - x0, this.snap(top + (fy + fh) * h) - y0);
    };
    if (cp === 0x2580) return rect(0, 0, 1, 0.5);
    if (cp >= 0x2581 && cp <= 0x2588) { const k = (cp - 0x2580) / 8; return rect(0, 1 - k, 1, k); }
    if (cp >= 0x2589 && cp <= 0x258f) { const k = (0x2590 - cp) / 8; return rect(0, 0, k, 1); }
    if (cp === 0x2590) return rect(0.5, 0, 0.5, 1);
    if (cp >= 0x2591 && cp <= 0x2593) {
      ctx.globalAlpha = (cp - 0x2590) / 4; rect(0, 0, 1, 1); ctx.globalAlpha = 1; return;
    }
    if (cp === 0x2594) return rect(0, 0, 1, 1 / 8);
    if (cp === 0x2595) return rect(7 / 8, 0, 1 / 8, 1);
    // Quadrants U+2596–U+259F: bits for upper-left, upper-right, lower-left, lower-right.
    const QUAD = { 0x2596: 4, 0x2597: 8, 0x2598: 1, 0x2599: 13, 0x259a: 9, 0x259b: 7, 0x259c: 11, 0x259d: 2, 0x259e: 6, 0x259f: 14 };
    const q = QUAD[cp] || 0;
    if (q & 1) rect(0, 0, 0.5, 0.5);
    if (q & 2) rect(0.5, 0, 0.5, 0.5);
    if (q & 4) rect(0, 0.5, 0.5, 0.5);
    if (q & 8) rect(0.5, 0.5, 0.5, 0.5);
  }
}

function sameCells(a, b) {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

function linkKey(link) { return link.id !== null ? `id:${link.id}:${link.uri}` : `n:${link.serial}`; }

function extrasKey(row) {
  if (!row.x) return '';
  let s = '';
  for (let i = 0; i < row.len; i++) {
    const e = row.x[i];
    if (e) s += `${i}:${e.zw}:${e.ul}:${e.link ? linkKey(e.link) : ''};`;
  }
  return s;
}
