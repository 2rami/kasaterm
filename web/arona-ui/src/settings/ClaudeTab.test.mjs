import assert from 'node:assert/strict';
import { after, before, test } from 'node:test';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { createServer } from 'vite';

let server;
let ClaudeTab;
before(async () => {
  server = await createServer({ configFile: false, server: { middlewareMode: true },
    esbuild: { jsx: 'automatic' } });
  ({ ClaudeTab } = await server.ssrLoadModule('/src/settings/ClaudeTab.tsx'));
});
after(async () => { await server?.close(); });

const account = (overrides = {}) => ({ id: 'work', name: 'Work', label: 'Work',
  sub: 'person@example.test', sub_kind: 'mute', slot: true, logged_in: true,
  usage_state: 'ready', usage: 37, ...overrides });
const render = (accounts, overrides = {}) => renderToStaticMarkup(createElement(ClaudeTab, {
  data: { accounts, account: 'work', codex_accounts: [], codex_account: '',
    shim_inject: false, autoswitch: false, autoswitch_pct: 90, model: '', effort: '',
    extra: '', ...overrides }, reload: async () => {},
}));

test('Claude hides the synthetic default and preserves the external-login explanation', () => {
  const html = render([account({ id: '', name: 'SYNTHETIC_DEFAULT' })], { account: '' });
  assert.doesNotMatch(html, /SYNTHETIC_DEFAULT/);
  assert.match(html, /등록된 Claude 계정이 없어요/);
  assert.match(html, /기존 Claude 로그인은 그대로 유지돼요/);
});

test('unselected named accounts stay visible without claiming logout', () => {
  const html = render([account()], { account: '' });
  assert.match(html, /계정을 선택해 주세요/);
  assert.match(html, /person@example.test/);
  assert.doesNotMatch(html, /로그인 필요/);
});

test('auth failure suppresses cached usage and does not erase selection', () => {
  const html = render([account({ logged_in: false, usage_state: 'logged_out' })]);
  assert.match(html, /로그인 필요/);
  assert.match(html, /선택됨/);
  assert.doesNotMatch(html, /37%/);
  assert.doesNotMatch(html, /사용량 확인됨/);
});

test('failed usage explicitly labels previous data and omits an expired reset countdown', () => {
  const html = render([account({ usage_state: 'failed', usage_stale: true, usage_resets: '2h13m' })]);
  assert.match(html, /조회 실패/);
  assert.match(html, /이전 조회 37%/);
  assert.doesNotMatch(html, /2h13m|사용량 확인됨/);
});

test('unknown usage remains checking and never becomes zero percent', () => {
  const html = render([account({ usage: null, usage_state: 'loading', logged_in: null })]);
  assert.match(html, /확인 중…/);
  assert.doesNotMatch(html, /0%|사용량 확인됨/);
});

test('auto switch requires two registered named accounts but an existing switch can turn off', () => {
  const switchTag = (html) => html.match(/<button[^>]*aria-label="자동 전환"[^>]*>/)?.[0];
  assert.match(switchTag(render([account()])), / disabled=""/);
  assert.doesNotMatch(switchTag(render([account()], { autoswitch: true })), / disabled=""/);
  assert.doesNotMatch(switchTag(render([account(), account({ id: 'other' })])), / disabled=""/);
  assert.doesNotMatch(switchTag(render([account(), account({ id: 'other', logged_in: false })])), / disabled=""/);
  assert.doesNotMatch(switchTag(render([account(), account({ id: 'other', logged_in: null })])), / disabled=""/);
  assert.match(switchTag(render([account(), account({ id: '' })])), / disabled=""/);
});

test('selection is a native button and management controls are separate', () => {
  const html = render([account()]);
  const selection = html.match(/<button[^>]*class="kt-account-select"[\s\S]*?<\/button>/)?.[0];
  assert.match(selection, /aria-pressed="true"/);
  assert.equal((selection.match(/<button/g) ?? []).length, 1);
  assert.doesNotMatch(selection, /다시 로그인|빈 창|빼기/);
  assert.match(html, /class="kt-account-actions" role="group"/);
});
