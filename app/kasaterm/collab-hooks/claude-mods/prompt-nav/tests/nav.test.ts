import { describe, expect, test } from 'claude-code/testing'

import { estimate, freshRequest, metrics, observe, parseCommand, parseItems, target } from '../hooks/nav'
import type { Item, Model } from '../hooks/nav'

function item(id: string, k: Item['k'], l = 1): Item {
  return { id, k, l, c: 0, t: k === 'u' ? `질문 ${id}` : '' }
}

// u1(2행) a1(10행) u2(2행) a2(20행) u3(2행) a3(6행) — 높이는 잰 값으로 고정한다.
function model(): Model {
  const items = [item('u1', 'u'), item('a1', 'a'), item('u2', 'u'), item('a2', 'a'), item('u3', 'u'), item('a3', 'a')]
  const heights = new Map([['u1', 2], ['a1', 10], ['u2', 2], ['a2', 20], ['u3', 2], ['a3', 6]])
  return { items, heights, onScreen: new Map(), notPrompt: new Set(), cols: 80, top: null }
}

function indexOf(m: Model) {
  return (id: string) => {
    const i = m.items.findIndex(it => it.id === id)
    return i < 0 ? undefined : i
  }
}

describe('metrics', () => {
  test('prompts sit at the sum of the rows above them', () => {
    const m = model()
    const at = metrics(m)
    expect(at.total).toBe(42)
    expect(at.prompts.map(p => p.row)).toEqual([0, 12, 34])
    expect(at.top).toBe(null)
  })

  test('the current prompt is the last one at or above the top row', () => {
    const m = model()
    observe(m, 'a2', { first: 5, last: 19, of: 20 }, indexOf(m))
    const at = metrics(m)
    expect(at.top).toBe(19)
    expect(at.current).toBe(1)
  })

  test('a row the person did not type is no prompt', () => {
    const m = model()
    m.notPrompt.add('u2')
    expect(metrics(m).prompts.map(p => p.id)).toEqual(['u1', 'u3'])
  })

  test('an unmeasured row is estimated from its text and the width', () => {
    expect(estimate({ id: 'x', k: 'a', l: 3, c: 0, t: '' }, 80)).toBe(4)
    expect(estimate({ id: 'x', k: 'a', l: 1, c: 760, t: '' }, 80)).toBe(11)
    expect(estimate({ id: 'x', k: 't', l: 1, c: 0, t: '' }, 80)).toBe(3)
  })
})

describe('observe', () => {
  test('a row cut at the top becomes the top', () => {
    const m = model()
    observe(m, 'u2', { first: 0, last: 1, of: 2 }, indexOf(m))
    observe(m, 'a1', { first: 4, last: 9, of: 10 }, indexOf(m))
    expect(metrics(m).top).toBe(6)
  })

  test('a whole row below the top does not move it', () => {
    const m = model()
    observe(m, 'a1', { first: 4, last: 9, of: 10 }, indexOf(m))
    observe(m, 'u2', { first: 0, last: 1, of: 2 }, indexOf(m))
    expect(metrics(m).top).toBe(6)
  })

  test('when the top row leaves, the highest row still reported takes over', () => {
    const m = model()
    observe(m, 'a1', { first: 4, last: 9, of: 10 }, indexOf(m))
    observe(m, 'u2', { first: 0, last: 1, of: 2 }, indexOf(m))
    observe(m, 'a1', null, indexOf(m))
    expect(metrics(m).top).toBe(12)
  })
})

describe('target', () => {
  test('prev goes to the head of the turn first, then one turn back', () => {
    const m = model()
    observe(m, 'a2', { first: 5, last: 19, of: 20 }, indexOf(m))
    expect(target(m, { op: 'prev' })).toEqual({ id: 'u2', block: 'start' })
    m.onScreen.clear()
    m.top = null
    observe(m, 'u2', { first: 0, last: 1, of: 2 }, indexOf(m))
    expect(target(m, { op: 'prev' })).toEqual({ id: 'u1', block: 'start' })
  })

  test('next goes to the following prompt and stops after the last', () => {
    const m = model()
    observe(m, 'a2', { first: 5, last: 19, of: 20 }, indexOf(m))
    expect(target(m, { op: 'next' })).toEqual({ id: 'u3', block: 'start' })
    m.top = { id: 'u3', first: 0 }
    expect(target(m, { op: 'next' })).toBe(null)
  })

  test('a row lands on the row that holds it', () => {
    const m = model()
    expect(target(m, { op: 'row', row: 20 })).toEqual({ id: 'a2', block: 'start' })
    expect(target(m, { op: 'row', row: -3 })).toEqual({ id: 'u1', block: 'start' })
    expect(target(m, { op: 'bottom' })).toEqual({ id: 'a3', block: 'end' })
    expect(target(m, { op: 'prompt', index: 2 })).toEqual({ id: 'u3', block: 'start' })
    expect(target(m, { op: 'prompt', index: 9 })).toBe(null)
  })
})

describe('requests', () => {
  test('only a new, recent request is taken', () => {
    expect(freshRequest('{"seq":3,"at":1000,"op":"prompt","index":2}', 2, 1500)).toEqual({ op: 'prompt', index: 2, seq: 3 })
    expect(freshRequest('{"seq":3,"at":1000,"op":"prev"}', 3, 1500)).toBe(null)
    expect(freshRequest('{"seq":4,"at":1000,"op":"prev"}', 3, 9000)).toBe(null)
    expect(freshRequest('{"seq":4,"at":1000,"op":"jump"}', 3, 1500)).toBe(null)
    expect(freshRequest('not json', 0, 0)).toBe(null)
  })

  test('the command takes a direction or a 1-based number', () => {
    expect(parseCommand('')).toEqual({ op: 'prev' })
    expect(parseCommand('다음')).toEqual({ op: 'next' })
    expect(parseCommand('3')).toEqual({ op: 'prompt', index: 2 })
    expect(parseCommand('0')).toBe(null)
    expect(parseCommand('where')).toBe(null)
  })

  test('the transcript helper output is read row by row', () => {
    const items = parseItems('[["a","u",1,4,"hi"],["b","x",1,1,""],["c","t",2,0,""],7]')
    expect(items.map(it => it.id)).toEqual(['a', 'c'])
    expect(items[0]?.t).toBe('hi')
  })
})
