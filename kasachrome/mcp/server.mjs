#!/usr/bin/env node
// stdio MCP 서버. ~/.claude.json user scope 에 등록되므로 Claude 계정을 바꿔도 그대로 산다 —
// claude-in-chrome 이 계정 OAuth 에 묶여 끊기던 문제를 이 경로가 대체한다.
import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js'
import { z } from 'zod'
import { WebSocket } from 'ws'
import { resolveIdentity } from './identity.mjs'
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { dirname, join, isAbsolute, extname } from 'node:path'
import { mkdir, writeFile } from 'node:fs/promises'
import { homedir } from 'node:os'
import { PORT } from '../extension/port.js'

// 서버 이름은 한 곳에만. 배포판은 이 한 줄만 치환하면 로그·MCP 핸드셰이크가 함께 따라온다.
const NAME = 'kasachrome'
const HERE = dirname(fileURLToPath(import.meta.url))
const BRIDGE = join(HERE, '..', 'bridge', 'server.mjs')
// 이름을 URL 로 두면 전역 URL 생성자를 가린다. 확장에서 그 사고가 한 번 났으므로 여기서도 피한다.
const LOCAL_BRIDGE_URL = `ws://127.0.0.1:${PORT}`
// 다리 후보 목록 — 앞에서부터 시도한다. 기본은 이 기계의 다리 하나(종전과 동일).
// 미니에선 KASACHROME_BRIDGE_URLS="ws://127.0.0.1:18800,ws://127.0.0.1:8777" 처럼
// 터널 너머 **맥북 다리를 먼저** 두면, 미니 학생이 로그인 살아 있는 맥북 크롬을
// 그대로 조작하고, 맥북이 덮여 닿지 않을 때만 미니 크롬으로 물러난다(2026-08-30
// 지시). 끊겼다 다시 붙을 때도 목록 맨 앞부터 다시 시도하므로, 맥북이 다시 열리면
// 다음 재연결에서 저절로 맥북 크롬으로 돌아온다.
const BRIDGE_URLS = (process.env.KASACHROME_BRIDGE_URLS || LOCAL_BRIDGE_URL)
  .split(',').map((s) => s.trim()).filter(Boolean)
let activeUrl = null

let ws = null
let ready = null
let nextId = 1
const pending = new Map()
let extensionUp = false
const IDENTITY = resolveIdentity()

function log(...a) { process.stderr.write(`[${NAME}] ${a.join(' ')}\n`) }

function open(url = LOCAL_BRIDGE_URL) {
  return new Promise((resolve, reject) => {
    const sock = new WebSocket(url)
    // 터널 청취구는 맥북이 잠든 직후에도 잠시 살아 있어(원격 sshd 소유) 접속만
    // 받고 응답이 없을 수 있다 — open 이 안 오면 짧게 끊고 다음 후보로 간다.
    const timer = setTimeout(() => fail(new Error(`OPEN_TIMEOUT: ${url}`)), 2500)
    const fail = (e) => { clearTimeout(timer); try { sock.close() } catch {}; reject(e) }
    sock.once('error', fail)
    sock.once('open', () => {
      clearTimeout(timer)
      sock.off('error', fail)
      if (activeUrl !== url) { log(`bridge: ${url}${url === LOCAL_BRIDGE_URL ? ' (이 기계 크롬)' : ' (원격 크롬)'}`) }
      activeUrl = url
      sock.send(JSON.stringify({ type: 'hello', role: 'client', identity: IDENTITY }))
      ws = sock
      sock.on('message', (raw) => {
        let msg
        try { msg = JSON.parse(raw.toString()) } catch { return }
        if (msg.type === 'status') { extensionUp = !!msg.extension; return }
        if (msg.type === 'profiles' || msg.type === 'select') {
          const q = pending.get(msg.id)
          if (!q) return
          pending.delete(msg.id)
          if (msg.ok === false) q.reject(new Error(msg.error || 'select failed'))
          else q.resolve({ profiles: msg.profiles || [], selected: msg.selected ?? null })
          return
        }
        if (msg.type !== 'result') return
        const p = pending.get(msg.id)
        if (!p) return
        pending.delete(msg.id)
        if (msg.ok) p.resolve(msg.result)
        else p.reject(new Error(msg.error || 'unknown error'))
      })
      sock.on('close', () => {
        ws = null
        ready = null
        for (const [, p] of pending) p.reject(new Error('BRIDGE_CLOSED: 브리지 연결이 끊겼습니다. 다시 시도하면 자동 재연결됩니다.'))
        pending.clear()
      })
      sock.on('error', () => {})
      resolve(sock)
    })
  })
}

// 첫 MCP 인스턴스가 브리지를 띄우고 나머지는 붙기만 한다. 포트가 이미 잡혀 있으면 브리지가 스스로 빠진다.
// 후보가 여럿이면 앞에서부터 시도하고, 전부 실패했을 때만 **이 기계의** 다리를 띄운다
// — 원격 다리는 남의 기계라 여기서 살릴 수 없다.
async function connect() {
  if (ws && ws.readyState === 1) return ws
  if (ready) return ready
  ready = (async () => {
    for (const url of BRIDGE_URLS) {
      try { return await open(url) } catch { /* 다음 후보로 */ }
    }
    log('bridge not running — starting it')
    spawn(process.execPath, [BRIDGE], { detached: true, stdio: 'ignore' }).unref()
    for (let i = 0; i < 30; i++) {
      await new Promise((r) => setTimeout(r, 200))
      try { return await open(LOCAL_BRIDGE_URL) } catch { /* 아직 리스닝 전 */ }
    }
    throw new Error('BRIDGE_UNREACHABLE: 브리지를 띄우지 못했습니다. `node bridge/server.mjs` 를 직접 실행해 로그를 확인하세요.')
  })()
  try { return await ready } finally { if (!ws) ready = null }
}

async function call(tool, args = {}, timeoutMs = 30000) {
  const sock = await connect()
  const id = nextId++
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject })
    sock.send(JSON.stringify({ type: 'call', id, tool, args, timeoutMs }))
    setTimeout(() => {
      if (pending.has(id)) { pending.delete(id); reject(new Error(`TIMEOUT: ${tool}`)) }
    }, timeoutMs + 2000)
  })
}

// call 과 달리 확장을 거치지 않고 브리지가 직접 답한다 — 확장이 하나도 없어도 목록은 나온다.
async function ask(type, extra = {}, timeoutMs = 5000) {
  const sock = await connect()
  const id = nextId++
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject })
    sock.send(JSON.stringify({ type, id, ...extra }))
    setTimeout(() => {
      if (pending.has(id)) { pending.delete(id); reject(new Error(`TIMEOUT: ${type}`)) }
    }, timeoutMs)
  })
}

function text(value) {
  return { content: [{ type: 'text', text: typeof value === 'string' ? value : JSON.stringify(value, null, 2) }] }
}

const server = new McpServer({ name: NAME, version: '0.1.0' })

const tabId = z.number().int().optional().describe('Target tab id from list_tabs. Omit to use the active tab.')
const ref = z.string().describe('Element ref like "e12" from read_page or find.')

function tool(name, description, schema, run, { timeoutMs = 30000 } = {}) {
  server.registerTool(name, { description, inputSchema: schema }, async (args) => {
    try {
      return await run(args)
    } catch (e) {
      return { content: [{ type: 'text', text: `ERROR: ${e.message}` }], isError: true }
    }
  })
  void timeoutMs
}

tool('browser_status', 'Check the bridge/extension connection and which tabs currently have the debugger attached. Call this first if any other tool errors.', {},
  async () => text(await call('status')))

tool('browser_list_profiles', 'List the Chrome profiles currently connected — one entry per profile that has the extension loaded. Each has an id, a label (the profile\'s Google account email when signed in) and a hint (tab count and the most common domains) so you can tell them apart when the label is empty. `selected` is the profile THIS session sends commands to; when it is null, commands go to the first-connected profile. Use this before browser_select_profile, and whenever a page you expect to be signed in appears signed out — that usually means you are driving the wrong profile.', {},
  async () => text(await ask('profiles')))

tool('browser_select_profile', 'Point this session\'s browser tools at a specific Chrome profile. Pass an id from browser_list_profiles; pass null to go back to the default (first-connected). The choice is per-session, so other panes keep whatever profile they picked — switching here never moves anyone else. Tab ids belong to the profile that issued them, so re-list tabs after switching instead of reusing ids from before.', { profile: z.string().nullable().optional().describe('Profile id from browser_list_profiles, or null to unset') },
  async (a) => text(await ask('select', { profile: a.profile ?? null })))

tool('browser_list_tabs', 'List all open tabs in the real Chrome profile (with tabId, url, title, active flag).', { windowId: z.number().int().optional() },
  async (a) => text(await call('list_tabs', a)))

tool('browser_new_tab', 'Open a new tab and wait for it to finish loading. This is the DEFAULT way to open a page — prefer it over browser_new_window every time, unless you need a specific window width and height. Opens in the BACKGROUND by default so the human keeps whatever they were looking at — the tab is fully operable while hidden. Pass active:true only when the page must actually be visible (animation, video, anything driven by rAF).\n\nThe tab is placed in YOUR OWN tab group (named and colored after you) so it never gets mixed in among the human\'s tabs — the returned groupId is that group. Tabs you open are yours to clean up: close each one with browser_close_tab the moment it has served its purpose — a page you already read is clutter sitting in the human\'s tab strip, and tabs pile up fast when you keep them "just in case". The one exception is the END of the task: leave exactly ONE tab open, the page that shows what you did — the layout you fixed, the flow that now works, the page the human asked about — so they can look at the result without making you run it again. Close every other tab you opened. If the task produced nothing worth looking at (you only read text, measured something, checked a value), close them all. Never close a tab you did not open.', { url: z.string().optional(), active: z.boolean().optional(), windowId: z.number().int().optional() },
  async (a) => text(await call('new_tab', a, 40000)))

tool('browser_new_window', 'Open a SEPARATE Chrome window. This is a LAST RESORT, not your default — use browser_new_tab for essentially every page you open. A background tab is invisible to the human; a window is not. Even opened unfocused it appears on their screen, and one per pane means four panes put four windows in front of someone trying to work. Reach for a window in exactly two cases: you need a specific width+height that a tab cannot give you, or the human asked for a window. For a phone-sized layout do NOT use this at all — use browser_emulate_device, which Chrome cannot clamp the way it clamps window width.\n\nIf an agent window already exists — no matter which pane opened it — this opens a tab there instead and answers with reused:true (resizing that window if you asked for a size). Agent windows are shared on purpose: the alternative is a window per pane. Pass reuse:false only when you genuinely need two windows side by side. Opens UNFOCUSED by default; pass focused:true only when the page must be visible to run (animation, media, rAF) — and if what you actually need is the human to type something, use browser_ask_human instead. Pass tabId to tear an existing tab out into its own window (that window is not registered as an agent window, and the torn-out tab keeps whatever group it had since it may be the human\'s). Windows you open are yours to clean up — close them with browser_close_window as soon as you are done, and before you finish the task. The end-of-task rule from browser_new_tab applies inside the window too: close the tabs you are finished with, and leave open the one page worth looking at. Close the window itself only when nothing in it is worth keeping.', {
  url: z.string().optional(), tabId: z.number().int().optional(),
  focused: z.boolean().optional(), incognito: z.boolean().optional(),
  width: z.number().int().optional(), height: z.number().int().optional(),
  reuse: z.boolean().optional(),
}, async (a) => text(await call('new_window', a, 40000)))

tool('browser_close_window', 'Close a window and every tab in it. Use it only on windows you opened yourself. Agent windows are shared between panes, so this refuses with WINDOW_SHARED when the window still holds tabs another agent is using — close your own tabs with browser_close_tab instead, or pass force:true if you really do mean to close theirs too.', { windowId: z.number().int(), force: z.boolean().optional() },
  async (a) => text(await call('close_window', a)))

tool('browser_tidy_windows', 'Merge scattered agent windows into one, keeping every tab alive. This MOVES tab groups between windows instead of closing anything, so device emulation, scroll position and half-typed input all survive — it is safe to run while other agents are still working in those tabs. A window counts as an agent window only when every tab in it sits in a tab group and at least one of those groups is ours; the human\'s own window always contains ungrouped tabs, so it is never touched. Use it when windows have piled up from before window reuse existed. Pass dryRun:true to see what would move before doing it.', { dryRun: z.boolean().optional() },
  async (a) => text(await call('tidy_windows', a, 40000)))

tool('browser_close_tab', 'Close a tab you opened. Call it the moment a tab has served its purpose instead of letting tabs pile up until the end — each one you keep "just in case" sits in the human\'s tab strip. The reply carries `remaining`: how many of the tabs YOU opened are still open. Do not take that to zero at the end of a task that produced something to look at — leave that one page open so the human can see the result without asking you to run it again (see browser_new_tab).', { tabId },
  async (a) => text(await call('close_tab', a)))

tool('browser_activate_tab', 'Switch to a tab within its window. Use this when a page needs to be visible — hidden tabs freeze rAF, media, smooth scrolling and some frameworks. The window itself is NOT raised, so this never steals focus from whatever app the human is using. If you need the human to actually look at the page and type something, use browser_ask_human instead — that one does raise the window, and it is the only tool that does.', { tabId },
  async (a) => text(await call('activate_tab', a)))

tool('browser_ask_human', 'Hand the screen to the human: activates the tab, raises its window to the front, and puts your reason on the on-page chip so they know why they were pulled over. This is the ONLY tool that takes focus, so it has exactly one use — you are blocked on something only a person can do: signing in, entering a 2FA code, solving a captcha, confirming a payment, answering a browser permission prompt. Those are also things you must not do yourself, so this is how that work gets handed over. Do NOT call it to show a result, to confirm a page rendered, or when a task finishes: testing and verification run in the background by design, and a window that jumps in front of someone mid-sentence costs them more than the check was worth. Pass reason (short, concrete: "구글 로그인 2단계 코드"). After calling, wait for the human to finish before continuing.', { tabId, reason: z.string().optional() },
  async (a) => text(await call('ask_human', a)))

tool('browser_navigate', 'Navigate a tab to a URL, or pass "back"/"forward" for history.', { tabId, url: z.string() },
  async (a) => text(await call('navigate', a, 45000)))

tool('browser_read_page', 'Accessibility-style snapshot with [ref=eN] handles for clicking. Always includes visibilityState — if it is "hidden", animations/scroll/media are frozen and you must not diagnose page code from it.', {
  tabId, filter: z.enum(['interactive', 'all']).optional(), maxChars: z.number().int().optional(),
}, async (a) => text(await call('read_page', a)))

tool('browser_get_text', 'Plain innerText of the page. Cheaper than read_page when you only need to read content.', { tabId, maxChars: z.number().int().optional() },
  async (a) => text(await call('get_text', a)))

tool('browser_find', 'Find elements by role/name/placeholder text and get their refs plus viewport coordinates.', { tabId, query: z.string() },
  async (a) => text(await call('find', a)))

tool('browser_screenshot', 'Screenshot a tab, or just one element with `ref`. The agent overlay — presence chip, operating border, avatar cursor — is taken down for the shot and put back after, so the image is the page alone and is safe to hand to a human or drop into a doc; pass overlay:true only when the overlay itself is what you are checking. Pass `path` (absolute) when the file itself is the deliverable — a release shot, an attachment, anything you will hand over rather than look at: the PNG is written there and you get back the path instead of the image, so it never enters your context. Visible-area capture of the active tab uses a quiet path with no debugging banner; fullPage or background tabs go through CDP. Do not reach for fullPage to check a fixed header or bottom bar: `position: fixed` is relative to the viewport, so in a whole-document capture those elements land wherever the first screenful ended — a bottom nav shows up stranded in the middle of the image with content continuing past it, which reads as "the bottom bar is missing".', {
  tabId, fullPage: z.boolean().optional(), format: z.enum(['png', 'jpeg']).optional(), quality: z.number().int().optional(),
  ref: z.string().optional().describe('Element ref from read_page/find — shoot only that element, cropped to its box. Scrolls it into view first, and works even when it sits below the fold. Cannot be combined with fullPage. Prefer this over a whole-viewport shot when you only need one card, panel, or chart: the crop is a fraction of the tokens and nothing around it competes for attention. The result reports back the element name and the exact box it captured — check them, since a stale or mistaken ref otherwise yields a confident picture of the wrong thing.'),
  padding: z.number().optional().describe('Extra CSS px around the element when ref is set (default 0). A little air keeps borders and shadows from being shaved off.'),
  overlay: z.boolean().optional().describe('Keep the agent overlay in the picture. Default false — it is hidden for the shot.'),
  path: z.string().optional().describe('Absolute path to write the image to (~ is expanded, parent dirs are created). Returns the path, not the image — the picture never lands in your context. A .jpg/.jpeg extension selects jpeg unless format says otherwise.'),
}, async (a) => {
  const { path: dest, ...shot } = a
  // ⚠️MCP 서버의 cwd 는 이 도구를 부른 pane 의 cwd 가 아니다(claude 가 서버를 따로 띄운다).
  // 상대경로를 받아주면 아무 데나 쓰이고, 파일이 안 보이는 이유를 아무도 못 찾는다.
  // `~` 도 셸이 아니면 안 풀려 「~」 라는 이름의 디렉토리가 생긴다 — 둘 다 조용히 어긋나는 길이라 막는다.
  let out = dest
  if (out?.startsWith('~/')) out = join(homedir(), out.slice(2))
  if (out && !isAbsolute(out)) throw new Error(`path must be absolute (got "${dest}") — the MCP server's cwd is not your pane's.`)
  // 확장자가 곧 의도다. .jpg 로 저장해 놓고 안에 PNG 가 들어 있으면 받는 쪽이 먼저 속는다.
  if (out && !shot.format && /^\.jpe?g$/i.test(extname(out))) shot.format = 'jpeg'
  const r = await call('screenshot', shot, 45000)
  // 걷고 찍었다는 사실을 밝힌다. 안 그러면 오버레이를 확인하려고 찍은 사람이 "왜 칩이 없지" 로 헛돈다.
  const note = r.overlayHidden ? [{ type: 'text', text: 'Agent overlay (chip, border, cursor) was hidden for this shot — pass overlay:true to keep it.' }] : []
  // ★무엇을 찍었는지 되돌려준다. ref 로 요소를 집을 때 그게 정말 의도한 요소였는지 확인할
  // 유일한 단서다 — 잘린 그림만 보면 엉뚱한 요소도 그럴듯해 보인다.
  if (r.target) {
    const { name, role, clip } = r.target
    note.push({
      type: 'text',
      text: `Cropped to ${r.target.ref} (${role || 'element'}${name ? ` "${name}"` : ''}) at ${Math.round(clip.width)}x${Math.round(clip.height)} from document position ${Math.round(clip.x)},${Math.round(clip.y)}. If that is not the element you meant, re-grab the ref with find or read_page.`,
    })
  }
  if (out) {
    const buf = Buffer.from(r.data, 'base64')
    await mkdir(dirname(out), { recursive: true })
    await writeFile(out, buf)
    // 이미지를 함께 싣지 않는 것이 이 옵션의 요점이다 — 파일이 결과물일 때 그림까지 대화에 들어오면
    // 한 장에 수천 토큰이 나가고, 그걸 피하려고 사람들이 screencapture 로 우회하던 것을 여기서 없앤다.
    return { content: [...note, { type: 'text', text: `Saved ${buf.length.toLocaleString()} bytes to ${out} (${r.format}, via ${r.via}). Not shown here — Read it if you need to look.` }] }
  }
  return { content: [...note, { type: 'image', data: r.data, mimeType: r.format === 'jpeg' ? 'image/jpeg' : 'image/png' }] }
})

tool('browser_click', 'Click an element by ref (or raw coordinate). Tries a synthetic click first; if nothing on the page changed it automatically retries as a real trusted input event. Set trusted:true to skip straight to the real event.', {
  tabId, ref: z.string().optional(), coordinate: z.array(z.number()).length(2).optional(),
  button: z.enum(['left', 'right', 'middle']).optional(), clickCount: z.number().int().optional(),
  modifiers: z.string().optional().describe('e.g. "meta", "ctrl+shift"'),
  trusted: z.boolean().optional().describe('Skip the synthetic attempt and send one real input event. Use for buttons that must fire exactly once.'),
  retry: z.boolean().optional().describe('Default true. Set false to never escalate — the click then fires at most once, but may not register on pages that only accept trusted input.'),
}, async (a) => text(await call('click', a)))

tool('browser_hover', 'Move the mouse over an element to reveal tooltips or hover menus.', { tabId, ref: z.string().optional(), coordinate: z.array(z.number()).length(2).optional() },
  async (a) => text(await call('hover', a)))

tool('browser_drag', 'Drag from one element/point to another with real mouse events.', {
  tabId, fromRef: z.string().optional(), toRef: z.string().optional(),
  from: z.array(z.number()).length(2).optional(), to: z.array(z.number()).length(2).optional(),
}, async (a) => text(await call('drag', a)))

tool('browser_swipe', 'Swipe with a real finger — the touch gesture a phone actually sends. Use this, not browser_drag, for anything a phone user swipes: a carousel or deck you flick sideways, pull-to-refresh, swipe-to-delete, a bottom sheet you drag up. Those UIs read touch events and decide from the direction lock and threshold distance, and drag never reaches that code because it sends mouse events.\n\nRequires phone emulation — call browser_emulate_device first (a tab with no touch is rejected rather than silently swallowing the gesture). The tab is brought to the front automatically, because a hidden tab never processes touch (its window is NOT raised, so this does not steal focus).\n\nBy default it swipes from the middle of the viewport; pass ref or coordinate to start somewhere specific, or from+to for exact control. distance defaults to 160px and is clamped to the room left in that direction.', {
  tabId, direction: z.enum(['left', 'right', 'up', 'down']).optional().describe('Which way the finger moves. Default "left" — the direction that advances a carousel.'),
  distance: z.number().optional().describe('How far the finger travels, in CSS px. Default 160; clamped to what fits.'),
  ref: z.string().optional().describe('Start at this element\'s center.'),
  coordinate: z.array(z.number()).length(2).optional().describe('Start at this exact point.'),
  from: z.array(z.number()).length(2).optional().describe('With `to`: exact start point, ignoring direction/distance.'),
  to: z.array(z.number()).length(2).optional().describe('With `from`: exact end point.'),
  steps: z.number().int().optional().describe('Intermediate touchMove events. Default 12 — enough for direction-lock logic to see the movement.'),
}, async (a) => text(await call('swipe', a)))

tool('browser_fill', 'Set the value of an input, textarea, select, checkbox or contenteditable. Uses native setters so React/Vue state follows; if the value does not stick (CodeMirror, validated forms) it retries with real keystrokes.', {
  tabId, ref, value: z.union([z.string(), z.boolean()]), trusted: z.boolean().optional(),
}, async (a) => text(await call('fill', a)))

tool('browser_type', 'Type text into the currently focused element.', { tabId, text: z.string(), trusted: z.boolean().optional() },
  async (a) => text(await call('type', a)))

tool('browser_press_key', 'Press a key (Enter, Tab, Escape, ArrowDown, a…) with optional modifiers. Always sent as a real key event.', {
  tabId, key: z.string(), modifiers: z.string().optional(), repeat: z.number().int().optional(),
}, async (a) => text(await call('press_key', a)))

tool('browser_scroll', 'Scroll the page or a scroll container. Falls back to a real wheel event when programmatic scrolling is a no-op (common in hidden tabs).', {
  tabId, direction: z.enum(['up', 'down', 'left', 'right']).optional(), amount: z.number().optional(),
  ref: z.string().optional(), coordinate: z.array(z.number()).length(2).optional(),
}, async (a) => text(await call('scroll', a)))

tool('browser_scroll_to', 'Scroll an element into view and return its viewport box.', { tabId, ref },
  async (a) => text(await call('scroll_to', a)))

tool('browser_eval_js', 'Evaluate JavaScript in the page and return the value. Top-level await works. Use fetch(url,{credentials:"include"}) here to hit internal dashboard APIs directly instead of waiting for a UI to render.\n\nAnything that may run longer than ~25s MUST use background:true — a single blocking call dies mid-flight because the extension worker sleeps while it waits (20s returns a value, 35s returns nothing). With background:true the call returns a jobId immediately; poll it with jobId (no code) until done is true. The job lives on that page — navigating away discards it.', {
  tabId, code: z.string().optional(),
  background: z.boolean().optional(), jobId: z.string().optional(),
}, async (a) => text(await call('eval_js', a, 45000)))

tool('browser_watch', 'Turn console and/or network collection on or off for a tab. While on, the tab keeps a debugger session (and its banner) so nothing is missed.', {
  tabId, console: z.boolean().optional(), network: z.boolean().optional(),
}, async (a) => text(await call('watch', a)))

tool('browser_console_logs', 'Read collected console messages. If collection was not on yet, this turns it on and tells you to re-trigger the action.', {
  tabId, pattern: z.string().optional(), onlyErrors: z.boolean().optional(), limit: z.number().int().optional(), clear: z.boolean().optional(),
}, async (a) => text(await call('console_logs', a)))

tool('browser_network_requests', 'Read collected network requests. If collection was not on yet, this turns it on and tells you to reload.', {
  tabId, urlPattern: z.string().optional(), onlyFailed: z.boolean().optional(), limit: z.number().int().optional(), clear: z.boolean().optional(),
}, async (a) => text(await call('network_requests', a)))

tool('browser_upload_file', 'Attach local files to a file input. Never click a file input — that opens a native dialog you cannot see.', {
  tabId, paths: z.array(z.string()), selector: z.string().optional(), ref: z.string().optional(),
}, async (a) => text(await call('upload_file', a)))

tool('browser_wait_for', 'Wait for text to appear or disappear, or just wait a fixed time.', {
  tabId, text: z.string().optional(), textGone: z.string().optional(), ms: z.number().int().optional(), timeoutMs: z.number().int().optional(),
}, async (a) => text(await call('wait_for', a, 60000)))

tool('browser_emulate_device', 'Give one TAB a real device viewport — size, pixel ratio, touch AND User-Agent — and keep it that way until you turn it off. Pass `device` with a name from the Chrome DevTools device list (`iphone-15-pro`, `iphone-14-pro-max`, `pixel-7`, `galaxy-s20-ultra`, `galaxy-z-fold-5`, `ipad-mini`, `ipad-air`, `ipad-pro-11`, `ipad-pro-12-9`, `surface-pro-7`, `surface-duo`, `nest-hub`, …). Names are matched loosely, so the DevTools label works as typed (`iPhone 14 Pro Max`, `iPad Pro 12.9`). **Call it with `list: true` to get the full table** of names, viewports, pixel ratios and aliases — do not guess. Landscape: append `-landscape` to the name (`ipad-pro-11-landscape`) or pass `landscape: true`. Only pass raw width/height when you need an exact size that no device has; the device\'s UA still applies. With no arguments at all it defaults to a 390x844 iPhone 12 Pro. This is the right tool for checking a mobile layout — not browser_resize_window, because a window is shared by everyone driving this browser (resizing it silently wipes someone else\'s setup) and Chrome will not go below 500px wide anyway.\n\n**It changes the User-Agent too.** Pages that pick a mobile view on the SERVER branch on UA, so a size-only override renders the desktop HTML squeezed into a phone width — a screen no real phone would ever show. Sec-CH-UA-Mobile/Platform client hints are set to match. The UA reaches the server on the NEXT request, so navigate again to see a server-side branch. Pass `ua: false` to keep the real UA, or `ua: "<string>"` to force one.\n\nIt fits the device into the window for you. A phone or tablet is taller than the space a desktop window gives the page, and the extra height does not shrink — it hangs off the bottom of the window, so anything at `bottom: 0` (a mobile tab bar, a sticky CTA) is scrolled out of sight even though the page believes it is on screen. You cannot detect this from the page: the override rewrites `outerHeight` too. So this tool measures the real window room first and scales the render down to fit, exactly like the DevTools device toolbar. CSS pixels are untouched, so every media query still matches the device. The result carries `windowRoom`, `scale`, and `fullyVisible` — check `fullyVisible` before concluding an element is missing from the design, and if `scale` is well under 1 the window is small and a bigger window will read better. Pass fit:false to opt out and get the raw clipped viewport.\n\n`mobile` and `touch` are separate: Surface Pro and Nest Hub render as desktop (mobile:false) but do take touch. Each device carries the right pair; override either one explicitly if you need to. The override lives on the debugger session, so this pins the session open; that leaves a debugging banner on the tab, which is the trade for it not reverting. Call again with off:true to clear it, restore the original UA and drop the banner. Touch emulation matters because `(hover: none)` and `(pointer: coarse)` rules only apply once the device has no mouse — shrinking a window can never reproduce those, since a mouse is still attached.', {
  tabId, device: z.string().optional(),
  width: z.number().int().optional(), height: z.number().int().optional(),
  deviceScaleFactor: z.number().optional(), mobile: z.boolean().optional(),
  touch: z.boolean().optional(), landscape: z.boolean().optional(),
  ua: z.union([z.boolean(), z.string()]).optional(),
  fit: z.boolean().optional(), off: z.boolean().optional(), list: z.boolean().optional(),
}, async (a) => text(await call('emulate_device', a, 20000)))

tool('browser_resize_window', 'Resize the window that owns a tab. Prefer browser_emulate_device for mobile-layout checks: a window is shared by every session driving this browser, so resizing wipes other people\'s setup, and Chrome clamps the width at 500px.', { tabId, width: z.number().int(), height: z.number().int() },
  async (a) => text(await call('resize_window', a)))

tool('browser_attach_debugger', 'Manually attach the debugger to a tab. Rarely needed — other tools attach on demand and detach after 15s idle.', { tabId },
  async (a) => text(await call('attach_debugger', a)))

tool('browser_detach_debugger', 'Detach the debugger and remove the banner from a tab.', { tabId },
  async (a) => text(await call('detach_debugger', a)))

tool('browser_set_task', 'Name what you are doing in the browser. It appears on the on-page overlay chip next to your avatar, so the person watching this Chrome can see which session is driving the page and why. Keep it to a few words; your own name is already shown, so do not repeat it. Set it once when you start a browsing task.', {
  task: z.string().describe('Short label for the current task, e.g. "checkout flow" or "landing page".'),
}, async (a) => text(await call('set_task', a, 5000)))

tool('browser_group_tabs', 'Put the given tabs into a Chrome tab group, naming and coloring it. Use it when the human is comparing several pages and needs them visually separated from the rest of the tab bar. Pass the returned groupId back to add more tabs to the same group later. ONLY groups the tabIds you pass — nothing is ever grouped automatically, and you must not build any flow that groups tabs the human did not ask for. Tabs from different windows get pulled into one window by Chrome, so the result says so when that happens. browser_ungroup_tabs is the inverse.', {
  tabIds: z.array(z.number().int()).min(1),
  title: z.string().optional(),
  color: z.enum(['grey', 'blue', 'red', 'yellow', 'green', 'pink', 'purple', 'cyan', 'orange']).optional(),
  groupId: z.number().int().optional(),
  collapsed: z.boolean().optional(),
}, async (a) => text(await call('group_tabs', a, 15000)))

tool('browser_list_groups', 'List every OPEN Chrome tab group with its title, color, window, tab count, and whether this session created it. What it cannot show: a group Chrome saved and then closed — the name-only shell pinned to the tab strip — is absent from `tabGroups.query` entirely, cannot be reopened by id, and has no delete API, so `empty: 0` means "nothing in reach", not "the tab strip is clean". Only the human can clear those, by right-clicking the shell. Prevention is the whole game: tabs this extension opens are pulled out of their group before closing so no shell is left behind.', {},
  async () => text(await call('list_groups', {}, 15000)))

tool('browser_ungroup_tabs', 'Pull tabs out of their tab groups. Omit tabIds to ungroup every grouped tab. A group disappears once its last tab leaves — there is no delete-group API.', {
  tabIds: z.array(z.number().int()).optional().describe('Specific tabs to ungroup. Omit for all.'),
}, async (a) => text(await call('ungroup_tabs', a, 15000)))

tool('browser_dev_reload', 'Reload this extension itself after its source changed. Only needed while developing the extension.', {},
  async () => text(await call('dev_reload', {}, 5000)))

tool('browser_cdp_raw', 'Escape hatch: send any raw Chrome DevTools Protocol command (e.g. "Emulation.setDeviceMetricsOverride"). Everything CDP can do is reachable here. One command is guarded: Input.dispatchTouchEvent on a hidden tab never gets acknowledged, so it is rejected with an explanation instead of hanging until the tool times out — for a whole gesture, prefer browser_swipe.', {
  tabId, method: z.string(), params: z.record(z.string(), z.any()).optional(),
}, async (a) => text(await call('cdp_raw', a, 45000)))

const transport = new StdioServerTransport()
await server.connect(transport)
await connect()
log(`ready (bridge ${activeUrl})`)
