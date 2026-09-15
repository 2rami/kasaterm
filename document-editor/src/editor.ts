import { Editor, type ChainedCommands } from '@tiptap/core';
import { TextSelection } from '@tiptap/pm/state';
import { closeHistory } from '@tiptap/pm/history';
import Placeholder from '@tiptap/extension-placeholder';
import { DocumentSource, extensions } from './document';
import { applyTheme } from './theme';
import { mountVault, type VaultState, type VaultChildren, type VaultSearch } from './vault';
import { floatingPlacement, type Rectangle } from './floating';
import { createSizedCaret } from './caret';
import { blockAt, insertAfterBlock, duplicateBlock, removeBlock, moveBlockBy, moveBlockTo, unwrapListItem, type BlockTarget } from './block-controls';
import './editor.css';
import './vault.css';
import './graph.css';
import type { VaultGraph } from './graph';

declare global { interface Window {
  __KASATERM_DOC_TOKEN__?: string;
  ipc?: { postMessage(message: string): void };
  kasatermEditor: { init(data: Init): void; setContent(data: Content): void; setTheme(theme: unknown): void; setVault(data: VaultState): void; setVaultChildren(data: VaultChildren): void; setVaultSearch(data: VaultSearch): void; setVaultGraph(data: VaultGraph): void; command(name: string, payload?: any): void; setSaveState(data: { state: string; message?: string }): void };
} }
type Content = { markdown: string; revision: number };
type Init = Content & { token: string; theme?: any; editable?: boolean };
let token = window.__KASATERM_DOC_TOKEN__ ?? '', revision = 0, editor: Editor | undefined;
let source = new DocumentSource(), composing = false, pending: { kind: string; extra: object }[] = [], muted = false;
let lastMarkdown = '', zoom = 1, slashRange: { from: number; to: number } | undefined;
let slashIndex = 0, slashItems: Action[] = [];
let generation = 0, compositionTimer: ReturnType<typeof setTimeout> | undefined;
let dismissedSelection = '', dismissedSlash = '', blockMenu = false;
let menuMode: 'insert' | 'convert' | 'manage' = 'convert', menuTarget: BlockTarget | undefined, hoverTarget: BlockTarget | undefined;
let dragSource: BlockTarget | undefined, dropTarget: { block: BlockTarget; after: boolean } | undefined;
type Action = { label: string; hint: string; icon: string; danger?: boolean; operation?: boolean; disabled?: boolean; run(chain: ChainedCommands): boolean };
document.body.append(document.createComment('THESIS: Edit the document itself. OWN-WORLD: existing neutral paper, sky focus, system sans. STORY: write, select, format, save. FIRST VIEWPORT: one centered reading measure; contextual tools beside text. FORM: user-pinned Notion/Linear canon. FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, and DESIGN.md'));
const root = document.createElement('main'); root.id = 'document';
const mount = document.createElement('div'); mount.id = 'editor'; root.append(mount);
const hint = document.createElement('div'); hint.className = 'document-hint'; hint.textContent = '/ 로 블록 추가 · 글자를 선택해 서식 변경'; root.append(hint);
const live = document.createElement('div'); live.className = 'save-notice'; live.setAttribute('role', 'status'); live.setAttribute('aria-live', 'polite');
const slash = document.createElement('div'); slash.className = 'context-menu slash-menu'; slash.hidden = true; slash.setAttribute('role', 'listbox'); slash.setAttribute('aria-label', '블록 추가');
const bubble = document.createElement('div'); bubble.className = 'context-menu bubble'; bubble.hidden = true; bubble.setAttribute('role', 'toolbar'); bubble.setAttribute('aria-label', '선택한 글자 서식');
const panel = document.createElement('div'); panel.className = 'context-menu utility-panel'; panel.hidden = true;
const blockTools = document.createElement('div'); blockTools.className = 'block-tools'; blockTools.hidden = true; blockTools.setAttribute('role', 'toolbar'); blockTools.setAttribute('aria-label', '블록 도구');
const addBlock = button('', () => openBlockMenu('insert'), '이 블록 아래에 추가 · /');
addBlock.className = 'block-add'; addBlock.setAttribute('aria-haspopup', 'listbox');
addBlock.innerHTML = '<svg viewBox="0 0 20 20" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><path d="M10 4v12M4 10h12"/></svg>';
const blockHandle = document.createElement('button'); blockHandle.type = 'button'; blockHandle.className = 'block-handle'; blockHandle.draggable = true;
blockHandle.title = '블록 메뉴 · 같은 단계 안에서 드래그하여 이동'; blockHandle.setAttribute('aria-label', blockHandle.title); blockHandle.setAttribute('aria-haspopup', 'listbox');
blockHandle.innerHTML = '<svg viewBox="0 0 20 20" width="18" height="18" fill="currentColor" aria-hidden="true"><circle cx="7" cy="5" r="1.3"/><circle cx="13" cy="5" r="1.3"/><circle cx="7" cy="10" r="1.3"/><circle cx="13" cy="10" r="1.3"/><circle cx="7" cy="15" r="1.3"/><circle cx="13" cy="15" r="1.3"/></svg>';
blockHandle.addEventListener('click', () => openBlockMenu('manage')); blockTools.append(addBlock, blockHandle);
const dropLine = document.createElement('div'); dropLine.className = 'block-drop-line'; dropLine.hidden = true;
let copyTarget: HTMLElement | null = null;
const copy = button('복사', async () => {
  if (!copyTarget) return;
  const text = copyTarget.textContent ?? '';
  try {
    if (navigator.clipboard?.writeText) await navigator.clipboard.writeText(text);
    else {
      const input = document.createElement('textarea'); input.value = text; input.style.position = 'fixed'; input.style.opacity = '0';
      document.body.append(input); input.select(); const copied = document.execCommand('copy'); input.remove();
      if (!copied) throw new Error('clipboard');
    }
    copy.textContent = '복사됨';
  } catch { copy.textContent = '복사 실패'; }
}, '코드 블록 복사');
copy.className = 'code-copy'; copy.hidden = true;
document.body.append(root, live, slash, bubble, panel, copy, blockTools, dropLine);
const vault = mountVault(root, (kind, payload) => send(kind, false, payload));
const sizedCaret = createSizedCaret();
function send(kind: string, includeContent = false, extra = {}) {
  if (!token) return;
  window.ipc?.postMessage(JSON.stringify({ kind, token, revision, ...(includeContent ? { markdown: current() } : {}), ...extra }));
}
function current() { return editor ? source.serialize(editor.getJSON()) : lastMarkdown; }
function report() {
  if (muted || composing || !editor) return;
  const markdown = current();
  if (markdown !== lastMarkdown) { lastMarkdown = markdown; revision++; send('change', true); }
}
function commit(kind: string, extra: object = {}) {
  if (composing || editor?.view.composing) { pending.push({ kind, extra }); return; }
  report(); send(kind, true, extra);
}
function button(label: string, action: () => void, title = label) {
  const b = document.createElement('button'); b.type = 'button'; b.textContent = label; b.title = title; b.setAttribute('aria-label', title);
  b.addEventListener('mousedown', e => e.preventDefault()); b.addEventListener('click', action); return b;
}
function position(element: HTMLElement, left: number, top: number) {
  element.style.left = Math.max(8, Math.min(left, innerWidth - element.offsetWidth - 8)) + 'px';
  element.style.top = Math.max(8, Math.min(top, innerHeight - element.offsetHeight - 8)) + 'px';
}
function contentBounds(): Rectangle {
  const canvas = root.parentElement!.getBoundingClientRect();
  return { left: Math.max(8, canvas.left + 8), right: Math.min(innerWidth - 8, canvas.right - 8), top: 8, bottom: innerHeight - 8 };
}
function selectionAnchor(): Rectangle | undefined {
  if (!editor) return;
  const { from, to } = editor.state.selection;
  try {
    // ProseMirror emits selection updates before WebKit commits window.getSelection().
    const start = editor.view.domAtPos(from), end = editor.view.domAtPos(to);
    const range = document.createRange(); range.setStart(start.node, start.offset); range.setEnd(end.node, end.offset);
    const rects = [...range.getClientRects()].filter(rect => rect.height > 0 && rect.bottom > 8 && rect.top < innerHeight - 8);
    if (rects.length) return { left: Math.min(...rects.map(rect => rect.left)), right: Math.max(...rects.map(rect => rect.right)), top: rects[0].top, bottom: rects.at(-1)!.bottom };
  } catch { /* Non-text node selections use the editor's own position geometry. */ }
  const start = editor.view.coordsAtPos(from), end = editor.view.coordsAtPos(to);
  if (end.bottom < 8 || start.top > innerHeight - 8) return;
  return { left: Math.min(start.left, end.left), right: Math.max(start.right, end.right), top: start.top, bottom: end.bottom };
}
function floatNear(element: HTMLElement, anchor: Rectangle, preferred: 'above' | 'below') {
  const bounds = contentBounds();
  element.hidden = false; element.style.maxWidth = `${bounds.right - bounds.left}px`; element.style.maxHeight = '';
  const placement = floatingPlacement(anchor, element.offsetWidth, element.offsetHeight, bounds, preferred);
  element.style.left = `${placement.left}px`; element.style.top = `${placement.top}px`; element.style.maxHeight = `${placement.maxHeight}px`; element.dataset.side = placement.side;
}
function dismissTools() {
  if (editor) { const selection = editor.state.selection; dismissedSelection = `${selection.from}:${selection.to}`; dismissedSlash = `${selection.from}:${selection.$from.parent.textContent}`; }
  blockMenu = false; menuTarget = undefined; bubble.hidden = slash.hidden = panel.hidden = blockTools.hidden = true;
}
function currentBlock(): BlockTarget | undefined {
  if (!editor) return;
  if (hoverTarget && editor.state.doc.nodeAt(hoverTarget.from) === hoverTarget.node) return hoverTarget;
  return blockAt(editor.state.doc, editor.state.selection.from);
}
function openBlockMenu(mode: 'insert' | 'manage') {
  if (!editor || composing || editor.view.composing) return;
  menuTarget = currentBlock(); if (!menuTarget) return;
  menuMode = mode; blockMenu = true; dismissedSlash = ''; slashIndex = 0; showBlockMenu();
}
function blockElement(block: BlockTarget): HTMLElement | undefined {
  const node = editor?.view.nodeDOM(block.from); return node instanceof HTMLElement ? node : node?.parentElement ?? undefined;
}
function updateBlockTools() {
  const target = currentBlock(), element = target && blockElement(target);
  blockTools.hidden = !editor || !editor.isEditable || composing || editor.view.composing || root.hidden || !element || !panel.hidden;
  if (blockTools.hidden || !element) return;
  const rect = element.getBoundingClientRect(), style = getComputedStyle(element), line = parseFloat(style.lineHeight) || 24;
  const top = rect.top + (Math.min(line, rect.height) - 24) / 2;
  if (top < 4 || top + 24 > innerHeight - 4) { blockTools.hidden = true; return; }
  const contentLeft = rect.left - (target?.node.type.name === 'listItem' ? 22 : 0);
  const left = Math.max(contentBounds().left, contentLeft - 50);
  if (left + 44 > contentLeft - 4) { blockTools.hidden = true; return; }
  blockTools.style.left = `${left}px`; blockTools.style.top = `${top}px`;
}
function menuIcon(name: string) {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg'); svg.setAttribute('viewBox', '0 0 24 24'); svg.setAttribute('aria-hidden', 'true');
  svg.setAttribute('fill', 'none'); svg.setAttribute('stroke', 'currentColor'); svg.setAttribute('stroke-width', '1.6'); svg.setAttribute('stroke-linecap', 'round'); svg.setAttribute('stroke-linejoin', 'round');
  const paths: Record<string, string> = {
    text: '<path d="M5 5h14M12 5v14M8 19h8"/>', bullet: '<circle cx="5" cy="6" r="1"/><circle cx="5" cy="12" r="1"/><circle cx="5" cy="18" r="1"/><path d="M10 6h10M10 12h10M10 18h10"/>',
    ordered: '<path d="M4 4h1v6M3 10h4M3 14c0-2 4-2 4 0 0 1-4 3-4 5h4M11 7h9M11 17h9"/>', task: '<rect x="4" y="4" width="16" height="16" rx="3"/><path d="m8 12 3 3 5-6"/>',
    quote: '<path d="M4 6h6v7H5l-1 5M14 6h6v7h-5l-1 5"/>', code: '<path d="m8 6-5 6 5 6m8-12 5 6-5 6M14 4l-4 16"/>', table: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9h18M9 4v16M15 4v16"/>',
    rule: '<path d="M4 12h16"/>', copy: '<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V4H4v12h4"/>', delete: '<path d="M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13M10 10v7M14 10v7"/>', up: '<path d="m6 11 6-6 6 6M12 5v15"/>', down: '<path d="m6 13 6 6 6-6M12 4v15"/>',
  };
  if (/^h[1-4]$/.test(name)) svg.innerHTML = `<text x="2" y="17" fill="currentColor" stroke="none" font-size="15" font-weight="600">H${name[1]}</text>`;
  else svg.innerHTML = paths[name] ?? paths.text;
  return svg;
}
function actions(): Action[] {
  return [
    { label: '텍스트', hint: '⌘⌥0', icon: 'text', run: chain => chain.setParagraph().run() },
    ...([1, 2, 3, 4] as const).map(level => ({ label: `제목 ${level}`, hint: `⌘⌥${level}`, icon: `h${level}`, run: (chain: ChainedCommands) => chain.setHeading({ level }).run() })),
    { label: '글머리 목록', hint: '-', icon: 'bullet', run: chain => chain.toggleBulletList().run() },
    { label: '번호 목록', hint: '1.', icon: 'ordered', run: chain => chain.toggleOrderedList().run() },
    { label: '할 일', hint: '[]', icon: 'task', run: chain => chain.toggleTaskList().run() },
    { label: '인용', hint: '>', icon: 'quote', run: chain => chain.toggleBlockquote().run() },
    { label: '코드 블록', hint: '```', icon: 'code', run: chain => chain.toggleCodeBlock().run() },
    { label: '표', hint: '3 × 3', icon: 'table', run: chain => chain.insertTable({ rows: 3, cols: 3, withHeaderRow: true }).run() },
    { label: '구분선', hint: '---', icon: 'rule', run: chain => chain.setHorizontalRule().run() },
  ];
}
function runSlash(index: number) {
  if (!editor || !slashItems[index] || slashItems[index].disabled) return;
  const action = slashItems[index], convert = blockMenu, target = menuTarget, mode = menuMode;
  blockMenu = false; menuTarget = undefined; hoverTarget = undefined; slash.hidden = true;
  if (target && editor.state.doc.nodeAt(target.from) !== target.node) { live.textContent = '블록 내용이 바뀌었어요. 메뉴를 다시 열어주세요.'; live.classList.add('visible'); return; }
  let chain = editor.chain().focus();
  if (!convert && slashRange) chain = chain.deleteRange(slashRange);
  if (convert && target && !action.operation) {
    chain = chain.command(({ tr }) => {
      const current = blockAt(tr.doc, target.from + 1); if (!current || current.from !== target.from) return false;
      if (mode === 'insert') insertAfterBlock(tr, current);
      else tr.setSelection(TextSelection.near(tr.doc.resolve(current.from + 1)));
      const selected = blockAt(tr.doc, tr.selection.from); if (selected) unwrapListItem(tr, selected);
      closeHistory(tr);
      return true;
    });
  }
  slashRange = undefined;
  if (!action.run(chain)) { live.textContent = '이 블록에서는 이 변경을 적용할 수 없어요.'; live.classList.add('visible'); }
  editor.view.focus(); updateMenus();
}
function showBlockMenu(query = '') {
  if (!editor) return;
  const focusMenu = blockMenu && (bubble.contains(document.activeElement) || slash.contains(document.activeElement) || blockTools.contains(document.activeElement));
  const available = actions();
  if (blockMenu && menuMode === 'manage' && menuTarget) {
    const target = menuTarget;
    available.push(
      { label: '복제', hint: '', icon: 'copy', operation: true, run: chain => chain.command(({ tr }) => { duplicateBlock(tr, target); return true; }).run() },
      { label: '위로 이동', hint: '', icon: 'up', operation: true, disabled: target.index === 0, run: chain => chain.command(({ tr }) => moveBlockBy(tr, target, -1)).run() },
      { label: '아래로 이동', hint: '', icon: 'down', operation: true, disabled: target.index === editor.state.doc.resolve(target.from).parent.childCount - 1, run: chain => chain.command(({ tr }) => moveBlockBy(tr, target, 1)).run() },
      { label: '삭제', hint: '', icon: 'delete', danger: true, operation: true, run: chain => chain.command(({ tr }) => { removeBlock(tr, target); return true; }).run() },
    );
  }
  slashItems = available.filter(a => (a.label + a.hint + a.icon).toLocaleLowerCase().includes(query.toLocaleLowerCase()));
  slashIndex = Math.min(slashIndex, Math.max(0, slashItems.length - 1));
  slash.replaceChildren();
  const title = document.createElement('div'); title.className = 'block-menu-title'; title.textContent = blockMenu && menuMode !== 'insert' ? '블록 변경' : '블록 추가'; slash.append(title);
  for (const [index, action] of slashItems.entries()) {
    const b = button('', () => runSlash(index), action.label); b.id = `block-option-${index}`; b.setAttribute('role', 'option'); b.setAttribute('aria-selected', String(index === slashIndex));
    b.disabled = Boolean(action.disabled);
    if (action.danger) b.classList.add('block-action-danger'); if (action.operation) b.classList.add('block-operation');
    const label = document.createElement('span'); label.className = 'block-menu-label'; label.textContent = action.label;
    const detail = document.createElement('kbd'); detail.textContent = action.hint; b.append(menuIcon(action.icon), label, detail); slash.append(b);
  }
  if (!slashItems.length) { const empty = document.createElement('p'); empty.textContent = '일치하는 블록이 없어요'; slash.append(empty); }
  const footer = button('Esc 닫기', () => { dismissTools(); editor?.view.focus(); }); footer.className = 'block-menu-footer'; slash.append(footer);
  slash.setAttribute('aria-activedescendant', `block-option-${slashIndex}`);
  const targetElement = menuTarget && blockElement(menuTarget);
  const rect = targetElement ? targetElement.getBoundingClientRect() : editor.view.coordsAtPos(editor.state.selection.from);
  const top = targetElement ? Math.max(8, Math.min(innerHeight - 32, parseFloat(blockTools.style.top) || rect.top)) : rect.top;
  floatNear(slash, { left: rect.left, top, bottom: targetElement ? top + 24 : rect.bottom, right: rect.left + Math.min(288, contentBounds().right - rect.left) }, 'below');
  bubble.hidden = true;
  if (focusMenu) slash.querySelector<HTMLButtonElement>('[aria-selected=true]')?.focus({ preventScroll: true });
}
function stepBlockMenu(direction: number) {
  for (let step = 0; step < slashItems.length; step++) {
    slashIndex = (slashIndex + direction + slashItems.length) % slashItems.length;
    if (!slashItems[slashIndex].disabled) break;
  }
}
function updateMenus() {
  sizedCaret.sync(editor, composing);
  if (!editor || composing || !editor.isEditable || root.hidden) { bubble.hidden = slash.hidden = blockTools.hidden = true; return; }
  const { from, to, $from, empty } = editor.state.selection;
  const inside = editor.isFocused || bubble.contains(document.activeElement) || slash.contains(document.activeElement);
  const anchor = selectionAnchor();
  bubble.hidden = empty || !inside || !anchor || dismissedSelection === `${from}:${to}` || !panel.hidden || editor.isActive('codeBlock') || editor.isActive('preservedMarkdown');
  if (!bubble.hidden && anchor) {
    floatNear(bubble, anchor, 'above');
    for (const child of bubble.querySelectorAll<HTMLButtonElement>('button[data-mark]')) child.setAttribute('aria-pressed', String(editor.isActive(child.dataset.mark!)));
    const kind = bubble.querySelector<HTMLButtonElement>('[data-block-type]');
    if (kind) kind.textContent = editor.isActive('heading') ? `제목 ${editor.getAttributes('heading').level}` : editor.isActive('taskList') ? '할 일' : editor.isActive('bulletList') ? '목록' : editor.isActive('orderedList') ? '번호' : '본문';
  }
  const raw = editor.isActive('codeBlock') || editor.isActive('preservedMarkdown');
  updateBlockTools();
  if (blockMenu) { showBlockMenu(); return; }
  const before = $from.parent.textBetween(0, $from.parentOffset, '\n', '\0');
  const match = empty && inside && !raw && /^\/([^\s/]*)$/.exec(before);
  if (!match || dismissedSlash === `${from}:${$from.parent.textContent}`) { slash.hidden = true; slashRange = undefined; return; }
  slashRange = { from: from - before.length, to: from };
  showBlockMenu(match[1]);
}
function linkPanel() {
  if (!editor) return;
  panel.replaceChildren(); panel.hidden = false;
  const field = document.createElement('input'); field.type = 'url'; field.placeholder = 'https://'; field.setAttribute('aria-label', '링크 주소'); field.value = editor.getAttributes('link').href ?? '';
  const apply = () => {
    const href = field.value.trim();
    if (!href) editor!.chain().focus().extendMarkRange('link').unsetLink().run();
    else if (/^(https?:|mailto:)/i.test(href)) editor!.chain().focus().extendMarkRange('link').setLink({ href }).run();
    else { field.setCustomValidity('https:// 또는 mailto: 주소를 입력해주세요'); field.reportValidity(); return; }
    panel.hidden = true;
  };
  field.addEventListener('keydown', e => { if (e.key === 'Enter') apply(); if (e.key === 'Escape') { panel.hidden = true; editor!.commands.focus(); } });
  panel.append(field, button('적용', apply), button('닫기', () => { panel.hidden = true; editor!.commands.focus(); }));
  const anchor = selectionAnchor(); if (anchor) floatNear(panel, anchor, 'above'); else position(panel, Math.max(8, innerWidth / 2 - 160), 20); bubble.hidden = true; field.focus();
}
const blockType = button('본문', () => { menuMode = 'convert'; menuTarget = undefined; blockMenu = true; dismissedSlash = ''; showBlockMenu(); }, '문단 종류 변경'); blockType.dataset.blockType = ''; blockType.setAttribute('aria-haspopup', 'listbox'); bubble.append(blockType);
for (const [label, mark, title, command] of [
  ['B', 'bold', '굵게 · ⌘B', 'toggleBold'], ['I', 'italic', '기울임 · ⌘I', 'toggleItalic'], ['S', 'strike', '취소선', 'toggleStrike'], ['코드', 'code', '인라인 코드', 'toggleCode'],
] as const) {
  const b = button(label, () => { (editor!.chain().focus()[command]() as any).run(); }, title); b.dataset.mark = mark; bubble.append(b);
}
bubble.append(button('링크', linkPanel));
function find() {
  if (!editor) return;
  panel.replaceChildren(); panel.hidden = false; panel.setAttribute('aria-label', '문서에서 찾기');
  const input = document.createElement('input'); input.type = 'search'; input.placeholder = '문서에서 찾기'; input.setAttribute('aria-label', '검색어');
  const count = document.createElement('span'); count.className = 'result-count'; let hits: { from: number; to: number }[] = [], index = -1;
  const select = (delta: number) => { if (!hits.length) return; index = (index + delta + hits.length) % hits.length; editor!.commands.setTextSelection(hits[index]); editor!.commands.scrollIntoView(); count.textContent = `${index + 1} / ${hits.length}`; };
  input.addEventListener('input', () => { hits = []; index = -1; const q = input.value.toLocaleLowerCase(); if (q) editor!.state.doc.descendants((node, pos) => { if (!node.isText) return; let offset = 0; const text = node.text!.toLocaleLowerCase(); while ((offset = text.indexOf(q, offset)) >= 0) { hits.push({ from: pos + offset, to: pos + offset + q.length }); offset += q.length; } }); count.textContent = hits.length ? `${hits.length}개` : '결과 없음'; if (hits.length) select(1); });
  input.addEventListener('keydown', e => { if (e.key === 'Enter') { e.preventDefault(); select(e.shiftKey ? -1 : 1); } if (e.key === 'Escape') { panel.hidden = true; editor!.commands.focus(); } });
  panel.append(input, count, button('이전', () => select(-1)), button('다음', () => select(1)), button('닫기', () => { panel.hidden = true; editor!.commands.focus(); })); position(panel, innerWidth - 390, 12); input.focus();
}
function headings() { const entries: { label: string; pos: number; level: number }[] = []; editor?.state.doc.descendants((node, pos) => { if (node.type.name === 'heading') entries.push({ label: node.textContent || '빈 제목', pos, level: node.attrs.level }); }); return entries; }
function toc() {
  panel.replaceChildren(); panel.hidden = false; panel.setAttribute('aria-label', '목차');
  panel.append(button('목차 닫기', () => { panel.hidden = true; }));
  const entries = headings();
  if (!entries.length) { const text = document.createElement('p'); text.textContent = '제목을 추가하면 목차에 표시돼요'; panel.append(text); }
  entries.forEach((entry, index) => { const b = button(entry.label, () => command('scrollToHeading', { index })); b.style.paddingInlineStart = `${12 + (entry.level - 1) * 12}px`; panel.append(b); });
  position(panel, innerWidth - 320, 12);
}
function command(name: string, payload?: any) {
  if (!editor) return;
  switch (name) {
    case 'save': commit('save'); break;
    case 'close': commit('close'); break;
    case 'requestFlush': case 'flush': commit('flush', payload?.requestId ? { requestId: payload.requestId } : {}); break;
    case 'undo': editor.commands.undo(); break;
    case 'redo': editor.commands.redo(); break;
    case 'focus': editor.commands.focus(); break;
    case 'find': find(); break;
    case 'toc': case 'outline': toc(); break;
    case 'scrollToHeading': {
      const slug = (text: string) => text.normalize('NFKC').toLocaleLowerCase().replace(/[^\p{L}\p{N}\s-]/gu, '').trim().replace(/\s+/g, '-');
      const anchor = typeof payload?.anchor === 'string' ? payload.anchor.replace(/^#/, '') : undefined;
      const item = anchor === undefined ? headings()[payload?.index ?? 0] : headings().find(item => item.label === anchor || slug(item.label) === slug(anchor));
      if (item) { editor.commands.setTextSelection(item.pos + 1); editor.commands.scrollIntoView(); panel.hidden = true; }
      else if (anchor !== undefined) { live.textContent = '문서에서 해당 제목을 찾지 못했어요.'; live.classList.add('visible'); }
      break;
    }
    case 'zoomIn': case 'zoom-in': zoom = Math.min(1.6, zoom + 0.1); break;
    case 'zoomOut': case 'zoom-out': zoom = Math.max(0.8, zoom - 0.1); break;
    case 'zoom-reset': zoom = 1; break;
  }
  document.documentElement.style.setProperty('--document-scale', String(zoom));
  if (name.startsWith('zoom')) updateMenus();
}
function setContent(data: Content) {
  if (composing) return;
  muted = true; source = new DocumentSource(); lastMarkdown = data.markdown; revision = data.revision;
  editor?.commands.setContent(source.load(data.markdown), { emitUpdate: false }); muted = false; slash.hidden = true; bubble.hidden = true;
  menuTarget = hoverTarget = undefined; blockMenu = false; resetBlockDrag(); updateBlockTools();
}
window.kasatermEditor = {
  init(data) {
    const ownGeneration = ++generation;
    sizedCaret.hide();
    clearTimeout(compositionTimer); pending = []; composing = false; slashRange = undefined; blockMenu = false; dismissedSelection = dismissedSlash = '';
    slash.hidden = bubble.hidden = panel.hidden = copy.hidden = blockTools.hidden = true; menuTarget = hoverTarget = undefined; resetBlockDrag(); live.textContent = ''; live.classList.remove('visible');
    token = data.token; window.__KASATERM_DOC_TOKEN__ = token; revision = data.revision; lastMarkdown = data.markdown; applyTheme(data.theme); editor?.destroy(); source = new DocumentSource();
    editor = new Editor({ element: mount, extensions: [...extensions(), Placeholder.configure({ placeholder: '내용을 입력하거나 / 로 블록을 추가하세요' })], content: source.load(data.markdown), editable: data.editable !== false,
      editorProps: { attributes: { class: 'document-content', role: 'textbox', 'aria-label': '마크다운 문서 본문', 'aria-multiline': 'true', spellcheck: 'false' },
        handleKeyDown(_view, event) {
          if (ownGeneration !== generation) return false;
          if ((event.metaKey || event.ctrlKey) && ['s', 'w', 'f', 'k'].includes(event.key.toLowerCase())) { event.preventDefault(); const key = event.key.toLowerCase(); if (key === 's') commit('save'); else if (key === 'w') commit('close'); else if (key === 'f') find(); else linkPanel(); return true; }
          if (event.isComposing || composing) return false;
          if (event.key === 'Tab' && !event.shiftKey && !bubble.hidden) { event.preventDefault(); bubble.querySelector<HTMLButtonElement>('button')?.focus({ preventScroll: true }); return true; }
          if (!slash.hidden) { if (event.key === 'Escape') { dismissTools(); return true; } if (event.key === 'ArrowDown' || event.key === 'ArrowUp') { event.preventDefault(); stepBlockMenu(event.key === 'ArrowDown' ? 1 : -1); updateMenus(); slash.querySelector('[aria-selected=true]')?.scrollIntoView({ block: 'nearest' }); return true; } if (event.key === 'Enter') { event.preventDefault(); runSlash(slashIndex); return true; } }
          return false;
        },
        handleClick(_view, _pos, event) { if (ownGeneration !== generation) return false; const anchor = (event.target as HTMLElement).closest('a'); if (anchor && (event.metaKey || event.ctrlKey)) { const href = anchor.getAttribute('href') ?? ''; if (/^(https?:|mailto:|wiki:)/i.test(href)) send('open-link', false, { href }); event.preventDefault(); return true; } return false; },
      }, onUpdate: () => { if (ownGeneration !== generation) return; report(); updateMenus(); }, onSelectionUpdate: () => { if (ownGeneration === generation) updateMenus(); }, onFocus: () => { if (ownGeneration === generation) updateMenus(); },
    });
    editor.view.dom.addEventListener('blur', () => { if (ownGeneration === generation) sizedCaret.hide(); });
    editor.view.dom.addEventListener('compositionstart', () => { if (ownGeneration !== generation) return; composing = true; sizedCaret.hide(); blockMenu = false; menuTarget = undefined; resetBlockDrag(); slash.hidden = bubble.hidden = blockTools.hidden = true; send('change'); });
    editor.view.dom.addEventListener('compositionend', () => { if (ownGeneration !== generation) return; composing = false; compositionTimer = setTimeout(() => { if (ownGeneration !== generation) return; report(); const queue = pending; pending = []; queue.forEach(item => commit(item.kind, item.extra)); updateMenus(); }, 0); });
  }, setContent, command, setTheme: applyTheme, ...vault,
  setSaveState(data) { live.textContent = data.state === 'error' ? (data.message || '저장하지 못했어요. 다시 저장해주세요.') : ''; live.classList.toggle('visible', data.state === 'error'); },
};
document.addEventListener('keydown', e => {
  if (e.defaultPrevented || e.isComposing || composing) return;
  const toolFocus = bubble.contains(document.activeElement) || slash.contains(document.activeElement) || blockTools.contains(document.activeElement);
  if (e.key === 'Escape') { dismissTools(); if (toolFocus) editor?.view.focus(); return; }
  if (!slash.hidden && toolFocus && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) {
    e.preventDefault(); stepBlockMenu(e.key === 'ArrowDown' ? 1 : -1);
    updateMenus(); const selected = slash.querySelector<HTMLButtonElement>('[aria-selected=true]'); selected?.focus({ preventScroll: true }); selected?.scrollIntoView({ block: 'nearest' });
  } else if (!slash.hidden && toolFocus && e.key === 'Enter') { e.preventDefault(); runSlash(slashIndex); }
});
document.addEventListener('mousedown', e => { if (!panel.contains(e.target as globalThis.Node) && !bubble.contains(e.target as globalThis.Node) && !slash.contains(e.target as globalThis.Node) && !blockTools.contains(e.target as globalThis.Node)) { panel.hidden = true; blockMenu = false; menuTarget = undefined; if (!root.contains(e.target as globalThis.Node)) dismissTools(); } });
function blockFromElement(target: EventTarget | null): BlockTarget | undefined {
  if (!editor || !(target instanceof HTMLElement)) return;
  const element = target.closest('p,h1,h2,h3,h4,h5,h6,li,blockquote,pre,table,hr');
  if (!element || !editor.view.dom.contains(element)) return;
  try { return blockAt(editor.state.doc, editor.view.posAtDOM(element, 0)); } catch { return; }
}
function resetBlockDrag() { dragSource = dropTarget = undefined; dropLine.hidden = true; blockHandle.classList.remove('is-dragging'); }
document.addEventListener('mousemove', event => {
  if (blockMenu || dragSource || blockTools.contains(event.target as globalThis.Node) || slash.contains(event.target as globalThis.Node)) return;
  hoverTarget = blockFromElement(event.target); updateBlockTools();
});
blockHandle.addEventListener('dragstart', event => {
  if (!editor || composing || editor.view.composing || !event.dataTransfer) { event.preventDefault(); return; }
  dragSource = currentBlock(); if (!dragSource) { event.preventDefault(); return; }
  blockMenu = false; menuTarget = undefined; slash.hidden = bubble.hidden = true;
  event.dataTransfer.effectAllowed = 'move'; event.dataTransfer.setData('application/x-kasaterm-block', 'local');
  const element = blockElement(dragSource); if (element) event.dataTransfer.setDragImage(element, 0, 0);
  blockHandle.classList.add('is-dragging');
});
document.addEventListener('dragover', event => {
  if (!dragSource || !editor) return;
  event.preventDefault(); event.stopPropagation();
  const target = blockFromElement(event.target), element = target && blockElement(target);
  const valid = target && element && target.from !== dragSource.from && target.parentStart === dragSource.parentStart && target.parentDepth === dragSource.parentDepth && editor.state.doc.nodeAt(dragSource.from) === dragSource.node;
  if (!valid || !target || !element) { dropTarget = undefined; dropLine.hidden = true; if (event.dataTransfer) event.dataTransfer.dropEffect = 'none'; return; }
  if (event.dataTransfer) event.dataTransfer.dropEffect = 'move';
  const rect = element.getBoundingClientRect(), after = event.clientY >= rect.top + rect.height / 2;
  dropTarget = { block: target, after }; dropLine.hidden = false;
  const bounds = contentBounds(), left = Math.max(bounds.left, rect.left), right = Math.min(bounds.right, rect.right);
  dropLine.style.left = `${left}px`; dropLine.style.width = `${Math.max(0, right - left)}px`; dropLine.style.top = `${Math.max(4, Math.min(innerHeight - 4, after ? rect.bottom : rect.top))}px`;
}, true);
document.addEventListener('drop', event => {
  if (!dragSource || !editor) return;
  event.preventDefault(); event.stopImmediatePropagation();
  const tr = editor.state.tr, destination = dropTarget;
  if (destination && editor.state.doc.nodeAt(dragSource.from) === dragSource.node && moveBlockTo(tr, dragSource, destination.block, destination.after)) {
    resetBlockDrag(); hoverTarget = undefined; editor.view.dispatch(tr); editor.view.focus(); updateMenus();
  } else {
    resetBlockDrag(); live.textContent = '같은 문서의 같은 단계 안에서만 블록을 이동할 수 있어요.'; live.classList.add('visible');
  }
}, true);
blockHandle.addEventListener('dragend', () => { resetBlockDrag(); updateBlockTools(); });
document.addEventListener('mouseover', e => {
  const target = e.target as HTMLElement;
  const link = target.closest('a'); if (link) link.title = '⌘ 또는 Ctrl 키를 누른 채 클릭하면 링크를 열어요';
  if (target === copy) return;
  copyTarget = target.closest<HTMLElement>('.document-content pre');
  copy.hidden = !copyTarget;
  if (copyTarget) { const rect = copyTarget.getBoundingClientRect(); copy.textContent = '복사'; position(copy, rect.right - 62, rect.top + 8); }
});
window.addEventListener('resize', () => { panel.hidden = true; updateMenus(); });
window.addEventListener('scroll', () => { copy.hidden = true; updateMenus(); }, { passive: true });
send('ready');
