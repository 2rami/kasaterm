import assert from 'node:assert/strict';
import { after, before, test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { createServer } from 'vite';

let server;
let ClassroomView;
let assignSprites;
before(async () => {
  server = await createServer({
    configFile: false,
    server: { middlewareMode: true, hmr: false },
    resolve: { alias: { '@': fileURLToPath(new URL('../', import.meta.url)) } },
    esbuild: { jsx: 'automatic' },
  });
  ({ ClassroomView } = await server.ssrLoadModule('/src/components/ClassroomView.tsx'));
  ({ assignSprites } = await server.ssrLoadModule('/src/lib/sprites.ts'));
});
after(async () => { await server?.close(); });

test('default character view uses the current theme without classroom artwork', () => {
  const html = renderToStaticMarkup(createElement(ClassroomView, { agents: [] }));
  assert.match(html, /background-color:var\(--cth-cream-100\)/);
  assert.doesNotMatch(html, /background-image:|classroom-floor\.png|furn-[^"\s]+\.png/);
});

test('explicit background and furniture remain available', () => {
  const html = renderToStaticMarkup(createElement(ClassroomView, {
    agents: [], background: 'custom-background.png',
    furniture: [{ id: 'plant', kind: 'plant', x: 50, y: 80, w: 10,
      sprite: 'custom-plant.png', block: [45, 70, 55, 80] }],
  }));
  assert.match(html, /custom-background\.png/);
  assert.match(html, /custom-plant\.png/);
});

test('sprite assignment preserves identity instead of assigning an unrelated character', () => {
  const agents = [
    { id: '%1', name: '배포 점검', character: '배포 점검' },
    { id: '%2', name: '사용자 캐릭터', character: '사용자 캐릭터' },
    { id: '%3', name: '기존 세션', character: '시로코' },
  ];
  const result = assignSprites(agents);
  assert.deepEqual(result.map((a) => a.spriteChar), agents.map((a) => a.character));
  assert.deepEqual(result.map(({ spriteChar, ...agent }) => agent), agents);
  assert.ok(agents.every((a) => !('spriteChar' in a)));
});
