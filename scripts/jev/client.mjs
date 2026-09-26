import { readFile } from 'node:fs/promises'
import { homedir } from 'node:os'
import { join } from 'node:path'

export const MODEL = 'typesafe/jev-1.13'
export const ENDPOINT = 'https://apis.opengateway.ai/v1/decisions'
export const MAX_BYTES = 128 * 1024

const record = (value) => value !== null && typeof value === 'object' && !Array.isArray(value)
const probability = (value) => typeof value === 'number' && Number.isFinite(value) && value >= 0 && value <= 1

export function choiceRequest(input) {
  if (!record(input)) throw new Error('Decision input must be an object')
  const { state, instructions, choices } = input
  if (state == null || !['string', 'object'].includes(typeof state) || typeof instructions !== 'string' || !instructions.trim()) {
    throw new Error('state and non-empty instructions are required')
  }
  if (!record(choices) || Object.keys(choices).length < 2 || Object.keys(choices).length > 64) {
    throw new Error('choices must contain between 2 and 64 options')
  }
  if (Object.entries(choices).some(([key, value]) => !key.trim() || typeof value !== 'string')) {
    throw new Error('Each choice needs a non-empty key and a text description')
  }
  const request = { model: MODEL, state, questions: { decision: { type: 'choice', instructions, criteria: choices } } }
  if (Buffer.byteLength(JSON.stringify(request)) > MAX_BYTES) throw new Error('Decision input exceeds 128 KiB; narrow the observation')
  return request
}

export function validateAnswer(body, choices) {
  const answer = body?.answers?.decision
  if (body?.model !== MODEL || answer?.type !== 'choice' || typeof answer.choice !== 'string' || !Object.hasOwn(choices, answer.choice)) {
    throw new Error('Jev returned an unknown choice or an invalid response')
  }
  if (!probability(answer.confidence) || !record(answer.probabilities)) throw new Error('Invalid Jev confidence or probabilities')
  const keys = Object.keys(choices)
  if (Object.keys(answer.probabilities).length !== keys.length || keys.some((key) => !Object.hasOwn(answer.probabilities, key) || !probability(answer.probabilities[key]))) {
    throw new Error('Jev probabilities do not match the supplied choices')
  }
  if (Math.abs(keys.reduce((sum, key) => sum + answer.probabilities[key], 0) - 1) > 0.01) {
    throw new Error('Jev probabilities do not sum to one')
  }
  return {
    model: body.model,
    choice: answer.choice,
    confidence: answer.confidence,
    probability: answer.probabilities[answer.choice],
    probabilities: answer.probabilities,
    usage: Object.fromEntries(['input_tokens', 'output_tokens', 'total_tokens']
      .filter((key) => Number.isSafeInteger(body.usage?.[key]) && body.usage[key] >= 0)
      .map((key) => [key, body.usage[key]])),
  }
}

async function readResponse(response) {
  if (!response.body) throw new Error('Empty Jev response')
  const chunks = []
  let size = 0
  const reader = response.body.getReader()
  try {
    for (;;) {
      const { done, value } = await reader.read()
      if (done) break
      size += value.byteLength
      if (size > MAX_BYTES) throw new Error('Jev response exceeds 128 KiB')
      chunks.push(value)
    }
  } finally {
    await reader.cancel().catch(() => {})
    reader.releaseLock()
  }
  try { return JSON.parse(Buffer.concat(chunks).toString('utf8')) } catch { throw new Error('Jev returned invalid JSON') }
}

export async function decide(input, {
  apiKey,
  env = process.env,
  keyFile = join(homedir(), '.config', 'opengateway.key'),
  endpoint = ENDPOINT,
  fetchImpl = fetch,
  timeoutMs = 15000,
} = {}) {
  const request = choiceRequest(input)
  const url = new URL(endpoint)
  if (url.protocol !== 'https:' && !(url.protocol === 'http:' && ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname))) {
    throw new Error('Jev requires HTTPS, except for an explicit loopback test server')
  }
  if (url.username || url.password || url.search || url.hash) throw new Error('Jev endpoint must not contain credentials, a query or a fragment')
  if (!Number.isInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 30000) throw new Error('Invalid Jev timeout')
  let key = apiKey ?? env.OPENGATEWAY_API_KEY
  if (key != null && typeof key !== 'string') throw new Error('Invalid OpenGateway API key')
  if (!key?.trim()) {
    try { key = await readFile(keyFile, 'utf8') } catch { throw new Error('Set OPENGATEWAY_API_KEY or create ~/.config/opengateway.key') }
  }
  key = key.trim()
  if (!key) throw new Error('OpenGateway API key is empty')
  const payload = JSON.stringify(request, (field, value) => {
    // Refuse pasted credentials without silently changing the user's declared choices.
    if (field.includes(key) || (typeof value === 'string' && value.includes(key))) {
      throw new Error('Decision input contains the API credential; remove it before retrying')
    }
    return value
  })
  const signal = AbortSignal.timeout(timeoutMs)
  const started = performance.now()
  let response
  try {
    response = await fetchImpl(url, {
      method: 'POST', redirect: 'error', signal,
      headers: { Authorization: `Bearer ${key}`, 'Content-Type': 'application/json' },
      body: payload,
    })
  } catch {
    throw new Error(signal.aborted ? `Jev timed out after ${timeoutMs} ms` : 'Cannot reach Jev endpoint')
  }
  if (!response.ok) {
    await response.body?.cancel().catch(() => {})
    // Upstream error bodies may echo request data or authentication headers.
    throw new Error(`Jev request failed (HTTP ${response.status}); no action was executed`)
  }
  let body
  try { body = await readResponse(response) } catch (error) {
    if (signal.aborted) throw new Error(`Jev timed out after ${timeoutMs} ms`)
    if (['Empty Jev response', 'Jev response exceeds 128 KiB', 'Jev returned invalid JSON'].includes(error.message)) throw error
    throw new Error('Cannot read Jev response')
  }
  return { ...validateAnswer(body, input.choices), latency_ms: Math.round(performance.now() - started) }
}
