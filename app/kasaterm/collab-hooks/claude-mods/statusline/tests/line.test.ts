import { expect, test } from 'claude-code/testing'

import { branchOf, displayName, plainParts, statuslineInput } from '../hooks/line'
import type { Engine } from '../hooks/line'

const ENGINE: Engine = {
  at_ms: 1000,
  session_id: 'old',
  cwd: '/repo',
  model: { id: 'claude-opus-5-5[1m]', display_name: 'Opus 5.5 (1M context)' },
  effort: { level: 'xhigh' },
  context_window: { used_percentage: 3, context_window_size: 1000000, total_input_tokens: 30000 },
}

test('a model id reads as the name the engine shows', () => {
  expect(displayName('claude-opus-5-5[1m]')).toBe('Opus 5.5')
  expect(displayName('claude-sonnet-5-5')).toBe('Sonnet 5.5')
  expect(displayName('claude-haiku-4-5-20251001')).toBe('Haiku 4.5')
  expect(displayName('claude-fable-5-1')).toBe('Fable 5.1')
  expect(displayName('gpt-5.5')).toBe('gpt-5.5')
})

test('HEAD reads as the branch the kasaterm status line shows', () => {
  expect(branchOf('ref: refs/heads/feat/rain\n')).toBe('feat/rain')
  expect(branchOf('3f2a9c0000000000000000000000000000000000\n')).toBe('HEAD')
  expect(branchOf('')).toBe('')
})

test('what the engine handed last stands until something fresher is known', () => {
  expect(statuslineInput(ENGINE, {}, 'now')).toEqual({ ...ENGINE, at_ms: undefined, session_id: 'now' } as never)
  const stale = statuslineInput(ENGINE, { effort: { value: 'low', at: 900 }, model: { value: 'claude-sonnet-5-5', at: 900 } }, 'now')
  expect(stale?.effort).toEqual({ level: 'xhigh' })
  expect(stale?.model).toEqual(ENGINE.model)
})

test('a fresh model, effort, folder and context replace the engine values', () => {
  const input = statuslineInput(
    ENGINE,
    {
      model: { value: 'claude-sonnet-5-5', at: 2000 },
      effort: { value: 'low', at: 2000 },
      cwd: { value: '/repo/sub', at: 2000 },
      context: { value: { percent: 41, window: 200000, tokens: 82000 }, at: 2000 },
    },
    'now',
  )
  expect(input?.model).toEqual({ id: 'claude-sonnet-5-5', display_name: 'Sonnet 5.5' })
  expect(input?.effort).toEqual({ level: 'low' })
  expect(input?.cwd).toBe('/repo/sub')
  expect(input?.context_window).toEqual({ used_percentage: 41, context_window_size: 200000, total_input_tokens: 82000 })
  const back = statuslineInput(ENGINE, { model: { value: 'claude-opus-5-5[1m]', at: 2000 } }, 'now')
  expect(back?.model).toEqual(ENGINE.model)
})

test('right after a model switch the line waits for the engine unless effort was learned after it', () => {
  expect(statuslineInput(ENGINE, { model: { value: 'claude-sonnet-5-5', at: 2000 } }, 'now')).toBeNull()
  expect(
    statuslineInput(ENGINE, { model: { value: 'claude-sonnet-5-5', at: 2000 }, effort: { value: 'xhigh', at: 1500 } }, 'now'),
  ).toBeNull()
  const learned = statuslineInput(
    ENGINE,
    { model: { value: 'claude-sonnet-5-5', at: 2000 }, effort: { value: 'medium', at: 2100 } },
    'now',
  )
  expect(learned?.effort).toEqual({ level: 'medium' })
})

test('a model without effort drops the effort part', () => {
  expect(statuslineInput(ENGINE, { effort: { value: '', at: 2000 } }, 'now')?.effort).toBeUndefined()
})

test('outside kasaterm the line has the kasaterm order and colours', () => {
  const parts = plainParts({ model: 'claude-opus-5-5[1m]', window: 1000000, percent: 92.4, branch: 'main', cwd: '/w/kasaterm/', effort: 'xhigh' })
  expect(parts.map(p => p.map(s => s.text).join(''))).toEqual([' Opus 5.5 1M', ' main', ' kasaterm', '92%', ' xhigh'])
  expect(parts[3]?.[0]?.color).toBe('#f7768e')
  expect(parts[0]?.[0]?.bold && parts[0]?.[1]?.dim).toBe(true)
  const bare = plainParts({ model: 'claude-sonnet-5-5', window: 0, branch: '', cwd: '/tmp' }, 'plain')
  expect(bare.map(p => p.map(s => s.text).join(''))).toEqual(['M Sonnet 5.5', 'dir tmp', '0%'])
})
