import { Editor } from '@tiptap/core';
import Placeholder from '@tiptap/extension-placeholder';
import { DocumentSource, extensions } from './document';
import { applyTheme } from './theme';
import { mountVault, type VaultState, type VaultChildren, type VaultSearch } from './vault';
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
type Action = { label: string; hint: string; run(): void };
document.body.append(document.createComment('THESIS: Edit the document itself. OWN-WORLD: existing neutral paper, sky focus, system sans. STORY: write, select, format, save. FIRST VIEWPORT: one centered reading measure; contextual tools beside text. FORM: user-pinned Notion/Linear canon. FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, and DESIGN.md'));
const root = document.createElement('main'); root.id = 'document';
const mount = document.createElement('div'); mount.id = 'editor'; root.append(mount);
const hint = document.createElement('div'); hint.className = 'document-hint'; hint.textContent = '/ 로 블록 추가 · 글자를 선택해 서식 변경'; root.append(hint);
const live = document.createElement('div'); live.className = 'save-notice'; live.setAttribute('role', 'status'); live.setAttribute('aria-live', 'polite');
const slash = document.createElement('div'); slash.className = 'context-menu slash-menu'; slash.hidden = true; slash.setAttribute('role', 'listbox'); slash.setAttribute('aria-label', '블록 추가');
const bubble = document.createElement('div'); bubble.className = 'context-menu bubble'; bubble.hidden = true; bubble.setAttribute('role', 'toolbar'); bubble.setAttribute('aria-label', '선택한 글자 서식');
const panel = document.createElement('div'); panel.className = 'context-menu utility-panel'; panel.hidden = true;
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
document.body.append(root, live, slash, bubble, panel, copy);
const vault = mountVault(root, (kind, payload) => send(kind, false, payload));
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
  if (!slashRange || !slashItems[index]) return;
  editor!.chain().focus().deleteRange(slashRange).run(); slashItems[index].run(); slash.hidden = true; slashRange = undefined;
}
function updateMenus() {
  if (!editor || composing) return;
  const { from, to, $from, empty } = editor.state.selection;
  bubble.hidden = empty || !editor.isEditable || editor.isActive('codeBlock') || editor.isActive('preservedMarkdown');
  if (!bubble.hidden) {
    const a = editor.view.coordsAtPos(from), b = editor.view.coordsAtPos(to);
    position(bubble, (a.left + b.left) / 2 - bubble.offsetWidth / 2, a.top - bubble.offsetHeight - 8);
    for (const child of bubble.querySelectorAll<HTMLButtonElement>('button[data-mark]')) child.setAttribute('aria-pressed', String(editor.isActive(child.dataset.mark!)));
  }
  const before = $from.parent.textBetween(0, $from.parentOffset, '\n', '\0');
  const match = empty && !editor.isActive('codeBlock') && !editor.isActive('preservedMarkdown') && /^\/([^\s/]*)$/.exec(before);
  if (!match) { slash.hidden = true; slashRange = undefined; return; }
  slashRange = { from: from - before.length, to: from };
  slashItems = actions().filter(a => (a.label + a.hint).includes(match[1]));
  slashIndex = Math.min(slashIndex, Math.max(0, slashItems.length - 1));
  slash.replaceChildren();
  for (const [index, action] of slashItems.entries()) {
    const b = button(action.label, () => runSlash(index)); b.setAttribute('role', 'option'); b.setAttribute('aria-selected', String(index === slashIndex));
    const detail = document.createElement('span'); detail.textContent = action.hint; b.append(detail); slash.append(b);
  }
  if (!slashItems.length) { const empty = document.createElement('p'); empty.textContent = '일치하는 블록이 없어요'; slash.append(empty); }
  slash.hidden = false; const rect = editor.view.coordsAtPos(from); position(slash, rect.left, rect.bottom + 8);
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
  position(panel, Math.max(8, innerWidth / 2 - 160), 20); field.focus();
}
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
}
function setContent(data: Content) {
  if (composing) return;
  muted = true; source = new DocumentSource(); lastMarkdown = data.markdown; revision = data.revision;
  editor?.commands.setContent(source.load(data.markdown), { emitUpdate: false }); muted = false; slash.hidden = true; bubble.hidden = true;
}
window.kasatermEditor = {
  init(data) {
    const ownGeneration = ++generation;
    clearTimeout(compositionTimer); pending = []; composing = false; slashRange = undefined;
    slash.hidden = bubble.hidden = panel.hidden = copy.hidden = true; live.textContent = ''; live.classList.remove('visible');
    token = data.token; window.__KASATERM_DOC_TOKEN__ = token; revision = data.revision; lastMarkdown = data.markdown; applyTheme(data.theme); editor?.destroy(); source = new DocumentSource();
    editor = new Editor({ element: mount, extensions: [...extensions(), Placeholder.configure({ placeholder: '내용을 입력하거나 / 로 블록을 추가하세요' })], content: source.load(data.markdown), editable: data.editable !== false,
      editorProps: { attributes: { class: 'document-content', role: 'textbox', 'aria-label': '마크다운 문서 본문', 'aria-multiline': 'true', spellcheck: 'false' },
        handleKeyDown(_view, event) {
          if (ownGeneration !== generation) return false;
          if ((event.metaKey || event.ctrlKey) && ['s', 'w', 'f', 'k'].includes(event.key.toLowerCase())) { event.preventDefault(); const key = event.key.toLowerCase(); if (key === 's') commit('save'); else if (key === 'w') commit('close'); else if (key === 'f') find(); else linkPanel(); return true; }
          if (event.isComposing || composing) return false;
          if (!slash.hidden) { if (event.key === 'Escape') { slash.hidden = true; return true; } if (event.key === 'ArrowDown' || event.key === 'ArrowUp') { event.preventDefault(); slashIndex = (slashIndex + (event.key === 'ArrowDown' ? 1 : -1) + slashItems.length) % Math.max(1, slashItems.length); updateMenus(); return true; } if (event.key === 'Enter') { event.preventDefault(); runSlash(slashIndex); return true; } }
          return false;
        },
        handleClick(_view, _pos, event) { if (ownGeneration !== generation) return false; const anchor = (event.target as HTMLElement).closest('a'); if (anchor && (event.metaKey || event.ctrlKey)) { const href = anchor.getAttribute('href') ?? ''; if (/^(https?:|mailto:)/i.test(href)) send('open-link', false, { href }); event.preventDefault(); return true; } return false; },
      }, onUpdate: () => { if (ownGeneration !== generation) return; report(); updateMenus(); }, onSelectionUpdate: () => { if (ownGeneration === generation) updateMenus(); },
    });
    editor.view.dom.addEventListener('compositionstart', () => { if (ownGeneration !== generation) return; composing = true; slash.hidden = bubble.hidden = true; send('change'); });
    editor.view.dom.addEventListener('compositionend', () => { if (ownGeneration !== generation) return; composing = false; compositionTimer = setTimeout(() => { if (ownGeneration !== generation) return; report(); const queue = pending; pending = []; queue.forEach(item => commit(item.kind, item.extra)); updateMenus(); }, 0); });
  }, setContent, command, setTheme: applyTheme, ...vault,
  setSaveState(data) { live.textContent = data.state === 'error' ? (data.message || '저장하지 못했어요. 다시 저장해주세요.') : ''; live.classList.toggle('visible', data.state === 'error'); },
};
document.addEventListener('keydown', e => { if (e.key === 'Escape') { panel.hidden = true; bubble.hidden = true; slash.hidden = true; } });
document.addEventListener('mousedown', e => { if (!panel.contains(e.target as globalThis.Node) && !bubble.contains(e.target as globalThis.Node) && !slash.contains(e.target as globalThis.Node)) panel.hidden = true; });
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
