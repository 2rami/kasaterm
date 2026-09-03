import assert from 'node:assert/strict';
import test from 'node:test';
import { LatestCommitCoordinator } from './latestCommit.ts';

const deferred = () => {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
};

const fakeFrames = () => {
  const queued = new Map();
  let id = 0;
  return {
    request(callback) { queued.set(++id, callback); return id; },
    cancel(frame) { queued.delete(frame); },
    tick() {
      const callbacks = [...queued.values()];
      queued.clear();
      callbacks.forEach((callback) => callback());
    },
  };
};

test('in-flight와 pending 미리보기 뒤에 최신 저장을 직렬화한다', async () => {
  const a = deferred();
  const b = deferred();
  const bStarted = deferred();
  const calls = [];
  const frames = fakeFrames();
  const queue = new LatestCommitCoordinator(
    (value) => {
      calls.push(`preview:${value}`);
      if (value === 'B') bStarted.resolve();
      return value === 'A' ? a.promise : b.promise;
    },
    async (value) => { calls.push(`commit:${value}`); },
    frames,
  );

  queue.pushPreview('A');
  frames.tick();
  queue.pushPreview('B');
  const saved = queue.commitLatest('C');
  a.resolve();
  await bStarted.promise;
  assert.deepEqual(calls, ['preview:A', 'preview:B']);
  b.resolve();
  await saved;
  assert.deepEqual(calls, ['preview:A', 'preview:B', 'commit:C']);
});

test('미리보기 실패와 관계없이 최신 값을 저장한다', async () => {
  const a = deferred();
  const calls = [];
  const frames = fakeFrames();
  const queue = new LatestCommitCoordinator(
    (value) => {
      calls.push(`preview:${value}`);
      return value === 'A' ? a.promise : Promise.reject(new Error('preview B'));
    },
    async (value) => { calls.push(`commit:${value}`); },
    frames,
  );

  queue.pushPreview('A');
  frames.tick();
  queue.pushPreview('B');
  const saved = queue.commitLatest('C');
  a.reject(new Error('preview A'));
  await saved;
  assert.deepEqual(calls, ['preview:A', 'preview:B', 'commit:C']);
});

test('빠른 연속 저장은 마지막 값만 보낸다', async () => {
  const calls = [];
  const frames = fakeFrames();
  const queue = new LatestCommitCoordinator(
    async (value) => { calls.push(`preview:${value}`); },
    async (value) => { calls.push(`commit:${value}`); },
    frames,
  );

  const first = queue.commitLatest('C');
  const second = queue.commitLatest('D');
  await Promise.all([first, second]);
  assert.deepEqual(calls, ['commit:D']);
});

test('팔레트 칸 전환 뒤에도 이미 큐에 든 값은 원래 칸으로 보낸다', async () => {
  const a = deferred();
  const calls = [];
  const frames = fakeFrames();
  const queue = new LatestCommitCoordinator(
    (value) => { calls.push(`old-preview:${value}`); return a.promise; },
    async (value) => { calls.push(`old-commit:${value}`); },
    frames,
  );

  queue.pushPreview('A');
  frames.tick();
  const saved = queue.commitLatest('C');
  queue.setSenders(
    async (value) => { calls.push(`new-preview:${value}`); },
    async (value) => { calls.push(`new-commit:${value}`); },
  );
  a.resolve();
  await saved;
  assert.deepEqual(calls, ['old-preview:A', 'old-commit:C']);
});

test('서로 다른 팔레트 칸의 저장은 각각 완료한다', async () => {
  const a = deferred();
  const calls = [];
  const frames = fakeFrames();
  const oldSlot = new LatestCommitCoordinator(
    () => a.promise,
    async (value) => { calls.push(`old-commit:${value}`); },
    frames,
  );
  const newSlot = new LatestCommitCoordinator(
    async () => undefined,
    async (value) => { calls.push(`new-commit:${value}`); },
    frames,
  );

  oldSlot.pushPreview('A');
  frames.tick();
  const oldSaved = oldSlot.commitLatest('C');
  oldSlot.dispose();
  const newSaved = newSlot.commitLatest('D');
  a.resolve();
  await Promise.all([oldSaved, newSaved]);
  assert.deepEqual(calls.sort(), ['new-commit:D', 'old-commit:C']);
});
