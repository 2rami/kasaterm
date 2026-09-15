import { test } from 'node:test';
import assert from 'node:assert/strict';
import { applyTheme } from '../src/theme.ts';

test('theme replacement updates the host tokens exactly without stale previous colors', () => {
  const values = new Map<string, string>();
  const root = { dataset: {} as Record<string, string>, style: { setProperty: (key: string, value: string) => values.set(key, value), removeProperty: (key: string) => values.delete(key) } };
  const supports = (value: string) => /^#[a-f\d]{6}$/i.test(value);
  applyTheme({ mode: 'light', background: '#f0eada', foreground: '#182127', accent: '#bb4400', muted: '#555544', selection: '#ddbb99', checkboxForeground: '#ffffff' }, root as unknown as HTMLElement, supports);
  assert.equal(values.get('--paper'), '#f0eada'); assert.equal(values.get('--selection'), '#ddbb99');
  applyTheme({ mode: 'dark', background: '#172333', foreground: '#ddeeff', accent: '#00aacc', surfaceHover: '#223344' }, root as unknown as HTMLElement, supports);
  assert.equal(root.dataset.theme, 'dark'); assert.equal(values.get('--paper'), '#172333'); assert.equal(values.get('--ink'), '#ddeeff');
  assert.equal(values.get('--hover'), '#223344'); assert.equal(values.has('--muted'), false);
  applyTheme('light', root as unknown as HTMLElement, supports);
  assert.equal(root.dataset.theme, 'light'); assert.equal(values.size, 0);
});
test('theme values reject declarations, URLs and custom property references', () => {
  const values = new Map<string, string>();
  const root = { dataset: {}, style: { setProperty: (key: string, value: string) => values.set(key, value), removeProperty: (key: string) => values.delete(key) } };
  applyTheme({ background: 'red; display:none', foreground: 'var(--private)', accent: 'url(https://example.com)', border: '#123456' }, root as unknown as HTMLElement, () => true);
  assert.deepEqual([...values], [['--line', '#123456']]);
});
