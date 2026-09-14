import test from 'node:test'
import assert from 'node:assert/strict'
import { tabSections, bookmarkResults, bookmarkMatches, canOpenBookmark } from './library-model.js'

test('all current-window tabs stay unique, grouped and naturally sorted without mutating browser order', () => {
  const tabs = [
    { id: 1, index: 0, pinned: true, groupId: -1 },
    { id: 2, index: 1, groupId: 10, title: 'first' },
    { id: 3, index: 2, groupId: 10, title: 'second' },
    { id: 4, index: 3, groupId: -1 },
    { id: 5, index: 4, groupId: 20 },
  ]
  const groups = [{ id: 10, title: '작업 10' }, { id: 20, title: '작업 2' }]
  const sections = tabSections(tabs, groups)
  assert.deepEqual(sections.map(s => s.key), ['pinned', '20', '10', 'loose'])
  assert.deepEqual(sections.find(s => s.id === 10).tabs.map(t => t.id), [2, 3])
  assert.equal(new Set(sections.flatMap(s => s.tabs.map(t => t.id))).size, tabs.length)
  assert.deepEqual(tabs.map(t => t.id), [1, 2, 3, 4, 5])
  assert.deepEqual(tabSections(tabs, groups, 'chrome').map(s => s.key), ['pinned', '10', '20', 'loose'])
  assert.deepEqual(tabSections(tabs, groups, 'name', '작업 10').flatMap(s => s.tabs.map(t => t.id)), [2, 3])
})

test('bookmark search keeps paths for identical titles and finds ancestor folders', () => {
  const tree = [{ title: '회사', children: [{ title: '설계', children: [{ id: 'a', title: '문서', url: 'https://example.com/a' }] }] }, { title: '개인', children: [{ id: 'b', title: '문서', url: 'https://example.com/b' }] }]
  assert.deepEqual(bookmarkResults(tree, '문서').map(item => item.path), ['회사 / 설계', '개인'])
  assert.deepEqual(bookmarkResults(tree, '회사').map(item => item.id), ['a'])
  assert.deepEqual(bookmarkResults(tree, '/b').map(item => item.id), ['b'])
  assert.deepEqual(bookmarkResults(tree, '없음'), [])
  assert.equal(bookmarkMatches({ title: '빈 폴더', children: [] }, '빈'), true)
})

test('bookmark open rejects executable and invalid URLs', () => {
  assert.equal(canOpenBookmark('javascript:alert(1)'), false)
  assert.equal(canOpenBookmark('data:text/html,test'), false)
  assert.equal(canOpenBookmark('not a url'), false)
  assert.equal(canOpenBookmark('https://example.com'), true)
  assert.equal(canOpenBookmark('chrome://bookmarks'), true)
})
