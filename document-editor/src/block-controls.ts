import type { Node as ProseMirrorNode } from '@tiptap/pm/model';
import { TextSelection, type Transaction } from '@tiptap/pm/state';
import { closeHistory } from '@tiptap/pm/history';

export type BlockTarget = { from: number; to: number; node: ProseMirrorNode; parentStart: number; parentDepth: number; index: number };

export function blockAt(doc: ProseMirrorNode, position: number): BlockTarget | undefined {
  const point = doc.resolve(Math.max(0, Math.min(position, doc.content.size)));
  let depth = Math.min(1, point.depth);
  for (let d = point.depth; d > 0; d--) {
    if (['listItem', 'taskItem'].includes(point.node(d).type.name)) { depth = d; break; }
  }
  if (depth === 0) {
    const node = point.nodeAfter ?? point.nodeBefore;
    if (!node?.isBlock) return;
    const from = point.nodeAfter ? point.pos : point.pos - node.nodeSize;
    return { from, to: from + node.nodeSize, node, parentStart: 0, parentDepth: 0, index: point.index() - (point.nodeAfter ? 0 : 1) };
  }
  const node = point.node(depth), from = point.before(depth);
  return { from, to: from + node.nodeSize, node, parentStart: point.start(depth - 1), parentDepth: depth - 1, index: point.index(depth - 1) };
}

function selectInside(tr: Transaction, position: number) {
  tr.setSelection(TextSelection.near(tr.doc.resolve(Math.max(0, Math.min(position, tr.doc.content.size)))));
  return closeHistory(tr).scrollIntoView();
}

export function insertAfterBlock(tr: Transaction, block: BlockTarget): Transaction {
  if (block.node.type.name === 'paragraph' && block.node.content.size === 0) return selectInside(tr, block.from + 1);
  const item = ['listItem', 'taskItem'].includes(block.node.type.name);
  const node = item ? block.node.type.createAndFill() : tr.doc.type.schema.nodes.paragraph.create();
  if (!node) return tr;
  tr.insert(block.to, node);
  return selectInside(tr, block.to + 1);
}

export function duplicateBlock(tr: Transaction, block: BlockTarget): Transaction {
  // Source fragments belong to one original block; a duplicate keeps its content, not that identity.
  const json = JSON.parse(JSON.stringify(block.node.toJSON(), (key, value) => key === 'sourceId' ? undefined : value));
  const copy = tr.doc.type.schema.nodeFromJSON(json);
  tr.insert(block.to, copy);
  return selectInside(tr, block.to + 1);
}

export function unwrapListItem(tr: Transaction, block: BlockTarget): Transaction {
  if (!['listItem', 'taskItem'].includes(block.node.type.name)) return tr;
  const point = tr.doc.resolve(block.from), list = point.parent;
  const start = point.before(block.parentDepth), end = point.after(block.parentDepth);
  const before: ProseMirrorNode[] = [], after: ProseMirrorNode[] = [], content: ProseMirrorNode[] = [];
  list.forEach((node, _offset, index) => { if (index < block.index) before.push(node); else if (index > block.index) after.push(node); });
  block.node.forEach(node => content.push(node));
  const pieces: ProseMirrorNode[] = [];
  if (before.length) pieces.push(list.type.create({ ...list.attrs, sourceId: null }, before));
  const cursor = start + (pieces[0]?.nodeSize ?? 0) + 1;
  pieces.push(...content);
  if (after.length) pieces.push(list.type.create({ ...list.attrs, sourceId: null, ...(list.type.name === 'orderedList' ? { start: Number(list.attrs.start ?? 1) + block.index + 1 } : {}) }, after));
  tr.replaceWith(start, end, pieces);
  return selectInside(tr, cursor);
}

export function removeBlock(tr: Transaction, block: BlockTarget): Transaction {
  let from = block.from, to = block.to;
  const point = tr.doc.resolve(block.from);
  if (['listItem', 'taskItem'].includes(block.node.type.name) && point.parent.childCount === 1) {
    from = point.before(block.parentDepth); to = point.after(block.parentDepth);
  }
  if (from === 0 && to === tr.doc.content.size) tr.replaceWith(from, to, tr.doc.type.schema.nodes.paragraph.create());
  else tr.delete(from, to);
  return selectInside(tr, from);
}

export function moveBlockTo(tr: Transaction, source: BlockTarget, target: BlockTarget, after: boolean): boolean {
  if (source.from === target.from || source.parentStart !== target.parentStart || source.parentDepth !== target.parentDepth) return false;
  const boundary = after ? target.to : target.from;
  if (boundary === source.from || boundary === source.to) return false;
  const destination = boundary > source.to ? boundary - source.node.nodeSize : boundary;
  tr.delete(source.from, source.to).insert(destination, source.node);
  selectInside(tr, destination + 1);
  return true;
}

export function moveBlockBy(tr: Transaction, block: BlockTarget, direction: -1 | 1): boolean {
  const parent = tr.doc.resolve(block.from).parent, index = block.index + direction;
  if (index < 0 || index >= parent.childCount) return false;
  const sibling = parent.child(index);
  const from = direction < 0 ? block.from - sibling.nodeSize : block.to;
  return moveBlockTo(tr, block, { ...block, node: sibling, from, to: from + sibling.nodeSize, index }, direction > 0);
}
