import { test } from 'node:test';
import assert from 'node:assert/strict';
import { DocumentSource } from '../src/document.ts';
import { getSchema } from '@tiptap/core';
import { extensions } from '../src/document.ts';

test('opening and focusing preserves the exact source, including CRLF and unsupported blocks', () => {
  const raw = '---\r\ntitle: 한글\r\n---\r\n\r\n# 제목\r\n\r\n<!-- 숨은 주석 -->\r\n\r\n[[문서|별칭]]\r\n\r\n[^1]: 원문 주석\r\n\r\n$$x^2$$\r\n';
  const source = new DocumentSource(); const doc = source.load(raw);
  assert.equal(source.serialize(doc), raw);
  assert.equal(source.serialize(getSchema(extensions()).nodeFromJSON(doc).toJSON()), raw);
});
test('editing a normal paragraph retains frontmatter, comments, wiki and opaque source exactly', () => {
  const raw = '---\ntitle: 한글\n---\n\n<!-- 숨은 값 -->\n\n[[다른 문서|이름]]\n\n바꿀 문장\n\n:::warning\n그대로\n:::\n';
  const source = new DocumentSource(); const doc = source.load(raw);
  const paragraph = doc.content!.find(n => n.type === 'paragraph')!;
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
