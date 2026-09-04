import assert from 'node:assert/strict';
import test from 'node:test';
import { resumeCommand } from './resumeCommand.ts';

test('Codex 자동 이어가기는 시작 업데이트만 건너뛴다', () => {
  const id = '01900000-0000-7000-8000-000000000001';
  assert.equal(
    resumeCommand(id, 'codex'),
    `codex resume ${id} -c check_for_update_on_startup=false`,
  );
});

test('Claude와 agy 이어가기는 Codex 설정을 받지 않는다', () => {
  const claude = resumeCommand('00000000-0000-4000-8000-000000000001', 'claude');
  const agy = resumeCommand('conversation-1', 'agy');
  assert.equal(claude, 'claude --resume 00000000-0000-4000-8000-000000000001');
  assert.equal(agy, 'agy --conversation conversation-1');
  assert.ok(!claude.includes('check_for_update_on_startup'));
  assert.ok(!agy.includes('check_for_update_on_startup'));
});
