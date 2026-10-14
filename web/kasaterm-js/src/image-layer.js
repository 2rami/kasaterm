// Inline pictures on their own canvas above the text, placed as the app's GPU renderer places
// them (kasa-gridview images.rs, kasa-pty inline.rs / kitty.rs):
// - iTerm (OSC 1337): centred in its cell box, never larger than the picture itself. It is
//   dropped once the text of the line it hangs on changes — the app compares the first 64
//   characters, trimmed, at every frame, so a picture a program has drawn over does not linger.
// - kitty, anchored: fitted to its box keeping the aspect ratio, enlarged if smaller.
// - kitty, Unicode placeholders (U+10EEEE): each run of placeholder cells shows its slice of
//   the picture's box; image id from the foreground colour and the third diacritic, placement
//   id from the underline colour, row and column from the first two diacritics.
// - sixel (xterm profile only): pixel for pixel from the cell box's corner.
// Pictures are decoded once into ImageBitmaps; kitty pictures are shared by image id.

const KITTY_PLACEHOLDER = 0x10eeee;
// kitty's rowcolumn-diacritics.txt — the index is the number.
const KITTY_DIACRITICS = [
  0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A, 0x034B, 0x034C, 0x0350, 0x0351,
  0x0352, 0x0357, 0x035B, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369, 0x036A, 0x036B, 0x036C, 0x036D,
  0x036E, 0x036F, 0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592, 0x0593, 0x0594, 0x0595, 0x0597, 0x0598, 0x0599,
  0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1, 0x05A8, 0x05A9, 0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611,
  0x0612, 0x0613, 0x0614, 0x0615, 0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6,
  0x06D7, 0x06D8, 0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2, 0x06E4, 0x06E7, 0x06E8, 0x06EB,
  0x06EC, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736, 0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743, 0x0745, 0x0747,
  0x0749, 0x074A, 0x07EB, 0x07EC, 0x07ED, 0x07EE, 0x07EF, 0x07F0, 0x07F1, 0x07F3, 0x0816, 0x0817, 0x0818, 0x0819,
  0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822, 0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A,
  0x082B, 0x082C, 0x082D, 0x0951, 0x0953, 0x0954, 0x0F82, 0x0F83, 0x0F86, 0x0F87, 0x135D, 0x135E, 0x135F, 0x17DD,
  0x193A, 0x1A17, 0x1A75, 0x1A76, 0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C, 0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F,
  0x1B70, 0x1B71, 0x1B72, 0x1B73, 0x1CD0, 0x1CD1, 0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4,
  0x1DC5, 0x1DC6, 0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5, 0x1DD6, 0x1DD7,
  0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF, 0x1DE0, 0x1DE1, 0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5,
  0x1DE6, 0x1DFE, 0x20D0, 0x20D1, 0x20D4, 0x20D5, 0x20D6, 0x20D7, 0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0,
  0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2, 0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6, 0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA,
  0x2DEB, 0x2DEC, 0x2DED, 0x2DEE, 0x2DEF, 0x2DF0, 0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8,
  0x2DF9, 0x2DFA, 0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1, 0xA8E0, 0xA8E1,
  0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9, 0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED, 0xA8EE, 0xA8EF,
  0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2, 0xAAB3, 0xAAB7, 0xAAB8, 0xAABE, 0xAABF, 0xAAC1, 0xFE20, 0xFE21, 0xFE22, 0xFE23,
  0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38, 0x1D185, 0x1D186, 0x1D187, 0x1D188, 0x1D189, 0x1D1AA, 0x1D1AB, 0x1D1AC,
  0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
];
const KITTY_DIACRITIC_INDEX = new Map(KITTY_DIACRITICS.map((cp, i) => [cp, i]));

// kitty takes an id from a 256-colour or true-colour value; anything else is 0.
function kittyColorId(c) {
  if (c >= COLOR_RGB) return c & 0xffffff;
  if (c >= COLOR_INDEXED) return c & 0xff;
  return 0;
}

function sameBytes(a, b) {
  if (a === b) return true;
  if (!a || !b || a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

// The first 64 characters of a line, trimmed — kasa-pty's grid_line_sig.
function lineSignature(term, y) {
  if (y < -term.historySize || y >= term.rows) return '';
  const row = term.line(y);
  let s = '';
  for (let x = 0; x < Math.min(64, term.cols); x++) s += String.fromCodePoint(row.cp(x));
  return s.trim();
}

class ImageLayer {
  constructor(view, canvas) {
    this.view = view;
    this.term = view.term;
    this.canvas = canvas;
    this.ctx = canvas.getContext('2d');
    this.placements = new Map();   // event id -> {e, sig}
    this.bitmaps = new Map();      // decode key -> {src, img: {bitmap, width, height} | null}
    this.drawn = false;
    this.ops = [];
  }

  get empty() { return this.placements.size === 0; }

  bitmapKey(e) { return e.protocol === 'kitty' ? `kitty:${e.imageId}` : `${e.protocol}:${e.id}`; }

  add(e) {
    this.placements.set(e.id, { e, sig: undefined });
    const key = this.bitmapKey(e), cached = this.bitmaps.get(key);
    // A picture moved (same id again) or placed twice keeps its pixels; new bytes are re-decoded.
    if (!cached || !sameBytes(cached.src, e.data)) {
      if (cached && cached.img) cached.img.bitmap.close();
      const entry = { src: e.data, img: null };
      this.bitmaps.set(key, entry);
      this.decode(e).then(img => {
        if (this.bitmaps.get(key) === entry) entry.img = img; else img.bitmap.close();
        this.view.schedule();
      }, () => {});
    }
    this.view.schedule();
  }

  remove(ids) {
    for (const id of ids) {
      const p = this.placements.get(id);
      if (!p) continue;
      this.placements.delete(id);
      const key = this.bitmapKey(p.e);
      if (![...this.placements.values()].some(q => this.bitmapKey(q.e) === key)) this.forget(key);
    }
    this.view.schedule();
  }

  forget(key) {
    const entry = this.bitmaps.get(key);
    if (entry && entry.img) entry.img.bitmap.close();
    this.bitmaps.delete(key);
  }

  clear() {
    for (const key of [...this.bitmaps.keys()]) this.forget(key);
    this.placements.clear();
    this.view.schedule();
  }

  image(e) {
    const entry = this.bitmaps.get(this.bitmapKey(e));
    return entry ? entry.img : null;
  }

  async decode(e) {
    let bitmap;
    if (e.protocol === 'iterm' || (e.protocol === 'kitty' && e.format === 'png')) {
      bitmap = await createImageBitmap(new Blob([e.data], { type: e.mime || 'image/png' }));
    } else {
      let px = e.data;
      if (e.compressed) {
        const stream = new Blob([px]).stream().pipeThrough(new DecompressionStream('deflate'));
        px = new Uint8Array(await new Response(stream).arrayBuffer());
      }
      const w = e.width, h = e.height, rgba = new Uint8ClampedArray(w * h * 4);
      if (e.format === 'rgb') {
        for (let i = 0, j = 0; i < w * h; i++, j += 3) {
          rgba[i * 4] = px[j]; rgba[i * 4 + 1] = px[j + 1]; rgba[i * 4 + 2] = px[j + 2]; rgba[i * 4 + 3] = 255;
        }
      } else {
        rgba.set(px.subarray(0, w * h * 4));
      }
      bitmap = await createImageBitmap(new ImageData(rgba, w, h));
    }
    return { bitmap, width: bitmap.width, height: bitmap.height };
  }

  resize(width, height) {
    if (this.canvas.width !== width || this.canvas.height !== height) {
      this.canvas.width = width;
      this.canvas.height = height;
    }
  }

  // This frame's pictures as draw operations in screen coordinates. kitty z<0 goes below the
  // text — the app draws it after the pane background and before the cells, so it sits under
  // cell backgrounds and glyphs — and is painted by the renderer inside each line; everything
  // else goes on the canvas above. Runs before the text renderer each frame.
  layout() {
    const term = this.term, r = this.view.renderer;
    this.ops = [];
    if (this.empty) return;
    // iTerm pictures whose line was rewritten are gone (the app's row signature).
    for (const [id, p] of this.placements) {
      if (p.e.protocol !== 'iterm') continue;
      const line = p.e.marker ? p.e.marker.line : -1;
      if (line < 0) continue;
      const sig = lineSignature(term, line - term.historySize);
      if (p.sig === undefined) p.sig = sig;
      else if (p.sig !== sig) this.remove([id]);
    }
    const cw = r.cellW, ch = r.cellH, offset = term.grid.displayOffset, gridH = term.rows * ch;
    const list = [...this.placements.values()].map(p => p.e).sort((a, b) => (a.z || 0) - (b.z || 0));
    for (const e of list) {
      if (e.virtual) continue;
      const line = e.marker ? e.marker.line : -1;
      if (line < 0) continue;
      const img = this.image(e);
      if (!img) continue;
      const row = line - term.historySize + offset;
      const box = { x: e.col * cw, y: row * ch, w: e.cols * cw, h: e.rows * ch };
      if (box.y + box.h <= 0 || box.y >= gridH) continue;
      if (e.protocol === 'iterm') this.ops.push({ e, img, box, clip: { x: box.x, y: 0, w: box.w, h: gridH }, grow: false, below: false });
      else if (e.protocol === 'kitty') this.ops.push({ e, img, box, clip: box, grow: true, below: (e.z || 0) < 0 });
      else this.ops.push({ e, img, box, sixel: true, below: false });
    }
    this.layoutPlaceholders(cw, ch, offset);
  }

  paint() {
    const ctx = this.ctx, r = this.view.renderer;
    const above = (this.ops || []).filter(op => !op.below);
    if (!above.length) {
      if (this.drawn) { ctx.setTransform(1, 0, 0, 1, 0, 0); ctx.clearRect(0, 0, this.canvas.width, this.canvas.height); this.drawn = false; }
      return;
    }
    ctx.setTransform(r.dpr, 0, 0, r.dpr, 0, 0);
    ctx.clearRect(0, 0, this.canvas.width / r.dpr, this.canvas.height / r.dpr);
    this.drawn = true;
    for (const op of above) this.draw(ctx, op);
  }

  // Below-text pictures crossing screen row `y`, for the renderer to paint in that line's band.
  belowAt(y) {
    if (!this.ops) return null;
    const ch = this.view.renderer.cellH, top = y * ch, bottom = top + ch;
    let out = null;
    for (const op of this.ops) {
      if (!op.below) continue;
      const y0 = Math.max(op.clip.y, op.box.y), y1 = Math.min(op.clip.y + op.clip.h, op.box.y + op.box.h);
      if (y1 <= top || y0 >= bottom) continue;
      (out || (out = [])).push(op);
    }
    return out;
  }

  // What the renderer compares to know a line must be repainted for its below-text pictures.
  belowKey(y) {
    const ops = this.belowAt(y);
    if (!ops) return '';
    return ops.map(op => `${op.e.id}:${op.box.x},${op.box.y},${op.box.w},${op.box.h}:${op.clip.x},${op.clip.y},${op.clip.w}`).join(';');
  }

  paintBelow(ctx, y) {
    const ops = this.belowAt(y);
    if (ops) for (const op of ops) this.draw(ctx, op);
  }

  draw(ctx, op) {
    if (op.sixel) this.drawSixel(ctx, op.e, op.img, op.box);
    else this.drawFit(ctx, op.img, op.box, op.clip, op.grow);
  }

  // Centred in the box; `grow` lets a small picture fill it, otherwise it stops at its own size.
  // The app caps at native size in physical pixels, so "its own size" is measured in the app's
  // cells (term.cellPixels, device pixels) and drawn in ours; without that, in device pixels.
  drawFit(ctx, img, box, clip, grow) {
    const r = this.view.renderer, cell = this.term.cellPixels;
    let scale = Math.min(box.w / img.width, box.h / img.height);
    if (!grow) scale = Math.min(scale, cell ? Math.min(r.cellW / cell.width, r.cellH / cell.height) : 1 / r.dpr);
    const w = img.width * scale, h = img.height * scale;
    ctx.save();
    ctx.beginPath();
    const gridH = this.term.rows * r.cellH;
    ctx.rect(clip.x, Math.max(clip.y, 0), clip.w, Math.min(clip.y + clip.h, gridH) - Math.max(clip.y, 0));
    ctx.clip();
    ctx.drawImage(img.bitmap, box.x + (box.w - w) / 2, box.y + (box.h - h) / 2, w, h);
    ctx.restore();
  }

  drawSixel(ctx, e, img, box) {
    const r = this.view.renderer, cell = this.term.cellPixels || { width: 8, height: 16 };
    const sx = r.cellW / cell.width, sy = r.cellH / cell.height;
    if (!e.transparent) {
      ctx.fillStyle = r.css(e.background, false);
      ctx.fillRect(box.x, box.y, img.width * sx, img.height * sy);
    }
    ctx.drawImage(img.bitmap, box.x, box.y, img.width * sx, img.height * sy);
  }

  // kitty Unicode placeholders: runs of U+10EEEE cells, each showing its tile of the box.
  layoutPlaceholders(cw, ch, offset) {
    const term = this.term;
    const virtual = [...this.placements.values()].map(p => p.e).filter(e => e.virtual);
    if (!virtual.length) return;
    for (let vr = 0; vr < term.rows; vr++) {
      const row = term.line(vr - offset);
      let left = null, run = null;
      const flush = () => {
        if (!run) return;
        const candidates = virtual.filter(e => (e.imageId >>> 0) === (run.id >>> 0));
        const e = candidates.find(v => v.placementId === run.pid) || candidates[0];
        const img = e && run.col >= run.imgCol ? this.image(e) : null;
        if (img) {
          const box = { x: (run.col - run.imgCol) * cw, y: (vr - run.imgRow) * ch, w: e.cols * cw, h: e.rows * ch };
          this.ops.push({ e, img, box, clip: { x: run.col * cw, y: vr * ch, w: run.len * cw, h: ch }, grow: true, below: (e.z || 0) < 0 });
        }
        run = null;
      };
      for (let c = 0; c <= term.cols; c++) {
        const mark = c < term.cols && row.cp(c) === KITTY_PLACEHOLDER ? this.readMark(row, c, left) : null;
        left = mark;
        if (run && mark && run.id === mark.id && run.pid === mark.placement && run.imgRow === mark.row
          && run.imgCol + run.len === mark.col) { run.len++; continue; }
        flush();
        if (mark) run = { id: mark.id, pid: mark.placement, imgRow: mark.row, imgCol: mark.col, col: c, len: 1 };
      }
    }
  }

  // kasa-pty kitty.rs read_mark: missing diacritics continue from the cell on the left.
  readMark(row, x, left) {
    const e = row.extra(x);
    const idLow = kittyColorId(row.fg(x)), placement = e && e.ul >= 0 ? kittyColorId(e.ul) : 0;
    const zw = e && e.zw ? [...e.zw].map(ch => KITTY_DIACRITIC_INDEX.get(ch.codePointAt(0))) : [];
    const d = i => zw[i];
    const same = left && left.idLow === idLow && left.placement === placement ? left : null;
    let r, c, msb;
    if (d(0) === undefined) {
      if (same) { r = same.row; c = same.col + 1; msb = same.msb; } else { r = 0; c = 0; msb = 0; }
    } else if (d(1) === undefined) {
      if (same && same.row === d(0)) { r = d(0); c = same.col + 1; msb = same.msb; } else { r = d(0); c = 0; msb = 0; }
    } else if (d(2) === undefined) {
      if (same && same.row === d(0) && same.col + 1 === d(1)) { r = d(0); c = d(1); msb = same.msb; } else { r = d(0); c = d(1); msb = 0; }
    } else {
      r = d(0); c = d(1); msb = d(2);
    }
    return { idLow, placement, row: r, col: c, msb, id: ((msb << 24) | idLow) >>> 0 };
  }
}
