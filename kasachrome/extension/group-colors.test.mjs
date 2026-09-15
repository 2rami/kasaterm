import test from 'node:test'
import assert from 'node:assert/strict'
import { GROUP_COLORS, chooseGroupColor } from './group-colors.js'

test('new colors avoid occupied preferences and balance exhausted palettes', () => {
  assert.equal(chooseGroupColor([{ color: 'blue' }], 'blue'), 'red')
  const groups = GROUP_COLORS.map((color) => ({ color }))
  assert.equal(chooseGroupColor(groups, 'orange'), 'orange')
  groups.push({ color: 'orange' })
  assert.equal(chooseGroupColor(groups, 'orange'), 'blue')
})

// 목 크롬. 탭 그룹 배정만 검증하므로 그룹/탭 조작에 필요한 만큼만 흉내낸다.
function mockChrome() {
  const groups = new Map([[100, { id: 100, windowId: 1, color: 'blue', title: 'Personal' }]])
  const tabs = new Map(Array.from({ length: 16 }, (_, i) => [i + 1, { id: i + 1, windowId: i === 4 ? 2 : 1 }]))
  const saved = {}
  const state = { groups, tabs, saved, nextId: 101, loads: 0, failNext: false }
  const tick = () => new Promise((resolve) => setImmediate(resolve))
  state.tick = tick
  state.settle = async (n = 10) => { for (let i = 0; i < n; i++) await tick() }
  state.chrome = {
    runtime: { getManifest: () => ({ name: 'Test' }) },
    storage: {
      local: { get: async () => ({}), set: async () => {} },
      session: {
        get: async () => { state.loads++; await tick(); return {} },
        set: async (value) => { await tick(); Object.assign(saved, value) },
      },
    },
    action: { setTitle() {}, setBadgeText() {} },
    tabs: {
      get: async (id) => {
        const t = tabs.get(id)
        if (!t) throw new Error(`No tab with id ${id}`)
        return t
      },
      query: (query, callback) => callback([...tabs.values()].filter((t) => t.groupId === query.groupId)),
      group: async ({ tabIds, groupId, createProperties }) => {
        await tick()
        if (state.failNext) { state.failNext = false; throw new Error('temporary failure') }
        const id = groupId ?? state.nextId++
        if (groupId == null) groups.set(id, { id, windowId: createProperties.windowId, color: 'grey' })
        for (const tabId of tabIds) tabs.get(tabId).groupId = id
        return id
      },
    },
    tabGroups: {
      get: async (id) => groups.get(id),
      query: async ({ windowId }) => [...groups.values()].filter((g) => g.windowId === windowId).map((g) => ({ ...g })),
      update: async (id, patch) => {
        await tick()
        Object.assign(groups.get(id), patch)
        return groups.get(id)
      },
    },
  }
  return state
}

async function withChrome(state, run) {
  const oldChrome = globalThis.chrome
  const oldSelf = globalThis.self
  globalThis.self = {}
  globalThis.chrome = state.chrome
  try {
    return await run(await import(`./sessions.js?test=${Date.now()}${Math.random()}`))
  } finally {
    globalThis.chrome = oldChrome
    globalThis.self = oldSelf
  }
}

test('concurrent requests share one group per room, preserve manual colors and recover after failure', async () => {
  const state = mockChrome()
  const { groups, tabs, saved } = state
  await withChrome(state, async ({ openSession, groupOwnTab }) => {
    for (const [client, team] of [['a', 'room-a'], ['b', 'room-a'], ['c', 'room-c'], ['d', 'room-d']]) {
      await openSession(client, { paneId: client, team, room: team, name: client, roomColor: 'blue' })
    }
    const [a, b, c, d] = await Promise.all([
      groupOwnTab('a', 1), groupOwnTab('b', 2), groupOwnTab('c', 3), groupOwnTab('d', 5),
    ])
    assert.equal(a, b)
    assert.notEqual(a, c)
    // 첫 로드를 공유해야 늦게 온 저장소 사본이 이미 만든 그룹을 지우지 않는다.
    assert.equal(state.loads, 1)
    assert.equal(groups.get(a).color, 'red')
    assert.equal(groups.get(c).color, 'yellow')
    assert.equal(groups.get(d).color, 'blue')
    // 사람이 만든 그룹은 건드리지 않는다.
    assert.equal(groups.get(100).title, 'Personal')
    assert.equal(groups.get(100).color, 'blue')
    // 합류한 그룹의 색은 사람이 바꿨을 수 있으니 덮어쓰지 않는다.
    groups.get(a).color = 'orange'
    assert.equal(await groupOwnTab('b', 4), a)
    assert.equal(groups.get(a).color, 'orange')
    // 묶기가 실패해도 탭 생성은 실패가 아니고, 다음 호출은 원래 그룹으로 돌아온다.
    state.failNext = true
    assert.equal(await groupOwnTab('c', 6), null)
    assert.equal(await groupOwnTab('c', 7), c)
    // 저장 큐가 비워지기 전에는 메모리만 맞고 저장소는 아직 옛 상태일 수 있다.
    await state.settle(6)
    assert.deepEqual(Object.keys(saved.taskGroups).sort(), ['room:room-a', 'room:room-c', 'room:room-d'])
    // 창을 넘나드는 그룹은 없다 — 다른 창에 있는 탭은 그 창에서 묶인다.
    assert.equal(tabs.get(5).windowId, 2)
    assert.equal(groups.get(d).windowId, 2)
    assert.equal(groups.get(a).windowId, 1)
  })
})

test('the same task shares a group across rooms, and different tasks split one room', async () => {
  const state = mockChrome()
  const { groups, tabs } = state
  await withChrome(state, async ({ openSession, groupOwnTab, setTask }) => {
    for (const [client, team] of [['a', 'room-a'], ['b', 'room-a'], ['c', 'room-c']]) {
      await openSession(client, { paneId: client, team, room: team, name: client, roomColor: 'blue' })
    }
    // 작업명이 없으면 종전대로 방으로 묶인다.
    const roomA = await groupOwnTab('a', 1)
    assert.equal(await groupOwnTab('b', 2), roomA)
    const roomC = await groupOwnTab('c', 3)
    assert.notEqual(roomA, roomC)

    // 같은 작업명이면 방이 달라도 한 그룹으로 모인다.
    setTask('a', '결제 플로')
    setTask('c', '결제 플로')
    await state.settle(20)
    const task = await groupOwnTab('a', 6)
    assert.equal(await groupOwnTab('c', 7), task)
    assert.notEqual(task, roomA)
    // 작업명을 정하는 순간 이미 연 탭도 따라온다.
    assert.equal(tabs.get(1).groupId, task)
    assert.equal(tabs.get(3).groupId, task)
    // 작업명을 안 정한 학생은 방 그룹에 남는다.
    assert.equal(tabs.get(2).groupId, roomA)
    // 제목이 곧 작업명이라 탭바만 봐도 무슨 일이 도는지 읽힌다.
    assert.match(groups.get(task).title, /^결제 플로/)

    // 작업명이 다르면 같은 방이어도 갈린다.
    setTask('b', '로그인')
    await state.settle(20)
    assert.notEqual(tabs.get(2).groupId, task)
    assert.notEqual(tabs.get(2).groupId, roomA)
    assert.match(groups.get(tabs.get(2).groupId).title, /^로그인/)
  })
})
