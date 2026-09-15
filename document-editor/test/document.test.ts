import { test } from 'node:test';
import assert from 'node:assert/strict';
import { DocumentSource } from '../src/document.ts';
import { getSchema } from '@tiptap/core';
import { extensions } from '../src/document.ts';
import { EditorState } from '@tiptap/pm/state';
import { history, undo, redo } from '@tiptap/pm/history';

test('opening and focusing preserves the exact source, including CRLF and unsupported blocks', () => {
  const raw = '---\r\ntitle: 한글\r\n---\r\n\r\n# 제목\r\n\r\n<!-- 숨은 주석 -->\r\n\r\n[[문서|별칭]]\r\n\r\n[^1]: 원문 주석\r\n\r\n$$x^2$$\r\n';
  const source = new DocumentSource(); const doc = source.load(raw);
  assert.equal(source.serialize(doc), raw);
  assert.equal(source.serialize(getSchema(extensions()).nodeFromJSON(doc).toJSON()), raw);
});
test('editing a normal paragraph retains frontmatter, comments, wiki and opaque source exactly', () => {
  const raw = '---\ntitle: 한글\n---\n\n<!-- 숨은 값 -->\n\n[[다른 문서|이름]]\n\n바꿀 문장\n\n:::warning\n그대로\n:::\n';
  const source = new DocumentSource(); const doc = source.load(raw);
  const paragraph = doc.content!.find(n => n.type === 'paragraph' && n.content?.some(child => child.text === '바꿀 문장'))!;
  paragraph.content![0].text = '바꾼 한글 문장';
  const saved = source.serialize(doc);
  for (const text of ['---\ntitle: 한글\n---', '<!-- 숨은 값 -->', '[[다른 문서|이름]]', ':::warning\n그대로\n:::']) assert.ok(saved.includes(text), text);
  assert.ok(saved.includes('바꾼 한글 문장')); assert.ok(!saved.includes('바꿀 문장'));
});
test('nested tasks remain rich editable nodes and serialize their checked state', () => {
  const source = new DocumentSource(); const doc = source.load('- [ ] 부모\n  - [ ] 자식\n- [x] 끝\n');
  const list = doc.content![0]; assert.equal(list.type, 'taskList');
  list.content![0].attrs!.checked = true;
  const saved = source.serialize(doc); assert.match(saved, /- \[x\] 부모/); assert.match(saved, /- \[ \] 자식/); assert.match(saved, /- \[x\] 끝/);
});
test('rich headings, formatting and tables survive an edit in another block', () => {
  const raw = '# 제목\n\n**굵게** *기울임* ~~취소~~ `코드`\n\n| 이름 | 상태 |\n| --- | --- |\n| 한글 | 준비 |\n\n마지막\n';
  const source = new DocumentSource(); const doc = source.load(raw);
  assert.ok(doc.content!.some(n => n.type === 'table'));
  doc.content!.at(-1)!.content![0].text = '마지막 수정';
  const saved = source.serialize(doc); assert.ok(saved.includes('**굵게** *기울임* ~~취소~~ `코드`')); assert.ok(saved.includes('| 한글 | 준비 |')); assert.ok(saved.includes('마지막 수정'));
});
test('opaque source itself can be edited without interpreting HTML', () => {
  const source = new DocumentSource(); const doc = source.load('<!-- first -->\n');
  assert.equal(doc.content![0].type, 'preservedMarkdown');
  doc.content![0].content![0].text = '<!-- second -->';
  assert.match(source.serialize(doc), /<!-- second -->/);
});
test('reference definitions omitted by the lexer survive edits and remain usable', () => {
  const raw = '[자료][ref]\n\n[ref]: https://example.com "이름"\n\n수정 대상\n';
  const source = new DocumentSource(); const doc = source.load(raw);
  doc.content!.at(-1)!.content![0].text = '수정 결과';
  const saved = source.serialize(doc);
  assert.ok(saved.includes('[자료][ref]'));
  assert.ok(saved.includes('[ref]: https://example.com "이름"'));
  assert.ok(saved.includes('수정 결과'));
});
test('wiki in continued task descriptions remains inline and checkbox history roundtrips safely', () => {
  const raw = '---\ntitle: 체크 확인\n---\n\n- [ ] 첫 작업\n  이어지는 설명 [[기록#다음|별칭]]\n  <!-- 숨은 주석 -->\n  [^note] 와 [자료][ref]\n  - [ ] 중첩 작업 [[하위문서]]\n- [x] 둘째 작업 [[문서]]\n\n[ref]: https://example.com\n\n[^note]: 각주 내용\n';
  const source = new DocumentSource(), schema = getSchema(extensions());
  const loaded = source.load(raw);
  assert.ok(loaded.content!.some(node => node.type === 'taskList'));
  let state = EditorState.create({ schema, doc: schema.nodeFromJSON(loaded), plugins: [history()] });
  let taskPosition = -1, tasks = 0, wiki = 0;
  state.doc.descendants((node, pos) => { if (node.type.name === 'taskItem') { tasks++; if (taskPosition < 0) taskPosition = pos; } if (node.type.name === 'wikiReference') wiki++; });
  assert.equal(tasks, 3); assert.equal(wiki, 3);
  const first = state.doc.nodeAt(taskPosition)!;
  state = state.apply(state.tr.setNodeMarkup(taskPosition, undefined, { ...first.attrs, checked: true }));
  const saved = source.serialize(state.doc.toJSON());
  assert.match(saved, /- \[x\] 첫 작업/);
  for (const preserved of ['[[기록#다음|별칭]]', '[[하위문서]]', '[[문서]]', '<!-- 숨은 주석 -->', '[^note]', '[자료][ref]', '[ref]: https://example.com', '[^note]: 각주 내용']) assert.ok(saved.includes(preserved), preserved);
  assert.ok(undo(state, tr => { state = state.apply(tr); }));
  assert.equal(source.serialize(state.doc.toJSON()), raw);
  assert.ok(redo(state, tr => { state = state.apply(tr); }));
  assert.equal(source.serialize(state.doc.toJSON()), saved);
  const reopened = schema.nodeFromJSON(new DocumentSource().load(saved));
  let reopenedTasks = 0; reopened.descendants(node => { if (node.type.name === 'taskItem') reopenedTasks++; });
  assert.equal(reopenedTasks, 3);
});
test('wiki variants preserve exact raw syntax while displaying aliases and targets', () => {
  const source = new DocumentSource();
  const doc = source.load('- [[대상]]\n- [[대상|보이는 이름]]\n- [[대상#제목]]\n');
  assert.equal(doc.content![0].type, 'bulletList');
  const schema = getSchema(extensions()), nodes: any[] = [];
  schema.nodeFromJSON(doc).descendants(node => { if (node.type.name === 'wikiReference') nodes.push(node); });
  assert.deepEqual(nodes.map(node => node.attrs.label), ['대상', '보이는 이름', '대상#제목']);
  assert.deepEqual(nodes.map(node => node.attrs.raw), ['[[대상]]', '[[대상|보이는 이름]]', '[[대상#제목]]']);
  const dom = schema.nodes.wikiReference.spec.toDOM!(nodes[2]) as any;
  assert.equal(dom[1].href, 'wiki:' + encodeURIComponent('대상#제목'));
});
test('HTML inside a checklist stays inert raw data instead of importing executable nodes', () => {
  const source = new DocumentSource(); const doc = source.load('- [ ] 안전 확인 <!-- keep --> <img src=x onerror="alert(1)">\n- [ ] 다음 작업 ![그림](missing.png)\n');
  assert.equal(doc.content![0].type, 'taskList');
  doc.content![0].content![0].attrs!.checked = true;
  const saved = source.serialize(doc);
  assert.ok(saved.includes('<!-- keep -->')); assert.ok(saved.includes('<img src=x onerror="alert(1)">')); assert.ok(saved.includes('![그림](missing.png)'));
  const names = new Set<string>(); getSchema(extensions()).nodeFromJSON(doc).descendants(node => { names.add(node.type.name); });
  assert.ok(names.has('preservedInline')); assert.ok(!names.has('image')); assert.ok(!names.has('html'));
});
test('mixed checkbox and plain bullet items stay rich and preserve unchanged source groups', () => {
  const raw = '- [ ] 작업\n- 설명 [[참고]]\n- [x] 완료\n\n끝 문단\n';
  const source = new DocumentSource(), doc = source.load(raw);
  assert.deepEqual(doc.content!.slice(0, 3).map(node => node.type), ['taskList', 'bulletList', 'taskList']);
  doc.content!.at(-1)!.content![0].text = '바뀐 끝';
  assert.ok(source.serialize(doc).startsWith('- [ ] 작업\n- 설명 [[참고]]\n- [x] 완료\n'));
  doc.content![0].content![0].attrs!.checked = true;
  const saved = source.serialize(doc);
  assert.match(saved, /- \[x\] 작업/); assert.ok(saved.includes('설명 [[참고]]')); assert.ok(saved.includes('바뀐 끝'));
  assert.ok(!new DocumentSource().load(saved).content!.some(node => node.type === 'preservedMarkdown'));
});
