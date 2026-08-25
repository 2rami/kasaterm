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
    let cols = 0, rowCount = 0;
    let cellW = 8, cellH = 17;

    function measure() {
      const r = ruler.getBoundingClientRect();
      if (r.width > 0) cellW = r.width;
      if (r.height > 0) cellH = r.height;
    }

    function resize(c, r) {
      if (c === cols && r === rowCount) return;
      cols = c; rowCount = r;
      for (const el of rows) el.remove();
      rows = [];
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
      resize(msg.cols, msg.rows);
      for (const [i, runs] of msg.dirty) {
        const row = rows[i];
        if (!row) continue;
        if (runs.length === 0) row.replaceChildren();
        else row.replaceChildren(...runs.map(runToSpan));
      }
      const [cr, cc] = msg.cursor;
      if (msg.cursorVisible && cr < rowCount) {
        cursorEl.style.display = '';
        cursorEl.style.transform = `translate(${cc * cellW}px, ${cr * cellH}px)`;
        cursorEl.style.width = `${cellW}px`;
        cursorEl.style.height = `${cellH}px`;
      } else {
        cursorEl.style.display = 'none';
      }
    }

    return {
      el: view,
      apply,
      get cols() { return cols; },
      get rows() { return rowCount; },
      get cell() { return { w: cellW, h: cellH }; },
      remeasure: measure,
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
