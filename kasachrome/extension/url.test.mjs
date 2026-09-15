import test from 'node:test'
import assert from 'node:assert/strict'
import { sameUrlKey, pickExistingTab } from './url.js'

test('trailing slashes do not make a second tab, query and hash do', () => {
  assert.equal(sameUrlKey('https://a.dev/docs/'), sameUrlKey('https://a.dev/docs'))
  assert.notEqual(sameUrlKey('https://a.dev/?page=2'), sameUrlKey('https://a.dev/'))
  // SPA 에서 해시는 화면을 가르는 라우트다. 같은 것으로 읽으면 다른 페이지를 열었다고 착각한다.
  assert.notEqual(sameUrlKey('https://a.dev/#/inbox'), sameUrlKey('https://a.dev/#/sent'))
})

test('an already open page is reused, preferring the agent own tab', () => {
  const tabs = [
    { id: 1, url: 'https://a.dev/docs' },
    { id: 2, url: 'https://b.dev/' },
    { id: 3, url: 'https://a.dev/docs/' },
  ]
  // 사람 탭밖에 없으면 그것을 돌려준다 — 부르는 쪽이 내 몫으로 삼지 않는다.
  assert.equal(pickExistingTab(tabs, 'https://a.dev/docs', new Set())?.id, 1)
  // 내가 연 탭이 있으면 그쪽이 언제나 낫다.
  assert.equal(pickExistingTab(tabs, 'https://a.dev/docs', new Set([3]))?.id, 3)
  assert.equal(pickExistingTab(tabs, 'https://c.dev/', new Set()), null)
})

test('a blank new tab is a state, not an address, so it is never reused', () => {
  const tabs = [{ id: 1, url: 'about:blank' }]
  assert.equal(pickExistingTab(tabs, 'about:blank', new Set([1])), null)
  assert.equal(pickExistingTab(tabs, '', new Set()), null)
})
