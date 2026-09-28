import assert from 'node:assert/strict';
import { after, before, test } from 'node:test';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { createServer } from 'vite';

let server;
let ClassroomView;
before(async () => {
  server = await createServer({
    configFile: false,
    server: { middlewareMode: true, hmr: false },
    resolve: { alias: { '@': fileURLToPath(new URL('../', import.meta.url)) } },
    esbuild: { jsx: 'automatic' },
  });
  ({ ClassroomView } = await server.ssrLoadModule('/src/components/ClassroomView.tsx'));
});
after(async () => { await server?.close(); });

function render(reducedMotion = false, status = 'blocked') {
  const originalWindow = globalThis.window;
  globalThis.window = { matchMedia: (query) => {
    assert.equal(query, '(prefers-reduced-motion: reduce)');
    return { matches: reducedMotion };
  } };
  try {
    return renderToStaticMarkup(createElement(ClassroomView, {
      agents: [{ id: '%1', name: '작업', character: '작업', status, accent: 'sky', project: 'KASA' }],
      selectedId: '%1',
      seats: [{ x: 30, y: 65, facing: 'front' }, { x: 70, y: 65, facing: 'front' }],
      onAdd: () => {},
    }));
  } finally {
    if (originalWindow === undefined) delete globalThis.window;
    else globalThis.window = originalWindow;
  }
}

test('character placement uses parent-sized transforms without blocking other controls', () => {
  const html = render();
  const position = html.match(/<div class="cth-character-position"[^>]*>/)?.[0];
  assert.match(position, /position:absolute;inset:0;pointer-events:none/);
  assert.match(position, /transform:translate\(30%, 65%\)/);
  assert.match(position, /transition:transform 0ms linear/);
  assert.match(position, /z-index:9999/);
  assert.match(html, /<button[^>]*title="작업 — 클릭하면 대화"[^>]*pointer-events:auto/);
  assert.doesNotMatch(html, /transition:(?:left|top)/);
});

test('reduced motion stops placement and nested sprite animations while retaining status', () => {
  const html = render(true);
  const position = html.match(/<div class="cth-character-position"[^>]*>/)?.[0];
  assert.match(position, /transition:none/);
  assert.match(html, /@media \(prefers-reduced-motion: reduce\)/);
  assert.match(html, /\.cth-classroom-view \* \{ animation: none !important; transition: none !important; \}/);
  assert.match(html, /확인 필요!/);
  assert.match(html, /title="이 자리에 캐릭터 부르기"/);
  assert.doesNotMatch(html, /schale-glyph-pulse|schale-enter/);
});

test('attention, waiting, and thinking states remain readable without decorative pulses', () => {
  for (const [status, text] of [['blocked', '확인 필요!'], ['waiting', '입력 대기'], ['thinking', '생각 중']]) {
    const html = render(false, status);
    assert.ok(html.includes(text), `${status} label remains visible`);
    assert.doesNotMatch(html, /schale-glyph-pulse|schale-enter/);
  }
});

test('command tabs update without a decorative transition', async () => {
  const source = await readFile(new URL('./CommandCenter.tsx', import.meta.url), 'utf8');
  assert.doesNotMatch(source, /\b(?:transition|animation)\s*:/);
  assert.match(source, /background: tab === t/);
  assert.match(source, /color: tab === t/);
});
