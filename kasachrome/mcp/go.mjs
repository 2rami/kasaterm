// browser_go — 판정 모델(Jev-Omni)이 화면을 보고 몇 걸음 스스로 누르다 멈추는 길.
// 큰 모델이 걸음마다 read_page(최대 4만 자)를 읽고 ref 를 골라 click 을 부르면 클릭 하나에
// 큰 모델 턴 2번 + kc 호출 2~3번이 든다. 여기서는 번호표를 붙인 화면 한 장(~550토큰)을
// Jev-Omni 에 한 번 묻고 바로 누른다 — 판정은 걸음당 0.2~0.6초, 큰 모델은 멈춘 뒤에만 이어받는다.
//
// 확장 쪽 코드는 따로 두지 않는다. 기존 eval_js(번호표·강조·합성 클릭·값 넣기)·cdp_raw(찍기)·
// click(합성 클릭이 안 먹을 때의 좌표 진짜 입력)·type 만 부르므로, 확장을 다시 불러오지 않은 기기에서도 돈다.
import { readFile, mkdir, writeFile } from 'node:fs/promises'
import { homedir, tmpdir } from 'node:os'
import { join } from 'node:path'

const DECISIONS_URL = 'https://apis.opengateway.ai/v1/decisions'
const MODEL = 'sionic-ai/jev-omni'
const KEY_FILE = join(homedir(), '.config', 'opengateway.key')

// 보기가 길면 Jev-Omni 가 뒤쪽 보기를 놓친다(보기 52개에서 43~51번째 정답을 0.2~0.4 로 헛짚음).
// 12개씩 나눈 질문 여럿에 각자 「이 묶음엔 없다」를 두고 한 요청에 보내면 정답 묶음 0.997.
const CHUNK = 12
const SINGLE_MAX = 14
// 화면 하나에 이보다 많으면 판정이 흐려지고 토큰이 질문 수만큼 곱해진다. 위에서부터 자른다.
const MAX_ELEMENTS = 60
// 판정 모델에 보내는 그림 폭. 데모 실측(1024px JPEG ≈ 500~850토큰)과 같게 둔다.
const SHOT_WIDTH = 1024
const SHOT_QUALITY = 70

// 데모(2026-10-31)에서 11번 다 맞힌 문장이다. 칸 상태를 안 주거나 아래 두 규칙이 없으면
// 비밀번호를 건너뛰고, 이미 켜진 체크박스를 또 눌러 끈다.
const INSTRUCTIONS = '목표로 가는 다음 한 걸음으로 누를 칸을 화면의 번호표를 보고 고른다. 이미 목표 상태인 칸은 다시 누르지 않는다(체크박스를 다시 누르면 꺼진다). 입력 칸은 비어 있는 것부터 채운다. 목표를 다 이뤘으면 done 을 고른다.'
const CHUNK_NOTE = ' 이 질문의 보기는 화면 번호표의 일부다. 이 묶음 안에 다음에 누를 칸이 없으면 skip 을 고른다.'
const CONTROL = {
  done: '목표를 이미 다 이뤘다 — 더 누를 것이 없다',
  none: '이 화면에는 목표를 이룰 방법이 전혀 없다',
  scroll: '목표에 필요한 칸이 화면 아래에 가려져 있어 아래로 내려 봐야 한다',
}

// 되돌릴 수 없는 일. Jev 의 선택은 조언일 뿐이라, 이런 버튼을 고르면 누르지 않고 큰 모델에게 넘긴다.
// 다른 페이지로 가는 진짜 링크는 뒤로 가기로 되돌릴 수 있어 걸지 않는다 — 걸면 the-internet 첫 화면의
// 「Add/Remove Elements」 같은 목차 링크가 다 막힌다.
const IRREVERSIBLE = new RegExp([
  '결제', '구매', '주문하기', '주문 ?완료', '바로 ?주문', '송금', '이체', '보내기', '전송', '발송', '삭제', '지우기',
  '게시하기', '^게시$', '등록하기', '올리기', '제출', '탈퇴', '해지', '환불', '장바구니', '카트',
  '\\b(pay|buy|purchase|checkout|check out|place order|order now|send|delete|remove|discard|post|publish|submit|transfer|unsubscribe|deactivate|add to (cart|bag|basket))\\b',
].join('|'), 'i')

export function isIrreversible(el) {
  if (!el?.action) return false
  return [el.label, el.name, el.id].some((s) => s && IRREVERSIBLE.test(s))
}

// 같은 사이트인지는 등록 도메인으로 본다. 로그인 때문에 www → accounts 로 가는 것은 같은 사이트다.
// 공용 호스팅(herokuapp.com 등)은 앱마다 다른 사이트라 한 칸 더 본다.
const SHARED_HOSTING = new Set(['herokuapp.com', 'github.io', 'vercel.app', 'netlify.app', 'pages.dev', 'web.app',
  'firebaseapp.com', 'appspot.com', 'azurewebsites.net', 'cloudfront.net', 'onrender.com', 'fly.dev', 'workers.dev'])
const SECOND_LEVEL = /^(co|or|go|ac|ne|re|pe|ed|mil|com|net|org|gov|edu)$/

export function siteOf(host = '') {
  const h = String(host).toLowerCase().replace(/\.$/, '').replace(/:\d+$/, '')
  if (!h.includes('.') || /^[\d.]+$/.test(h) || h.includes(':') || h.startsWith('[')) return h
  const parts = h.split('.')
  let n = parts.length >= 3 && parts.at(-1).length === 2 && SECOND_LEVEL.test(parts.at(-2)) ? 3 : 2
  if (SHARED_HOSTING.has(parts.slice(-2).join('.'))) n = 3
  return parts.slice(-n).join('.')
}

// 값은 목표 문장에 그대로 적힌 것만 받는다. 큰 모델이 대화 밖의 비밀을 몰래 넣는 길을 막고,
// Jev 가 보는 목표와 실제로 넣는 값이 어긋나지 않게 한다.
export function checkValues(goal, values = {}) {
  for (const [field, value] of Object.entries(values || {})) {
    if (typeof value !== 'string' || !value) throw new Error(`VALUES_INVALID: values.${field} 는 빈 문자열이 아닌 글이어야 합니다.`)
    if (!goal.includes(value)) throw new Error(`VALUES_NOT_IN_GOAL: values.${field} 의 값이 목표 문장에 없습니다 — 넣을 값은 goal 에 그대로 적으세요.`)
  }
}

const norm = (s) => String(s || '').toLowerCase().replace(/[\s_\-:]/g, '')

// 칸 이름은 페이지마다 name·id·placeholder·라벨 중 아무 데나 있다. 하나라도 맞으면 그 값.
export function valueFor(el, values = {}) {
  const keys = [el.name, el.id, el.placeholder, el.label].map(norm).filter(Boolean)
  for (const [field, value] of Object.entries(values || {})) {
    if (keys.includes(norm(field))) return value
  }
  return undefined
}

export function describeElement(el) {
  return `${el.n}번 ${el.kind}${el.label ? ` '${el.label}'` : ''}${el.state ? ` · ${el.state}` : ''}`
}

// 칸 상태 문장. 그림만으로는 체크박스 켜짐·입력 칸 채움을 Jev 가 자주 놓쳐서 글로 함께 준다.
export function fieldStatus(els) {
  return els.filter((e) => e.state).map((e) => `${e.n}번 ${e.kind}${e.label ? ` '${e.label}'` : ''} ${e.state}`).join(', ')
}

// 누른 뒤 다른 페이지로 넘어갔으면 「한 일」 끝에 어디로 갔는지 붙인다. 목적지에 닿았는지 가르는 근거다 — 없으면
// 위키백과에서 검색해 문서에 닿은 뒤에도 검색창을 다시 골랐다(done 0.18). 첫 화면 state 에 늘 넣으면 데모로 검증된
// 흐름이 흔들려서(장바구니 none 0.87→0.68) 이동이 있었을 때만 넣는다.
export function pageLine(title, url) {
  let where = ''
  try {
    const u = new URL(url)
    where = (u.host + u.pathname).replace(/\/$/, '')
  } catch {}
  return [String(title || '').replace(/\s+/g, ' ').trim().slice(0, 80), where].filter(Boolean).join(' · ')
}

export function buildRequest({ goal, els, history = [], canScroll = false }) {
  const hist = history.length ? history.join(' → ') : '아직 아무것도 하지 않았다.'
  const status = fieldStatus(els)
  const state = `목표: ${goal}\n지금까지 한 일: ${hist}${status ? `\n지금 칸 상태: ${status}` : ''}`
  const control = { done: CONTROL.done, none: CONTROL.none, ...(canScroll ? { scroll: CONTROL.scroll } : {}) }
  const scrollNote = canScroll ? ' 필요한 칸이 화면 아래에 있을 것 같으면 scroll 을 고른다.' : ''
  if (els.length <= SINGLE_MAX) {
    const criteria = Object.fromEntries(els.map((e) => [`e${e.n}`, describeElement(e)]))
    return { state, mode: 'single', options: els.length + Object.keys(control).length,
      questions: { q: { type: 'choice', instructions: INSTRUCTIONS + scrollNote, criteria: { ...criteria, ...control } } } }
  }
  const questions = {
    act: { type: 'choice', instructions: `지금 화면에서 목표로 가는 다음 한 걸음이 무엇인지 고른다. 이미 목표 상태인 칸은 다시 누르지 않는다.${scrollNote}`,
      criteria: { press: '화면의 번호표 칸 하나를 누르거나 채워야 한다', ...control } },
  }
  for (let i = 0; i < els.length; i += CHUNK) {
    const part = els.slice(i, i + CHUNK)
    questions[`g${i / CHUNK + 1}`] = { type: 'choice', instructions: INSTRUCTIONS + CHUNK_NOTE,
      criteria: { ...Object.fromEntries(part.map((e) => [`e${e.n}`, describeElement(e)])), skip: '이 묶음 번호에는 다음에 누를 칸이 없다' } }
  }
  return { state, mode: 'chunked', options: els.length + Object.keys(control).length, questions }
}

const prob = (a, k) => a?.probabilities?.[k] ?? (a?.choice === k ? a?.confidence ?? 0 : 0)
const round = (p) => Math.round(p * 1000) / 1000

// 응답을 「무엇을, 얼마나 확신하고」 하나로 접는다. 나눠 물었으면 「누른다」 확률 × 그 묶음 안 확률.
export function readAnswers(answers, plan) {
  if (plan.mode === 'single') {
    const a = answers.q
    const top = Object.entries(a.probabilities || {}).sort((x, y) => y[1] - x[1]).slice(0, 3).map(([key, p]) => ({ key, p: round(p) }))
    return { choice: a.choice, p: prob(a, a.choice), top }
  }
  const act = answers.act
  const picks = []
  const all = []
  for (const [k, a] of Object.entries(answers)) {
    if (!k.startsWith('g')) continue
    for (const [key, p] of Object.entries(a.probabilities || {})) if (key !== 'skip') all.push({ key, p })
    if (a.choice && a.choice !== 'skip') picks.push({ key: a.choice, p: prob(a, a.choice) })
  }
  const pressP = prob(act, 'press')
  const top = all.sort((x, y) => y.p - x.p).slice(0, 3).map(({ key, p }) => ({ key, p: round(p * pressP) }))
  if (act.choice !== 'press') {
    const ctl = Object.entries(act.probabilities || {}).filter(([k]) => k !== 'press').map(([key, p]) => ({ key, p: round(p) }))
    return { choice: act.choice, p: prob(act, act.choice), top: [...ctl, ...top].sort((x, y) => y.p - x.p).slice(0, 3) }
  }
  const best = picks.sort((x, y) => y.p - x.p)[0]
  if (!best) return { choice: null, p: 0, top }
  return { choice: best.key, p: pressP * best.p, top }
}

// 고른 것을 그대로 해도 되는지. null 이면 진행, 아니면 멈출 까닭.
export function stopFor({ pick, threshold, el, values, canScroll, previous, sig }) {
  if (!pick.choice || pick.p < threshold) return 'low_confidence'
  if (pick.choice === 'done') return 'done'
  if (pick.choice === 'none') return 'none'
  if (pick.choice === 'scroll') return canScroll ? null : 'none'
  if (!el) return 'low_confidence'
  if (isIrreversible(el)) return 'irreversible'
  if (el.field && valueFor(el, values) === undefined) return 'needs_value'
  // 같은 칸을 같은 화면에서 또 고르면 눌러도 아무것도 안 바뀐 것이다 — 그대로 두면 maxSteps 까지 헛돈다.
  if (previous && previous.key === `${el.kind}|${el.label}` && previous.sig === sig) return 'repeat'
  return null
}

const WHY = {
  done: '목표를 이뤘다고 판단했다',
  none: '이 화면에는 목표로 갈 방법이 없다고 판단했다',
  low_confidence: '확신이 문턱보다 낮아 멈췄다 — candidates 중에서 이어받으세요',
  irreversible: '되돌릴 수 없는 버튼을 골라 누르지 않고 멈췄다 — 사람이나 큰 모델이 확인하고 누르세요',
  needs_value: '값이 없는 입력 칸을 골랐다 — values 에 그 칸 값을 주고 다시 부르세요',
  repeat: '같은 칸을 같은 화면에서 다시 골라 멈췄다(눌러도 바뀌지 않음)',
  max_steps: 'maxSteps 에 닿았다',
  left_site: '다른 사이트로 넘어가 멈췄다',
  new_tab: '링크가 새 탭으로 열려 멈췄다 — browser_list_tabs 로 새 탭을 찾아 이어가세요',
  page_changed: '고른 칸이 판정하는 사이에 두 번 연달아 사라졌다(화면이 계속 바뀜)',
  error: '도중에 오류가 났다 — note 를 보세요',
}

// ── 페이지 안에서 도는 함수들(eval_js 로 문자열이 되어 간다 — 바깥 변수를 쓰지 않는다) ──

function pageMark({ max }) {
  document.getElementById('__kc_go_marks')?.remove()
  document.getElementById('__kc_go_pick')?.remove()
  document.querySelectorAll('[data-kc-go],[data-kc-go-at]').forEach((e) => { e.removeAttribute('data-kc-go'); e.removeAttribute('data-kc-go-at') })
  // 담당 칩·테두리를 찍는 동안만 걷는다(screenshot 도구와 같은 data-shot 채널). 칩이 우상단 칸을 가리면 Jev 가 못 본다.
  document.getElementById('__cc_overlay')?.setAttribute('data-shot', '1')
  const SEL = 'a[href], button, input:not([type=hidden]), select, textarea, summary, [role=button], [role=link], [role=checkbox], [role=radio], [role=switch], [role=tab], [role=menuitem], [role=option], [contenteditable=""], [contenteditable=true]'
  const TEXTY = ['text', 'email', 'search', 'tel', 'url', 'number', 'password', 'date', 'datetime-local', 'month', 'time', 'week', '']
  const vw = innerWidth
  const vh = innerHeight
  const clean = (s) => String(s || '').replace(/\s+/g, ' ').trim()
  const layer = document.createElement('div')
  layer.id = '__kc_go_marks'
  layer.style.cssText = 'position:fixed;inset:0;pointer-events:none;z-index:2147483646'
  const out = []
  const taken = []
  for (const el of document.querySelectorAll(SEL)) {
    if (out.length >= max) break
    if (el.disabled || el.getAttribute('aria-disabled') === 'true' || el.closest('#__cc_overlay')) continue
    if (taken.some((t) => t.contains(el))) continue
    const type = el.tagName === 'INPUT' ? (el.getAttribute('type') || 'text').toLowerCase() : ''
    const role = el.getAttribute('role') || ''
    const toggle = type === 'checkbox' || type === 'radio'
    let target = el
    let r = el.getBoundingClientRect()
    const st = getComputedStyle(el)
    // 꾸민 체크박스는 진짜 input 을 투명·0px 로 숨기고 라벨을 그린다. 그때는 라벨이 누를 자리다.
    if (toggle && (Number(st.opacity) === 0 || r.width < 4 || r.height < 4) && el.labels?.[0]) {
      target = el.labels[0]
      r = target.getBoundingClientRect()
    } else if (st.visibility === 'hidden' || st.display === 'none' || Number(st.opacity) === 0) continue
    if (r.width < 4 || r.height < 4 || r.bottom <= 0 || r.right <= 0 || r.top >= vh || r.left >= vw) continue
    const cx = Math.min(Math.max(r.left + r.width / 2, 0), vw - 1)
    const cy = Math.min(Math.max(r.top + r.height / 2, 0), vh - 1)
    const hit = document.elementFromPoint(cx, cy)
    if (hit && !(target.contains(hit) || (el.labels && [...el.labels].some((l) => l.contains(hit))))) continue
    const n = out.length + 1
    el.setAttribute('data-kc-go', n)
    // 누를 자리가 요소 자신이 아니면(꾸민 체크박스의 라벨) 따로 표시해 강조·클릭이 같은 자리를 쓰게 한다.
    if (target !== el) target.setAttribute('data-kc-go-at', n)
    taken.push(el)
    // 칸을 감싼 라벨은 칸 자신의 글자(선택 칸의 보기 전부)까지 innerText 에 담는다. 칸을 뺀 사본으로 읽는다.
    let labelText = ''
    if (el.labels?.[0]) {
      const copy = el.labels[0].cloneNode(true)
      copy.querySelectorAll('select, textarea, input').forEach((c) => c.remove())
      labelText = clean(copy.textContent)
    }
    const aria = clean(el.getAttribute('aria-label'))
    const imgAlt = clean(el.querySelector?.('img[alt]')?.getAttribute('alt'))
    let kind = '버튼'
    let state = ''
    let field = false
    let action = false
    let label = ''
    if (type === 'checkbox' || role === 'checkbox' || role === 'switch') {
      kind = '체크박스'
      const on = type ? el.checked : el.getAttribute('aria-checked') === 'true'
      state = on ? '켜져 있음' : '꺼져 있음'
      label = labelText || aria
    } else if (type === 'radio' || role === 'radio') {
      kind = '라디오 버튼'
      const on = type ? el.checked : el.getAttribute('aria-checked') === 'true'
      state = on ? '골라져 있음' : '안 골라져 있음'
      label = labelText || aria
    } else if (el.tagName === 'SELECT') {
      kind = '선택 칸'
      field = true
      state = `고른 값 '${clean(el.selectedOptions?.[0]?.text).slice(0, 30)}'`
      label = labelText || aria || el.name || el.id
    } else if ((el.tagName === 'INPUT' && TEXTY.includes(type)) || el.tagName === 'TEXTAREA' || el.isContentEditable) {
      kind = type === 'password' ? '비밀번호 칸' : '입력 칸'
      field = true
      const filled = el.isContentEditable ? clean(el.innerText) : el.value
      state = filled ? '입력됨' : '비어 있음'
      label = labelText || aria || el.placeholder || el.name || el.id
    } else {
      const isLink = el.tagName === 'A' || role === 'link'
      const href = el.getAttribute('href') || ''
      const nav = isLink && role !== 'button' && href && !/^(#|javascript:)/i.test(href)
      kind = nav ? '링크' : role === 'tab' ? '탭' : role === 'menuitem' ? '메뉴' : role === 'option' ? '보기' : '버튼'
      action = !nav
      label = aria || clean(el.innerText) || clean(el.value) || clean(el.title) || imgAlt || el.name || el.id
    }
    out.push({ n, kind, label: clean(label).slice(0, 40), state, field, action, name: el.name || '', id: el.id || '', placeholder: el.placeholder || '' })
    // 번호표는 칸 왼쪽 바깥에 둔다. 안쪽 모서리에 얹으면 글자 첫머리를 가려 「A/B Testing」이 「1 /B」로 읽힌다.
    const bw = 9 + String(n).length * 9
    const outside = r.left - bw - 2 >= 0
    const badge = document.createElement('div')
    badge.textContent = n
    badge.style.cssText = `position:fixed;left:${outside ? r.left - bw - 2 : Math.max(0, r.left - 4)}px;top:${Math.max(0, outside ? r.top + Math.min(0, (r.height - 19) / 2) : r.top - 4)}px;min-width:${bw - 8}px;text-align:center;background:#111;color:#fff;font:bold 15px/19px sans-serif;padding:0 4px;border-radius:4px`
    const frame = document.createElement('div')
    frame.style.cssText = `position:fixed;left:${r.left}px;top:${r.top}px;width:${r.width}px;height:${r.height}px;outline:1.5px solid rgba(17,17,17,.55)`
    layer.append(frame, badge)
  }
  document.documentElement.appendChild(layer)
  const doc = document.documentElement
  return {
    els: out, vw, vh, sx: scrollX, sy: scrollY, dpr: devicePixelRatio || 1,
    canScroll: scrollY + vh < Math.max(doc.scrollHeight, document.body?.scrollHeight || 0) - 8,
    url: location.href, title: document.title, visibility: document.visibilityState,
  }
}

function pageAim({ n, color, text, hold = 900 }) {
  const el = document.querySelector(`[data-kc-go="${n}"]`)
  document.getElementById('__kc_go_marks')?.remove()
  document.getElementById('__cc_overlay')?.removeAttribute('data-shot')
  if (!el) return false
  const target = document.querySelector(`[data-kc-go-at="${n}"]`) || el
  let r = target.getBoundingClientRect()
  if (r.top < 0 || r.bottom > innerHeight || r.left < 0 || r.right > innerWidth) {
    target.scrollIntoView({ block: 'center', inline: 'center', behavior: 'auto' })
    r = target.getBoundingClientRect()
  }
  // 담당 칩·커서와 같은 세션 색으로 고른 칸을 잠깐 두른다 — 누가 왜 여기를 누르는지 화면에서 보이게.
  document.getElementById('__kc_go_pick')?.remove()
  const ring = document.createElement('div')
  ring.id = '__kc_go_pick'
  ring.style.cssText = `position:fixed;left:${r.left - 5}px;top:${r.top - 5}px;width:${r.width + 10}px;height:${r.height + 10}px;border:2px solid ${color};border-radius:8px;box-shadow:0 0 0 3px ${color}33,0 0 18px ${color}99;pointer-events:none;z-index:2147483646;transition:opacity .4s ease`
  const tag = document.createElement('div')
  tag.textContent = text
  tag.style.cssText = `position:absolute;left:-2px;top:-24px;background:${color};color:#fff;font:600 12px/18px -apple-system,system-ui,sans-serif;padding:1px 8px;border-radius:9px;white-space:nowrap`
  ring.appendChild(tag)
  document.documentElement.appendChild(ring)
  setTimeout(() => { ring.style.opacity = '0' }, hold)
  setTimeout(() => ring.remove(), hold + 500)
  return true
}

function pageFill({ n, value }) {
  const el = document.querySelector(`[data-kc-go="${n}"]`)
  if (!el) return { found: false }
  if (el.tagName === 'SELECT') {
    const want = String(value).trim().toLowerCase()
    const opt = [...el.options].find((o) => o.value.toLowerCase() === want || o.text.trim().toLowerCase() === want)
    if (!opt) return { found: true, stuck: false, reason: 'no-option' }
    el.value = opt.value
    el.dispatchEvent(new Event('input', { bubbles: true }))
    el.dispatchEvent(new Event('change', { bubbles: true }))
    return { found: true, stuck: el.value === opt.value }
  }
  el.focus()
  if (el.isContentEditable) return { found: true, stuck: false, reason: 'contenteditable' }
  // React·Vue 는 value 프로퍼티를 가로채므로 네이티브 setter 로 써야 상태가 따라온다(content.js fill 과 같은 이유).
  const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype
  const setter = Object.getOwnPropertyDescriptor(proto, 'value')?.set
  if (setter) setter.call(el, value)
  else el.value = value
  el.dispatchEvent(new Event('input', { bubbles: true }))
  el.dispatchEvent(new Event('change', { bubbles: true }))
  return { found: true, stuck: el.value === value }
}

// 먼저 페이지 안 합성 클릭을 하고, 350ms 안에 아무것도 안 바뀌었을 때만 진짜 입력(CDP)으로 넘긴다
// (content.js click 과 같은 순서). ⚠️처음부터 CDP 로 누르면, 그 클릭이 폼 제출로 페이지를 넘긴 뒤 그
// 탭의 CDP 마우스 입력이 통째로 버려진다 — 디버거를 떼었다 붙여도, 다른 주소로 가도 안 풀렸다
// (2026-10-31 Chrome for Testing 146 실측: 로그인 버튼 뒤 「Add Element」 클릭이 페이지에 이벤트 0개).
// 체크박스는 DOM 이 안 바뀌고 checked 만 바뀌므로 그것도 변화로 센다 — 안 세면 진짜 입력으로 한 번 더 눌러 도로 끈다.
function pageClick({ n }) {
  const el = document.querySelector(`[data-kc-go="${n}"]`)
  if (!el) return { found: false }
  const target = document.querySelector(`[data-kc-go-at="${n}"]`) || el
  const r = target.getBoundingClientRect()
  const x = Math.round(r.left + r.width / 2)
  const y = Math.round(r.top + r.height / 2)
  const link = el.closest('a[href]')
  const newTab = !!link && /^_blank$/i.test(link.getAttribute('target') || '')
  // 준비 동작(focus)은 관찰 밖에서 끝낸다. 안에서 하면 activeElement 가 늘 바뀌어 모든 클릭이 「변화 있음」이 된다.
  target.focus?.({ preventScroll: true })
  const before = { url: location.href, origin: performance.timeOrigin, active: document.activeElement, sy: scrollY, checked: el.checked, aria: el.getAttribute('aria-checked') }
  const ours = (node) => {
    for (let p = node; p; p = p.parentNode || p.host) if (['__kc_go_pick', '__kc_go_marks', '__cc_overlay'].includes(p.id)) return true
    return false
  }
  return new Promise((resolve) => {
    let mutated = false
    let navigating = false
    let done = false
    const mo = new MutationObserver((list) => { if (list.some((m) => !ours(m.target))) mutated = true })
    mo.observe(document.documentElement, { childList: true, subtree: true, attributes: true, characterData: true })
    const changed = () => mutated || navigating || newTab || location.href !== before.url || document.activeElement !== before.active
      || scrollY !== before.sy || el.checked !== before.checked || el.getAttribute('aria-checked') !== before.aria
    const finish = () => {
      if (done) return
      done = true
      mo.disconnect()
      window.navigation?.removeEventListener('navigate', onNav)
      resolve({ found: true, x, y, newTab, navigating, changed: changed(), url: before.url, origin: before.origin })
    }
    // 문서째 바뀌는 이동은 응답이 올 때까지 DOM 이 그대로라 무반응으로 읽힌다. 시작 순간 오는 navigate 로 잡고 곧장 답한다.
    const onNav = () => { navigating = true; finish() }
    window.navigation?.addEventListener('navigate', onNav)
    target.click()
    // 바뀐 것이 보이면 100ms 에 끊는다(그만큼은 그려질 틈을 준다). 안 보이면 350ms 까지 보고 진짜 입력으로 넘긴다 —
    // 느린 프레임워크 갱신을 무반응으로 오판하면 두 번 눌린다.
    setTimeout(() => { if (changed()) finish() }, 100)
    setTimeout(finish, 350)
  })
}

function pageSelect({ n }) {
  const el = document.querySelector(`[data-kc-go="${n}"]`)
  if (!el) return false
  el.focus()
  if (el.isContentEditable) {
    const range = document.createRange()
    range.selectNodeContents(el)
    getSelection().removeAllRanges()
    getSelection().addRange(range)
  } else el.select?.()
  return true
}

// 값은 페이지 안에서 비교해 참/거짓만 돌려받는다. 값을 결과로 실어 오면 로그·대화에 남고, 확장이 결과의
// 비밀번호를 <concealed> 로 바꾸는 기간에는 비교도 틀린다.
function pageHasValue({ n, value }) {
  const el = document.querySelector(`[data-kc-go="${n}"]`)
  return !!el && (el.isContentEditable ? el.innerText.trim() : el.value) === value
}

function pageClean({ hide }) {
  document.getElementById('__kc_go_marks')?.remove()
  document.querySelectorAll('[data-kc-go],[data-kc-go-at]').forEach((e) => { e.removeAttribute('data-kc-go'); e.removeAttribute('data-kc-go-at') })
  const ov = document.getElementById('__cc_overlay')
  if (hide) ov?.setAttribute('data-shot', '1')
  else ov?.removeAttribute('data-shot')
  const ring = document.getElementById('__kc_go_pick')
  if (ring) ring.style.visibility = hide ? 'hidden' : ''
  return { url: location.href, title: document.title, dpr: devicePixelRatio || 1, vw: innerWidth, vh: innerHeight, sx: scrollX, sy: scrollY }
}

const js = (fn, arg) => `return (${fn.toString()})(${JSON.stringify(arg)})`

// ── Jev 호출 ──

export async function decideJev(body, { fetchImpl = fetch, timeoutMs = 15000 } = {}) {
  let key
  try {
    key = (await readFile(KEY_FILE, 'utf8')).trim()
  } catch {
    throw new Error('JEV_KEY_MISSING: ~/.config/opengateway.key 가 없습니다(OpenGateway 키).')
  }
  // 하루 몇 건 나는 502·503 은 게이트웨이 keep-alive 경쟁이라 한 번 더 부르면 된다.
  for (let attempt = 0; ; attempt++) {
    const res = await fetchImpl(DECISIONS_URL, {
      method: 'POST',
      headers: { authorization: `Bearer ${key}`, 'content-type': 'application/json' },
      body: JSON.stringify({ model: MODEL, ...body }),
      signal: AbortSignal.timeout(timeoutMs),
    })
    if ((res.status === 502 || res.status === 503) && attempt === 0) continue
    const raw = await res.text()
    if (!res.ok) throw new Error(`JEV_HTTP_${res.status}: ${raw.slice(0, 200)}`)
    return JSON.parse(raw)
  }
}

// ── 실행 ──

const ms = (t0) => Math.round(performance.now() - t0)
const sleep = (n) => new Promise((r) => setTimeout(r, n))

// 클릭이 문서를 바꿨으면 새 문서가 읽힐 때까지 기다린다. 안 그러면 다음 번호표가 이동 직전 페이지에 붙는다.
// 'complete' 는 기다리지 않는다 — 광고 iframe 이 도는 페이지는 10초를 넘긴다. 'loading' 만 벗어나면 된다.
// 이동이 시작조차 안 되는 경우(navigate 를 받고 막은 SPA·내려받기)는 3초만 본다.
async function settle(call, tabId, before) {
  const t0 = Date.now()
  let url = null
  for (;;) {
    await sleep(80)
    try {
      const [href, origin, state] = (await call('eval_js', { tabId, code: 'return [location.href, performance.timeOrigin, document.readyState]' }, 3000)).value
      url = href
      const moved = origin !== before.origin || href !== before.url
      if (state !== 'loading' && (moved || !before.navigating)) return url
      if (!moved && Date.now() - t0 > 3000) return url
    } catch { /* 문서가 바뀌는 사이에는 평가가 실패한다 */ }
    if (Date.now() - t0 > 10000) return url
  }
}

export async function runGo(args, { call, color = '#4C8DFF', decide = decideJev, outDir } = {}) {
  const { goal, tabId, maxSteps = 8, values = {}, threshold = 0.7 } = args
  // ⚠️빈 tabId 가 0 이 되면 확장이 「활성 탭」으로 받아 사람의 탭을 누른다. go 는 탭을 꼭 집어야 한다.
  if (!Number.isInteger(tabId) || tabId <= 0) throw new Error('TAB_REQUIRED: go 는 tabId(양의 정수)가 꼭 필요합니다 — browser_list_tabs·new_tab 의 tabId 를 주세요.')
  if (!goal || !goal.trim()) throw new Error('GOAL_REQUIRED: goal 이 비었습니다.')
  checkValues(goal, values)

  const dir = outDir || join(tmpdir(), 'kasachrome-go', `${Date.now().toString(36)}-${tabId}`)
  await mkdir(dir, { recursive: true })
  const evalJs = async (fn, arg) => (await call('eval_js', { tabId, code: js(fn, arg) })).value
  const capture = async (view, file) => {
    const scale = Math.min(1, SHOT_WIDTH / (view.vw * view.dpr))
    const r = await call('cdp_raw', { tabId, method: 'Page.captureScreenshot', params: {
      format: 'jpeg', quality: SHOT_QUALITY, captureBeyondViewport: false,
      clip: { x: view.sx, y: view.sy, width: view.vw, height: view.vh, scale },
    } }, 20000)
    if (file) await writeFile(join(dir, file), Buffer.from(r.data, 'base64'))
    return r.data
  }

  const steps = []
  const history = []
  let ended = 'max_steps'
  let candidates
  let previous = null
  let startSite = null
  let lastView = null
  let note
  // 판정하는 0.3초 사이에 화면이 다시 그려지면(검색 추천 목록 등) 번호 붙인 칸이 새 노드로 바뀐다. 한 번은 다시 보고, 연달아면 멈춘다.
  let lost = 0
  let lastUrl = null
  const markLost = (step) => {
    step.did = '칸이 사라져 다시 봄'
    lost++
    if (lost >= 2) ended = 'page_changed'
    return lost >= 2
  }
  try {
    for (let i = 1; i <= maxSteps; i++) {
      const t = {}
      let t0 = performance.now()
      const view = await evalJs(pageMark, { max: MAX_ELEMENTS })
      t.mark = ms(t0)
      lastView = view
      if (!/^https?:/i.test(view.url)) throw new Error(`UNSUPPORTED_PAGE: ${view.url} 에서는 go 를 쓸 수 없습니다(http/https 만).`)
      const site = siteOf(new URL(view.url).host)
      if (startSite === null) startSite = site
      else if (site !== startSite) { ended = 'left_site'; break }
      if (i === 1 && view.visibility === 'hidden' && Object.keys(values).length) {
        note = '탭이 숨어 있습니다 — 숨은 탭은 입력이 안 먹을 수 있으니 browser_new_window 로 보이는 창에서 부르세요.'
      }

      t0 = performance.now()
      const image = await capture(view, `step-${i}.jpg`)
      t.shot = ms(t0)

      if (lastUrl && view.url !== lastUrl && history.length) history[history.length - 1] += ` → 「${pageLine(view.title, view.url)}」 페이지로 넘어감`
      lastUrl = view.url
      const plan = buildRequest({ goal, els: view.els, history, canScroll: view.canScroll })
      t0 = performance.now()
      const res = await decide({ state: plan.state, questions: plan.questions, images: [`data:image/jpeg;base64,${image}`] })
      t.judge = ms(t0)
      const pick = readAnswers(res.answers, plan)
      // 판정 근거를 남긴다. 목표 문장(값이 들어 있을 수 있다)은 빼고 보기와 확률만.
      await writeFile(join(dir, `step-${i}.json`), JSON.stringify({ options: Object.fromEntries(Object.entries(plan.questions).map(([k, q]) => [k, q.criteria])), answers: res.answers }, null, 1))
      const el = pick.choice?.startsWith('e') ? view.els.find((e) => `e${e.n}` === pick.choice) : null
      const sig = `${view.url}|${fieldStatus(view.els)}`
      const stop = stopFor({ pick, threshold, el, values, canScroll: view.canScroll, previous, sig })
      const label = el ? describeElement(el) : pick.choice || '(없음)'
      const step = { n: i, pick: label, p: round(pick.p), options: plan.options, tokens: res.usage?.input_tokens, ms: t }
      steps.push(step)
      if (stop) {
        ended = stop
        if (['low_confidence', 'irreversible', 'needs_value', 'repeat'].includes(stop)) {
          candidates = pick.top.map(({ key, p }) => {
            const c = key.startsWith('e') ? view.els.find((e) => `e${e.n}` === key) : null
            return { pick: c ? describeElement(c) : key, p }
          })
        }
        if (stop === 'irreversible' && el) await evalJs(pageAim, { n: el.n, color: '#F26D6D', text: `멈춤 · ${el.label || el.kind}`, hold: 4000 })
        break
      }

      t0 = performance.now()
      if (pick.choice === 'scroll') {
        await evalJs(pageClean, { hide: false })
        await call('scroll', { tabId, direction: 'down', amount: Math.max(3, Math.round(view.vh * 0.8 / 100)) })
        t.act = ms(t0)
        step.did = '아래로 내림'
        history.push('화면을 아래로 내림')
      } else {
        const aim = await evalJs(pageAim, { n: el.n, color, text: `${el.n} · ${Math.round(pick.p * 100)}%` })
        if (!aim) {
          if (markLost(step)) break
          continue
        }
        const value = el.field ? valueFor(el, values) : undefined
        if (value !== undefined) {
          const r = await evalJs(pageFill, { n: el.n, value })
          if (!r?.found) {
            if (markLost(step)) break
            continue
          }
          if (r.reason === 'no-option') {
            t.act = ms(t0)
            ended = 'needs_value'
            note = [note, `${el.label || el.kind} 선택 칸에 그 값과 같은 보기가 없습니다.`].filter(Boolean).join(' ')
            break
          }
          if (!r.stuck) {
            // 값이 안 붙는 칸(CodeMirror·검증 폼·contenteditable)은 진짜 키 입력으로 넣는다.
            // 포커스는 페이지 안에서 준다 — CDP 마우스로 누르면 위 pageClick 주석의 함정을 밟는다.
            await evalJs(pageSelect, { n: el.n })
            await call('type', { tabId, text: value, trusted: true })
            if (!await evalJs(pageHasValue, { n: el.n, value })) note = `${el.label || el.kind} 칸에 값이 안 붙었을 수 있습니다 — 마지막 화면을 확인하세요.`
          }
          t.act = ms(t0)
          step.did = '입력함'
          history.push(`${el.label || el.kind} 칸에 입력함`)
        } else {
          const r = await evalJs(pageClick, { n: el.n }).catch((e) => {
            // 답보다 이동이 먼저 끝나 문맥이 사라졌다 — 클릭은 먹었다.
            if (/context|navigat|closed|destroyed/i.test(e.message)) return { found: true, changed: true, navigating: true, url: view.url }
            throw e
          })
          if (!r?.found) {
            if (markLost(step)) break
            continue
          }
          if (!r.changed) {
            await call('click', { tabId, coordinate: [r.x, r.y], trusted: true })
            step.via = 'cdp'
          }
          t.act = ms(t0)
          step.did = '누름'
          history.push(`${describeElement(el).replace(/^\d+번 /, '')} 누름`)
          if (r.newTab) { ended = 'new_tab'; break }
          t0 = performance.now()
          const url = await settle(call, tabId, r)
          t.wait = ms(t0)
          if (url && siteOf(new URL(url).host) !== startSite) { ended = 'left_site'; break }
        }
      }
      lost = 0
      previous = el ? { key: `${el.kind}|${el.label}`, sig } : null
    }
  } catch (e) {
    // 도중에 깨져도 지금까지의 걸음과 마지막 화면은 돌려준다 — 그래야 큰 모델이 어디서부터 이어갈지 안다.
    ended = 'error'
    note = [note, e.message].filter(Boolean).join(' ')
  } finally {
    try { await evalJs(pageClean, { hide: false }) } catch {}
  }

  // 마지막 화면 — 번호표·담당 칩 없이 찍어 큰 모델이 이어받을 때 보게 한다.
  let shot = null
  try {
    const view = await evalJs(pageClean, { hide: true })
    lastView = { ...lastView, ...view }
    await capture(view, 'final.jpg')
    shot = join(dir, 'final.jpg')
  } catch (e) {
    note = [note, `마지막 화면을 못 찍었습니다: ${e.message}`].filter(Boolean).join(' ')
  } finally {
    try { await evalJs(pageClean, { hide: false }) } catch {}
  }

  const last = steps.at(-1)
  return {
    ended,
    why: WHY[ended] + (last && ['done', 'none', 'low_confidence'].includes(ended) ? ` (p=${last.p})` : ''),
    steps,
    ...(candidates ? { candidates } : {}),
    url: lastView?.url,
    title: lastView?.title,
    shot,
    ...(note ? { note } : {}),
  }
}
