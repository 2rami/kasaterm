#!/usr/bin/env node
import { execFile } from 'node:child_process'
import { promisify, parseArgs } from 'node:util'
import { readFile, realpath, stat } from 'node:fs/promises'
import { fileURLToPath } from 'node:url'
import { decide, choiceRequest, MAX_BYTES } from './client.mjs'
import { boardPlan, browserPlan, advisory } from './adapters.mjs'

const exec = promisify(execFile)
const HELP = `kasa-jev — OpenGateway Jev advisory experiment (never executes a suggestion)

  kasa-jev smoke
  kasa-jev board [--local | --all] [--machine ID] [--dry-run]
  kasa-jev board --api BASE [--api-token-file FILE]
  kasa-jev browser --tab-id N --goal "goal" [--dry-run]
  kasa-jev choose [--file request.json] [--dry-run]

choose reads {state, instructions, choices} from a file or stdin.
Key: OPENGATEWAY_API_KEY or ~/.config/opengateway.key.
Output is JSON. --dry-run previews the exact model request without an API call.
browser requires a connected kc profile and a freshly listed, explicitly chosen tab.
`

async function observe(program, args) {
  try {
    const { stdout } = await exec(program, args, { timeout: 12000, maxBuffer: 2 * 1024 * 1024, windowsHide: true })
    return JSON.parse(stdout)
  } catch {
    throw new Error(`${program} observation failed; check its connection with its normal CLI`)
  }
}

async function inputFrom(file) {
  let text
  if (file) {
    if ((await stat(file)).size > MAX_BYTES) throw new Error('Input exceeds 128 KiB')
    text = await readFile(file, 'utf8')
  } else {
    if (process.stdin.isTTY) throw new Error('choose needs --file or JSON on stdin')
    const chunks = []
    let size = 0
    for await (const chunk of process.stdin) {
      size += chunk.byteLength
      if (size > MAX_BYTES) throw new Error('Input exceeds 128 KiB')
      chunks.push(chunk)
    }
    text = Buffer.concat(chunks).toString('utf8')
  }
  if (Buffer.byteLength(text) > MAX_BYTES) throw new Error('Input exceeds 128 KiB')
  try { return JSON.parse(text) } catch { throw new Error('choose requires valid JSON input') }
}

export async function runCli(argv, { runObservation = observe, request = decide, readInput = inputFrom } = {}) {
  const { values, positionals } = parseArgs({ args: argv, allowPositionals: true, strict: true, options: {
    help: { type: 'boolean', short: 'h' }, 'dry-run': { type: 'boolean' },
    local: { type: 'boolean' }, all: { type: 'boolean' }, machine: { type: 'string' },
    api: { type: 'string' }, 'api-token-file': { type: 'string' },
    'tab-id': { type: 'string' }, goal: { type: 'string' }, file: { type: 'string' },
  } })
  if (values.help || !positionals.length) return HELP
  if (positionals.length !== 1) throw new Error('Expected one command; run kasa-jev --help')
  const command = positionals[0]
  const allowed = {
    smoke: [], choose: ['file'], browser: ['tab-id', 'goal'],
    board: ['all', 'local', 'machine', 'api', 'api-token-file'],
  }[command]
  if (!allowed) throw new Error('Unknown command; run kasa-jev --help')
  if (Object.keys(values).some((key) => !['help', 'dry-run', ...allowed].includes(key))) throw new Error('Option does not apply to this command')
  const started = performance.now()
  let plan
  if (command === 'board') {
    if (values.local && values.all) throw new Error('Choose either --local or --all')
    if (values['api-token-file'] && !values.api) throw new Error('--api-token-file requires --api')
    const args = []
    if (values.api) args.push('--api', values.api)
    if (values['api-token-file']) args.push('--api-token-file', values['api-token-file'])
    args.push('board', values.local ? '--local' : '--all')
    plan = boardPlan(await runObservation('kasaterm-cli', args), { machine: values.machine })
  } else if (command === 'browser') {
    if (!/^\d+$/.test(values['tab-id'] ?? '') || !values.goal?.trim()) throw new Error('browser needs --tab-id and --goal')
    const tabId = Number(values['tab-id'])
    if (!Number.isSafeInteger(tabId)) throw new Error('Invalid tab ID')
    const profiles = await runObservation('kc', ['list_profiles'])
    if (!profiles.profiles?.length) throw new Error('No Chrome profile connected; enable the Kasachrome extension first')
    const snapshot = await runObservation('kc', ['read_page', '--tabId', String(tabId), '--filter', 'interactive', '--maxChars', '12000'])
    plan = browserPlan(snapshot, { tabId, goal: values.goal })
    plan.context.browser = { host_id: profiles.chrome?.hostId, profile_id: profiles.selected ?? profiles.profiles[0]?.id }
  } else {
    const input = command === 'smoke' ? {
      state: 'The test runner reports two failed tests. No commands should be executed.',
      instructions: 'Select the next read-only step.',
      choices: { inspect: 'Read the failure details.', wait: 'No issue to inspect; wait.' },
    } : await readInput(values.file)
    choiceRequest(input)
    plan = { input, context: { source: command }, candidates: new Map(Object.entries(input.choices).map(([key, description]) => [key, { choice: key, description }])) }
  }
  const observationMs = Math.round(performance.now() - started)
  if (values['dry-run']) return { advisory_only: true, executed: false, context: plan.context, request: plan.input ? choiceRequest(plan.input) : null }
  const decision = plan.input ? await request(plan.input) : null
  return { ...advisory(plan, decision), timing: { observation_ms: observationMs, total_ms: Math.round(performance.now() - started) } }
}

if (process.argv[1] && await realpath(process.argv[1]).catch(() => '') === fileURLToPath(import.meta.url)) {
  try {
    const result = await runCli(process.argv.slice(2))
    process.stdout.write(typeof result === 'string' ? result : JSON.stringify(result, null, 2) + '\n')
  } catch (error) {
    const message = error.code?.startsWith('ERR_PARSE_ARGS') ? 'Invalid CLI options; run kasa-jev --help' : error.message
    process.stderr.write(`kasa-jev: ${message}\n`)
    process.exitCode = 1
  }
}
