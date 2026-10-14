// Inline pictures as the parser sees them: sixel (DCS q), iTerm (OSC 1337 File=) and kitty
// (APC G) arrive as commands; what comes out is the decoded picture and where it sits, as
// 'image' / 'imagedelete' events for the view to draw. Where the cursor ends up is part of the
// screen, so it is decided here — iTerm and kitty exactly as the app (kasa-pty's inline.rs and
// kitty.rs), sixel as xterm.js's image addon (the app does not show sixel).
//
// Also XTGETTCAP's capability table, which answers from the same xterm-256color description
// programs are given in TERM.

const IMAGE_SLOTS = 64;                 // kasa-pty INLINE_IMAGE_SLOTS
const IMAGE_PIXEL_BUDGET = 64000000;    // kasa-pty INLINE_IMAGE_PIXEL_BUDGET
const KITTY_MAX_BYTES = 64 * 1024 * 1024;
const KITTY_MAX_SIDE = 10000;
const KITTY_MAX_BOX_CELLS = 1000;
const SIXEL_PIXEL_LIMIT = 16777216;     // xterm.js image addon pixelLimit
const SIXEL_PALETTE_LIMIT = 4096;
const DEFAULT_CELL = { width: 8, height: 16 };

// Lenient base64, as kasa-pty decodes inline pictures: anything outside the alphabet is skipped.
const B64 = (() => {
  const t = new Int8Array(256).fill(-1);
  const a = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
  for (let i = 0; i < 64; i++) t[a.charCodeAt(i)] = i;
  return t;
})();

function base64Bytes(src) {
  const out = new Uint8Array(Math.floor(src.length * 3 / 4) + 3);
  let n = 0, acc = 0, bits = 0;
  for (let i = 0; i < src.length; i++) {
    const v = B64[typeof src === 'string' ? src.charCodeAt(i) & 0xff : src[i]];
    if (v < 0) continue;
    acc = ((acc << 6) | v) & 0xffffff;
    bits += 6;
    if (bits >= 8) { bits -= 8; out[n++] = (acc >> bits) & 0xff; }
  }
  return out.subarray(0, n);
}

function pngSize(b) {
  if (b.length < 24 || b[0] !== 0x89 || b[1] !== 0x50 || b[2] !== 0x4e || b[3] !== 0x47) return null;
  const u32 = o => ((b[o] << 24) | (b[o + 1] << 16) | (b[o + 2] << 8) | b[o + 3]) >>> 0;
  return { width: u32(16), height: u32(20) };
}

function imageMime(b) {
  if (pngSize(b)) return 'image/png';
  if (b[0] === 0xff && b[1] === 0xd8) return 'image/jpeg';
  if (b[0] === 0x47 && b[1] === 0x49 && b[2] === 0x46) return 'image/gif';
  if (b[0] === 0x52 && b[1] === 0x49 && b[2] === 0x46 && b[3] === 0x46) return 'image/webp';
  return 'application/octet-stream';
}

// ── sixel ──

// The VT340's power-on palette, in percent.
const VT340_PALETTE = [
  [0, 0, 0], [20, 20, 80], [80, 13, 13], [20, 80, 20], [80, 20, 80], [20, 80, 80], [80, 80, 20], [53, 53, 53],
  [26, 26, 26], [33, 33, 60], [60, 26, 26], [33, 60, 33], [60, 33, 60], [33, 60, 60], [60, 60, 33], [80, 80, 80],
];

const pct = v => Math.round(Math.min(100, v) * 255 / 100);
const rgba = (r, g, b) => ((r & 0xff) | ((g & 0xff) << 8) | ((b & 0xff) << 16) | 0xff000000) >>> 0;

// DEC HLS puts blue at 0°, red at 120° and green at 240°.
function hlsColor(h, l, s) {
  const hue = ((h + 240) % 360) / 360, lig = Math.min(100, l) / 100, sat = Math.min(100, s) / 100;
  if (sat === 0) { const v = Math.round(lig * 255); return rgba(v, v, v); }
  const q = lig < 0.5 ? lig * (1 + sat) : lig + sat - lig * sat, p = 2 * lig - q;
  const ch = t => {
    t = (t + 1) % 1;
    const v = t < 1 / 6 ? p + (q - p) * 6 * t : t < 1 / 2 ? q : t < 2 / 3 ? p + (q - p) * (2 / 3 - t) * 6 : p;
    return Math.round(v * 255);
  };
  return rgba(ch(hue + 1 / 3), ch(hue), ch(hue - 1 / 3));
}

// Sixel data (the bytes between `q` and ST) → { width, height, pixels: RGBA }, or null when it
// draws nothing or is larger than the pixel limit. Unpainted pixels stay transparent; whether
// the background shows through is the DCS's second parameter, passed on to the view.
function decodeSixel(data, paletteLimit = SIXEL_PALETTE_LIMIT, pixelLimit = SIXEL_PIXEL_LIMIT) {
  const palette = new Uint32Array(paletteLimit);
  VT340_PALETTE.forEach(([r, g, b], i) => { if (i < paletteLimit) palette[i] = rgba(pct(r), pct(g), pct(b)); });
  const rows = [];
  let x = 0, band = 0, color = palette[0], declaredW = 0, declaredH = 0, maxX = -1, maxY = -1, over = false;
  const numbers = (i) => {
    const out = [];
    let v = 0, has = false;
    for (; i < data.length; i++) {
      const c = data[i];
      if (c >= 0x30 && c <= 0x39) { v = Math.min(v * 10 + c - 0x30, 0xffffff); has = true; } else if (c === 0x3b) { out.push(has ? v : 0); v = 0; has = false; } else break;
    }
    out.push(has ? v : 0);
    return [out, i];
  };
  const paint = (bits, count) => {
    if (bits) {
      for (let k = 0; k < 6; k++) {
        if (!(bits & (1 << k))) continue;
        const y = band * 6 + k;
        if ((x + count) * (y + 1) > pixelLimit) { over = true; return; }
        let row = rows[y];
        if (!row || row.length < x + count) {
          const grown = new Uint32Array(Math.max(x + count, row ? row.length * 2 : 64));
          if (row) grown.set(row);
          row = rows[y] = grown;
        }
        row.fill(color, x, x + count);
        if (y > maxY) maxY = y;
      }
      if (x + count - 1 > maxX) maxX = x + count - 1;
    }
    x += count;
  };
  for (let i = 0; i < data.length && !over;) {
    const c = data[i];
    if (c >= 0x3f && c <= 0x7e) { paint(c - 0x3f, 1); i++; continue; }
    if (c === 0x21) {
      const [[n], j] = numbers(i + 1);
      i = j;
      if (i < data.length && data[i] >= 0x3f && data[i] <= 0x7e) { paint(data[i] - 0x3f, Math.max(1, n)); i++; }
      continue;
    }
    if (c === 0x23) {
      const [p, j] = numbers(i + 1);
      i = j;
      const reg = p[0] % paletteLimit;
      if (p.length >= 5) {
        if (p[1] === 1) palette[reg] = hlsColor(p[2], p[3], p[4]);
        else if (p[1] === 2) palette[reg] = rgba(pct(p[2]), pct(p[3]), pct(p[4]));
      }
      color = palette[reg];
      continue;
    }
    if (c === 0x22) {
      const [p, j] = numbers(i + 1);
      i = j;
      if (p.length >= 4) { declaredW = p[2]; declaredH = p[3]; }
      continue;
    }
    if (c === 0x24) x = 0;
    else if (c === 0x2d) { x = 0; band++; }
    i++;
  }
  const width = Math.max(maxX + 1, declaredW), height = Math.max(maxY + 1, declaredH);
  if (width <= 0 || height <= 0 || width * height > pixelLimit) return null;
  const pixels = new Uint8ClampedArray(width * height * 4);
  const view = new Uint32Array(pixels.buffer);
  for (let y = 0; y < height; y++) {
    const row = rows[y];
    if (row) view.set(row.length > width ? row.subarray(0, width) : row, y * width);
  }
  return { width, height, pixels };
}

// ── kitty graphics (kasa-pty kitty.rs) ──

const KITTY_KEYS = { a: 'action', q: 'quiet', i: 'id', I: 'number', p: 'placement', f: 'format', t: 'medium', o: 'compressed',
  s: 'width', v: 'height', S: 'size', O: 'offset', m: 'more', c: 'cols', r: 'rows', x: 'x', y: 'y', w: 'w', h: 'h',
  C: 'noMove', U: 'virtual', d: 'delete', z: 'z', P: 'parent' };

function kittyCommand(control) {
  const cmd = { action: null, quiet: 0, id: 0, number: 0, placement: 0, format: 0, medium: null, compressed: false,
    width: 0, height: 0, size: 0, offset: 0, more: false, cols: 0, rows: 0, x: 0, y: 0, w: 0, h: 0,
    noMove: false, virtual: false, delete: null, z: 0, parent: 0 };
  for (const kv of control.split(',')) {
    const eq = kv.indexOf('=');
    if (eq !== 1) continue;
    const name = KITTY_KEYS[kv[0]], v = kv.slice(2);
    if (!name) continue;
    const num = /^[0-9]+$/.test(v) ? Math.min(Number(v), 0xffffffff) : 0;
    if (name === 'action' || name === 'medium' || name === 'delete') cmd[name] = v ? v[0] : null;
    else if (name === 'compressed') cmd.compressed = v[0] === 'z';
    else if (name === 'more' || name === 'noMove' || name === 'virtual') cmd[name] = v[0] === '1';
    else if (name === 'z') cmd.z = /^-?[0-9]+$/.test(v) ? Number(v) | 0 : 0;
    else if (name === 'quiet') cmd.quiet = num & 0xff;
    else cmd[name] = num;
  }
  return cmd;
}

function kittyReply(cmd, id, number, placement, msg) {
  const ok = msg === 'OK';
  if ((id === 0 && number === 0) || cmd.quiet >= 2 || (ok && cmd.quiet === 1)) return null;
  let keys = id ? `i=${id}` : '';
  if (number) keys += (keys ? ',' : '') + `I=${number}`;
  if (placement) keys += `,p=${placement}`;
  return `\x1b_G${keys};${msg}\x1b\\`;
}

// Cells a placement covers: both given, one given (the other from the picture's shape), or the
// picture's pixels over the cell size.
function kittyBox(cmd, width, height, cell) {
  const sw = Math.max(1, cmd.w > 0 ? Math.min(cmd.w, width) : width), sh = Math.max(1, cmd.h > 0 ? Math.min(cmd.h, height) : height);
  const cw = cell.width, ch = cell.height;
  let cols, rows;
  if (!cmd.cols && !cmd.rows) { cols = Math.ceil(sw / cw); rows = Math.ceil(sh / ch); }
  else if (!cmd.rows) { cols = cmd.cols; rows = Math.ceil(cmd.cols * cw * sh / sw / ch); }
  else if (!cmd.cols) { cols = Math.ceil(cmd.rows * ch * sw / sh / cw); rows = cmd.rows; }
  else { cols = cmd.cols; rows = cmd.rows; }
  const clamp = v => Math.max(1, Math.min(KITTY_MAX_BOX_CELLS, v));
  return [clamp(cols), clamp(rows)];
}

// Pixel data in the browser: direct (base64) transmission only — files and shared memory are
// on the app's machine. A zlib-compressed payload is handed on for the view to inflate.
function kittyLoad(cmd, payload) {
  if ((cmd.medium || 'd') !== 'd') return { error: 'EBADF:Failed to read image file' };
  const data = base64Bytes(payload);
  const format = cmd.format || 32;
  if (format === 100) {
    const size = cmd.compressed ? null : pngSize(data);
    if (!size || !size.width || !size.height) return { error: 'EBADPNG:not a PNG image' };
    if (size.width > KITTY_MAX_SIDE || size.height > KITTY_MAX_SIDE) return { error: 'EFBIG:image dimensions too large' };
    return { image: { format: 'png', data, width: size.width, height: size.height, compressed: false } };
  }
  if (format === 24 || format === 32) {
    const { width, height } = cmd;
    if (!width || !height) return { error: 'EINVAL:s and v are required for raw pixel data' };
    if (width > KITTY_MAX_SIDE || height > KITTY_MAX_SIDE) return { error: 'EFBIG:image dimensions too large' };
    if (!cmd.compressed && data.length < width * height * (format / 8)) return { error: 'ENODATA:insufficient image data' };
    return { image: { format: format === 32 ? 'rgba' : 'rgb', data, width, height, compressed: cmd.compressed } };
  }
  return { error: 'EINVAL:unknown image format' };
}

class KittyGraphics {
  constructor() {
    this.images = [];       // {id, number, uid, width, height, format, data, compressed}
    this.placements = [];   // {uid, image, id, cols, rows, virtual, marker, col, z}; marker null if virtual
    this.pending = null;
    this.nextAutoId = 0;
  }

  image(id) { return this.images.find(im => im.id === id); }

  newestNumbered(number) {
    let best = null;
    for (const im of this.images) if (im.number === number && (!best || im.uid > best.uid)) best = im;
    return best ? best.id : null;
  }

  freshId() {
    for (;;) {
      this.nextAutoId = (this.nextAutoId + 1) >>> 0;
      const id = (0x80000000 | this.nextAutoId) >>> 0;
      if (!this.image(id)) return id;
    }
  }

  // One APC body (after `G`). `at` is {line (buffer line), col}; returns {reply, cursor, placed,
  // removed placements} — `cursor` is [lines down, column] for the caller to move by. `env`
  // carries the grid's width and scrollback, the cell size, `mark(line)` for anchors and
  // `nextUid()`.
  handle(body, at, env) {
    const semi = body.indexOf(';');
    const control = semi < 0 ? body : body.slice(0, semi), payload = semi < 0 ? '' : body.slice(semi + 1);
    let cmd = kittyCommand(control), data;
    const out = { reply: null, cursor: null, placed: null, removed: [] };
    if (cmd.action === 'd') return this.remove(cmd, at, env, out);
    if (cmd.action !== null) this.pending = null;
    if (this.pending) {
      const p = this.pending;
      this.pending = null;
      if (p.data.length + payload.length > KITTY_MAX_BYTES * 4 / 3 + 4) {
        out.reply = kittyReply(p.cmd, p.cmd.id, p.cmd.number, 0, 'EFBIG:image too large');
        return out;
      }
      p.data += payload;
      if (cmd.more) { this.pending = p; return out; }
      if (cmd.quiet) p.cmd.quiet = cmd.quiet;
      cmd = p.cmd; at = p.at; data = p.data;
    } else {
      const action = cmd.action || 't';
      if (cmd.more && (action === 't' || action === 'T' || action === 'q') && (cmd.medium || 'd') === 'd') {
        this.pending = { cmd, at, data: payload };
        return out;
      }
      data = payload;
    }
    if (cmd.id && cmd.number) { out.reply = kittyReply(cmd, cmd.id, cmd.number, 0, 'EINVAL:i and I are mutually exclusive'); return out; }
    const action = cmd.action || 't';
    if (action === 'q') {
      const loaded = kittyLoad(cmd, data);
      out.reply = kittyReply(cmd, cmd.id, cmd.number, 0, loaded.error || 'OK');
      return out;
    }
    if (action === 't' || action === 'T') {
      const loaded = kittyLoad(cmd, data);
      if (loaded.error) { out.reply = kittyReply(cmd, cmd.id, cmd.number, 0, loaded.error); return out; }
      const id = cmd.id || this.freshId();
      const image = { ...loaded.image, id, number: cmd.number, uid: env.nextUid() };
      const i = this.images.findIndex(im => im.id === id);
      if (i >= 0) this.images[i] = image; else this.images.push(image);
      if (action === 'T') return this.put(cmd, id, at, env, out);
      out.reply = kittyReply(cmd, id, cmd.number, 0, 'OK');
      return out;
    }
    if (action === 'p') {
      const id = cmd.id || this.newestNumbered(cmd.number);
      if (id !== null && this.image(id)) return this.put(cmd, id, at, env, out);
      out.reply = kittyReply(cmd, cmd.id, cmd.number, cmd.placement, 'ENOENT:image not found');
      return out;
    }
    out.reply = kittyReply(cmd, cmd.id, cmd.number, 0, 'EINVAL:unsupported action');
    return out;
  }

  put(cmd, id, at, env, out) {
    if (cmd.parent) { out.reply = kittyReply(cmd, id, cmd.number, cmd.placement, 'EINVAL:relative placements are not supported'); return out; }
    const im = this.image(id);
    if (!im) { out.reply = kittyReply(cmd, id, cmd.number, cmd.placement, 'ENOENT:image not found'); return out; }
    const [cols, rows] = kittyBox(cmd, im.width, im.height, env.cell);
    const placement = { uid: env.nextUid(), image: id, id: cmd.placement, cols, rows, virtual: cmd.virtual,
      marker: cmd.virtual ? null : env.mark(at.line), col: at.col, z: cmd.z };
    const drop = p => { out.removed.push(p); return false; };
    if (placement.virtual) {
      this.placements = this.placements.filter(p => !(p.virtual && p.image === id && p.id === cmd.placement) || drop(p));
    } else if (cmd.placement) {
      this.placements = this.placements.filter(p => !(p.image === id && p.id === cmd.placement) || drop(p));
    }
    this.placements.push(placement);
    out.placed = { placement, image: im };
    out.reply = kittyReply(cmd, id, cmd.number, cmd.placement, 'OK');
    if (!placement.virtual && !cmd.noMove) {
      const right = at.col + cols;
      out.cursor = right >= env.cols ? [rows, 0] : [rows - 1, right];
    }
    return out;
  }

  // `a=d`. Screen positions (1-based) become buffer lines; uppercase also frees pictures no
  // placement uses any more.
  remove(cmd, at, env, out) {
    this.pending = null;
    const what = cmd.delete || 'a', free = what !== what.toLowerCase();
    const cellLine = y => env.history + Math.max(1, y) - 1, cellCol = x => Math.max(1, x) - 1;
    const lineOf = p => p.marker.line;
    const hits = (p, line, col) => !p.virtual && (line === null || (line >= lineOf(p) && line < lineOf(p) + p.rows))
      && (col === null || (col >= p.col && col < p.col + p.cols));
    const touched = [], dropImages = [];
    const take = pred => {
      this.placements = this.placements.filter(p => {
        if (!pred(p)) return true;
        touched.push(p.image);
        out.removed.push(p);
        return false;
      });
    };
    switch (what.toLowerCase()) {
      case 'a': take(p => !p.virtual); break;
      case 'i': take(p => p.image === cmd.id && (!cmd.placement || p.id === cmd.placement)); if (free && !cmd.placement) dropImages.push(cmd.id); break;
      case 'n': {
        const id = this.newestNumbered(cmd.number);
        if (id !== null) { take(p => p.image === id && (!cmd.placement || p.id === cmd.placement)); if (free && !cmd.placement) dropImages.push(id); }
        break;
      }
      case 'c': if (at) take(p => hits(p, at.line, at.col)); break;
      case 'p': take(p => hits(p, cellLine(cmd.y), cellCol(cmd.x))); break;
      case 'q': take(p => hits(p, cellLine(cmd.y), cellCol(cmd.x)) && p.z === cmd.z); break;
      case 'x': take(p => hits(p, null, cellCol(cmd.x))); break;
      case 'y': take(p => hits(p, cellLine(cmd.y), null)); break;
      case 'z': take(p => !p.virtual && p.z === cmd.z); break;
      case 'r':
        take(p => p.image >= cmd.x && p.image <= cmd.y);
        if (free) for (const im of this.images) if (im.id >= cmd.x && im.id <= cmd.y) dropImages.push(im.id);
        break;
    }
    if (free) {
      for (const id of touched) if (!this.placements.some(p => p.image === id)) dropImages.push(id);
      for (const id of dropImages) {
        this.placements = this.placements.filter(p => p.image !== id || (out.removed.push(p), false));
        this.images = this.images.filter(im => im.id !== id);
      }
    }
    return out;
  }

  // Events that break every anchor (reflow, screen switch, scrollback cleared or at its cap).
  dropAnchored() {
    const gone = this.placements.filter(p => !p.virtual);
    this.placements = this.placements.filter(p => p.virtual);
    return gone;
  }

  // The oldest picture goes, with its placements (over the slot or pixel budget).
  evict(uid) {
    const im = this.images.find(i => i.uid === uid);
    if (!im) return [];
    this.images = this.images.filter(i => i !== im);
    const gone = this.placements.filter(p => p.image === im.id);
    this.placements = this.placements.filter(p => p.image !== im.id);
    return gone;
  }
}

// ── XTGETTCAP ──

// What `TERM=xterm-256color` promises, plus the extensions programs ask terminals about.
const TERMCAP = {
  TN: 'xterm-256color', name: 'xterm-256color', Co: '256', colors: '256', RGB: '8/8/8', Tc: true,
  setrgbf: '\x1b[38;2;%p1%d;%p2%d;%p3%dm', setrgbb: '\x1b[48;2;%p1%d;%p2%d;%p3%dm',
  Smulx: '\x1b[4:%p1%dm', Setulc: '\x1b[58:2::%p1%{65536}%/%d:%p1%{256}%/%{255}%&%d:%p1%{255}%&%dm',
  Ss: '\x1b[%p1%d q', Se: '\x1b[2 q', Sync: '\x1b[?2026%?%p1%{1}%-%tl%eh%;',
  BD: '\x1b[?2004l', BE: '\x1b[?2004h', PS: '\x1b[200~', PE: '\x1b[201~', fsl: '\x07', tsl: '\x1b]2;',
  smcup: '\x1b[?1049h\x1b[22;0;0t', rmcup: '\x1b[?1049l\x1b[23;0;0t', kmous: '\x1b[<',
  kbs: '\x7f', kcuu1: '\x1bOA', kcud1: '\x1bOB', kcuf1: '\x1bOC', kcub1: '\x1bOD', khome: '\x1bOH', kend: '\x1bOF',
  kich1: '\x1b[2~', kdch1: '\x1b[3~', kpp: '\x1b[5~', knp: '\x1b[6~', kcbt: '\x1b[Z',
  kf1: '\x1bOP', kf2: '\x1bOQ', kf3: '\x1bOR', kf4: '\x1bOS', kf5: '\x1b[15~', kf6: '\x1b[17~', kf7: '\x1b[18~',
  kf8: '\x1b[19~', kf9: '\x1b[20~', kf10: '\x1b[21~', kf11: '\x1b[23~', kf12: '\x1b[24~',
};

const toHex = s => Array.from(textEncoder.encode(s), b => b.toString(16).padStart(2, '0')).join('').toUpperCase();

function fromHex(h) {
  if (!/^(?:[0-9a-fA-F]{2})+$/.test(h)) return null;
  let s = '';
  for (let i = 0; i < h.length; i += 2) s += String.fromCharCode(parseInt(h.slice(i, i + 2), 16));
  return s;
}

// One reply per name, as xterm: `DCS 1 + r name=value ST`; the first unknown name ends the
// answer with `DCS 0 + r ST`.
function termcapReplies(request, cols, rows) {
  const out = [];
  for (const hex of request.split(';')) {
    const name = fromHex(hex);
    let value = name === null ? undefined : name === 'cols' ? String(cols) : name === 'lines' ? String(rows) : TERMCAP[name];
    if (value === undefined) { out.push('\x1bP0+r\x1b\\'); break; }
    out.push(value === true ? `\x1bP1+r${hex.toUpperCase()}\x1b\\` : `\x1bP1+r${hex.toUpperCase()}=${toHex(value)}\x1b\\`);
  }
  return out;
}
