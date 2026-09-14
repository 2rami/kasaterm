import test from 'node:test'
import assert from 'node:assert/strict'
import { GROUP_COLORS, chooseGroupColor, planGroupColors } from './group-colors.js'

test('new colors avoid occupied preferences and balance exhausted palettes', () => {
  assert.equal(chooseGroupColor([{ color: 'blue' }], 'blue'), 'red')
  const groups = GROUP_COLORS.map((color) => ({ color }))
  assert.equal(chooseGroupColor(groups, 'orange'), 'orange')
  groups.push({ color: 'orange' })
  assert.equal(chooseGroupColor(groups, 'orange'), 'blue')
})

test('manual cleanup reserves later unique colors and is stable after cleanup', () => {
  const groups = [{ id: 1, color: 'blue' }, { id: 2, color: 'blue' }, { id: 3, color: 'red' }]
  assert.deepEqual(planGroupColors(groups), [{ groupId: 2, color: 'yellow' }])
  const many = Array.from({ length: 20 }, (_, id) => ({ id, color: 'blue' }))
  const changes = new Map(planGroupColors(many).map((g) => [g.groupId, g.color]))
  const result = many.map((g) => ({ ...g, color: changes.get(g.id) || g.color }))
  const counts = GROUP_COLORS.map((color) => result.filter((g) => g.color === color).length)
  assert.ok(Math.max(...counts) - Math.min(...counts) <= 1)
  assert.deepEqual(planGroupColors(result), [])
})

test('concurrent Chrome requests share room creation, preserve manual colors and recover after failure', async () => {
  const groups = new Map([[100, { id: 100, windowId: 1, color: 'blue', title: 'Personal' }]])
  const tabs = new Map(Array.from({ length: 16 }, (_, i) => [i + 1, { id: i + 1, windowId: i === 4 ? 2 : 1 }]))
  let nextId = 101
  let loads = 0
  let failNext = false
  const saved = {}
  const tick = () => new Promise((resolve) => setImmediate(resolve))
  const oldChrome = globalThis.chrome
  const oldSelf = globalThis.self
  globalThis.self = {}
  globalThis.chrome = {
    runtime: { getManifest: () => ({ name: 'Test' }) },
    storage: {
      local: { get: async () => ({}), set: async () => {} },
      session: {
        get: async () => { loads++; await tick(); return {} },
        set: async (value) => { await tick(); Object.assign(saved, value) },
      },
    },
    action: { setTitle() {}, setBadgeText() {} },
    tabs: {
      get: async (id) => tabs.get(id),
      query: (query, callback) => callback([...tabs.values()].filter((t) => t.groupId === query.groupId)),
      group: async ({ tabIds, groupId, createProperties }) => {
        await tick()
        if (failNext) { failNext = false; throw new Error('temporary failure') }
        const id = groupId ?? nextId++
        if (groupId == null) groups.set(id, { id, windowId: createProperties.windowId, color: 'grey' })
        for (const tabId of tabIds) tabs.get(tabId).groupId = id
        return id
      },
    },
    tabGroups: {
      get: async (id) => groups.get(id),
      query: async ({ windowId }) => [...groups.values()].filter((g) => g.windowId === windowId).map((g) => ({ ...g })),
      update: async (id, patch) => { await tick(); Object.assign(groups.get(id), patch); return groups.get(id) },
    },
  }
  try {
    const { openSession, groupOwnTab } = await import(`./sessions.js?test=${Date.now()}`)
    for (const [client, team] of [['a', 'room-a'], ['b', 'room-a'], ['c', 'room-c'], ['d', 'room-d']]) {
      await openSession(client, { paneId: client, team, room: team, name: client, roomColor: 'blue' })
    }
    const [a, b, c, d] = await Promise.all([
      groupOwnTab('a', 1), groupOwnTab('b', 2), groupOwnTab('c', 3), groupOwnTab('d', 5),
    ])
    assert.equal(a, b)
    assert.notEqual(a, c)
    assert.equal(loads, 1)
    assert.equal(groups.get(a).color, 'red')
    assert.equal(groups.get(c).color, 'yellow')
    assert.equal(groups.get(d).color, 'blue')
    assert.equal(groups.get(100).title, 'Personal')
    assert.equal(groups.get(100).color, 'blue')
    groups.get(a).color = 'orange'
    assert.equal(await groupOwnTab('b', 4), a)
    assert.equal(groups.get(a).color, 'orange')
    failNext = true
    assert.equal(await groupOwnTab('c', 6), null)
    assert.equal(await groupOwnTab('c', 7), c)
    // 저장 큐가 비워지기 전에는 메모리만 맞고 저장소는 아직 옛 상태일 수 있다.
    for (let i = 0; i < 6; i++) await tick()
    assert.deepEqual(Object.keys(saved.roomGroups).sort(), ['room-a', 'room-c', 'room-d'])
  } finally {
    globalThis.chrome = oldChrome
    globalThis.self = oldSelf
  }
})
