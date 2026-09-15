import { Extension, Node, getSchema, type JSONContent } from '@tiptap/core';
import StarterKit from '@tiptap/starter-kit';
import { Markdown, MarkdownManager } from '@tiptap/markdown';
import { TaskList, TaskItem } from '@tiptap/extension-list';
import { TableKit } from '@tiptap/extension-table';
import { Marked } from 'marked';
import { WikiReference, PreservedInline, PreservedHtml, PreservedImage } from './inline-preservation';

export const RawBlock = Node.create({
  name: 'preservedMarkdown', group: 'block', content: 'text*', code: true, defining: true,
  marks: '', whitespace: 'pre',
  parseHTML: () => [{ tag: 'pre[data-preserved-markdown]' }],
  renderHTML: () => ['pre', { 'data-preserved-markdown': '', 'aria-label': '원문 그대로 편집하는 블록' }, ['code', 0]],
  renderMarkdown: node => (node.content ?? []).map(n => n.text ?? '').join(''),
  markdownTokenizer: {
    name: 'preservedMarkdown', level: 'block', start: source => source.search(/^\s*(?:\[[^\]\r\n]+\]:|:::)/m),
    tokenize(source) {
      const match = /^(?:\[[^\]\r\n]+\]:[^\r\n]*(?:\r?\n[ \t]+[^\r\n]+)*|:::[^\r\n]*\r?\n[\s\S]*?\r?\n:::)(?:\r?\n|$)/.exec(source);
      return match ? { type: 'preservedMarkdown', raw: match[0] } : undefined;
    },
  },
  parseMarkdown: token => ({ type: 'preservedMarkdown', content: token.raw ? [{ type: 'text', text: token.raw }] : [] }),
});
const SourceIdentity = Extension.create({
  name: 'sourceIdentity',
  addGlobalAttributes: () => [{
    types: ['paragraph', 'heading', 'bulletList', 'orderedList', 'taskList', 'blockquote', 'codeBlock', 'horizontalRule', 'table', 'preservedMarkdown'],
    attributes: { sourceId: { default: null, rendered: false } },
  }],
});
export function extensions() {
  return [StarterKit.configure({ trailingNode: false, link: { openOnClick: false, autolink: false } }),
    Markdown, TaskList, TaskItem.configure({ nested: true, HTMLAttributes: { 'data-type': 'taskItem' } }), TableKit.configure({ table: { resizable: false } }), RawBlock, WikiReference, PreservedInline, PreservedHtml, PreservedImage, SourceIdentity];
}
const manager = new MarkdownManager({ extensions: extensions() });
const schema = getSchema(extensions());
const lexer = new Marked();
const supported = new Set(['heading', 'paragraph', 'list', 'blockquote', 'code', 'hr', 'table']);
const unusual = /<!--|<\/?[a-zA-Z][^>]*>|!?\[\[|\[\^[^\]]+\]|\[[^\]]+\]\[[^\]]*\]|^\s*\[[^\]]+\]:|^\s*:::|\$\$|\\\[|\\\(|^\s*\$[^$]+\$\s*$|!\[[^\]]*\]\(|\{[%{]|^\s*\|.*\{.*\}/m;
const blockOnlySyntax = /^\s*(?:\[[^\]]+\]:|:::)|^\s*\$\$|^\s*\\\[/m;
function fingerprint(node: JSONContent): string {
  return JSON.stringify(node, (key, value) => key === 'sourceId' ? undefined : value);
}
function rawNode(raw: string): JSONContent {
  return { type: 'preservedMarkdown', content: raw ? [{ type: 'text', text: raw }] : [] };
}

/** Preserve original blocks until that block changes, including its spelling and spacing. */
export class DocumentSource {
  private ledger = new Map<string, { raw: string; json: string; separator: string }>();
  private groups = new Map<string, { ids: string[]; raw: string }>();
  private original = '';
  private initial = '';
  private prefix = '';
  private serial = 0;
  load(markdown: string): JSONContent {
    this.ledger.clear(); this.groups.clear(); this.original = markdown; this.prefix = ''; this.serial = 0;
    const content: JSONContent[] = [];
    let input = markdown;
    const preamble = /^(?:\uFEFF)?---\r?\n[\s\S]*?\r?\n(?:---|\.\.\.)(?:\r?\n|$)/.exec(input);
    if (preamble) { content.push(rawNode(preamble[0].replace(/\r?\n$/, ''))); input = input.slice(preamble[0].length); }
    const tokens = lexer.lexer(input);
    let offset = 0;
    const retainGap = (raw: string) => {
      if (!raw) return;
      const node = schema.nodeFromJSON(rawNode(raw)).toJSON() as JSONContent;
      const id = String(++this.serial); node.attrs = { ...node.attrs, sourceId: id };
      this.ledger.set(id, { raw, json: fingerprint(node), separator: '' }); content.push(node);
    };
    for (const token of tokens) {
      // Lexers consume reference definitions without returning nodes. Keep those gaps.
      const start = input.indexOf(token.raw, offset);
      if (start > offset) retainGap(input.slice(offset, start));
      offset = start >= 0 ? start + token.raw.length : offset;
      if (token.type === 'space') {
        if (content.length) {
          const previous = content[content.length - 1];
          const entry = this.ledger.get(previous.attrs?.sourceId);
          if (entry) entry.separator += token.raw;
          else this.prefix += token.raw;
        } else this.prefix += token.raw;
        continue;
      }
      let nodes: JSONContent[];
      if (!supported.has(token.type) || (token.type === 'table' && unusual.test(token.raw)) || (token.type !== 'code' && token.type !== 'list' && blockOnlySyntax.test(token.raw))) {
        nodes = [rawNode(token.raw.replace(/\r?\n$/, ''))];
      } else {
        try { nodes = manager.parse(token.raw.replace(/(?:\r?\n)+$/, '')).content ?? []; }
        catch { nodes = []; }
        if (!nodes.length) nodes = [rawNode(token.raw.replace(/\r?\n$/, ''))];
      }
      const ids: string[] = [];
      for (const parsed of nodes) {
        const node = schema.nodeFromJSON(parsed).toJSON() as JSONContent;
        const id = String(++this.serial); ids.push(id);
        node.attrs = { ...node.attrs, sourceId: id };
        const raw = nodes.length === 1 ? token.raw : manager.serialize({ type: 'doc', content: [node] }) + '\n';
        this.ledger.set(id, { raw, json: fingerprint(node), separator: '' });
        content.push(node);
      }
      if (ids.length > 1) this.groups.set(ids[0], { ids, raw: token.raw });
    }
    retainGap(input.slice(offset));
    // Frontmatter is editable and remains exact until its own text is changed.
    if (preamble) {
      const node = schema.nodeFromJSON(content[0]).toJSON() as JSONContent, id = String(++this.serial);
      content[0] = node;
      node.attrs = { ...node.attrs, sourceId: id };
      this.ledger.set(id, { raw: preamble[0], json: fingerprint(node), separator: this.prefix });
      this.prefix = '';
    }
    const doc = schema.nodeFromJSON({ type: 'doc', content: content.length ? content : [{ type: 'paragraph' }] }).toJSON() as JSONContent;
    this.initial = fingerprint(doc);
    return doc;
  }
  serialize(doc: JSONContent): string {
    if (fingerprint(doc) === this.initial) return this.original;
    const nodes = doc.content ?? [], chunks: string[] = [];
    for (let index = 0; index < nodes.length; index++) {
      const node = nodes[index], group = this.groups.get(node.attrs?.sourceId);
      // Mixed task/bullet lists may expand into several rich lists without becoming raw text.
      if (group && group.ids.every((id, offset) => nodes[index + offset]?.attrs?.sourceId === id && fingerprint(nodes[index + offset]) === this.ledger.get(id)?.json)) {
        chunks.push(group.raw + (this.ledger.get(group.ids.at(-1)!)?.separator ?? '')); index += group.ids.length - 1; continue;
      }
      const entry = this.ledger.get(node.attrs?.sourceId);
      if (entry && fingerprint(node) === entry.json) { chunks.push(entry.raw + entry.separator); continue; }
      const value = node.type === 'preservedMarkdown'
        ? (node.content ?? []).map(n => n.text ?? '').join('')
        : manager.serialize({ type: 'doc', content: [node] });
      chunks.push(value.replace(/\n+$/, '') + '\n\n');
    }
    return this.prefix + chunks.map((chunk, index) => index < chunks.length - 1 && !chunk.endsWith('\n\n') ? chunk + (chunk.endsWith('\n') ? '\n' : '\n\n') : chunk).join('');
  }
}
