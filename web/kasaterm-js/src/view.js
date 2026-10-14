// Browser view: puts a Terminal on the page and turns keys, IME, paste, mouse, wheel and touch
// into bytes for the program.
//
// Text input goes through a hidden textarea at the cursor, and what reaches the program is the
// change in that textarea — never the keydown's character. Mobile keyboards compose Hangul
// without composition events (keyCode 0 or 229) and xterm.js leaked the bare jamo
// ("ㅇㅣㄹㅓㅎㄱㅔ"); diffing the textarea works however the IME behaves (same as grid.js).
// Key chords follow the kasaterm app (input.rs): Shift/Option+Enter is LF, Option+←/→ moves by
// word, Cmd+Backspace kills the line.

const IS_MAC = typeof navigator !== 'undefined' && /Mac|iPhone|iPad|iPod/.test(navigator.platform || navigator.userAgent);
const WORD_SEPARATORS = ',│`|:"\' ()[]{}<>\t';
const URL_RE = /\b(?:https?|file):\/\/[^\s"'<>`]+[^\s"'<>`.,;:!?)\]}]/g;
const CTRL_CODES = { Space: 0, Digit2: 0, BracketLeft: 27, Digit3: 27, Backslash: 28, Digit4: 28, BracketRight: 29,
  Digit5: 29, Digit6: 30, Minus: 31, Slash: 31, Digit7: 31, Digit8: 127 };
const FKEYS = { F1: 'P', F2: 'Q', F3: 'R', F4: 'S', F5: 15, F6: 17, F7: 18, F8: 19, F9: 20, F10: 21, F11: 23, F12: 24 };
const TILDE_KEYS = { Insert: 2, Delete: 3, PageUp: 5, PageDown: 6 };
const ARROWS = { ArrowUp: 'A', ArrowDown: 'B', ArrowRight: 'C', ArrowLeft: 'D' };

class View {
  constructor(term, parent, options = {}) {
    this.term = term;
    this.parent = parent;
    this.options = options;
    const doc = parent.ownerDocument;
    this.root = doc.createElement('div');
    this.root.className = 'kasaterm';
    Object.assign(this.root.style, {
      position: 'relative', overflow: 'hidden', outline: 'none', userSelect: 'none', webkitUserSelect: 'none',
      touchAction: 'none', cursor: 'text', width: '100%', height: '100%',
    });
    this.canvas = doc.createElement('canvas');
    this.canvas.style.display = 'block';
    this.textarea = doc.createElement('textarea');
    for (const [k, v] of Object.entries({ autocapitalize: 'off', autocorrect: 'off', autocomplete: 'off', spellcheck: 'false', 'aria-label': options.ariaLabel || '터미널 입력' })) this.textarea.setAttribute(k, v);
    // Visible to the IME but not to the eye: off-screen or zero-sized, mobile IMEs give up on
    // composing and commit bare jamo.
    Object.assign(this.textarea.style, {
      position: 'absolute', left: '0', top: '0', width: '2em', padding: '0', margin: '0', border: '0', outline: 'none',
      resize: 'none', overflow: 'hidden', color: 'transparent', background: 'transparent', caretColor: 'transparent',
      zIndex: '3', whiteSpace: 'pre', opacity: '0.01',
    });
    this.preedit = doc.createElement('div');
    Object.assign(this.preedit.style, { position: 'absolute', left: '0', top: '0', display: 'none', zIndex: '4',
      whiteSpace: 'pre', textDecoration: 'underline', pointerEvents: 'none' });
    this.scrollbar = doc.createElement('div');
    // The scrollbar only shows while the view is scrolled back or being scrolled, so it does not
    // sit over the last column as a permanent bar.
    Object.assign(this.scrollbar.style, { position: 'absolute', top: '0', right: '0', width: '10px', bottom: '0', zIndex: '2',
      display: 'none', touchAction: 'none', opacity: '0', transition: 'opacity .25s' });
    this.thumb = doc.createElement('div');
    Object.assign(this.thumb.style, { position: 'absolute', right: '2px', width: '5px', borderRadius: '3px', opacity: '0.4' });
    this.scrollbar.append(this.thumb);
    this.scrolledUntil = 0;
    this.scrollbar.addEventListener('pointerenter', () => { this.scrollbarHover = true; this.schedule(); });
    this.scrollbar.addEventListener('pointerleave', () => { this.scrollbarHover = false; this.schedule(); });
    // Pictures sit on their own canvas above the text so text frames repaint only changed lines.
    this.imageCanvas = doc.createElement('canvas');
    Object.assign(this.imageCanvas.style, { position: 'absolute', left: '0', top: '0', zIndex: '1', pointerEvents: 'none' });
    this.root.append(this.canvas, this.imageCanvas, this.textarea, this.preedit, this.scrollbar);
    parent.append(this.root);

    this.renderer = new Renderer(term, this.canvas, options);
    this.images = new ImageLayer(this, this.imageCanvas);
    this.applyTheme();
    this.selection = null;
    this.hover = '';
    this.hoverLink = null;
    this.focused = false;
    this.blinkOn = true;
    this.composing = false;
    this.prevInput = '';
    this.flushTimer = null;
    this.frame = 0;
    this.wheelRest = 0;
    this.disposers = [];

    this.listen(term, 'write', () => this.schedule());
    this.listen(term, 'resize', () => { this.selection = null; this.renderer.resize(); this.schedule(); });
    this.listen(term, 'image', e => this.images.add(e));
    this.listen(term, 'imagedelete', ids => this.images.remove(ids));
    this.listen(term, 'screen', () => { this.selection = null; this.schedule(); });
    this.listen(term, 'colors', () => { this.renderer.rgb.clear(); this.renderer.invalidate(); this.schedule(); });
    this.listen(term, 'cursorstyle', () => this.schedule());
    this.listen(term, 'sync-end', () => this.schedule());
    // Scrolling through the core API (term.scrollLines and friends) moves the viewport too.
    this.listen(term, 'viewport', () => this.schedule());
    this.bindInput();
    this.bindPointer();
    this.renderer.resize();
    this.schedule();
    if (doc.fonts && doc.fonts.ready) doc.fonts.ready.then(() => this.remeasure());
    if (typeof window !== 'undefined' && window.matchMedia) {
      const watchDpr = () => {
        const mq = window.matchMedia(`(resolution: ${window.devicePixelRatio}dppx)`);
        const on = () => { this.remeasure(); watchDpr(); };
        mq.addEventListener('change', on, { once: true });
      };
      watchDpr();
    }
  }

  listen(target, name, fn, opts) {
    if (target.on && !target.addEventListener) { const d = target.on(name, fn); this.disposers.push(() => d.dispose()); return; }
    target.addEventListener(name, fn, opts);
    this.disposers.push(() => target.removeEventListener(name, fn, opts));
  }

  dispose() {
    cancelAnimationFrame(this.frame);
    clearInterval(this.blinkTimer);
    for (const d of this.disposers) d();
    this.images.clear();
    this.root.remove();
  }

  // ── geometry ──

  get cell() { return { width: this.renderer.cellW, height: this.renderer.cellH }; }

  remeasure() {
    this.renderer.measure();
    this.renderer.resize();
    this.applyTheme();
    this.schedule();
  }

  setFont(fontSize, fontFamily) {
    this.renderer.setFont(fontSize, fontFamily);
    this.renderer.resize();
    this.applyTheme();
    this.schedule();
  }

  setTheme(theme) {
    this.renderer.setTheme(theme);
    this.applyTheme();
    this.schedule();
  }

  applyTheme() {
    const t = this.renderer.theme, font = `${this.renderer.fontSize}px ${this.renderer.fontFamily}`;
    this.root.style.background = t.background;
    this.textarea.style.font = font;
    this.textarea.style.height = `${this.renderer.cellH}px`;
    Object.assign(this.preedit.style, { font, lineHeight: `${this.renderer.cellH}px`, color: t.foreground, background: t.background });
    this.thumb.style.background = t.foreground;
  }

  proposeDimensions() {
    const r = this.parent.getBoundingClientRect();
    if (!r.width || !r.height) return null;
    return {
      cols: Math.max(2, Math.floor(r.width / this.renderer.cellW)),
      rows: Math.max(1, Math.floor(r.height / this.renderer.cellH)),
    };
  }

  fit() {
    const d = this.proposeDimensions();
    if (d) this.term.resize(d.cols, d.rows);
    return d;
  }

  cellAt(clientX, clientY) {
    const r = this.canvas.getBoundingClientRect();
    const x = Math.floor((clientX - r.left) / this.renderer.cellW);
    const y = Math.floor((clientY - r.top) / this.renderer.cellH);
    return { x: Math.max(0, Math.min(this.term.cols - 1, x)), y: Math.max(0, Math.min(this.term.rows - 1, y)),
      outside: clientX < r.left || clientY < r.top || clientX >= r.right || clientY >= r.bottom };
  }

  // ── frames ──

  schedule() {
    if (this.frame) return;
    this.frame = requestAnimationFrame(() => { this.frame = 0; this.draw(); });
  }

  draw() {
    this.images.layout();
    this.renderer.paint(this);
    this.images.resize(this.canvas.width, this.canvas.height);
    this.imageCanvas.style.width = this.canvas.style.width;
    this.imageCanvas.style.height = this.canvas.style.height;
    this.images.paint();
    const cur = this.term.cursor, { width, height } = this.cell;
    const y = (cur.line + this.term.grid.displayOffset) * height;
    const pos = `translate(${cur.col * width}px, ${y}px)`;
    this.textarea.style.transform = pos;
    this.preedit.style.transform = pos;
    this.drawScrollbar();
  }

  drawScrollbar() {
    const hist = this.term.historySize, rows = this.term.rows;
    if (hist === 0 || this.term.isAltScreen || this.options.scrollbar === false) { this.scrollbar.style.display = 'none'; return; }
    const h = this.canvas.getBoundingClientRect().height || rows * this.cell.height;
    const total = hist + rows;
    const thumbH = Math.max(16, h * rows / total);
    const top = (h - thumbH) * (1 - this.term.grid.displayOffset / hist);
    this.scrollbar.style.display = '';
    this.thumb.style.height = `${thumbH}px`;
    this.thumb.style.top = `${top}px`;
    const shown = this.term.grid.displayOffset > 0 || this.scrollbarHover || performance.now() < this.scrolledUntil;
    this.scrollbar.style.opacity = shown ? '1' : '0';
    if (!shown || this.term.grid.displayOffset > 0) return;
    clearTimeout(this.scrollbarTimer);
    this.scrollbarTimer = setTimeout(() => this.schedule(), Math.max(0, this.scrolledUntil - performance.now()) + 20);
  }

  // ── what the renderer asks ──

  cursorState() {
    const term = this.term;
    if (!(term.mode & MODE.SHOW_CURSOR) || term.grid.displayOffset > term.rows) return '';
    const style = term.resolvedCursorStyle;
    if (!this.focused) return this.options.cursorInactive === 'none' ? '' : 'hollow';
    if (style.blinking && !this.blinkOn) return '';
    return style.shape;
  }

  selectionSpan(line, cols) {
    const s = this.selectionRange();
    if (!s) return '';
    const abs = line + this.term.historySize;
    if (abs < s.start.abs || abs > s.end.abs) return '';
    const from = abs === s.start.abs ? s.start.col : 0;
    const to = abs === s.end.abs ? s.end.col + 1 : cols;
    return from < to ? `${from}:${to}` : '';
  }

  hoverSpan(line) {
    if (!this.hover) return '';
    return this.hover.line === undefined || this.hover.line === line ? this.hover.key : '';
  }

  // ── input ──

  input(data) {
    if (!data) return;
    if (this.term.grid.displayOffset !== 0) { this.term.grid.displayOffset = 0; this.schedule(); }
    this.blinkOn = true;
    this.term.emit('input', data);
    this.term.emit('data', data);
  }

  paste(text) {
    text = text.replace(/\r?\n/g, '\r');
    if (this.term.mode & MODE.BRACKETED_PASTE) text = `\x1b[200~${text.replace(/\x1b\[20[01]~/g, '')}\x1b[201~`;
    this.input(text);
  }

  focus() { this.textarea.focus({ preventScroll: true }); }
  blur() { this.textarea.blur(); }

  flushInput() {
    this.flushTimer = null;
    const ta = this.textarea;
    const a = [...this.prevInput], b = [...ta.value];
    let i = 0;
    while (i < a.length && i < b.length && a[i] === b[i]) i++;
    this.prevInput = ta.value;
    this.input('\x7f'.repeat(a.length - i) + b.slice(i).join(''));
    if (ta.value.length > 256 && !this.composing) { ta.value = ''; this.prevInput = ''; }
  }

  flushNow() { if (this.flushTimer) clearTimeout(this.flushTimer); this.flushInput(); }

  clearInput() { this.flushNow(); this.textarea.value = ''; this.prevInput = ''; }

  bindInput() {
    const ta = this.textarea;
    this.listen(ta, 'compositionstart', () => { this.composing = true; });
    this.listen(ta, 'compositionupdate', e => {
      this.preedit.textContent = e.data || '';
      this.preedit.style.display = e.data ? '' : 'none';
    });
    this.listen(ta, 'compositionend', () => {
      this.composing = false;
      this.preedit.style.display = 'none';
      this.flushNow();
    });
    this.listen(ta, 'input', () => {
      // IMEs with composition events send once at the end; keyboards without them stream.
      if (this.composing) return;
      if (!this.flushTimer) this.flushTimer = setTimeout(() => this.flushInput(), 0);
    });
    this.listen(ta, 'keydown', e => this.onKeyDown(e));
    this.listen(ta, 'paste', e => {
      const text = e.clipboardData && e.clipboardData.getData('text/plain');
      e.preventDefault();
      if (!text) return;
      this.clearInput();
      this.paste(text);
    });
    this.listen(ta, 'copy', e => {
      const text = this.selectionText();
      if (!text) return;
      e.clipboardData.setData('text/plain', text);
      e.preventDefault();
    });
    this.listen(ta, 'focus', () => {
      this.focused = true;
      if (this.term.mode & MODE.FOCUS_IN_OUT) this.input('\x1b[I');
      this.startBlink();
      this.schedule();
    });
    this.listen(ta, 'blur', () => {
      this.focused = false;
      if (this.term.mode & MODE.FOCUS_IN_OUT) this.input('\x1b[O');
      this.schedule();
    });
  }

  startBlink() {
    clearInterval(this.blinkTimer);
    this.blinkOn = true;
    this.blinkTimer = setInterval(() => {
      if (!this.focused) { clearInterval(this.blinkTimer); return; }
      if (!this.term.resolvedCursorStyle.blinking) return;
      this.blinkOn = !this.blinkOn;
      this.schedule();
    }, 530);
  }

  onKeyDown(e) {
    if (e.isComposing || this.composing || e.keyCode === 229) return;
    if (this.keyHandler && this.keyHandler(e) === false) return;
    const copyChord = IS_MAC ? e.metaKey && e.code === 'KeyC' : e.ctrlKey && e.shiftKey && e.code === 'KeyC';
    if (copyChord && this.selection) return;   // the copy event takes it
    const seq = this.keySequence(e);
    if (seq === null) return;   // plain characters arrive through the textarea
    e.preventDefault();
    this.clearInput();
    this.selection = null;
    this.input(seq);
  }

  keySequence(e) {
    const term = this.term, appCursor = (term.mode & MODE.APP_CURSOR) !== 0, alt = term.isAltScreen;
    const mod = 1 + (e.shiftKey ? 1 : 0) + (e.altKey ? 2 : 0) + (e.ctrlKey ? 4 : 0);
    if (IS_MAC && e.metaKey) {
      if (e.key === 'Backspace') return '\x15';
      if (e.key === 'ArrowLeft') return alt ? '\x1b[H' : '\x01';
      if (e.key === 'ArrowRight') return alt ? '\x1b[F' : '\x05';
      return null;
    }
    if (!IS_MAC && e.ctrlKey && e.shiftKey && (e.code === 'KeyC' || e.code === 'KeyV')) return null;
    const key = e.key;
    if (key === 'Enter') return e.shiftKey || e.altKey ? '\n' : '\r';
    if (key === 'Backspace') {
      if (e.ctrlKey) return '\x17';
      if (e.altKey) return '\x1b\x7f';
      return this.textarea.value ? null : '\x7f';
    }
    if (key === 'Tab') return e.shiftKey ? '\x1b[Z' : '\t';
    if (key === 'Escape') return '\x1b';
    if (ARROWS[key]) {
      const l = ARROWS[key];
      if (e.altKey && !e.ctrlKey && !e.shiftKey && !alt && (l === 'C' || l === 'D')) return l === 'D' ? '\x1bb' : '\x1bf';
      if (mod > 1) return `\x1b[1;${mod}${l}`;
      return appCursor ? `\x1bO${l}` : `\x1b[${l}`;
    }
    if (key === 'Home' || key === 'End') {
      const l = key === 'Home' ? 'H' : 'F';
      return mod > 1 ? `\x1b[1;${mod}${l}` : `\x1b[${l}`;
    }
    if (TILDE_KEYS[key]) return `\x1b[${TILDE_KEYS[key]}${mod > 1 ? ';' + mod : ''}~`;
    if (FKEYS[key] !== undefined) {
      const f = FKEYS[key];
      if (typeof f === 'string') return mod > 1 ? `\x1b[1;${mod}${f}` : `\x1bO${f}`;
      return `\x1b[${f}${mod > 1 ? ';' + mod : ''}~`;
    }
    if (e.ctrlKey && !e.metaKey) {
      // Physical keys, so a Korean layout's ㅊ still makes Ctrl+C.
      let code = null;
      if (/^Key[A-Z]$/.test(e.code)) code = e.code.charCodeAt(3) - 64;
      else if (CTRL_CODES[e.code] !== undefined) code = CTRL_CODES[e.code];
      if (code !== null) return (e.altKey ? '\x1b' : '') + String.fromCharCode(code);
      return null;
    }
    if (e.altKey && this.options.optionAsMeta !== false && !(IS_MAC && this.options.optionAsMeta === undefined)) {
      let ch = null;
      if (/^Key[A-Z]$/.test(e.code)) ch = e.shiftKey ? e.code[3] : e.code[3].toLowerCase();
      else if (/^Digit\d$/.test(e.code) && !e.shiftKey) ch = e.code[5];
      else if (key.length === 1 && key.charCodeAt(0) < 127) ch = key;
      if (ch) return '\x1b' + ch;
    }
    return null;
  }

  // ── pointer: selection, links, mouse reports, wheel, touch scroll ──

  mouseMode() { return this.term.mode & (MODE.MOUSE_REPORT_CLICK | MODE.MOUSE_DRAG | MODE.MOUSE_MOTION); }

  report(button, cell, release, motion, e) {
    const term = this.term;
    let b = button + (e.shiftKey ? 4 : 0) + (e.altKey || e.metaKey ? 8 : 0) + (e.ctrlKey ? 16 : 0) + (motion ? 32 : 0);
    const x = cell.x + 1, y = cell.y + 1;
    if (term.mode & MODE.SGR_MOUSE) { this.input(`\x1b[<${b};${x};${y}${release ? 'm' : 'M'}`); return; }
    if (release) b = 3 + (b & ~3);
    // The legacy encoding is single bytes; past column 95 they would not be ASCII, so those
    // reports are dropped rather than mangled by UTF-8 on the way out.
    const limit = term.mode & MODE.UTF8_MOUSE ? 2048 : 128;
    if (32 + x >= limit || 32 + y >= limit) return;
    this.input('\x1b[M' + String.fromCharCode(32 + b) + String.fromCharCode(32 + x) + String.fromCharCode(32 + y));
  }

  bindPointer() {
    const root = this.root;
    let drag = null, touch = null;
    this.listen(root, 'pointerdown', e => {
      if (e.target === this.scrollbar || e.target === this.thumb) { this.dragScrollbar(e); return; }
      const cell = this.cellAt(e.clientX, e.clientY);
      if (e.pointerType === 'touch') {
        touch = { id: e.pointerId, y: e.clientY, x: e.clientX, moved: false, rest: 0 };
        root.setPointerCapture(e.pointerId);
        return;
      }
      e.preventDefault();
      this.focus();
      const link = this.linkAt(e.clientX, e.clientY);
      if (link && (IS_MAC ? e.metaKey : e.ctrlKey)) { this.openLink(link.uri); return; }
      if (this.mouseMode() && !e.shiftKey) {
        const button = e.button === 1 ? 1 : e.button === 2 ? 2 : 0;
        this.report(button, cell, false, false, e);
        drag = { mouse: true, button, last: cell };
        root.setPointerCapture(e.pointerId);
        return;
      }
      if (e.button !== 0) return;
      const abs = cell.y - this.term.grid.displayOffset + this.term.historySize;
      // PointerEvent.detail is not a click count in every browser; count clicks on one cell here.
      const now = performance.now(), last = this.lastClick;
      const count = last && now - last.time < 400 && last.x === cell.x && last.y === cell.y ? last.count % 3 + 1 : 1;
      this.lastClick = { time: now, x: cell.x, y: cell.y, count };
      const mode = count === 3 ? 'line' : count === 2 ? 'word' : 'char';
      this.selection = { anchor: { abs, col: cell.x }, head: { abs, col: cell.x }, mode, active: mode !== 'char' };
      drag = { mouse: false };
      root.setPointerCapture(e.pointerId);
      this.schedule();
    });
    this.listen(root, 'pointermove', e => {
      if (touch && e.pointerId === touch.id) { this.touchScroll(e, touch); return; }
      const cell = this.cellAt(e.clientX, e.clientY);
      if (drag && drag.mouse) {
        const m = this.term.mode;
        if ((m & (MODE.MOUSE_DRAG | MODE.MOUSE_MOTION)) && (cell.x !== drag.last.x || cell.y !== drag.last.y)) {
          this.report(drag.button, cell, false, true, e);
          drag.last = cell;
        }
        return;
      }
      if (drag) {
        const abs = cell.y - this.term.grid.displayOffset + this.term.historySize;
        this.selection.head = { abs, col: cell.x };
        this.selection.active = true;
        this.autoScroll(e);
        this.schedule();
        return;
      }
      if ((this.term.mode & MODE.MOUSE_MOTION) && e.buttons === 0 && !cell.outside) {
        if (!this.lastMotion || this.lastMotion.x !== cell.x || this.lastMotion.y !== cell.y) {
          this.report(3, cell, false, true, e);
          this.lastMotion = cell;
        }
      }
      this.updateHover(e);
    });
    const end = e => {
      if (touch && e.pointerId === touch.id) {
        if (!touch.moved) this.focus();
        touch = null;
        return;
      }
      if (drag && drag.mouse) this.report(drag.button, this.cellAt(e.clientX, e.clientY), true, false, e);
      else if (drag && this.selection && !this.selection.active) this.selection = null;
      drag = null;
      this.schedule();
    };
    this.listen(root, 'pointerup', end);
    this.listen(root, 'pointercancel', end);
    this.listen(root, 'contextmenu', e => { if (this.mouseMode()) e.preventDefault(); });
    this.listen(root, 'wheel', e => this.onWheel(e), { passive: false });
    this.listen(root, 'pointerleave', () => { if (this.hover) { this.hover = ''; this.hoverLink = null; root.style.cursor = 'text'; this.schedule(); } });
  }

  scrollLines(delta) {
    this.scrolledUntil = performance.now() + 1200;
    const g = this.term.grid;
    const next = Math.max(0, Math.min(this.term.historySize, g.displayOffset - delta));
    if (next !== g.displayOffset) { g.displayOffset = next; this.schedule(); }
  }

  scrollToBottom() { this.scrollLines(this.term.historySize); }

  // Wheel and swipe share one rule: a program that takes the mouse gets wheel reports; a
  // full-screen program with alternate scroll gets arrow keys; otherwise the view scrolls.
  scrollBy(lines, e, cell) {
    if (!lines) return;
    const term = this.term;
    if (this.mouseMode() && !e.shiftKey) {
      for (let i = 0; i < Math.min(Math.abs(lines), 10); i++) this.report(lines < 0 ? 64 : 65, cell, false, false, e);
      return;
    }
    if (term.isAltScreen && (term.mode & MODE.ALTERNATE_SCROLL)) {
      const l = lines < 0 ? 'A' : 'B', seq = term.mode & MODE.APP_CURSOR ? `\x1bO${l}` : `\x1b[${l}`;
      this.input(seq.repeat(Math.min(Math.abs(lines), 10)));
      return;
    }
    this.scrollLines(lines);
  }

  onWheel(e) {
    e.preventDefault();
    const h = this.cell.height;
    const px = e.deltaMode === 1 ? e.deltaY * h : e.deltaMode === 2 ? e.deltaY * h * this.term.rows : e.deltaY;
    this.wheelRest += px / h;
    const lines = Math.trunc(this.wheelRest);
    this.wheelRest -= lines;
    this.scrollBy(lines, e, this.cellAt(e.clientX, e.clientY));
  }

  touchScroll(e, touch) {
    const dy = e.clientY - touch.y;
    if (!touch.moved && Math.abs(dy) + Math.abs(e.clientX - touch.x) < 8) return;
    touch.moved = true;
    touch.rest += -dy / this.cell.height;
    touch.y = e.clientY;
    const lines = Math.trunc(touch.rest);
    touch.rest -= lines;
    this.scrollBy(lines, e, this.cellAt(e.clientX, e.clientY));
  }

  autoScroll(e) {
    const r = this.canvas.getBoundingClientRect();
    if (e.clientY < r.top) this.scrollLines(-1);
    else if (e.clientY > r.bottom) this.scrollLines(1);
  }

  dragScrollbar(e) {
    e.preventDefault();
    const bar = this.scrollbar.getBoundingClientRect();
    const move = ev => {
      const hist = this.term.historySize;
      const f = Math.max(0, Math.min(1, (ev.clientY - bar.top) / bar.height));
      this.term.grid.displayOffset = Math.round(hist * (1 - f));
      this.schedule();
    };
    move(e);
    const up = () => { window.removeEventListener('pointermove', move); window.removeEventListener('pointerup', up); };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  }

  // ── selection ──

  selectionRange() {
    const s = this.selection;
    if (!s || !s.active) return null;
    let [a, b] = [s.anchor, s.head];
    if (b.abs < a.abs || (b.abs === a.abs && b.col < a.col)) [a, b] = [b, a];
    let start = { ...a }, end = { ...b };
    if (s.mode === 'word') { start = this.wordEdge(start, -1); end = this.wordEdge(end, 1); }
    if (s.mode === 'line') {
      start = { abs: this.logicalLineEdge(start.abs, -1), col: 0 };
      end = { abs: this.logicalLineEdge(end.abs, 1), col: this.term.cols - 1 };
    }
    return { start, end };
  }

  rowAtAbs(abs) {
    const y = abs - this.term.historySize;
    return y < -this.term.historySize || y >= this.term.rows ? null : this.term.line(y);
  }

  wordEdge(pos, dir) {
    const row = this.rowAtAbs(pos.abs);
    if (!row) return pos;
    const sep = x => {
      if (row.flags(x) & WIDE_CHAR_SPACER) return false;
      const t = row.text(x);
      return t === ' ' || WORD_SEPARATORS.includes(t);
    };
    let col = pos.col;
    if (row.flags(col) & WIDE_CHAR_SPACER) col--;
    if (sep(col)) return { abs: pos.abs, col };
    while (col + dir >= 0 && col + dir < this.term.cols && !sep(col + dir)) col += dir;
    if (dir > 0 && (row.flags(col) & WIDE_CHAR)) col++;
    return { abs: pos.abs, col };
  }

  logicalLineEdge(abs, dir) {
    const last = this.term.cols - 1;
    for (;;) {
      if (dir < 0) {
        const above = this.rowAtAbs(abs - 1);
        if (!above || !(above.flags(last) & WRAPLINE)) return abs;
        abs--;
      } else {
        const row = this.rowAtAbs(abs);
        if (!row || !(row.flags(last) & WRAPLINE) || !this.rowAtAbs(abs + 1)) return abs;
        abs++;
      }
    }
  }

  selectionText() {
    const r = this.selectionRange();
    if (!r) return '';
    const cols = this.term.cols, out = [];
    for (let abs = r.start.abs; abs <= r.end.abs; abs++) {
      const row = this.rowAtAbs(abs);
      if (!row) continue;
      const from = abs === r.start.abs ? r.start.col : 0, to = abs === r.end.abs ? r.end.col + 1 : cols;
      let s = '';
      for (let x = from; x < to; x++) {
        if (row.flags(x) & (WIDE_CHAR_SPACER | LEADING_WIDE_CHAR_SPACER)) continue;
        const t = row.text(x);
        s += t;
        if (t === '\t') while (x + 1 < to && !this.term.tabs[x + 1] && row.cp(x + 1) === 32) x++;
      }
      const wrapped = (row.flags(cols - 1) & WRAPLINE) !== 0 && abs !== r.end.abs;
      out.push(wrapped ? s : s.replace(/ +$/, ''));
      if (!wrapped && abs !== r.end.abs) out.push('\n');
    }
    return out.join('');
  }

  selectAll() {
    const t = this.term;
    this.selection = { anchor: { abs: 0, col: 0 }, head: { abs: t.historySize + t.rows - 1, col: t.cols - 1 }, mode: 'char', active: true };
    this.schedule();
  }

  clearSelection() { this.selection = null; this.schedule(); }

  // ── links ──

  linkAt(clientX, clientY) {
    const cell = this.cellAt(clientX, clientY);
    if (cell.outside) return null;
    const line = cell.y - this.term.grid.displayOffset;
    const row = this.term.line(line);
    const e = row.extra(cell.x);
    if (e && e.link) return { uri: e.link.uri, line, key: 'L' + linkKey(e.link), whole: true };
    let text = '';
    const at = [];
    for (let x = 0; x < this.term.cols; x++) {
      if (row.flags(x) & WIDE_CHAR_SPACER) continue;
      const t = row.text(x);
      for (let k = 0; k < t.length; k++) at.push(x);
      text += t;
    }
    URL_RE.lastIndex = 0;
    for (let m; (m = URL_RE.exec(text));) {
      const from = at[m.index], to = at[m.index + m[0].length - 1] + 1;
      if (cell.x >= from && cell.x < to) return { uri: m[0], line, key: `R${from}:${to}` };
    }
    return null;
  }

  updateHover(e) {
    const link = this.linkAt(e.clientX, e.clientY);
    const hover = link ? (link.whole ? { key: link.key } : { key: link.key, line: link.line }) : '';
    const key = hover ? hover.key + (hover.line ?? '') : '';
    const prev = this.hover ? this.hover.key + (this.hover.line ?? '') : '';
    if (key === prev) return;
    this.hover = hover;
    this.hoverLink = link;
    this.root.style.cursor = link ? 'pointer' : 'text';
    this.schedule();
  }

  openLink(uri) {
    if (this.options.onLink) { this.options.onLink(uri); return; }
    if (!/^(https?:|mailto:)/i.test(uri)) return;
    window.open(uri, '_blank', 'noopener,noreferrer');
  }
}

// Browser entry points on Terminal: `term.open(el)` puts it on the page.
Terminal.prototype.open = function open(parent, options = {}) {
  this.view = new View(this, parent, { ...this.viewOptions, ...options });
  return this.view;
};
Terminal.prototype.focus = function focus() { if (this.view) this.view.focus(); };
Terminal.prototype.fit = function fit() { return this.view ? this.view.fit() : null; };
Terminal.prototype.setTheme = function setTheme(theme) { if (this.view) this.view.setTheme(theme); };
// The core may have its own dispose (add-ons, listeners); the view is torn down first, then that.
const disposeCore = Terminal.prototype.dispose;
Terminal.prototype.dispose = function dispose() {
  if (this.view) this.view.dispose();
  this.view = null;
  if (disposeCore) disposeCore.call(this);
};
