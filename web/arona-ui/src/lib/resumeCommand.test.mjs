import assert from 'node:assert/strict';
import test from 'node:test';
import { resumeCommand } from './resumeCommand.ts';

test('Codex 자동 이어가기는 시작 업데이트만 건너뛴다', () => {
  const id = '01a06773-4e83-7782-a251-361937f953fc';
  assert.equal(
    resumeCommand(id, 'codex'),
    `codex resume ${id} -c check_for_update_on_startup=false`,
  );
});

test('Claude와 agy 이어가기는 Codex 설정을 받지 않는다', () => {
  const claude = resumeCommand('6043e850-19a1-4b83-834c-190081ede618', 'claude');
  const agy = resumeCommand('conversation-1', 'agy');
  assert.equal(claude, 'claude --resume 6043e850-19a1-4b83-834c-190081ede618');
  assert.equal(agy, 'agy --conversation conversation-1');
  assert.ok(!claude.includes('check_for_update_on_startup'));
  assert.ok(!agy.includes('check_for_update_on_startup'));
});
