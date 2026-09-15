import { Editor } from '@tiptap/core';
import Placeholder from '@tiptap/extension-placeholder';
import { DocumentSource, extensions } from './document';
import { applyTheme } from './theme';
import { mountVault, type VaultState, type VaultChildren, type VaultSearch } from './vault';
import { floatingPlacement, type Rectangle } from './floating';
import { createSizedCaret } from './caret';
import './editor.css';
import './vault.css';

declare global { interface Window {
  __KASATERM_DOC_TOKEN__?: string;
  ipc?: { postMessage(message: string): void };
  kasatermEditor: { init(data: Init): void; setContent(data: Content): void; setTheme(theme: unknown): void; setVault(data: VaultState): void; setVaultChildren(data: VaultChildren): void; setVaultSearch(data: VaultSearch): void; command(name: string, payload?: any): void; setSaveState(data: { state: string; message?: string }): void };
} }
type Content = { markdown: string; revision: number };
type Init = Content & { token: string; theme?: any; editable?: boolean };
let token = window.__KASATERM_DOC_TOKEN__ ?? '', revision = 0, editor: Editor | undefined;
let source = new DocumentSource(), composing = false, pending: { kind: string; extra: object }[] = [], muted = false;
let lastMarkdown = '', zoom = 1, slashRange: { from: number; to: number } | undefined;
let slashIndex = 0, slashItems: Action[] = [];
let generation = 0, compositionTimer: ReturnType<typeof setTimeout> | undefined;
let dismissedSelection = '', dismissedSlash = '', blockMenu = false;
type Action = { label: string; hint: string; run(): void };
document.body.append(document.createComment('THESIS: Edit the document itself. OWN-WORLD: existing neutral paper, sky focus, system sans. STORY: write, select, format, save. FIRST VIEWPORT: one centered reading measure; contextual tools beside text. FORM: user-pinned Notion/Linear canon. FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, and DESIGN.md'));
const root = document.createElement('main'); root.id = 'document';
const mount = document.createElement('div'); mount.id = 'editor'; root.append(mount);
const hint = document.createElement('div'); hint.className = 'document-hint'; hint.textContent = '/ 로 블록 추가 · 글자를 선택해 서식 변경'; root.append(hint);
const live = document.createElement('div'); live.className = 'save-notice'; live.setAttribute('role', 'status'); live.setAttribute('aria-live', 'polite');
const slash = document.createElement('div'); slash.className = 'context-menu slash-menu'; slash.hidden = true; slash.setAttribute('role', 'listbox'); slash.setAttribute('aria-label', '블록 추가');
const bubble = document.createElement('div'); bubble.className = 'context-menu bubble'; bubble.hidden = true; bubble.setAttribute('role', 'toolbar'); bubble.setAttribute('aria-label', '선택한 글자 서식');
const panel = document.createElement('div'); panel.className = 'context-menu utility-panel'; panel.hidden = true;
const addBlock = button('', () => { blockMenu = true; dismissedSlash = ''; showBlockMenu(); }, '블록 추가 · /');
addBlock.className = 'block-add'; addBlock.hidden = true; addBlock.setAttribute('aria-haspopup', 'listbox');
addBlock.innerHTML = '<svg viewBox="0 0 20 20" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true"><path d="M10 4v12M4 10h12"/></svg>';
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
document.body.append(root, live, slash, bubble, panel, copy, addBlock);
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
  blockMenu = false; bubble.hidden = slash.hidden = panel.hidden = addBlock.hidden = true;
}
function actions(): Action[] {
  const chain = () => editor!.chain().focus();
  return [
    { label: '본문', hint: '일반 텍스트', run: () => { chain().setParagraph().run(); } },
    ...([1, 2, 3] as const).map(level => ({ label: `제목 ${level}`, hint: '#'.repeat(level), run: () => { chain().toggleHeading({ level }).run(); } })),
    { label: '할 일', hint: '체크박스', run: () => { chain().toggleTaskList().run(); } },
    { label: '글머리 목록', hint: '항목 나열', run: () => { chain().toggleBulletList().run(); } },
    { label: '번호 목록', hint: '순서가 있는 항목', run: () => { chain().toggleOrderedList().run(); } },
    { label: '인용', hint: '인용문', run: () => { chain().toggleBlockquote().run(); } },
    { label: '코드 블록', hint: '코드와 서식 보존', run: () => { chain().toggleCodeBlock().run(); } },
    { label: '표', hint: '3열 × 3행', run: () => { chain().insertTable({ rows: 3, cols: 3, withHeaderRow: true }).run(); } },
    { label: '구분선', hint: '내용 나누기', run: () => { chain().setHorizontalRule().run(); } },
  ];
}
function runSlash(index: number) {
  if (!editor || !slashItems[index]) return;
  const convert = blockMenu; blockMenu = false;
  if (!convert && slashRange) editor.chain().focus().deleteRange(slashRange).run();
  slashItems[index].run(); slash.hidden = true; slashRange = undefined;
}
function showBlockMenu(query = '') {
  if (!editor) return;
  const focusMenu = blockMenu && (bubble.contains(document.activeElement) || slash.contains(document.activeElement) || document.activeElement === addBlock);
  slashItems = actions().filter(a => (a.label + a.hint).includes(query));
  slashIndex = Math.min(slashIndex, Math.max(0, slashItems.length - 1));
  slash.replaceChildren();
  for (const [index, action] of slashItems.entries()) {
    const b = button(action.label, () => runSlash(index)); b.id = `block-option-${index}`; b.setAttribute('role', 'option'); b.setAttribute('aria-selected', String(index === slashIndex));
    const detail = document.createElement('span'); detail.textContent = action.hint; b.append(detail); slash.append(b);
  }
  if (!slashItems.length) { const empty = document.createElement('p'); empty.textContent = '일치하는 블록이 없어요'; slash.append(empty); }
  slash.setAttribute('aria-activedescendant', `block-option-${slashIndex}`);
  const rect = editor.view.coordsAtPos(editor.state.selection.from);
  floatNear(slash, { left: rect.left, top: rect.top, bottom: rect.bottom, right: rect.left + Math.min(272, contentBounds().right - rect.left) }, 'below');
  bubble.hidden = true;
  if (focusMenu) slash.querySelector<HTMLButtonElement>('[aria-selected=true]')?.focus({ preventScroll: true });
}
function updateMenus() {
  sizedCaret.sync(editor, composing);
  if (!editor || composing || !editor.isEditable || root.hidden) { bubble.hidden = slash.hidden = addBlock.hidden = true; return; }
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
  const rect = editor.view.coordsAtPos(from);
  const emptyBlock = empty && $from.parent.type.name === 'paragraph' && $from.parent.content.size === 0;
  addBlock.hidden = !emptyBlock || !inside || !panel.hidden || raw || rect.bottom < 8 || rect.top > innerHeight - 8;
  if (!addBlock.hidden) {
    addBlock.style.left = `${Math.max(contentBounds().left - 4, rect.left - 28)}px`;
    addBlock.style.top = `${rect.top + (rect.bottom - rect.top - 22) / 2}px`;
  }
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
const blockType = button('본문', () => { blockMenu = true; dismissedSlash = ''; showBlockMenu(); }, '문단 종류 변경'); blockType.dataset.blockType = ''; blockType.setAttribute('aria-haspopup', 'listbox'); bubble.append(blockType);
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
    case 'scrollToHeading': { const item = headings()[payload?.index ?? 0]; if (item) { editor.commands.setTextSelection(item.pos + 1); editor.commands.scrollIntoView(); panel.hidden = true; } break; }
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
}
window.kasatermEditor = {
  init(data) {
    const ownGeneration = ++generation;
    sizedCaret.hide();
    clearTimeout(compositionTimer); pending = []; composing = false; slashRange = undefined; blockMenu = false; dismissedSelection = dismissedSlash = '';
    slash.hidden = bubble.hidden = panel.hidden = copy.hidden = addBlock.hidden = true; live.textContent = ''; live.classList.remove('visible');
    token = data.token; window.__KASATERM_DOC_TOKEN__ = token; revision = data.revision; lastMarkdown = data.markdown; applyTheme(data.theme); editor?.destroy(); source = new DocumentSource();
    editor = new Editor({ element: mount, extensions: [...extensions(), Placeholder.configure({ placeholder: '내용을 입력하거나 / 로 블록을 추가하세요' })], content: source.load(data.markdown), editable: data.editable !== false,
      editorProps: { attributes: { class: 'document-content', role: 'textbox', 'aria-label': '마크다운 문서 본문', 'aria-multiline': 'true', spellcheck: 'false' },
        handleKeyDown(_view, event) {
          if (ownGeneration !== generation) return false;
          if ((event.metaKey || event.ctrlKey) && ['s', 'w', 'f', 'k'].includes(event.key.toLowerCase())) { event.preventDefault(); const key = event.key.toLowerCase(); if (key === 's') commit('save'); else if (key === 'w') commit('close'); else if (key === 'f') find(); else linkPanel(); return true; }
          if (event.isComposing || composing) return false;
          if (event.key === 'Tab' && !event.shiftKey && !bubble.hidden) { event.preventDefault(); bubble.querySelector<HTMLButtonElement>('button')?.focus({ preventScroll: true }); return true; }
          if (!slash.hidden) { if (event.key === 'Escape') { dismissTools(); return true; } if (event.key === 'ArrowDown' || event.key === 'ArrowUp') { event.preventDefault(); slashIndex = (slashIndex + (event.key === 'ArrowDown' ? 1 : -1) + slashItems.length) % Math.max(1, slashItems.length); updateMenus(); slash.querySelector('[aria-selected=true]')?.scrollIntoView({ block: 'nearest' }); return true; } if (event.key === 'Enter') { event.preventDefault(); runSlash(slashIndex); return true; } }
          return false;
        },
        handleClick(_view, _pos, event) { if (ownGeneration !== generation) return false; const anchor = (event.target as HTMLElement).closest('a'); if (anchor && (event.metaKey || event.ctrlKey)) { const href = anchor.getAttribute('href') ?? ''; if (/^(https?:|mailto:)/i.test(href)) send('open-link', false, { href }); event.preventDefault(); return true; } return false; },
      }, onUpdate: () => { if (ownGeneration !== generation) return; report(); updateMenus(); }, onSelectionUpdate: () => { if (ownGeneration === generation) updateMenus(); }, onFocus: () => { if (ownGeneration === generation) updateMenus(); },
    });
    editor.view.dom.addEventListener('blur', () => { if (ownGeneration === generation) sizedCaret.hide(); });
    editor.view.dom.addEventListener('compositionstart', () => { if (ownGeneration !== generation) return; composing = true; sizedCaret.hide(); blockMenu = false; slash.hidden = bubble.hidden = addBlock.hidden = true; send('change'); });
    editor.view.dom.addEventListener('compositionend', () => { if (ownGeneration !== generation) return; composing = false; compositionTimer = setTimeout(() => { if (ownGeneration !== generation) return; report(); const queue = pending; pending = []; queue.forEach(item => commit(item.kind, item.extra)); updateMenus(); }, 0); });
  }, setContent, command, setTheme: applyTheme, ...vault,
  setSaveState(data) { live.textContent = data.state === 'error' ? (data.message || '저장하지 못했어요. 다시 저장해주세요.') : ''; live.classList.toggle('visible', data.state === 'error'); },
};
document.addEventListener('keydown', e => {
  if (e.defaultPrevented || e.isComposing || composing) return;
  const toolFocus = bubble.contains(document.activeElement) || slash.contains(document.activeElement) || document.activeElement === addBlock;
  if (e.key === 'Escape') { dismissTools(); if (toolFocus) editor?.view.focus(); return; }
  if (!slash.hidden && toolFocus && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) {
    e.preventDefault(); slashIndex = (slashIndex + (e.key === 'ArrowDown' ? 1 : -1) + slashItems.length) % Math.max(1, slashItems.length);
    updateMenus(); const selected = slash.querySelector<HTMLButtonElement>('[aria-selected=true]'); selected?.focus({ preventScroll: true }); selected?.scrollIntoView({ block: 'nearest' });
  } else if (!slash.hidden && toolFocus && e.key === 'Enter') { e.preventDefault(); runSlash(slashIndex); }
});
document.addEventListener('mousedown', e => { if (!panel.contains(e.target as globalThis.Node) && !bubble.contains(e.target as globalThis.Node) && !slash.contains(e.target as globalThis.Node) && !addBlock.contains(e.target as globalThis.Node)) { panel.hidden = true; blockMenu = false; if (!root.contains(e.target as globalThis.Node)) dismissTools(); } });
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
