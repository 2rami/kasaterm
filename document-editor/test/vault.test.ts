import { test } from 'node:test';
import assert from 'node:assert/strict';
import { visibleRows, type VaultEntry } from '../src/vault.ts';

test('collapsed folders do not materialize their children', () => {
  const folder: VaultEntry = { id:'notes', name:'기록', kind:'folder' };
  const children = new Map([['notes', { entries:Array.from({length:10000},(_,i) => ({id:`notes/${i}.md`,name:`${i}.md`,kind:'markdown' as const})) }]]);
  const closed = visibleRows([folder], children, new Set(), new Set());
  assert.equal(closed.length, 1);
  const open = visibleRows([folder], children, new Set(['notes']), new Set());
  assert.equal(open.length, 10001); assert.equal(open[1].depth, 1);
});
test('lazy folders expose loading and paginated children without losing relative IDs', () => {
  const folder: VaultEntry = {id:'한글',name:'한글',kind:'folder'};
  const loading = visibleRows([folder],new Map(),new Set(['한글']),new Set(['한글']));
  assert.equal(loading[1].loading,true);
  const rows = visibleRows([folder],new Map([['한글',{entries:[{id:'한글/문서.md',name:'문서.md',kind:'markdown'}],nextCursor:256}]]),new Set(['한글']),new Set(),512);
  assert.equal(rows[1].entry?.id,'한글/문서.md');
  assert.deepEqual(rows[2],{depth:1,parentId:'한글',more:256});
  assert.deepEqual(rows[3],{depth:0,parentId:'',more:512});
});
test('Rust null cursors terminate a listing instead of rendering an endless more row', () => {
  const rows = visibleRows([],new Map(),new Set(),new Set(),null as unknown as undefined);
  assert.equal(rows.length,0);
});
