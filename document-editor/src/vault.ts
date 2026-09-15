export type VaultEntry = { id: string; name: string; kind: 'folder' | 'markdown' | 'image' | 'pdf' | 'file' };
export type VaultState = { id?: string; name?: string; available: boolean; error?: string; activeId?: string; hasDocument: boolean; entries?: VaultEntry[]; nextCursor?: number; recent?: { id: string; name: string }[] };
export type VaultChildren = { parentId: string; entries: VaultEntry[]; nextCursor?: number; error?: string };
export type VaultSearch = { query: string; requestId: string; entries: VaultEntry[]; nextCursor?: number; error?: string };
type Row = { entry?: VaultEntry; depth: number; parentId?: string; more?: number; loading?: boolean; error?: string };
type Listing = { entries: VaultEntry[]; nextCursor?: number; error?: string };
type Sender = (kind: string, payload?: Record<string, unknown>) => void;

export function visibleRows(roots: VaultEntry[], children: Map<string, Listing>, expanded: Set<string>, loading: Set<string>, nextCursor?: number): Row[] {
  const rows: Row[] = [];
  const visit = (entries: VaultEntry[], depth: number) => {
    for (const entry of entries) {
      rows.push({ entry, depth });
      if (entry.kind !== 'folder' || !expanded.has(entry.id) || depth > 64) continue;
      const child = children.get(entry.id);
      if (child) {
        visit(child.entries, depth + 1);
        if (child.error) rows.push({ depth: depth + 1, parentId: entry.id, error: child.error });
        if (child.nextCursor != null) rows.push({ depth: depth + 1, parentId: entry.id, more: child.nextCursor });
      } else if (loading.has(entry.id)) rows.push({ depth: depth + 1, loading: true });
    }
  };
  visit(roots, 0);
  if (nextCursor != null) rows.push({ depth: 0, parentId: '', more: nextCursor });
  return rows;
}

const icons: Record<string, string> = {
  folder: '<path d="M3 6h6l2 2h10v11H3z"/>',
  markdown: '<path d="M6 3h9l4 4v14H6zM14 3v5h5M9 12h7M9 16h7"/>',
  image: '<rect x="3" y="4" width="18" height="16" rx="2"/><circle cx="8" cy="9" r="1"/><path d="m4 18 5-5 4 3 3-5 5 7"/>',
  pdf: '<path d="M6 3h9l4 4v14H6zM14 3v5h5M9 13h7M9 17h4"/>',
  file: '<path d="M6 3h9l4 4v14H6zM14 3v5h5"/>',
  menu: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M9 4v16M5 8h2M5 12h2"/>',
  chevron: '<path d="m9 5 7 7-7 7"/>',
};
function icon(kind: string) {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('viewBox', '0 0 24 24'); svg.setAttribute('fill', 'none'); svg.setAttribute('stroke', 'currentColor'); svg.setAttribute('stroke-width', '1.6'); svg.setAttribute('stroke-linecap', 'round'); svg.setAttribute('stroke-linejoin', 'round'); svg.setAttribute('aria-hidden', 'true'); svg.innerHTML = icons[kind] ?? icons.file; return svg;
}
function action(label: string, click: () => void, primary = false) {
  const b = document.createElement('button'); b.type = 'button'; b.className = 'document-button' + (primary ? ' document-button--primary' : ''); b.textContent = label; b.addEventListener('click', click); return b;
}

export function mountVault(documentRoot: HTMLElement, send: Sender) {
  const shell = document.createElement('div'); shell.className = 'vault-workspace';
  const aside = document.createElement('aside'); aside.className = 'vault-sidebar'; aside.setAttribute('aria-label', '볼트 파일');
  const heading = document.createElement('div'); heading.className = 'vault-heading';
  const title = document.createElement('strong');
  const change = action('바꾸기', () => choose()); change.title = '다른 볼트 열기'; heading.append(icon('folder'), title, change);
  const search = document.createElement('input'); search.className = 'vault-search'; search.type = 'search'; search.placeholder = '파일 찾기'; search.setAttribute('aria-label', '볼트 파일명 검색');
  const status = document.createElement('p'); status.className = 'vault-status'; status.setAttribute('role', 'status');
  const tree = document.createElement('div'); tree.className = 'vault-tree'; tree.setAttribute('role', 'tree'); tree.setAttribute('aria-label', '폴더와 문서');
  const rowsRoot = document.createElement('div'); tree.append(rowsRoot); aside.append(heading, search, status, tree);
  const canvas = document.createElement('div'); canvas.className = 'vault-canvas';
  const welcome = document.createElement('section'); welcome.className = 'vault-welcome';
  const toggle = action('', () => { shell.classList.toggle('vault-sidebar-open'); toggle.setAttribute('aria-expanded', String(shell.classList.contains('vault-sidebar-open'))); }); toggle.classList.add('vault-toggle'); toggle.append(icon('menu')); toggle.setAttribute('aria-label', '파일 목록 열기 또는 접기');
  const veil = document.createElement('button'); veil.type = 'button'; veil.className = 'vault-veil'; veil.setAttribute('aria-label', '파일 목록 닫기'); veil.addEventListener('click', () => { shell.classList.remove('vault-sidebar-open'); });
  documentRoot.before(shell); canvas.append(welcome, documentRoot); shell.append(aside, veil, canvas, toggle);
  let state: VaultState | undefined, expanded = new Set<string>(), children = new Map<string, Listing>(), loading = new Set<string>();
  let rows: Row[] = [], searchResult: VaultSearch | undefined, searchTimer: ReturnType<typeof setTimeout> | undefined, queryId = 0, picking = false;
  const rowHeight = 32;
  function choose(id?: string) { if (picking) return; picking = true; renderWelcome(); send('vault-choose', id ? { nodeId: id } : {}); }
  function renderWelcome() {
    if (!state) return;
    welcome.replaceChildren();
    const graphic = icon('folder'); graphic.classList.add('vault-welcome-icon');
    const h = document.createElement('h1'); h.textContent = state.available ? '문서를 선택하세요' : '폴더 하나를 작업 공간으로';
    const p = document.createElement('p'); p.textContent = state.available ? '왼쪽에서 문서를 열고 바로 이어서 작성하세요.' : '마크다운 문서가 있는 폴더를 볼트로 열어보세요.';
    const chooseButton = action(picking ? '폴더 선택 중…' : '볼트 열기', () => choose(), true); chooseButton.disabled = picking;
    welcome.append(graphic, h, p);
    if (!state.available) welcome.append(chooseButton);
    if (state.error) { const error = document.createElement('p'); error.className = 'vault-error'; error.setAttribute('role', 'alert'); error.textContent = state.error; welcome.append(error); }
    if (!state.available && state.recent?.length) {
      const recents = document.createElement('div'); recents.className = 'vault-recents';
      const label = document.createElement('h2'); label.textContent = '최근 문서'; recents.append(label);
      for (const recent of state.recent) { const b = action(recent.name, () => send('vault-open', { nodeId: recent.id })); b.prepend(icon('markdown')); b.disabled = picking; recents.append(b); }
      welcome.append(recents);
    }
  }
  function requestFolder(id: string, cursor?: number) { loading.add(id); send('vault-list', { nodeId: id, ...(cursor !== undefined ? { cursor } : {}) }); updateRows(); }
  function open(entry: VaultEntry) {
    if (entry.kind === 'folder') {
      if (search.value.trim()) {
        clearTimeout(searchTimer); search.value = ''; searchResult = undefined; queryId++;
        const parts = entry.id.split('/');
        for (let i = 1; i < parts.length; i++) { const ancestor = parts.slice(0, i).join('/'); expanded.add(ancestor); if (!children.has(ancestor)) requestFolder(ancestor); }
      }
      if (expanded.has(entry.id)) expanded.delete(entry.id);
      else { expanded.add(entry.id); if (!children.has(entry.id)) requestFolder(entry.id); }
      updateRows();
    } else {
      send('vault-open', { nodeId: entry.id });
      if (innerWidth < 700) shell.classList.remove('vault-sidebar-open');
    }
  }
  function updateRows() {
    if (!state) return;
    const query = search.value.trim();
    rows = query ? (searchResult?.entries ?? []).map(entry => ({ entry, depth: 0 })) : visibleRows(state.entries ?? [], children, expanded, loading, state.nextCursor);
    if (query && searchResult?.nextCursor != null) rows.push({ depth: 0, more: searchResult.nextCursor });
    status.textContent = query ? (!searchResult ? '파일을 찾고 있어요…' : searchResult.error || (!rows.length ? '일치하는 파일이 없어요' : '')) : state.error || (!rows.length ? '이 볼트는 비어 있어요' : '');
    renderRows();
  }
  function renderRows() {
    const start = Math.max(0, Math.floor(tree.scrollTop / rowHeight) - 5), count = Math.ceil((tree.clientHeight || 640) / rowHeight) + 12;
    rowsRoot.replaceChildren(); rowsRoot.style.paddingTop = `${start * rowHeight}px`; rowsRoot.style.paddingBottom = `${Math.max(0, rows.length - start - count) * rowHeight}px`;
    rows.slice(start, start + count).forEach(row => {
      const b = document.createElement('button'); b.type = 'button'; b.className = 'vault-row'; b.style.paddingInlineStart = `${12 + row.depth * 16}px`;
      if (!row.entry) {
        b.textContent = row.loading ? '불러오는 중…' : row.error ? '읽지 못했어요 · 다시 시도' : '더 보기'; b.disabled = Boolean(row.loading); b.title = row.error ?? '';
        b.addEventListener('click', () => { if (search.value.trim()) send('vault-search', { query: search.value.trim(), requestId: String(queryId), cursor: row.more }); else requestFolder(row.parentId ?? '', row.more); });
      } else {
        const entry = row.entry; b.dataset.nodeId = entry.id; b.setAttribute('role', 'treeitem'); b.setAttribute('aria-level', String(row.depth + 1)); b.setAttribute('aria-selected', String(state?.activeId === entry.id));
        const disclosure = icon('chevron'); disclosure.classList.add('vault-disclosure'); if (entry.kind !== 'folder') disclosure.style.visibility = 'hidden'; else { b.setAttribute('aria-expanded', String(expanded.has(entry.id))); if (expanded.has(entry.id)) disclosure.classList.add('expanded'); }
        const name = document.createElement('span'); name.textContent = entry.name; b.title = entry.id; b.append(disclosure, icon(entry.kind), name); b.addEventListener('click', () => open(entry));
        b.addEventListener('keydown', e => {
          if (e.key === 'ArrowRight' && entry.kind === 'folder' && !expanded.has(entry.id)) { e.preventDefault(); open(entry); }
          if (e.key === 'ArrowLeft' && entry.kind === 'folder' && expanded.has(entry.id)) { e.preventDefault(); open(entry); }
          if (e.key === 'ArrowDown' || e.key === 'ArrowUp') { e.preventDefault(); const position = rows.indexOf(row) + (e.key === 'ArrowDown' ? 1 : -1); if (position < 0 || position >= rows.length) return; tree.scrollTop = Math.max(0, position * rowHeight - tree.clientHeight / 2); renderRows(); const id = rows[position].entry?.id; [...rowsRoot.querySelectorAll<HTMLButtonElement>('button')].find(button => button.dataset.nodeId === id)?.focus(); }
        });
      }
      rowsRoot.append(b);
    });
  }
  tree.addEventListener('scroll', renderRows, { passive: true });
  search.addEventListener('input', () => { clearTimeout(searchTimer); searchResult = undefined; queryId++; tree.scrollTop = 0; updateRows(); const query = search.value.trim(), requestId = String(queryId); if (query) searchTimer = setTimeout(() => send('vault-search', { query, requestId }), 180); });
  window.addEventListener('resize', renderRows);
  shell.classList.add('vault-disabled');
  return {
    setVault(next: VaultState) {
      const changed = !state || (next.id ?? next.name) !== (state.id ?? state.name);
      if (changed) { expanded.clear(); children.clear(); loading.clear(); search.value = ''; searchResult = undefined; tree.scrollTop = 0; }
      state = next; picking = false; shell.classList.remove('vault-disabled'); shell.classList.toggle('vault-has-vault', next.available); shell.classList.toggle('vault-has-document', next.hasDocument);
      title.textContent = next.name ?? '볼트'; documentRoot.hidden = !next.hasDocument; welcome.hidden = next.hasDocument; aside.hidden = !next.available; toggle.hidden = !next.available;
      renderWelcome(); updateRows();
    },
    setVaultChildren(data: VaultChildren) {
      loading.delete(data.parentId);
      if (!state) return;
      if (data.parentId === '') state = { ...state, entries: data.entries, nextCursor: data.nextCursor, error: data.error };
      else children.set(data.parentId, { entries: data.entries, nextCursor: data.nextCursor, error: data.error });
      updateRows();
    },
    setVaultSearch(data: VaultSearch) {
      if (data.requestId !== String(queryId) || data.query !== search.value.trim()) return;
      searchResult = data; updateRows();
    },
  };
}
