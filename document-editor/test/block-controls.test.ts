import { test } from 'node:test';
import assert from 'node:assert/strict';
import { getSchema } from '@tiptap/core';
import { EditorState } from '@tiptap/pm/state';
import { history, undo } from '@tiptap/pm/history';
import { extensions } from '../src/document.ts';
import { blockAt, duplicateBlock, insertAfterBlock, moveBlockBy, moveBlockTo, removeBlock, unwrapListItem } from '../src/block-controls.ts';

const schema = getSchema(extensions());
const p = (text = '', attrs = {}) => schema.nodes.paragraph.create(attrs, text ? schema.text(text) : undefined);
const state = (...nodes: ReturnType<typeof p>[]) => EditorState.create({ doc: schema.nodes.doc.create(null, nodes), plugins: [history()] });

test('a nested task targets its row, not the surrounding list', () => {
  const nested = schema.nodes.taskList.create(null, [schema.nodes.taskItem.create({ checked: false }, [p('nested')])]);
  const list = schema.nodes.taskList.create(null, [schema.nodes.taskItem.create({ checked: true }, [p('parent'), nested]), schema.nodes.taskItem.create({}, [p('sibling')])]);
  const s = state(list); let position = 0;
  s.doc.descendants((node, pos) => { if (node.isText && node.text === 'nested') position = pos; });
  const block = blockAt(s.doc, position)!;
  assert.equal(block.node.type.name, 'taskItem'); assert.equal(block.node.textContent, 'nested');
  const next = s.apply(removeBlock(s.tr, block));
  assert.equal(next.doc.textContent, 'parentsibling'); assert.equal(next.doc.firstChild!.firstChild!.attrs.checked, true);
  next.doc.check();
});

test('duplicate keeps content and custom mark attributes but drops source identity', () => {
  const original = p('keep', { sourceId: 'original' });
  const s = state(original), next = s.apply(duplicateBlock(s.tr, blockAt(s.doc, 1)!));
  assert.equal(next.doc.childCount, 2); assert.equal(next.doc.child(0).attrs.sourceId, 'original');
  assert.equal(next.doc.child(1).attrs.sourceId, null); assert.equal(next.doc.child(1).textContent, 'keep');
});

test('deleting the only block leaves a valid editable paragraph and can be undone', () => {
  let s = state(p('keep'));
  s = s.apply(removeBlock(s.tr, blockAt(s.doc, 1)!)); s.doc.check();
  assert.equal(s.doc.firstChild!.type.name, 'paragraph'); assert.equal(s.doc.textContent, '');
  undo(s, tr => { s = s.apply(tr); }); assert.equal(s.doc.textContent, 'keep');
});

test('insert below a task adds an unchecked sibling without changing the current item', () => {
  const s = state(schema.nodes.taskList.create(null, [schema.nodes.taskItem.create({ checked: true }, [p('done')])]));
  const next = s.apply(insertAfterBlock(s.tr, blockAt(s.doc, 3)!)); next.doc.check();
  assert.equal(next.doc.firstChild!.childCount, 2); assert.equal(next.doc.firstChild!.child(0).attrs.checked, true);
  assert.equal(next.doc.firstChild!.child(1).attrs.checked, false);
});

test('same-parent move preserves content and is a single undoable edit', () => {
  let s = state(p('one'), p('two'), p('three'));
  const source = blockAt(s.doc, 1)!, target = blockAt(s.doc, s.doc.child(0).nodeSize + s.doc.child(1).nodeSize + 1)!;
  const tr = s.tr; assert.equal(moveBlockTo(tr, source, target, true), true); s = s.apply(tr);
  assert.equal(s.doc.textContent, 'twothreeone'); s.doc.check();
  undo(s, undoTr => { s = s.apply(undoTr); }); assert.equal(s.doc.textContent, 'onetwothree');
  assert.equal(moveBlockBy(s.tr, blockAt(s.doc, 1)!, -1), false);
});

test('drag across different list parents is rejected without a document change', () => {
  const list = (text: string) => schema.nodes.bulletList.create(null, [schema.nodes.listItem.create(null, [p(text)])]);
  const s = state(list('one'), list('two')), tr = s.tr;
  assert.equal(moveBlockTo(tr, blockAt(s.doc, 3)!, blockAt(s.doc, s.doc.firstChild!.nodeSize + 3)!, true), false);
  assert.equal(tr.docChanged, false);
});

test('converting one task unwraps only that row while keeping siblings and nested children', () => {
  const child = schema.nodes.bulletList.create(null, [schema.nodes.listItem.create(null, [p('nested')])]);
  const item = (text: string) => schema.nodes.taskItem.create({ checked: true }, [p(text)]);
  const list = schema.nodes.taskList.create(null, [item('before'), schema.nodes.taskItem.create({ checked: false }, [p('target'), child]), item('after')]);
  const s = state(list); let position = 0;
  s.doc.descendants((node, pos) => { if (node.isText && node.text === 'target') position = pos; });
  const next = s.apply(unwrapListItem(s.tr, blockAt(s.doc, position)!)); next.doc.check();
  assert.deepEqual(Array.from({ length: next.doc.childCount }, (_, i) => next.doc.child(i).type.name), ['taskList', 'paragraph', 'bulletList', 'taskList']);
  assert.equal(next.doc.textContent, 'beforetargetnestedafter');
  assert.equal(next.doc.firstChild!.firstChild!.attrs.checked, true); assert.equal(next.doc.lastChild!.firstChild!.attrs.checked, true);
});
