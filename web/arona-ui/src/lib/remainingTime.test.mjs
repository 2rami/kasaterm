import assert from 'node:assert/strict';
import test from 'node:test';
import { formatRemainingTime } from './utils.ts';

test('account reset countdown chooses minutes, hours, and days at their boundaries', () => {
  for (const [seconds, expected] of [
    [0, '곧'],
    [59, '곧'],
    [60, '1분'],
    [3599, '59분'],
    [3600, '1시간'],
    [484 * 60, '8시간 4분'],
    [24 * 3600 - 1, '23시간 59분'],
    [24 * 3600, '1일'],
    [177 * 3600, '7일 9시간'],
  ]) {
    assert.equal(formatRemainingTime(seconds * 1000), expected, `${seconds}s`);
  }
});

test('expired and invalid account reset times do not produce misleading numeric durations', () => {
  assert.equal(formatRemainingTime(-1000), '곧');
  assert.equal(formatRemainingTime(Number.NaN), '');
  assert.equal(formatRemainingTime(Infinity), '');
});
