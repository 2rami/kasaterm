import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtemp } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
  buildRequest, checkValues, describeElement, fieldStatus, isIrreversible, pageLine, readAnswers, runGo, siteOf, stopFor, valueFor,
} from './go.mjs'

const link = (n, label) => ({ n, kind: '링크', label, state: '', field: false, action: false, name: '', id: '', placeholder: '' })
const button = (n, label, extra = {}) => ({ n, kind: '버튼', label, state: '', field: false, action: true, name: '', id: '', placeholder: '', ...extra })
const box = (n, on, label = '') => ({ n, kind: '체크박스', label, state: on ? '켜져 있음' : '꺼져 있음', field: false, action: false, name: '', id: '', placeholder: '' })
const input = (n, name, filled = false, kind = '입력 칸') => ({ n, kind, label: name, state: filled ? '입력됨' : '비어 있음', field: true, action: false, name, id: name, placeholder: '' })

test('보기는 번호표 문장과 done·none, 내릴 곳이 있을 때만 scroll', () => {
  const els = [link(1, 'Checkboxes'), box(2, false), box(3, true)]
  const plan = buildRequest({ goal: '체크박스 두 개를 모두 켠다', els })
  assert.equal(plan.mode, 'single')
  const c = plan.questions.q.criteria
  assert.deepEqual(Object.keys(c), ['e1', 'e2', 'e3', 'done', 'none'])
  assert.equal(c.e1, "1번 링크 'Checkboxes'")
  assert.equal(c.e2, '2번 체크박스 · 꺼져 있음')
  assert.equal(plan.options, 5)
  assert.ok(!plan.questions.q.instructions.includes('scroll'))
  const scrolled = buildRequest({ goal: 'x', els, canScroll: true })
  assert.ok('scroll' in scrolled.questions.q.criteria)
  assert.ok(scrolled.questions.q.instructions.includes('scroll'))
})

test('state 에 목표·한 일·칸 상태가 들어간다(칸 상태가 없으면 비밀번호를 건너뛰던 데모 함정)', () => {
  const els = [input(1, 'username', true), input(2, 'password', false, '비밀번호 칸'), button(3, 'Login')]
  const plan = buildRequest({ goal: '로그인한다', els, history: ['username 칸에 입력함'] })
  assert.equal(plan.state, "목표: 로그인한다\n지금까지 한 일: username 칸에 입력함\n지금 칸 상태: 1번 입력 칸 'username' 입력됨, 2번 비밀번호 칸 'password' 비어 있음")
  assert.ok(plan.questions.q.instructions.includes('이미 목표 상태인 칸은 다시 누르지 않는다'))
  assert.ok(plan.questions.q.instructions.includes('비어 있는 것부터'))
  const first = buildRequest({ goal: 'g', els: [link(1, 'a')] })
  assert.equal(first.state, '목표: g\n지금까지 한 일: 아직 아무것도 하지 않았다.')
})

test('칸 상태 문장은 상태가 있는 칸만', () => {
  assert.equal(fieldStatus([box(1, true, 'checkbox 1'), link(2, 'x'), box(3, false)]), "1번 체크박스 'checkbox 1' 켜져 있음, 3번 체크박스 꺼져 있음")
  assert.equal(fieldStatus([link(1, 'x')]), '')
})

test('보기가 14개를 넘으면 12개씩 나누고 묶음마다 skip, 다음 걸음 종류는 따로 묻는다', () => {
  const els = Array.from({ length: 30 }, (_, i) => link(i + 1, `L${i + 1}`))
  const plan = buildRequest({ goal: 'g', els, canScroll: true })
  assert.equal(plan.mode, 'chunked')
  assert.deepEqual(Object.keys(plan.questions), ['act', 'g1', 'g2', 'g3'])
  assert.deepEqual(Object.keys(plan.questions.act.criteria), ['press', 'done', 'none', 'scroll'])
  assert.equal(Object.keys(plan.questions.g1.criteria).length, 13)
  assert.equal(Object.keys(plan.questions.g3.criteria).length, 7)
  assert.equal(plan.questions.g3.criteria.e30, "30번 링크 'L30'")
  assert.ok('skip' in plan.questions.g2.criteria)
  assert.equal(buildRequest({ goal: 'g', els: els.slice(0, 14) }).mode, 'single')
})

test('응답 접기 — 한 질문은 그대로, 나눴으면 「누른다」 × 묶음 안 확률로 가장 높은 칸', () => {
  const single = readAnswers({ q: { choice: 'e2', probabilities: { e1: 0.01, e2: 0.97, done: 0.02 } } }, { mode: 'single' })
  assert.equal(single.choice, 'e2')
  assert.equal(single.p, 0.97)
  assert.deepEqual(single.top.map((t) => t.key), ['e2', 'done', 'e1'])

  const chunked = { mode: 'chunked' }
  const r = readAnswers({
    act: { choice: 'press', probabilities: { press: 0.9, done: 0.05, none: 0.05 } },
    g1: { choice: 'e7', probabilities: { e7: 0.95, e3: 0.03, skip: 0.02 } },
    g2: { choice: 'e13', probabilities: { e13: 0.6, skip: 0.4 } },
  }, chunked)
  assert.equal(r.choice, 'e7')
  assert.ok(Math.abs(r.p - 0.855) < 1e-9)
  assert.equal(r.top[0].key, 'e7')

  const ctl = readAnswers({ act: { choice: 'done', probabilities: { press: 0.1, done: 0.9 } }, g1: { choice: 'skip', probabilities: { skip: 0.9, e1: 0.1 } } }, chunked)
  assert.equal(ctl.choice, 'done')
  assert.equal(ctl.p, 0.9)

  const nothing = readAnswers({ act: { choice: 'press', probabilities: { press: 0.8 } }, g1: { choice: 'skip', probabilities: { skip: 0.7, e1: 0.3 } } }, chunked)
  assert.equal(nothing.choice, null)
  assert.equal(nothing.p, 0)
})

test('멈춤 조건', () => {
  const base = { threshold: 0.7, values: {}, canScroll: true, previous: null, sig: 's' }
  assert.equal(stopFor({ ...base, pick: { choice: 'done', p: 0.99 } }), 'done')
  assert.equal(stopFor({ ...base, pick: { choice: 'none', p: 0.9 } }), 'none')
  assert.equal(stopFor({ ...base, pick: { choice: 'done', p: 0.5 } }), 'low_confidence')
  assert.equal(stopFor({ ...base, pick: { choice: null, p: 0 } }), 'low_confidence')
  assert.equal(stopFor({ ...base, pick: { choice: 'e1', p: 0.69 }, el: link(1, 'a') }), 'low_confidence')
  assert.equal(stopFor({ ...base, pick: { choice: 'scroll', p: 0.9 } }), null)
  assert.equal(stopFor({ ...base, canScroll: false, pick: { choice: 'scroll', p: 0.9 } }), 'none')
  assert.equal(stopFor({ ...base, pick: { choice: 'e1', p: 0.99 }, el: link(1, 'a') }), null)
  assert.equal(stopFor({ ...base, pick: { choice: 'e1', p: 0.99 }, el: button(1, '결제하기') }), 'irreversible')
  assert.equal(stopFor({ ...base, pick: { choice: 'e1', p: 0.99 }, el: input(1, 'search') }), 'needs_value')
  assert.equal(stopFor({ ...base, values: { search: 'kc' }, pick: { choice: 'e1', p: 0.99 }, el: input(1, 'search') }), null)
  const again = { key: '버튼|More', sig: 's' }
  assert.equal(stopFor({ ...base, previous: again, pick: { choice: 'e1', p: 0.99 }, el: button(1, 'More') }), 'repeat')
  assert.equal(stopFor({ ...base, previous: again, sig: 't', pick: { choice: 'e1', p: 0.99 }, el: button(1, 'More') }), null)
})

test('되돌릴 수 없는 버튼은 막고, 같은 낱말이라도 다른 페이지로 가는 링크는 둔다', () => {
  for (const label of ['결제하기', '바로 구매', '주문하기', '메시지 보내기', '삭제', '게시하기', '제출', '장바구니 담기', 'Delete', 'Pay now', 'Buy', 'Checkout', 'Place order', 'Send', 'Submit', 'Post', 'Publish', 'Add to cart', 'Unsubscribe']) {
    assert.ok(isIrreversible(button(1, label)), label)
  }
  assert.ok(isIrreversible(button(1, '', { id: 'delete-account' })))
  for (const label of ['Login', '로그인', 'Add Element', 'Posts', 'Resend code', '주문내역', '다음']) {
    assert.ok(!isIrreversible(button(1, label)), label)
  }
  assert.ok(!isIrreversible(link(1, 'Add/Remove Elements')))
  assert.ok(!isIrreversible(link(1, '결제 내역')))
})

test('값은 목표 문장에 적힌 것만', () => {
  checkValues('아이디 tomsmith, 비밀번호 SuperSecretPassword! 로 로그인한다', { username: 'tomsmith', password: 'SuperSecretPassword!' })
  checkValues('아무거나', undefined)
  assert.throws(() => checkValues('tomsmith 로 로그인', { password: 'hunter2' }), /VALUES_NOT_IN_GOAL/)
  assert.throws(() => checkValues('x', { a: '' }), /VALUES_INVALID/)
})

test('칸 이름은 name·id·placeholder·라벨 아무 데서나, 대소문자·띄어쓰기 무시', () => {
  const values = { username: 'tom', 'E-mail': 'a@b.c' }
  assert.equal(valueFor({ name: 'username' }, values), 'tom')
  assert.equal(valueFor({ id: 'UserName' }, values), 'tom')
  assert.equal(valueFor({ placeholder: 'email' }, values), 'a@b.c')
  assert.equal(valueFor({ label: 'Password' }, values), undefined)
})

test('같은 사이트 판정', () => {
  assert.equal(siteOf('www.example.com'), siteOf('accounts.example.com'))
  assert.equal(siteOf('shop.coupang.co.kr'), 'coupang.co.kr')
  assert.notEqual(siteOf('the-internet.herokuapp.com'), siteOf('evil.herokuapp.com'))
  assert.equal(siteOf('localhost:3000'), 'localhost')
  assert.equal(siteOf('127.0.0.1'), '127.0.0.1')
  assert.notEqual(siteOf('the-internet.herokuapp.com'), siteOf('elementalselenium.com'))
})

test('페이지 줄은 제목과 주소(쿼리 빼고)', () => {
  assert.equal(pageLine('Seoul - Wikipedia', 'https://en.wikipedia.org/wiki/Seoul?x=1#a'), 'Seoul - Wikipedia · en.wikipedia.org/wiki/Seoul')
  assert.equal(pageLine('', 'https://a.com/'), 'a.com')
  assert.equal(pageLine('t', 'not a url'), 't')
})

test('누른 뒤 페이지가 바뀌었을 때만 「한 일」에 어디로 갔는지 붙는다', async () => {
  const home = 'https://the-internet.herokuapp.com/'
  const { call } = fakeBrowser([{ url: home, els: [link(1, 'Checkboxes')] }, { url: `${home}checkboxes`, els: [box(1, false), box(2, true)] }, { url: `${home}checkboxes`, els: [box(1, true), box(2, true)] }])
  const states = []
  const picks = ['e1', 'e1', 'done']
  await runGo({ goal: '체크박스 두 개를 모두 켠다', tabId: 7 }, { call, decide: async (body) => { states.push(body.state); return answer(picks.shift()) }, outDir: await mkdtemp(join(tmpdir(), 'kcgo-')) })
  assert.ok(!states[0].includes('넘어감'))
  assert.match(states[1], /링크 'Checkboxes' 누름 → 「t · the-internet\.herokuapp\.com\/checkboxes」 페이지로 넘어감/)
  assert.match(states[2], /넘어감 → 체크박스 · 꺼져 있음 누름$/m)
})

test('번호 문장', () => {
  assert.equal(describeElement(input(2, 'password', false, '비밀번호 칸')), "2번 비밀번호 칸 'password' · 비어 있음")
  assert.equal(describeElement(box(1, true)), '1번 체크박스 · 켜져 있음')
})

// ── 고리 전체: 가짜 브라우저와 가짜 판정 ──

function fakeBrowser(screens) {
  let i = 0
  const calls = []
  const call = async (tool, args) => {
    calls.push({ tool, code: args.code?.match(/function (page\w+)/)?.[1] })
    const screen = screens[Math.min(i, screens.length - 1)]
    if (tool === 'cdp_raw') return { data: Buffer.from('jpeg').toString('base64') }
    if (tool !== 'eval_js') return {}
    const fn = args.code.match(/function (page\w+)/)?.[1]
    if (!fn) return { value: [screen.url, 1, 'complete'] }
    if (fn === 'pageMark') return { value: { els: screen.els, vw: 1280, vh: 720, sx: 0, sy: 0, dpr: 2, canScroll: false, url: screen.url, title: 't', visibility: 'visible' } }
    if (fn === 'pageAim') return { value: { x: 10, y: 10 } }
    if (fn === 'pageClick') { i++; return { value: { found: true, changed: true, navigating: false, x: 10, y: 10, url: screen.url, origin: 1 } } }
    if (fn === 'pageFill') { i++; return { value: { found: true, stuck: true } } }
    if (fn === 'pageClean') return { value: { url: screen.url, title: 't', dpr: 2, vw: 1280, vh: 720, sx: 0, sy: 0 } }
    return { value: true }
  }
  return { call, calls }
}

const answer = (choice, p = 0.99) => ({ answers: { q: { choice, probabilities: { [choice]: p, none: 1 - p } } }, usage: { input_tokens: 500 } })

test('빈 tabId(0)는 아무것도 부르기 전에 거절', async () => {
  const { call, calls } = fakeBrowser([])
  await assert.rejects(runGo({ goal: 'g', tabId: 0 }, { call, decide: async () => answer('none') }), /TAB_REQUIRED/)
  await assert.rejects(runGo({ goal: 'g', tabId: Number('') }, { call, decide: async () => answer('none') }), /TAB_REQUIRED/)
  await assert.rejects(runGo({ goal: 'g', tabId: 1.5 }, { call, decide: async () => answer('none') }), /TAB_REQUIRED/)
  await assert.rejects(runGo({ goal: 'tom 으로', tabId: 7, values: { password: 'x1' } }, { call, decide: async () => answer('none') }), /VALUES_NOT_IN_GOAL/)
  assert.equal(calls.length, 0)
})

test('누르고·채우고·done 으로 끝난다, 걸음마다 시간을 쪼개 준다', async () => {
  const url = 'https://the-internet.herokuapp.com/login'
  const els = [input(1, 'username'), input(2, 'password', false, '비밀번호 칸'), button(3, 'Login')]
  const { call, calls } = fakeBrowser([{ url, els }, { url, els }, { url, els }, { url: 'https://the-internet.herokuapp.com/secure', els: [] }])
  const picks = ['e1', 'e2', 'e3', 'done']
  const r = await runGo({ goal: 'tomsmith / pw!! 로 로그인', tabId: 7, values: { username: 'tomsmith', password: 'pw!!' } },
    { call, decide: async () => answer(picks.shift()), outDir: await mkdtemp(join(tmpdir(), 'kcgo-')) })
  assert.equal(r.ended, 'done')
  assert.deepEqual(r.steps.map((s) => s.did), ['입력함', '입력함', '누름', undefined])
  for (const k of ['mark', 'shot', 'judge', 'act']) assert.equal(typeof r.steps[0].ms[k], 'number')
  assert.match(r.shot, /final\.jpg$/)
  assert.ok(!JSON.stringify(r).includes('pw!!'), '결과에 값이 실리면 안 된다')
  assert.equal(calls.filter((c) => c.tool === 'click').length, 0, '합성 클릭이 먹었으면 진짜 입력을 보내지 않는다')
})

test('되돌릴 수 없는 버튼을 고르면 누르지 않고 후보와 함께 멈춘다', async () => {
  const url = 'https://shop.example.com/cart'
  const { call, calls } = fakeBrowser([{ url, els: [link(1, 'Home'), button(2, '결제하기')] }])
  const r = await runGo({ goal: '결제한다', tabId: 7 }, { call, decide: async () => answer('e2'), outDir: await mkdtemp(join(tmpdir(), 'kcgo-')) })
  assert.equal(r.ended, 'irreversible')
  assert.equal(r.candidates[0].pick, "2번 버튼 '결제하기'")
  assert.equal(calls.filter((c) => c.code === 'pageClick' || c.tool === 'click').length, 0)
})

test('확신이 낮으면 후보 셋을 돌려주고, 다른 사이트로 가면 멈춘다', async () => {
  const url = 'https://the-internet.herokuapp.com/'
  const low = fakeBrowser([{ url, els: [link(1, 'A'), link(2, 'B')] }])
  const decideLow = async () => ({ answers: { q: { choice: 'e1', probabilities: { e1: 0.5, e2: 0.4, none: 0.1 } } } })
  const r = await runGo({ goal: 'g', tabId: 7 }, { call: low.call, decide: decideLow, outDir: await mkdtemp(join(tmpdir(), 'kcgo-')) })
  assert.equal(r.ended, 'low_confidence')
  assert.deepEqual(r.candidates.map((c) => c.pick), ["1번 링크 'A'", "2번 링크 'B'", 'none'])

  const away = fakeBrowser([{ url, els: [link(1, 'Elemental Selenium')] }, { url: 'https://elementalselenium.com/', els: [link(1, 'x')] }])
  const r2 = await runGo({ goal: 'g', tabId: 7 }, { call: away.call, decide: async () => answer('e1'), outDir: await mkdtemp(join(tmpdir(), 'kcgo-')) })
  assert.equal(r2.ended, 'left_site')
  assert.equal(r2.steps.length, 1)
})

test('maxSteps 에 닿으면 멈춘다', async () => {
  const url = 'https://a.example.com/'
  const screens = Array.from({ length: 5 }, (_, i) => ({ url: `${url}${i}`, els: [link(1, `next ${i}`)] }))
  const { call } = fakeBrowser(screens)
  const r = await runGo({ goal: 'g', tabId: 7, maxSteps: 3 }, { call, decide: async () => answer('e1'), outDir: await mkdtemp(join(tmpdir(), 'kcgo-')) })
  assert.equal(r.ended, 'max_steps')
  assert.equal(r.steps.length, 3)
})
