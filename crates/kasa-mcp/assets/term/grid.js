// 셀 그리드 렌더러 — 서버(kasa-pty)가 이미 파싱해 둔 화면을 그대로 그린다.
//
// 여기에 VT 파서가 없는 것이 요점이다. 예전에는 그리드를 ANSI 로 되돌려 보내고
// 브라우저의 xterm.js 가 그걸 다시 파싱했는데, 그 파서가 키 입력까지 자기 방식으로
// 가로채면서 모바일 IME 를 깨뜨렸다(조합 중인 자모가 그대로 나가 「ㅇㅣㄹㅓㅎㄱㅔ」).
// 이제 입력은 이 파일이 쥐고, 화면은 서버가 준 셀을 옮겨 그리기만 한다.
//
// 프레임 형식은 서버의 `gridwire.rs` 를 보라. 런은 `[텍스트, fg, bg, flags]` 다.

(function (global) {
  'use strict';

  const BOLD = 1, ITALIC = 2, UNDERLINE = 4, INVERSE = 8, DIM = 16;

  // xterm 기본 16색과 같은 자리 — 테마의 fg/bg 는 null 로 오므로 여기 없다.
  const BASE16 = [
    '#12161c', '#f7768e', '#9ece6a', '#e0af68', '#7aa2f7', '#bb9af7', '#7dcfff', '#a9b1d6',
    '#414868', '#ff7a93', '#b9f27c', '#ff9e64', '#7da6ff', '#c0a3ff', '#0db9d7', '#c0caf5',
  ];

  function color256(n) {
    if (n < 16) return BASE16[n];
    if (n < 232) {
      const i = n - 16, r = (i / 36) | 0, g = ((i % 36) / 6) | 0, b = i % 6;
      const v = (c) => (c ? 55 + c * 40 : 0);
      return `rgb(${v(r)},${v(g)},${v(b)})`;
    }
    const v = 8 + (n - 232) * 10;
    return `rgb(${v},${v},${v})`;
  }

  // null = 테마 기본(CSS 가 정한다) · 숫자 = 팔레트 · 배열 = 트루컬러
  function css(c) {
    if (c === null || c === undefined) return null;
    if (typeof c === 'number') return color256(c);
    return `rgb(${c[0]},${c[1]},${c[2]})`;
  }

  const validSlug = value => typeof value === 'string' && /^[a-z0-9_-]{1,128}$/i.test(value);
  const validKey = value => typeof value === 'string' && /^[a-f0-9]{64}$/i.test(value);
  const validRect = rect => rect && ['x', 'y', 'width', 'height'].every(key =>
    Number.isFinite(rect[key]) && Math.abs(rect[key]) <= 10000) && rect.width > 0 && rect.height > 0;
  const builtins = new Set(['claude', 'codex', 'terminal', 'server', 'laptop', 'schale-logo', 'schale-classroom']);

  function validAsset(asset) {
    if (!asset) return false;
    switch (asset.kind) {
      case 'sprite': return validSlug(asset.slug) && validSlug(asset.motion)
        && Number.isInteger(asset.frame) && asset.frame >= 0 && asset.frame < 512;
      case 'avatar': return validSlug(asset.slug);
      case 'inline': return validKey(asset.id);
      case 'animation': return Array.isArray(asset.frames) && asset.frames.length > 0
        && asset.frames.length <= 512 && asset.frames.every(validKey);
      case 'builtin': return builtins.has(asset.name);
      case 'solid': return Array.isArray(asset.color) && asset.color.length === 4
        && asset.color.every(channel => Number.isInteger(channel) && channel >= 0 && channel <= 255);
      default: return false;
    }
  }

  function validScenePacket(msg, pane, revision, key) {
    const scene = msg.scene;
    if (scene === null || scene === undefined) return true;
    if (!pane || scene.paneId !== pane || scene.cols !== msg.cols || scene.rows !== msg.rows
        || scene.offset !== 0 || !validKey(scene.sourceKey) || msg.sourceKey !== scene.sourceKey
        || !Number.isSafeInteger(scene.revision) || scene.revision < 0
        || msg.sceneRevision !== scene.revision || scene.revision < revision
        || (scene.revision === revision && key !== scene.sourceKey)
        || !Array.isArray(scene.overlays) || scene.overlays.length > 128
        || msg.dirty.length !== msg.rows || new Set(msg.dirty.map(row => row[0])).size !== msg.rows) return false;
    const ids = new Set();
    return scene.overlays.every(overlay => {
      const motion = overlay.motion;
      if (typeof overlay.id !== 'string' || !overlay.id || overlay.id.length > 128 || ids.has(overlay.id)
          || !validRect(overlay.rect) || (overlay.clip != null && !validRect(overlay.clip))
          || !Number.isInteger(overlay.z) || Math.abs(overlay.z) > 2147483647
          || !['contain', 'cover', 'fill', 'scale-down'].includes(overlay.fit)
          || !['center', 'bottom'].includes(overlay.anchor) || !validAsset(overlay.asset)) return false;
      ids.add(overlay.id);
      return !motion || ((overlay.asset.kind === 'sprite' || overlay.asset.kind === 'animation')
        && Number.isInteger(motion.frames) && motion.frames > 0 && motion.frames <= 512
        && (overlay.asset.kind !== 'animation' || overlay.asset.frames.length === motion.frames)
        && Number.isInteger(motion.frameMs) && motion.frameMs >= 10 && motion.frameMs <= 4294967295
        && Number.isSafeInteger(motion.startedAtMs) && motion.startedAtMs >= 0
        && typeof motion.looping === 'boolean');
    });
  }

  function SceneLayer(view, root, cell) {
    root = typeof root === 'string' && (!root || /^\/(?!\/)/.test(root)) && !/[?#\\\r\n]/.test(root) ? root.replace(/\/$/, '') : '';
    const back = document.createElement('div'), front = document.createElement('div');
    back.className = 'kg-scene kg-scene-back'; front.className = 'kg-scene kg-scene-front';
    back.setAttribute('aria-hidden', 'true'); front.setAttribute('aria-hidden', 'true');
    view.append(back, front);
    const reduced = global.matchMedia?.('(prefers-reduced-motion: reduce)');
    const assets = new Map();
    let records = new Map(), scene = null, pane = null, revision = -1, sourceKey = null;
    let generation = 0, timer = null, shown = true;
    const retryDelays = [500, 1000, 2000, 5000];

    function assetURL(asset, frame) {
      if (asset.kind === 'sprite') return `${root}/character-sprite?slug=${encodeURIComponent(asset.slug)}&motion=${encodeURIComponent(asset.motion)}&frame=${frame}`;
      if (asset.kind === 'avatar') return `${root}/term/avatar/${encodeURIComponent(asset.slug)}.png`;
      if (asset.kind === 'builtin') return `${root}/term/visual-builtin/${asset.name}`;
      const id = asset.kind === 'animation' ? asset.frames[frame] : asset.id;
      return `${root}/term/visual-asset?pane=${encodeURIComponent(scene.paneId)}&id=${id}`;
    }
    function load(url) {
      const requestedScene = `${scene.sourceKey}:${scene.revision}`;
      let attempt = 0;
      if (assets.has(url)) {
        const cached = assets.get(url);
        if (!cached.failed || (cached.failedScene === requestedScene && Date.now() < cached.retryAt)) {
          assets.delete(url); assets.set(url, cached); return cached.promise;
        }
        if (cached.failedScene === requestedScene) attempt = cached.attempt + 1;
        assets.delete(url);
      }
      const entry = { controller: new AbortController(), blobURL: null, bytes: 0, attempt };
      const timeout = setTimeout(() => entry.controller.abort(), 8000);
      entry.promise = fetch(url, { credentials: 'same-origin', signal: entry.controller.signal })
        .then(response => { if (!response.ok) throw new Error('asset unavailable'); return response.blob(); })
        .then(blob => {
          const mime = blob.type.split(';')[0].trim().toLowerCase();
          if (!/^image\/(png|jpeg|gif|webp|svg\+xml)$/.test(mime) || blob.size > 32 * 1024 * 1024) throw new Error('invalid asset');
          if (assets.get(url) !== entry) return null;
          entry.bytes = blob.size;
          entry.blobURL = URL.createObjectURL(blob);
          return entry.blobURL;
        }).catch(() => {
          entry.failed = true; entry.failedScene = requestedScene;
          entry.retryAt = attempt < retryDelays.length ? Date.now() + retryDelays[attempt] : Infinity;
          return null;
        }).finally(() => {
          clearTimeout(timeout);
          if (entry.failed && assets.get(url) === entry && scene && shown && document.visibilityState === 'visible') paint();
        });
      assets.set(url, entry);
      return entry.promise;
    }
    function evict(protectedURLs) {
      let bytes = [...assets.values()].reduce((total, entry) => total + entry.bytes, 0);
      for (const [url, entry] of assets) {
        if (assets.size <= 256 && bytes <= 64 * 1024 * 1024) break;
        if (protectedURLs.has(url)) continue;
        entry.controller.abort(); if (entry.blobURL) URL.revokeObjectURL(entry.blobURL);
        bytes -= entry.bytes; assets.delete(url);
      }
    }
    function stop() { clearTimeout(timer); timer = null; }
    function frameOf(overlay, now) {
      const motion = overlay.motion;
      const initial = overlay.asset.kind === 'sprite' ? overlay.asset.frame : 0;
      if (!motion || reduced?.matches) return initial;
      const frame = Math.floor(Math.max(0, now - motion.startedAtMs) / motion.frameMs);
      return motion.looping ? frame % motion.frames : Math.min(frame, motion.frames - 1);
    }
    function position(record) {
      const { w, h } = cell();
      const overlay = record.overlay, rect = overlay.rect;
      const clip = overlay.clip || { x: 0, y: 0, width: scene.cols, height: scene.rows };
      Object.assign(record.el.style, { left: `${clip.x * w}px`, top: `${clip.y * h}px`,
        width: `${clip.width * w}px`, height: `${clip.height * h}px`, zIndex: String(overlay.z) });
      Object.assign(record.content.style, { left: `${(rect.x - clip.x) * w}px`, top: `${(rect.y - clip.y) * h}px`,
        width: `${rect.width * w}px`, height: `${rect.height * h}px`,
        objectFit: overlay.fit, objectPosition: overlay.anchor === 'bottom' ? '50% 100%' : '50% 50%' });
    }
    function paint() {
      stop();
      if (!scene) return;
      const now = Date.now(), active = shown && document.visibilityState === 'visible';
      let delay = Infinity;
      const protectedURLs = new Set();
      for (const record of records.values()) {
        const overlay = record.overlay, asset = overlay.asset;
        position(record);
        if (asset.kind === 'solid') {
          record.content.style.backgroundColor = `rgba(${asset.color[0]},${asset.color[1]},${asset.color[2]},${asset.color[3] / 255})`;
          continue;
        }
        if (!active) continue;
        const frame = frameOf(overlay, now), url = assetURL(asset, frame);
        protectedURLs.add(url);
        const cached = assets.get(url);
        if (record.url !== url || record.generation !== generation || (cached?.failed && now >= cached.retryAt)) {
          record.url = url; record.generation = generation;
          const version = generation;
          load(url).then(blobURL => {
            if (!blobURL || generation !== version || records.get(overlay.id) !== record || record.url !== url) return;
            record.content.src = blobURL; record.content.hidden = false;
          });
        }
        const retry = assets.get(url);
        if (retry?.failed && Number.isFinite(retry.retryAt)) delay = Math.min(delay, retry.retryAt - now);
        const motion = overlay.motion;
        if (active && motion && !reduced?.matches
            && (motion.looping || now < motion.startedAtMs + motion.frames * motion.frameMs)) {
          delay = Math.min(delay, motion.frameMs - Math.max(0, now - motion.startedAtMs) % motion.frameMs);
          const next = assetURL(asset, motion.looping ? (frame + 1) % motion.frames : Math.min(frame + 1, motion.frames - 1));
          protectedURLs.add(next); load(next);
        }
      }
      evict(protectedURLs);
      if (active && Number.isFinite(delay)) timer = setTimeout(paint, Math.max(10, Math.min(delay, 60000)));
    }
    function clear(reset = false) {
      generation++; stop(); scene = null; records.clear(); back.replaceChildren(); front.replaceChildren();
      delete view.dataset.sceneRevision; delete view.dataset.sourceKey;
      if (reset) {
        pane = null; revision = -1; sourceKey = null;
        for (const entry of assets.values()) { entry.controller.abort(); if (entry.blobURL) URL.revokeObjectURL(entry.blobURL); }
        assets.clear();
      }
    }
    function apply(next) {
      if (!next) { clear(); return; }
      generation++; scene = next; revision = next.revision; sourceKey = next.sourceKey;
      const nextRecords = new Map();
      for (const overlay of [...next.overlays].sort((a, b) => a.z - b.z)) {
        const solid = overlay.asset.kind === 'solid', family = JSON.stringify(overlay.asset);
        let record = records.get(overlay.id);
        if (!record || record.solid !== solid || record.family !== family) {
          const el = document.createElement('div'), content = document.createElement(solid ? 'div' : 'img');
          el.className = 'kg-scene-item'; el.dataset.overlayId = overlay.id;
          content.className = 'kg-scene-content';
          if (!solid) { content.alt = ''; content.draggable = false; content.hidden = true; }
          el.appendChild(content); record = { el, content, solid, family, url: null };
        }
        record.overlay = overlay; nextRecords.set(overlay.id, record);
      }
      records = nextRecords;
      back.replaceChildren(...[...records.values()].filter(record => record.overlay.z < 0).map(record => record.el));
      front.replaceChildren(...[...records.values()].filter(record => record.overlay.z >= 0).map(record => record.el));
      view.dataset.sceneRevision = String(revision); view.dataset.sourceKey = sourceKey;
      paint();
    }
    function resume() {
      for (const entry of assets.values()) if (entry.failed) { entry.attempt = -1; entry.retryAt = 0; }
      paint();
    }
    document.addEventListener('visibilitychange', () => document.visibilityState === 'visible' ? resume() : stop());
    reduced?.addEventListener('change', paint);
    global.addEventListener('pagehide', stop);
    global.addEventListener('pageshow', resume);
    global.addEventListener('online', resume);
    return { valid: msg => validScenePacket(msg, pane, revision, sourceKey), apply, clear,
      measure: paint, setPane(id) { if (pane && pane !== id) clear(true); pane = id; },
      setVisible(value) { shown = value; if (shown) paint(); else stop(); }, get active() { return !!scene; } };
  }

  function KasaGrid(root, opts) {
    opts = opts || {};
    const view = document.createElement('div');
    view.className = 'kg';
    const cursorEl = document.createElement('div');
    cursorEl.className = 'kg-cursor';
    view.appendChild(cursorEl);
    root.appendChild(view);

    // 셀 크기는 폰트에서 재야 커서 위치가 맞는다. 한글은 2셀을 차지하지만 열 번호가
    // 셀 기준이라 `col * cellW` 가 그대로 옳다.
    const ruler = document.createElement('span');
    ruler.className = 'kg-ruler';
    ruler.textContent = 'M';
    view.appendChild(ruler);

    let rows = [];       // 행 DOM
    let rowRuns = [];
    let cols = 0, rowCount = 0;
    let cellW = 8, cellH = 17;
    let cursor = null;
    const scene = SceneLayer(view, opts.assetRoot || '', () => ({ w: cellW, h: cellH }));

    function positionCursor() {
      if (!cursor) return;
      const [row, col, visible] = cursor;
      cursorEl.style.display = visible && row < rowCount ? '' : 'none';
      cursorEl.style.transform = `translate(${col * cellW}px, ${row * cellH}px)`;
      cursorEl.style.width = `${cellW}px`;
      cursorEl.style.height = `${cellH}px`;
    }

    function measure() {
      let r = ruler.getBoundingClientRect();
      // fitWidth 가 zoom 을 걸어 두면 잰 값도 줄어 있다 — 셀 크기는 zoom 이전 값이어야
      // 거울이 서버에 알리는 폭이 안 흔들린다(줄어든 셀로 재면 폭을 더 크게 알리고,
      // 그 폭으로 접히면 zoom 이 풀려 다시 재는 되먹임이 초당 수백 프레임을 만들었다).
      let z = parseFloat(view.style.zoom) || 1;
      // Picture mode hides the grid before its first frame; measure its font
      // outside that hidden ancestor instead of retaining guessed cell sizes.
      if (!r.width || !r.height) {
        const probe = ruler.cloneNode(true);
        const style = getComputedStyle(view);
        for (const property of ['fontFamily', 'fontSize', 'lineHeight', 'fontWeight', 'letterSpacing']) {
          probe.style[property] = style[property];
        }
        document.body.appendChild(probe);
        r = probe.getBoundingClientRect();
        probe.remove();
        z = 1;
      }
      if (r.width > 0) cellW = r.width / z;
      if (r.height > 0) cellH = r.height / z;
      if (cols) view.style.width = `${cols * cellW}px`;
      positionCursor();
      scene.measure();
    }

    function resize(c, r) {
      if (c === cols && r === rowCount) return;
      cols = c; rowCount = r;
      for (const el of rows) el.remove();
      rows = [];
      rowRuns = [];
      for (let i = 0; i < r; i++) {
        const d = document.createElement('div');
        d.className = 'kg-row';
        view.appendChild(d);
        rows.push(d);
      }
      measure();
      view.style.width = `${cols * cellW}px`;
    }

    function runToSpan(run) {
      const [text, fg, bg, flags] = run;
      const s = document.createElement('span');
      s.textContent = text;
      const st = s.style;
      // inverse 는 색을 서로 바꾼다. 한쪽이 기본색이면 CSS 변수가 받아 준다.
      const f = css(flags & INVERSE ? bg : fg);
      const b = css(flags & INVERSE ? fg : bg);
      if (flags & INVERSE) {
        st.color = f || 'var(--kg-bg)';
        st.background = b || 'var(--kg-fg)';
      } else {
        if (f) st.color = f;
        if (b) st.background = b;
      }
      if (flags & BOLD) st.fontWeight = '700';
      if (flags & ITALIC) st.fontStyle = 'italic';
      if (flags & UNDERLINE) st.textDecoration = 'underline';
      if (flags & DIM) st.opacity = '.6';
      return s;
    }

    // 프레임 하나를 화면에 반영한다. `dirty` 는 바뀐 행만 온다.
    function apply(msg) {
      if (!Number.isInteger(msg.cols) || !Number.isInteger(msg.rows) || msg.cols < 1 || msg.rows < 1
          || msg.cols > 1000 || msg.rows > 1000 || !Array.isArray(msg.dirty)
          || !msg.dirty.every(row => Array.isArray(row) && Number.isInteger(row[0]) && row[0] >= 0
            && row[0] < msg.rows && Array.isArray(row[1])) || !Array.isArray(msg.cursor)
          || !scene.valid(msg)) return false;
      resize(msg.cols, msg.rows);
      for (const [i, runs] of msg.dirty) {
        const row = rows[i];
        if (!row) continue;
        rowRuns[i] = runs;
        if (runs.length === 0) row.replaceChildren();
        else row.replaceChildren(...runs.map(runToSpan));
      }
      cursor = [msg.cursor[0], msg.cursor[1], msg.cursorVisible];
      positionCursor();
      scene.apply(msg.scene);
      return true;
    }

    return {
      el: view,
      apply,
      get cols() { return cols; },
      get rows() { return rowCount; },
      get cell() { return { w: cellW, h: cellH }; },
      remeasure: measure,
      resetScene: () => scene.clear(true),
      setPane: id => scene.setPane(id),
      setSceneVisible: value => scene.setVisible(value),
      get hasScene() { return scene.active; },
      setTheme(tokens) {
        if (Array.isArray(tokens?.ansi) && tokens.ansi.length === 16) {
          tokens.ansi.forEach((color, index) => {
            if (typeof color === 'string' && CSS.supports('color', color)) BASE16[index] = color;
          });
          rows.forEach((row, index) => row.replaceChildren(...(rowRuns[index] || []).map(runToSpan)));
        }
      },
    };
  }


  // ── 입력 ───────────────────────────────────────────────────────────────
  //
  // 한글이 깨지던 자리가 여기다. xterm.js 는 조합 중임을 keyCode 229 로만 알아보는데,
  // 어떤 폰 키보드는 **compositionstart 를 아예 안 쏘고 keyCode 0** 으로 자모를 보낸다
  // (2026-08-25 실측). 그래서 xterm 은 그걸 평범한 키로 착각해 자모를 그대로 흘리고
  // (「ㅇㅣㄹㅓㅎㄱㅔ」), 정작 조합된 글자는 `_keyDownSeen` 가드에 걸려 버렸다.
  //
  // 여기서는 **keydown 의 글자를 아예 쓰지 않는다.** 화면에 나갈 글자의 정본은 언제나
  // textarea 의 내용이고, 우리는 그 변화만 PTY 에 옮긴다 — 조합을 누가 어떻게 하든 상관이
  // 없어진다.
  function attachInput(grid, send, modes) {
    const ta = document.createElement('textarea');
    ta.className = 'kg-input';
    ta.setAttribute('autocapitalize', 'off');
    ta.setAttribute('autocorrect', 'off');
    ta.setAttribute('autocomplete', 'off');
    ta.setAttribute('spellcheck', 'false');
    ta.setAttribute('aria-label', '터미널 입력');
    grid.el.appendChild(ta);

    let composing = false;
    let prev = '';      // PTY 에 이미 반영된 textarea 내용
    let timer = null;

    // 조합 중에는 글자가 제자리에서 바뀐다(이→일→이러). 공통 앞부분을 뺀 만큼만
    // 지우고 새로 쓴다 — 코드포인트 단위라야 이모지에서 안 깨진다.
    function flush() {
      timer = null;
      const a = [...prev], b = [...ta.value];
      let i = 0;
      while (i < a.length && i < b.length && a[i] === b[i]) i++;
      prev = ta.value;
      const out = '\x7f'.repeat(a.length - i) + b.slice(i).join('');
      if (out) send(out);
    }
    function flushNow() { if (timer) { clearTimeout(timer); } flush(); }
    function clear() { flushNow(); ta.value = ''; prev = ''; }

    ta.addEventListener('compositionstart', () => { composing = true; });
    ta.addEventListener('compositionend', () => { composing = false; flushNow(); });
    ta.addEventListener('input', () => {
      // 조합 이벤트를 주는 IME(데스크톱)는 끝날 때 한 번만 보낸다 — 중간 상태가
      // 터미널에 안 보여 깔끔하다. 조합 이벤트가 없는 폰은 이 경로로 그때그때 간다.
      if (composing) return;
      if (!timer) timer = setTimeout(flush, 0);
    });

    // 방향키는 앱이 DECCKM 을 켰는지에 따라 SS3 여야 한다 — CSI 로 보내면 claude·vim 의
    // 줄 이동이 조용히 무시된다. 그 모드는 서버가 프레임마다 알려주므로 어긋날 일이 없다.
    function arrow(letter) {
      return (modes.appCursor ? '\x1bO' : '\x1b[') + letter;
    }
    const PLAIN = {
      Enter: '\r', Tab: '\t', Escape: '\x1b', Backspace: '\x7f', Delete: '\x1b[3~',
      Home: '\x1b[H', End: '\x1b[F', PageUp: '\x1b[5~', PageDown: '\x1b[6~',
    };
    const ARROWS = { ArrowUp: 'A', ArrowDown: 'B', ArrowRight: 'C', ArrowLeft: 'D' };

    ta.addEventListener('keydown', (e) => {
      if (e.isComposing || composing) return;   // 조합 중인 키는 IME 것이다
      // Backspace 는 textarea 에 지울 게 남아 있으면 input 이벤트가 알아서 처리한다.
      if (e.key === 'Backspace' && ta.value) return;

      let seq = null;
      if (ARROWS[e.key]) seq = arrow(ARROWS[e.key]);
      else if (PLAIN[e.key]) seq = PLAIN[e.key];
      else if (e.ctrlKey && e.key.length === 1) {
        const c = e.key.toUpperCase().charCodeAt(0);
        if (c >= 64 && c <= 95) seq = String.fromCharCode(c - 64);   // Ctrl+A → \x01
        else if (e.key === ' ') seq = '\0';
      }
      if (seq === null) return;   // 평범한 글자는 textarea 가 받는다 — 여기서 손대지 않는다
      e.preventDefault();
      clear();                    // 밀린 조합을 먼저 내보내고 버퍼를 접는다
      send(seq);
    });

    // 붙여넣기는 textarea 를 거치지 않고 바로 — 줄바꿈이 든 텍스트가 한 번에 간다.
    ta.addEventListener('paste', (e) => {
      const t = e.clipboardData && e.clipboardData.getData('text');
      if (!t) return;
      e.preventDefault();
      clear();
      send(modes.bracketedPaste ? `\x1b[200~${t}\x1b[201~` : t);
    });

    // ⚠️ textarea 를 화면 밖으로 밀거나 크기를 0 으로 만들지 마라 — 모바일 IME 는
    // 조합할 자리가 실재해야 후보창을 띄운다. 커서 자리에 두면 후보창도 글자가 나올
    // 곳에 뜬다.
    function moveTo(row, col) {
      const { w, h } = grid.cell;
      ta.style.transform = `translate(${col * w}px, ${row * h}px)`;
      ta.style.height = `${h}px`;
    }
    grid.el.addEventListener('pointerup', () => ta.focus());
    return { el: ta, focus: () => ta.focus(), moveTo, clear };
  }

  global.KasaGridInput = attachInput;
  global.KasaGrid = KasaGrid;
})(window);
