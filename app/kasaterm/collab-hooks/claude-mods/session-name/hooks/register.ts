import type { EngineInterface, Register } from 'claude-code'

import { NAME_MAX, tidy, titlesIn } from './name'
import type { Titles } from './name'

const ASK = `사람이 코딩 에이전트에게 맡긴 말을 보고 이 작업 세션의 이름을 한국어 명사구로 지어라. ${NAME_MAX}자 이내, 이름 한 줄만. 따옴표·마침표 없이.`
// $.fs.read 의 상한. 넘는 전사본은 이름 유무를 모르니 손대지 않는다.
const READ_CAP = 4 * 1024 * 1024

// 전사본마다 한 번 — /clear 는 새 전사본으로 넘어가 다시 이름을 받는다. 다시 실리면 비지만,
// 그때는 전사본에 남은 custom-title 이 다시 막는다.
const settled = new Set<string>()

async function titles($: EngineInterface, path: string): Promise<Titles | undefined> {
  try {
    if (!(await $.fs.exists(path))) return { custom: '', ai: '' }
    if ((await $.fs.stat(path)).size > READ_CAP) return undefined
    return titlesIn(await $.fs.read(path))
  } catch {
    return undefined
  }
}

async function coin($: EngineInterface, prompt: string): Promise<string> {
  const r = await $.model.complete({
    model: 'haiku',
    system: ASK,
    prompt: prompt.slice(0, 2000),
    maxTokens: 60,
    effort: 'low',
    timeoutMs: 5000,
  })
  return r.isAnswered ? tidy(r.text) : ''
}

// 이름을 지을 말이 없으면(`prompt` 가 비면) claude 가 스스로 지은 요약만 쓴다.
async function nameFor($: EngineInterface, path: string, prompt: string): Promise<string> {
  if (settled.has(path)) return ''
  const t = await titles($, path)
  if (!t || t.custom) {
    settled.add(path)
    return ''
  }
  const name = t.ai ? tidy(t.ai) : prompt ? await coin($, prompt) : ''
  if (name) settled.add(path)
  return name
}

export const register: Register = on => {
  // 되살린 칸은 말을 걸기 전에도 이름이 보이게 — 전사본의 요약으로 뜨자마자 붙인다.
  on('classic.SessionStart', async ($, e, next) => {
    const r = await next(e)
    if (r.sessionTitle) {
      settled.add(e.transcript_path)
      return r
    }
    const name = await nameFor($, e.transcript_path, '')
    return name ? { ...r, sessionTitle: name } : r
  }).catch(($, e, next) => next(e))

  on('classic.UserPromptSubmit', async ($, e, next) => {
    const r = await next(e)
    // kasaterm 의 tell --title 이 설정 훅으로 낸 이름 — 그대로 둔다.
    if (r.sessionTitle) {
      settled.add(e.transcript_path)
      return r
    }
    // 백그라운드 끝남 알림 같은 기계 턴이나 슬래시 명령으로는 이름을 짓지 않는다.
    const byHand = (e.source === undefined || e.source === 'user') && !e.prompt.trimStart().startsWith('/')
    const name = await nameFor($, e.transcript_path, byHand ? e.prompt : '')
    return name ? { ...r, sessionTitle: name } : r
  }).catch(($, e, next) => next(e))
}
