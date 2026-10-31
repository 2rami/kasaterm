import { test } from 'node:test'
import assert from 'node:assert/strict'
import { cast } from './cli.mjs'

const tabId = { name: 'tabId', type: 'integer' }

test('an empty integer is refused instead of becoming tab 0', () => {
  // `kc close_tab --tabId ""` 가 Number("") = 0 이 되어 사람의 활성 탭을 닫았다.
  for (const raw of ['', ' ', '0x1', '1e3', '1.5', 'abc', '١']) {
    assert.throws(() => cast(raw, tabId), /--tabId 은 정수여야/, JSON.stringify(raw))
  }
  assert.equal(cast('12', tabId), 12)
  assert.equal(cast(' 12 ', tabId), 12)
  assert.equal(cast('-3', tabId), -3)
})

test('numbers take decimals but not empty values', () => {
  const amount = { name: 'amount', type: 'number' }
  assert.equal(cast('1.5', amount), 1.5)
  assert.equal(cast('.5', amount), 0.5)
  assert.throws(() => cast('', amount), /--amount 은 숫자여야/)
  assert.throws(() => cast('Infinity', amount), /--amount 은 숫자여야/)
})

test('integer list items are checked one by one', () => {
  const tabIds = { name: 'tabIds', type: 'array', item: 'integer' }
  assert.deepEqual(cast('1,2', tabIds), [1, 2])
  assert.throws(() => cast('1,,2', tabIds), /--tabIds 은 정수여야/)
})
