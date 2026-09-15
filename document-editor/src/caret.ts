import type { Editor } from '@tiptap/core';

/** A visual caret only: WebKit retains selection, input, composition and accessibility ownership. */
export function createSizedCaret() {
  const element = document.createElement('span'); element.className = 'sized-caret'; element.hidden = true; element.setAttribute('aria-hidden', 'true'); document.body.append(element);
  let owner: HTMLElement | undefined;
  const forcedColors = matchMedia('(forced-colors: active)');
  function hide() { element.hidden = true; owner?.classList.remove('has-sized-caret'); owner = undefined; }
  return {
    hide,
    sync(editor: Editor | undefined, composing: boolean) {
      if (!editor || editor.isDestroyed || composing || editor.view.composing || !editor.isEditable || !editor.isFocused || !editor.view.hasFocus() || forcedColors.matches || !editor.state.selection.empty || !editor.state.selection.$from.parent.isTextblock || editor.isActive('preservedMarkdown')) { hide(); return; }
      const dom = editor.view.dom;
      if (!dom.isConnected || dom.closest('[hidden]')) { hide(); return; }
      const pos = editor.state.selection.from;
      let fontElement: HTMLElement = dom;
      try {
        const point = editor.view.domAtPos(pos);
        let node: globalThis.Node | null = point.node;
        if (node.nodeType === globalThis.Node.ELEMENT_NODE && node.childNodes.length) node = node.childNodes[Math.max(0, Math.min(point.offset - 1, node.childNodes.length - 1))];
        fontElement = (node.nodeType === globalThis.Node.ELEMENT_NODE ? node : node.parentElement) as HTMLElement ?? dom;
      } catch { hide(); return; }
      const fontSize = parseFloat(getComputedStyle(fontElement).fontSize);
      const rect = editor.view.coordsAtPos(pos), body = dom.getBoundingClientRect();
      if (!Number.isFinite(fontSize) || fontSize <= 0 || rect.left < Math.max(0, body.left) || rect.left > Math.min(innerWidth, body.right) || rect.bottom < 0 || rect.top > innerHeight) { hide(); return; }
      if (owner !== dom) { owner?.classList.remove('has-sized-caret'); owner = dom; }
      const top = rect.top + (rect.bottom - rect.top - fontSize) / 2;
      if (top < 0 || top + fontSize > innerHeight) { hide(); return; }
      element.style.left = `${rect.left}px`; element.style.top = `${top}px`; element.style.height = `${fontSize}px`;
      element.hidden = false; dom.classList.add('has-sized-caret');
    },
  };
}
