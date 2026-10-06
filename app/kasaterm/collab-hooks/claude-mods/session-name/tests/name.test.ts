import { expect, test } from 'claude-code/testing'
import type { On } from 'claude-code'

import { tidy, titlesIn } from '../hooks/name'

type World = { transcript?: string; settings?: string; reply?: string }

// 엔진 자리 — 설정 훅(kasaterm 의 tell --title)·전사본·모델 답을 정해 두고 모델 호출 수를 센다.
function world(on: On, w: World) {
  const asked: string[] = []
  on('classic.UserPromptSubmit', () => (w.settings ? { sessionTitle: w.settings } : {}))
  on('fs.exists', () => ({ value: w.transcript !== undefined }))
  on('fs.stat', () => ({ value: { kind: 'file', size: w.transcript?.length ?? 0, mtimeMs: 0, isLink: false } }))
  on('fs.read', () => ({ value: w.transcript ?? '' }))
  on('model.complete', ($, e) => {
    asked.push(e.prompt)
    return { value: { isAnswered: true, text: w.reply ?? '', usage: { input_tokens: 1, output_tokens: 1, cache_read_input_tokens: 0, cache_creation_input_tokens: 0 } } }
  })
  return asked
}

const line = (v: Record<string, unknown>) => JSON.stringify(v)
const AT = { transcript_path: '/t/s.jsonl' } as const
const PROMPT = { ...AT, prompt: 'Tunnet 이 우리 카사넷이랑 비슷한지 봐줘', source: 'user' } as const

test('a fresh session is named from its first prompt', async ($, on) => {
  const asked = world(on, { reply: '「Tunnet 비교」' })
  const r = await $.classic.UserPromptSubmit(PROMPT)
  expect(r.sessionTitle).toBe('Tunnet 비교')
  expect(asked).toEqual([PROMPT.prompt])
})

test('a name a person or kasaterm gave is never replaced', async ($, on) => {
  const asked = world(on, {
    transcript: [line({ type: 'custom-title', customTitle: '1Password 승인' }), line({ type: 'ai-title', aiTitle: '다른 요약' })].join('\n'),
    reply: '엉뚱한 이름',
  })
  const r = await $.classic.UserPromptSubmit(PROMPT)
  expect(r.sessionTitle).toBeUndefined()
  expect(asked).toEqual([])
})

test("kasaterm's own title from tell --title passes through untouched", async ($, on) => {
  const asked = world(on, { settings: '지금 일', reply: '엉뚱한 이름' })
  const r = await $.classic.UserPromptSubmit(PROMPT)
  expect(r.sessionTitle).toBe('지금 일')
  expect(asked).toEqual([])
})

test("claude's own summary names an ongoing session without a model call", async ($, on) => {
  const asked = world(on, { transcript: line({ type: 'ai-title', aiTitle: 'Tunnet 저장소 비교' }) + '\n' })
  const r = await $.classic.UserPromptSubmit({ ...AT, prompt: '좋아', source: 'user' })
  expect(r.sessionTitle).toBe('Tunnet 저장소 비교')
  expect(asked).toEqual([])
})

test('a machine turn or a slash command does not coin a name', async ($, on) => {
  const asked = world(on, { reply: '엉뚱한 이름' })
  expect((await $.classic.UserPromptSubmit({ ...AT, prompt: '<task-notification>끝남</task-notification>', source: 'system' })).sessionTitle).toBeUndefined()
  expect((await $.classic.UserPromptSubmit({ ...AT, prompt: '/effort high', source: 'user' })).sessionTitle).toBeUndefined()
  expect(asked).toEqual([])
})

test('the name is given once', async ($, on) => {
  const asked = world(on, { reply: '카사넷 비교' })
  expect((await $.classic.UserPromptSubmit(PROMPT)).sessionTitle).toBe('카사넷 비교')
  expect((await $.classic.UserPromptSubmit(PROMPT)).sessionTitle).toBeUndefined()
  expect(asked.length).toBe(1)
})

test('a restored session is named from its summary as soon as it starts', async ($, on) => {
  on('classic.SessionStart', () => ({}))
  const asked = world(on, { transcript: line({ type: 'ai-title', aiTitle: '미러링 방식 변경' }) + '\n', reply: '엉뚱한 이름' })
  const r = await $.classic.SessionStart({ ...AT, source: 'resume' })
  expect(r.sessionTitle).toBe('미러링 방식 변경')
  expect((await $.classic.UserPromptSubmit(PROMPT)).sessionTitle).toBeUndefined()
  expect(asked).toEqual([])
})

test('a brand-new session waits for its first prompt', async ($, on) => {
  on('classic.SessionStart', () => ({}))
  const asked = world(on, { reply: '카사넷 비교' })
  expect((await $.classic.SessionStart({ ...AT, source: 'startup' })).sessionTitle).toBeUndefined()
  expect((await $.classic.UserPromptSubmit(PROMPT)).sessionTitle).toBe('카사넷 비교')
  expect(asked).toEqual([PROMPT.prompt])
})

test('after /clear the new transcript gets a name of its own', async ($, on) => {
  const asked = world(on, { reply: '카사넷 비교' })
  expect((await $.classic.UserPromptSubmit(PROMPT)).sessionTitle).toBe('카사넷 비교')
  const cleared = { ...PROMPT, transcript_path: '/t/after-clear.jsonl' }
  expect((await $.classic.UserPromptSubmit(cleared)).sessionTitle).toBe('카사넷 비교')
  expect(asked.length).toBe(2)
})

test('titles are read from records, not from text that merely mentions them', async () => {
  const quoted = line({ type: 'user', message: '"type":"custom-title" 을 grep 해 봐' })
  expect(titlesIn(quoted)).toEqual({ custom: '', ai: '' })
  expect(tidy('이름: "모드 시험."\n둘째 줄')).toBe('모드 시험')
  expect([...tidy('가'.repeat(40))].length).toBe(24)
})
