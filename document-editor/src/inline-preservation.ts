import { Extension, Node } from '@tiptap/core';

export const WikiReference = Node.create({
  name: 'wikiReference', group: 'inline', inline: true, atom: true,
  addAttributes: () => ({ raw: { default: '' }, target: { default: '' }, label: { default: '' } }),
  parseHTML: () => [{ tag: 'a[data-wiki-reference]', getAttrs: element => ({ raw: element.getAttribute('data-wiki-raw') ?? '', target: element.getAttribute('data-wiki-target') ?? '', label: element.textContent ?? '' }) }],
  renderHTML: ({ node }) => ['a', { 'data-wiki-reference': '', 'data-wiki-raw': node.attrs.raw, 'data-wiki-target': node.attrs.target, href: `wiki:${encodeURIComponent(node.attrs.target)}`, title: node.attrs.raw }, node.attrs.label],
  markdownTokenizer: {
    name: 'wikiReference', level: 'inline', start: source => source.search(/!?\[\[/),
    tokenize(source) {
      const match = /^!?\[\[([^\]\r\n]+)\]\]/.exec(source);
      return match ? { type: 'wikiReference', raw: match[0], text: match[1] } : undefined;
    },
  },
  parseMarkdown: token => {
    const text = String(token.text ?? ''), split = text.indexOf('|');
    const target = split < 0 ? text : text.slice(0, split), label = split < 0 ? text : text.slice(split + 1);
    return { type: 'wikiReference', attrs: { raw: token.raw, target, label } };
  },
  renderMarkdown: node => node.attrs?.raw ?? '',
});

export const PreservedInline = Node.create({
  name: 'preservedInline', group: 'inline', inline: true, atom: true,
  addAttributes: () => ({ raw: { default: '' } }),
  parseHTML: () => [{ tag: 'span[data-preserved-inline]', getAttrs: element => ({ raw: element.getAttribute('data-raw') ?? element.textContent ?? '' }) }],
  renderHTML: ({ node }) => ['span', { 'data-preserved-inline': '', 'data-raw': node.attrs.raw, title: '원문으로 보존되는 구문' }, node.attrs.raw],
  markdownTokenizer: {
    name: 'preservedInline', level: 'inline',
    start: source => source.search(/<!--|<\/?[A-Za-z]|\[\^|\[[^\]\r\n]+\]\[|\$|\\[[(]|\{[%{]/),
    tokenize(source) {
      const match = /^(?:<!--[\s\S]*?-->|<\/?[A-Za-z](?:[^"'<>]|"[^"]*"|'[^']*')*>|\[\^[^\]\r\n]+\]|\[[^\]\r\n]+\]\[[^\]\r\n]*\]|\$\$[\s\S]*?\$\$|\$[^$\r\n]+\$|\\\[[\s\S]*?\\\]|\\\([^\r\n]*?\\\)|\{\{[\s\S]*?\}\}|\{%[\s\S]*?%\})/.exec(source);
      return match ? { type: 'preservedInline', raw: match[0] } : undefined;
    },
  },
  parseMarkdown: token => ({ type: 'preservedInline', attrs: { raw: token.raw } }),
  renderMarkdown: node => node.attrs?.raw ?? '',
});

// Markdown HTML is data, not a DOM import. A comment must not downgrade its surrounding task list.
export const PreservedHtml = Extension.create({
  name: 'preservedHtml', markdownTokenName: 'html',
  parseMarkdown: token => token.block
    ? { type: 'preservedMarkdown', content: token.raw ? [{ type: 'text', text: token.raw }] : [] }
    : { type: 'preservedInline', attrs: { raw: token.raw } },
});
export const PreservedImage = Extension.create({
  name: 'preservedImage', markdownTokenName: 'image',
  parseMarkdown: token => ({ type: 'preservedInline', attrs: { raw: token.raw } }),
});
