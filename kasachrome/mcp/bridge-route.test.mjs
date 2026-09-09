import assert from 'node:assert/strict'
import { test } from 'node:test'
import { bridgeRoute, needsFreshBrowserHandles } from './bridge-route.mjs'

const local = 'ws://127.0.0.1:8777'
const book = 'ws://127.0.0.1:19601'
const mini = 'ws://127.0.0.1:19602'

test('an explicit remote choice is strict even when old settings list a local fallback', () => {
  const route = bridgeRoute({ kasachrome_machine: 'book', kasachrome_bridge_urls: `${book},${local}` }, [local], local)
  assert.deepEqual(route.urls, [book])
  assert.equal(route.allowLocalStart, false)
})

test('an unreachable or not-yet-configured selected device cannot launch the local browser', () => {
  for (const raw of [undefined, '', local, [local]]) {
    const route = bridgeRoute({ kasachrome_machine: 'book', kasachrome_bridge_urls: raw }, [local], local)
    assert.deepEqual(route.urls, [])
    assert.equal(route.allowLocalStart, false)
  }
})

test('explicit local choice overrides stale remote URLs and environment', () => {
  assert.deepEqual(bridgeRoute({ kasachrome_machine: '', kasachrome_bridge_urls: book }, [mini], local).urls, [local])
})

test('unconfigured installs preserve their existing environment fallback', () => {
  assert.deepEqual(bridgeRoute({}, [book, local], local).urls, [book, local])
  assert.equal(bridgeRoute({}, [book, local], local).allowLocalStart, true)
})

test('selection or port changes invalidate the connected route, unrelated settings do not', () => {
  const original = { kasachrome_machine: 'book', kasachrome_bridge_urls: book }
  const key = bridgeRoute(original, [local], local).key
  assert.equal(bridgeRoute({ ...original, theme: 'light' }, [local], local).key, key)
  assert.notEqual(bridgeRoute({ ...original, kasachrome_machine: 'mini' }, [local], local).key, key)
  assert.notEqual(bridgeRoute({ ...original, kasachrome_bridge_urls: mini }, [local], local).key, key)
})

test('device-local tab and window handles require rediscovery after switching', () => {
  for (const args of [{ tabId: 1 }, { windowId: 1 }, { tabIds: [1] }]) assert.equal(needsFreshBrowserHandles(args), true)
  for (const args of [{}, { url: 'https://example.test' }]) assert.equal(needsFreshBrowserHandles(args), false)
  assert.equal(needsFreshBrowserHandles({}, 'close_tab'), true, 'implicit active tab is also device-local')
  assert.equal(needsFreshBrowserHandles({}, 'list_tabs'), false)
  assert.equal(needsFreshBrowserHandles({}, 'new_tab'), false)
})
