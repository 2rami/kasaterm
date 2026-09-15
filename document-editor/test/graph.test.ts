import { test } from 'node:test';
import assert from 'node:assert/strict';
import { graphLayout } from '../src/graph.ts';

test('graph layout keeps real nodes, resolves neighbors and ignores dangling or self edges', () => {
  const points = graphLayout({ nodes: [{ id: 'a', name: '가' }, { id: 'b', name: '나' }, { id: 'c', name: '다' }, { id: 'a', name: 'duplicate' }], edges: [{ source: 'a', target: 'b' }, { source: 'b', target: 'a' }, { source: 'a', target: 'missing' }, { source: 'c', target: 'c' }] });
  assert.equal(points.length, 3);
  assert.deepEqual([...points[0].neighbors], ['b']);
  assert.deepEqual([...points[1].neighbors], ['a']);
  assert.equal(points[2].neighbors.size, 0);
  assert.ok(points.every(p => Number.isFinite(p.x) && Number.isFinite(p.y)));
  assert.ok(Math.hypot(points[0].x - points[1].x, points[0].y - points[1].y) < Math.hypot(points[0].x - points[2].x, points[0].y - points[2].y));
});
test('empty graph and a large cyclic graph complete without recursion or pairwise simulation', () => {
  assert.deepEqual(graphLayout({ nodes: [], edges: [] }), []);
  const nodes = Array.from({ length: 5000 }, (_, i) => ({ id: String(i), name: `문서 ${i}` }));
  const edges = nodes.map((node, i) => ({ source: node.id, target: String((i + 1) % nodes.length) }));
  const points = graphLayout({ nodes, edges });
  assert.equal(points.length, nodes.length);
  assert.ok(points.every(p => p.neighbors.size === 2 && Number.isFinite(p.x)));
  assert.deepEqual(graphLayout({ nodes, edges }), points);
});
