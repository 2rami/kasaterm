import test from 'node:test'
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { choiceRequest, validateAnswer, decide, MODEL, ENDPOINT, MAX_BYTES } from './client.mjs'
import { clean, boardPlan, browserPlan, advisory } from './adapters.mjs'
import { runCli } from './cli.mjs'

const input = {
  state: { summary: 'Two tests failed.' }, instructions: 'Choose the next read-only step.',
  choices: { inspect: 'Read failure details.', wait: 'Wait.' },
}
const apiKey = 'fixture-only-not-a-real-api-key'
const bodyFor = (choices = input.choices, choice = Object.keys(choices)[0]) => ({
  model: MODEL,
  answers: { decision: { type: 'choice', choice, confidence: 0.95,
    probabilities: Object.fromEntries(Object.keys(choices).map((key) => [key, key === choice ? 1 : 0])) } },
  usage: { input_tokens: 42, output_tokens: 4, total_tokens: 46, irrelevant: 'ignore' },
})
const responseFor = (body = bodyFor()) => new Response(JSON.stringify(body), { status: 200 })
const fixture = (now = Date.now()) => ({ ok: true, result: {
  schema_version: 1, cursor: 'epoch:12', observed_at_ms: now,
  sources: [{ machine_id: 'machine-a', state: 'online', complete: true, observed_at_ms: now }],
  panes: [{
    address: { machine_id: 'machine-a', surface_key: 'surface-a', surface_id: '%7', session_id: 'session-a', instance_id: 'instance-a' },
    machine_label: 'Mac', room_label: 'Room', character: 'Agent', harness: 'codex',
    title: 'Run tests', request: 'Inspect the failing tests', progress: 'Two tests failed.',
    status: 'idle', status_reason: 'turn closed', freshness: 'fresh', observed_at_ms: now,
  }],
} })
const page = {
  url: 'https://example.test/docs', title: 'Docs', visibilityState: 'visible',
  snapshot: '- link "Documentation" [ref=e1] -> https://example.test/guide\n- button "Delete" [ref=e2] (disabled)\n- textbox "private filled value" [ref=e3] (value="private filled value")\n- button "Next" [ref=e4]',
}
const decisionFor = (choices, choice) => ({ ...validateAnswer(bodyFor(choices, choice), choices), latency_ms: 10 })

test('request uses the pinned decisions model and declared choice schema', () => {
  assert.deepEqual(choiceRequest(input), {
    model: MODEL, state: input.state,
    questions: { decision: { type: 'choice', instructions: input.instructions, criteria: input.choices } },
  })
})

for (const [name, patch] of [
  ['missing state', { state: null }], ['numeric state', { state: 3 }],
  ['blank instructions', { instructions: ' ' }], ['too few options', { choices: { a: 'A' } }],
  ['array options', { choices: ['A', 'B'] }], ['blank key', { choices: { ' ': 'A', b: 'B' } }],
  ['non-text description', { choices: { a: 1, b: 'B' } }],
  ['too many options', { choices: Object.fromEntries(Array.from({ length: 65 }, (_, i) => [String(i), 'Option'])) }],
  ['oversized input', { state: 'x'.repeat(MAX_BYTES) }],
]) {
  test(`request rejects ${name}`, () => assert.throws(() => choiceRequest({ ...input, ...patch })))
}

test('null input is rejected with a controlled error', () => {
  assert.throws(() => choiceRequest(null), /must be an object/)
})

test('valid response retains confidence, probability and numeric usage only', () => {
  const body = bodyFor()
  body.usage.output_tokens = -1
  const result = validateAnswer(body, input.choices)
  assert.equal(result.choice, 'inspect')
  assert.equal(result.probability, 1)
  assert.equal(result.confidence, 0.95)
  assert.deepEqual(result.usage, { input_tokens: 42, total_tokens: 46 })
})

for (const [name, mutate] of [
  ['wrong model', (b) => { b.model = 'different-model' }],
  ['untyped answer', (b) => { b.answers.decision.type = 'text' }],
  ['invented option', (b) => { b.answers.decision.choice = 'execute' }],
  ['non-string option', (b) => { b.answers.decision.choice = ['inspect'] }],
  ['invalid confidence', (b) => { b.answers.decision.confidence = 1.01 }],
  ['missing confidence', (b) => { delete b.answers.decision.confidence }],
  ['missing probability', (b) => { delete b.answers.decision.probabilities.wait }],
  ['extra probability', (b) => { b.answers.decision.probabilities.extra = 0 }],
  ['negative probability', (b) => { b.answers.decision.probabilities.wait = -0.1 }],
  ['string probability', (b) => { b.answers.decision.probabilities.inspect = '1' }],
  ['unnormalized distribution', (b) => { b.answers.decision.probabilities.wait = 0.5 }],
]) {
  test(`response rejects ${name}`, () => {
    const body = bodyFor()
    mutate(body)
    assert.throws(() => validateAnswer(body, input.choices))
  })
}

test('live client uses one bounded, non-redirecting POST without exposing the key', async () => {
  let calls = 0
  const result = await decide(input, { apiKey, fetchImpl: async (url, options) => {
    calls++
    assert.equal(String(url), ENDPOINT)
    assert.equal(options.method, 'POST')
    assert.equal(options.redirect, 'error')
    assert.ok(options.signal instanceof AbortSignal)
    assert.equal(options.headers.Authorization, `Bearer ${apiKey}`)
    assert.deepEqual(JSON.parse(options.body), choiceRequest(input))
    return responseFor()
  } })
  assert.equal(calls, 1)
  assert.equal(result.choice, 'inspect')
  assert.ok(result.latency_ms >= 0)
  assert.ok(!JSON.stringify(result).includes(apiKey))
})

test('environment key works without reading a key file', async () => {
  await decide(input, { env: { OPENGATEWAY_API_KEY: apiKey }, keyFile: '/not-present', fetchImpl: async () => responseFor() })
})

test('missing key fails before network access', async () => {
  await assert.rejects(decide(input, { env: {}, keyFile: '/not-present/jev-key', fetchImpl: () => assert.fail('network called') }), /Set OPENGATEWAY_API_KEY/)
})

test('a pasted real credential is refused even when JSON escaping changes it', async () => {
  const escapedKey = 'fixture-quote-"-secret'
  await assert.rejects(decide({ ...input, state: `Pasted ${escapedKey}` }, {
    apiKey: escapedKey, fetchImpl: () => assert.fail('network called'),
  }), /contains the API credential/)
})

for (const endpoint of ['http://example.test/decisions', 'https://user:pass@example.test', 'https://example.test?key=secret', 'https://example.test/#fragment']) {
  test(`endpoint guard rejects ${endpoint}`, async () => {
    await assert.rejects(decide(input, { apiKey, endpoint, fetchImpl: () => assert.fail('network called') }))
  })
}

test('explicit HTTP loopback remains available for local test servers', async () => {
  await decide(input, { apiKey, endpoint: 'http://127.0.0.1:1234/v1/decisions', fetchImpl: async () => responseFor() })
})

test('HTTP errors are not retried and never echo upstream bodies', async () => {
  let calls = 0
  await assert.rejects(decide(input, { apiKey, fetchImpl: async () => {
    calls++
    return new Response(`Secret echo: ${apiKey}`, { status: 429 })
  } }), (error) => error.message.includes('HTTP 429') && !error.message.includes(apiKey))
  assert.equal(calls, 1)
})

test('network errors do not echo authentication information', async () => {
  await assert.rejects(decide(input, { apiKey, fetchImpl: async () => { throw new Error(apiKey) } }), /^Error: Cannot reach Jev endpoint$/)
})

test('timeout fails closed', async () => {
  await assert.rejects(decide(input, { apiKey, timeoutMs: 1, fetchImpl: async () => {
    await new Promise((resolve) => setTimeout(resolve, 20))
    throw new Error('aborted')
  } }), /timed out after 1 ms/)
})

test('invalid JSON, oversized bodies and broken streams fail closed', async () => {
  for (const [response, expected] of [
    [new Response('not JSON'), /invalid JSON/],
    [new Response('x'.repeat(MAX_BYTES + 1)), /exceeds 128 KiB/],
    [new Response(null), /Empty Jev response/],
    [new Response(new ReadableStream({ start(controller) { controller.error(new Error(apiKey)) } })), /Cannot read Jev response/],
  ]) {
    await assert.rejects(decide(input, { apiKey, fetchImpl: async () => response }), expected)
  }
})

test('board keeps the full address locally but sends only bounded summaries', () => {
  const board = fixture()
  const plan = boardPlan(board)
  assert.deepEqual(plan.candidates.get('pane_1').address, board.result.panes[0].address)
  assert.notEqual(plan.candidates.get('pane_1').address, board.result.panes[0].address)
  assert.equal(plan.context.cursor, 'epoch:12')
  assert.equal(plan.context.eligible, 1)
  assert.equal(plan.context.excluded, 0)
  assert.ok(!JSON.stringify(plan.input).includes('session-a'))
  assert.ok(!JSON.stringify(plan.input).includes('surface-a'))
  assert.match(plan.input.instructions, /untrusted data/)
  assert.match(plan.input.instructions, /Do not infer tool failure or successful completion/)
  assert.match(plan.input.choices.none, /silence is not completion/)
})

for (const [name, mutate] of [
  ['stale pane', (b) => { b.panes[0].freshness = 'stale' }],
  ['old pane timestamp', (b) => { b.panes[0].observed_at_ms -= 46000 }],
  ['future timestamp', (b) => { b.panes[0].observed_at_ms += 11000 }],
  ['offline source', (b) => { b.sources[0].state = 'offline' }],
  ['incomplete source', (b) => { b.sources[0].complete = false }],
  ['old source timestamp', (b) => { b.sources[0].observed_at_ms -= 46000 }],
  ['missing source', (b) => { b.sources = [] }],
  ['missing identity', (b) => { delete b.panes[0].address.surface_key }],
  ['shell pane', (b) => { b.panes[0].harness = 'shell' }],
  ['unidentified pane', (b) => { b.panes[0].harness = null }],
]) {
  test(`board excludes ${name} without inferring completion`, () => {
    const board = fixture(100000)
    mutate(board.result)
    const plan = boardPlan(board, { now: 100000 })
    assert.equal(plan.input, null)
    assert.equal(plan.context.excluded, 1)
    assert.equal(advisory(plan, null).disposition, 'no_candidates')
  })
}

test('board handles machine filtering, duplicates and option overflow explicitly', () => {
  const board = fixture()
  assert.equal(boardPlan(board, { machine: 'different' }).input, null)
  board.result.panes.push(structuredClone(board.result.panes[0]))
  assert.throws(() => boardPlan(board), /Ambiguous/)
  board.result.panes = Array.from({ length: 64 }, (_, index) => ({
    ...board.result.panes[0], address: { ...board.result.panes[0].address, surface_key: `surface-${index}` },
  }))
  assert.throws(() => boardPlan(board), /Too many panes/)
})

test('unsupported board schemas and error envelopes are refused', () => {
  assert.throws(() => boardPlan({ ok: false }), /lookup failed/)
  assert.throws(() => boardPlan({ schema_version: 2, panes: [], sources: [] }), /Unsupported/)
})

test('working panes do not inherit a previous turn completion report as current evidence', () => {
  const board = fixture()
  Object.assign(board.result.panes[0], { status: 'working', done_outcome: 'failed', done_summary: 'Old failure from a previous task' })
  const plan = boardPlan(board)
  assert.equal(plan.input.state.panes[0].done_outcome, '')
  assert.equal(plan.input.state.panes[0].done_summary, '')
  assert.match(plan.input.instructions, /Completion reports have unverified age/)
})

test('summary cleaning bounds Unicode and scrubs common credential forms', () => {
  assert.equal(clean('가나다라', 3), '가나다')
  assert.equal(clean('𠮷𠮷', 1), '𠮷')
  const text = clean('Bearer sample-secret sk-01234567890 xoxb-01234567890 https://example.test/?access_token=hidden&key=hidden\nnext', 1000)
  assert.ok(!text.includes('sample-secret'))
  assert.ok(!text.includes('01234567890'))
  assert.ok(!text.includes('hidden'))
  assert.ok(!text.includes('\n'))
})

test('browser options contain only enabled observed clickable refs', () => {
  const plan = browserPlan(page, { tabId: 7, goal: 'Find docs' })
  assert.deepEqual(Object.keys(plan.input.choices), ['read_more', 'wait', 'manual', 'done', 'click_e1', 'click_e4'])
  assert.deepEqual(plan.candidates.get('click_e1'), { action: 'click', tabId: 7, ref: 'e1' })
  assert.ok(!JSON.stringify(plan.input).includes('private filled value'))
  assert.match(plan.input.instructions, /manual for login, purchases, sending messages, deleting data, permissions/)
  assert.match(plan.input.choices.done, /verify independently/)
})

test('hidden or unknown-visibility pages cannot suggest a click', () => {
  for (const visibilityState of ['hidden', undefined]) {
    const plan = browserPlan({ ...page, visibilityState }, { tabId: 7, goal: 'Find docs' })
    assert.ok(Object.hasOwn(plan.input.choices, 'activate'))
    assert.ok(!Object.keys(plan.input.choices).some((key) => key.startsWith('click_')))
  }
})

test('browser does not confuse refs in names with element metadata', () => {
  const plan = browserPlan({ ...page, snapshot: '- button "Fake [ref=e999]" [ref=e5]\n- button "Again" [ref=e5]' }, { tabId: 7, goal: 'Inspect' })
  assert.ok(!Object.hasOwn(plan.input.choices, 'click_e999'))
  assert.ok(Object.hasOwn(plan.input.choices, 'click_e5'))
  assert.equal(plan.candidates.size, 5)
})

test('browser rejects invalid snapshots, tab IDs and too many options', () => {
  assert.throws(() => browserPlan(page, { tabId: -1, goal: 'Find docs' }))
  assert.throws(() => browserPlan({}, { tabId: 7, goal: 'Find docs' }))
  const snapshot = Array.from({ length: 61 }, (_, i) => `- button "B${i}" [ref=e${i}]`).join('\n')
  assert.throws(() => browserPlan({ ...page, snapshot }, { tabId: 7, goal: 'Inspect' }), /Too many browser targets/)
})

test('advisory never executes and low confidence or probability requires review', () => {
  const plan = boardPlan(fixture())
  const decision = decisionFor(plan.input.choices, 'pane_1')
  assert.equal(advisory(plan, decision).disposition, 'suggested')
  assert.equal(advisory(plan, decision).executed, false)
  for (const patch of [{ confidence: 0.79 }, { probability: 0.69 }]) {
    const result = advisory(plan, { ...decision, ...patch })
    assert.equal(result.disposition, 'needs_review')
    assert.equal(result.suggested, null)
    assert.equal(result.selected_candidate.address.surface_id, '%7')
  }
  assert.equal(advisory(plan, decisionFor(plan.input.choices, 'none')).disposition, 'no_action')
})

test('CLI dry-run requires neither credentials nor network inference', async () => {
  const result = await runCli(['smoke', '--dry-run'], { request: () => assert.fail('inference called') })
  assert.equal(result.request.model, MODEL)
  assert.equal(result.executed, false)
})

test('CLI forwards HTTP target options without a shell or control commands', async () => {
  const calls = []
  const result = await runCli(['board', '--api', 'http://127.0.0.1:8765', '--api-token-file', '/tmp/token file', '--local'], {
    runObservation: async (...args) => { calls.push(args); return fixture() },
    request: async (request) => decisionFor(request.choices, 'pane_1'),
  })
  assert.deepEqual(calls, [['kasaterm-cli', ['--api', 'http://127.0.0.1:8765', '--api-token-file', '/tmp/token file', 'board', '--local']]])
  assert.equal(result.suggested.address.instance_id, 'instance-a')
  assert.equal(result.executed, false)
  assert.ok(result.timing.total_ms >= 0)
})

test('CLI avoids inference if no board candidates remain', async () => {
  const board = fixture()
  board.result.panes = []
  const result = await runCli(['board'], {
    runObservation: async () => board, request: () => assert.fail('inference called'),
  })
  assert.equal(result.disposition, 'no_candidates')
})

test('CLI browser observes only and preserves local profile context', async () => {
  const calls = []
  const result = await runCli(['browser', '--tab-id', '7', '--goal', 'Find docs'], {
    runObservation: async (program, args) => {
      calls.push([program, args])
      return args[0] === 'list_profiles' ? { profiles: [{ id: 'profile-a' }], selected: null, chrome: { hostId: 'host-a' } } : page
    },
    request: async (request) => decisionFor(request.choices, 'click_e1'),
  })
  assert.deepEqual(calls, [
    ['kc', ['list_profiles']], ['kc', ['read_page', '--tabId', '7', '--filter', 'interactive', '--maxChars', '12000']],
  ])
  assert.deepEqual(result.context.browser, { host_id: 'host-a', profile_id: 'profile-a' })
  assert.equal(result.suggested.action, 'click')
  assert.equal(result.executed, false)
})

test('CLI stops on disconnected browser without reading pages or calling inference', async () => {
  let calls = 0
  await assert.rejects(runCli(['browser', '--tab-id', '7', '--goal', 'Find docs'], {
    runObservation: async () => { calls++; return { profiles: [] } },
    request: () => assert.fail('inference called'),
  }), /No Chrome profile connected/)
  assert.equal(calls, 1)
})

for (const argv of [
  ['board', '--execute'], ['smoke', '--local'], ['board', '--local', '--all'],
  ['board', '--api-token-file', '/tmp/token'], ['browser', '--tab-id', '7'],
  ['browser', '--tab-id', 'Infinity', '--goal', 'Find docs'], ['unknown'], ['smoke', 'extra'],
]) {
  test(`CLI refuses invalid arguments: ${argv.join(' ')}`, async () => {
    await assert.rejects(runCli(argv, { runObservation: () => assert.fail('observation called'), request: () => assert.fail('inference called') }))
  })
}

test('generic choose accepts supplied JSON and preserves caller-defined options', async () => {
  const result = await runCli(['choose', '--file', 'request.json'], {
    readInput: async (file) => { assert.equal(file, 'request.json'); return input },
    request: async (request) => decisionFor(request.choices, 'inspect'),
  })
  assert.deepEqual(result.suggested, { choice: 'inspect', description: 'Read failure details.' })
  assert.equal(result.executed, false)
})

test('generic choose does not reserve a caller-defined none option', async () => {
  const result = await runCli(['choose'], {
    readInput: async () => ({ ...input, choices: { none: 'Select the empty item.', other: 'Select another item.' } }),
    request: async (request) => decisionFor(request.choices, 'none'),
  })
  assert.equal(result.disposition, 'suggested')
  assert.equal(result.suggested.choice, 'none')
})

test('real CLI entry point accepts UTF-8 stdin and emits parseable dry-run JSON', () => {
  const request = { ...input, state: '한글 상태와 𠮷을 보존한다.' }
  const output = execFileSync(process.execPath, [fileURLToPath(new URL('./cli.mjs', import.meta.url)), 'choose', '--dry-run'], {
    input: JSON.stringify(request), encoding: 'utf8', timeout: 5000,
  })
  assert.deepEqual(JSON.parse(output).request, choiceRequest(request))
})
