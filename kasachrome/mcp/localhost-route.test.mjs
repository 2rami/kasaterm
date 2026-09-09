import test from 'node:test'
import assert from 'node:assert/strict'
import { isLoopbackUrl, sourceBase, resolveBrowserArgs } from './localhost-route.mjs'

test('loopback hosts do not match lookalike public hosts', () => {
  for (const url of ['http://localhost:3000/a', 'http://localhost.:3000/a', 'http://127.0.0.1:4000/', 'http://127.0.0.2:4000/', 'http://[::1]/']) assert.ok(isLoopbackUrl(url))
  for (const url of ['https://localhost.example/a', 'https://example.com', 'not a url']) assert.ok(!isLoopbackUrl(url))
})
test('source lookup uses only the owning socket port file', () => {
  assert.equal(sourceBase({KASATERM_SOCKET_PATH:'/tmp/fixture.sock'}, (path) => {
    assert.equal(path, '/tmp/fixture.mcp_port'); return '49876\n'
  }), 'http://127.0.0.1:49876')
  assert.throws(() => sourceBase({}), /SOURCE_APP_UNKNOWN/)
  assert.throws(() => sourceBase({KASATERM_SOCKET_PATH:'/tmp/fixture.sock'}, () => '0'), /INVALID/)
})
test('selected remote resolves localhost and preserves other arguments', async () => {
  const args = {url:'http://localhost:3000/path?q=1#hash', active:true}
  const result = await resolveBrowserArgs('new_tab', args, {selected:'laptop'}, {
    base:'http://127.0.0.1:49876', fetch: async (url) => {
      assert.equal(url.searchParams.get('url'), args.url)
      assert.equal(url.searchParams.get('machine'), 'laptop')
      return {ok:true, json:async () => ({ok:true,machine:'laptop',url:'http://127.0.0.1:54001/path?q=1#hash'})}
    },
  })
  assert.deepEqual(result, {url:'http://127.0.0.1:54001/path?q=1#hash',active:true})
  assert.equal(args.url, 'http://localhost:3000/path?q=1#hash')
})
test('resolver failures and selection races never use original localhost', async () => {
  for (const value of [{ok:false,error:'offline'}, {ok:true,machine:'other',url:'http://localhost:3000'}]) {
    await assert.rejects(resolveBrowserArgs('navigate', {url:'http://localhost:3000'}, {selected:'laptop'}, {
      base:'http://127.0.0.1:49876', fetch:async () => ({ok:true,json:async()=>value}),
    }), /LOCALHOST_FORWARD_FAILED/)
  }
})
test('local selection and public URLs need no resolver', async () => {
  const fetch = () => { throw new Error('unexpected request') }
  const local = {url:'http://localhost:3000'}
  assert.equal(await resolveBrowserArgs('new_tab',local,{selected:''},{fetch}), local)
  const publicArgs = {url:'https://example.com'}
  assert.equal(await resolveBrowserArgs('navigate',publicArgs,{selected:'laptop'},{fetch}), publicArgs)
})
