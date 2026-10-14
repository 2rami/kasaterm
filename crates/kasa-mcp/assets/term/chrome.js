(function (global) {
  'use strict';
  const validColor = color => typeof color === 'string' && CSS.supports('color', color);
  const terminalColors = ['black', 'red', 'green', 'yellow', 'blue', 'magenta', 'cyan', 'white',
    'brightBlack', 'brightRed', 'brightGreen', 'brightYellow', 'brightBlue', 'brightMagenta', 'brightCyan', 'brightWhite'];

  function terminalTheme(tokens) {
    const palette = tokens?.palette || {};
    const theme = { background: palette.bg, foreground: palette.fg, cursor: palette.accent,
      selectionBackground: palette.surface_active };
    for (const [index, key] of terminalColors.entries()) theme[key] = tokens?.ansi?.[index];
    return Object.fromEntries(Object.entries(theme).filter(([, value]) => validColor(value)));
  }

  function create(options) {
    const root = options.root;
    let paneId = options.pane || '', machine = '', tokens = null, inflight = false, generation = 0;
    let timer = null, lastPane = null, lastRows = [];
    const requests = new Set(), failedAvatars = new Set();
    const el = id => document.getElementById(id);
    const icon = name => `url("${root}/term/icon/${name}.svg")`;
    const avatar = el('avatar'), fallback = el('ahole');
    if (avatar) avatar.onerror = () => {
      failedAvatars.add(avatar.getAttribute('src'));
      avatar.style.display = 'none';
      if (fallback) fallback.style.display = 'flex';
    };

    function applyTokens(value) {
      if (!value?.palette) return;
      tokens = value;
      for (const [key, color] of Object.entries(value.palette)) {
        if (validColor(color)) document.documentElement.style.setProperty(`--kt-${key.replaceAll('_', '-')}`, color);
      }
      for (const key of ['radius_sm', 'radius_md']) {
        const radius = value.shape?.[key];
        if (Number.isFinite(radius)) document.documentElement.style.setProperty(`--kt-${key.replaceAll('_', '-')}`, `${Math.max(0, radius)}px`);
      }
      document.documentElement.dataset.theme = value.theme || '';
      options.onTheme?.(value);
    }

    // 나쵸 봇이 띄운 칸의 색 — 데스크톱 `theme::nacho` 와 같은 값(나쵸 앱 테마의 하늘색 눈, 밝은 판은 글자색).
    function nachoColor() {
      const bg = /^#([0-9a-f]{6})$/i.exec(tokens?.palette?.bg || '');
      const n = bg ? parseInt(bg[1], 16) : 0;
      const light = 0.299 * (n >> 16) + 0.587 * ((n >> 8) & 255) + 0.114 * (n & 255) > 128;
      return light ? '#0F7591' : '#38C0E0';
    }
    const nachoVia = { slack: '슬랙', discord: '디코', app: '앱' };

    function updatePane(pane) {
      if (!pane) return;
      lastPane = pane;
      const harness = (pane.harness || '').toLowerCase();
      const nacho = pane.nacho || null;
      const harnessIcon = nacho?.icon || (harness.includes('codex') ? 'codex' : harness.includes('claude') ? 'claude' : 'terminal');
      if (fallback) {
        fallback.style.setProperty('--harness-icon', icon(harnessIcon));
        fallback.style.color = nacho ? nachoColor() : '';
      }
      const nachoName = nacho ? ['나쵸', nachoVia[nacho.via]].filter(Boolean).join(' · ') : '';
      const name = nachoName || pane.name || (harness.includes('codex') ? 'Codex' : harness.includes('claude') ? 'Claude Code' : '터미널');
      if (el('who')) el('who').textContent = name;
      const title = pane.title || pane.peer_name || '터미널 세션';
      if (el('whot')) el('whot').textContent = nacho?.place ? `${nacho.place} · ${title}` : title;
      const color = nacho ? nachoColor() : pane.color || tokens?.character_accents?.[pane.name];
      if (el('who')) el('who').style.color = validColor(color) ? color : '';
      // 나쵸 칸은 판이 붙인 학생 얼굴이 있어도 창구 아이콘이 선다 — 얼굴이 나쵸 칸을 학생처럼 보이게 한다.
      if (avatar && pane.slug && !nacho) {
        const source = `${root}/term/avatar/${encodeURIComponent(pane.slug)}.png`;
        if (failedAvatars.has(source)) {
          avatar.style.display = 'none';
          if (fallback) fallback.style.display = 'flex';
        } else {
          if (avatar.getAttribute('src') !== source) avatar.src = source;
          avatar.style.display = 'block';
          if (fallback) fallback.style.display = 'none';
        }
      } else if (avatar) {
        avatar.style.display = 'none';
        if (fallback) fallback.style.display = 'flex';
      }
      const label = pane.mirror_of || machine || '이 기기';
      const machineName = el('machine-name');
      if (machineName) { machineName.textContent = label; machineName.title = label; }
      const machineIcon = /macbook|맥북|laptop/i.test(label) ? 'laptop' : /mini|미니|server/i.test(label) ? 'server' : 'terminal';
      el('machine-badge')?.style.setProperty('--machine-icon', icon(machineIcon));
      if (el('model-label')) el('model-label').textContent = [pane.model_label, pane.effort_label].filter(Boolean).join(' · ');
      const labels = { working: '작업 중', busy: '작업 중', running: '작업 중', waiting: '입력 기다림', idle: '대기', done: '완료', completed: '완료', error: '확인 필요' };
      const state = el('pane-state');
      if (state) { state.textContent = labels[pane.status] || ''; state.dataset.state = pane.status || ''; }
    }

    async function read(path) {
      const controller = new AbortController();
      requests.add(controller);
      const timeout = setTimeout(() => controller.abort(), options.timeoutMs || 4000);
      try {
        const response = await fetch(`${root}${path}`, { signal: controller.signal });
        if (!response.ok) throw new Error(`HTTP ${response.status}`);
        return await response.json();
      } finally { clearTimeout(timeout); requests.delete(controller); }
    }
    async function refresh() {
      if (inflight || document.visibilityState !== 'visible') return;
      inflight = true;
      const version = generation;
      try {
        await Promise.allSettled([
          read('/design-tokens').then(value => { if (version === generation) applyTokens(value); }),
          read('/term/panes').then(value => {
            if (version !== generation || !Array.isArray(value)) return;
            lastRows = value;
            updatePane(value.find(pane => pane.id === paneId));
          }),
          machine ? Promise.resolve() : read('/mobile/me').then(value => {
            if (version !== generation) return;
            machine = value?.machine || machine;
            updatePane(lastPane);
          }),
        ]);
      } finally { if (version === generation) inflight = false; }
    }
    document.addEventListener('visibilitychange', refresh);
    function start() { if (timer === null) timer = setInterval(refresh, 5000); refresh(); }
    window.addEventListener('pageshow', start);
    window.addEventListener('pagehide', () => {
      clearInterval(timer); timer = null; generation++; inflight = false;
      for (const request of requests) request.abort();
    });
    start();
    return { updatePane, refresh, setPane(id) { paneId = id; updatePane(lastRows.find(pane => pane.id === id)); refresh(); } };
  }
  global.KasaTermChrome = { create, terminalTheme };
})(window);
