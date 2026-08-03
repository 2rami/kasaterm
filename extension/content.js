// 페이지 안에서 도는 조용한 경로. debugger 를 붙이지 않으므로 배너가 뜨지 않고 DevTools 와 공존한다.
// 합성 이벤트가 씹히는 페이지에서는 background 가 같은 동작을 CDP 로 다시 시도한다.
if (!window.__ccInjected) {
  window.__ccInjected = true

  const S = { refs: new Map(), seq: 0 }

  // 오버레이 마크업을 바꿀 때마다 올린다 — 열려 있던 탭의 옛 오버레이를 갈아끼우는 기준이 된다.
  const OVERLAY_V = '10'

  const ROLE_BY_TAG = {
    a: 'link', button: 'button', select: 'combobox', textarea: 'textbox',
    img: 'img', h1: 'heading', h2: 'heading', h3: 'heading', h4: 'heading',
    h5: 'heading', h6: 'heading', nav: 'navigation', main: 'main',
    form: 'form', table: 'table', ul: 'list', ol: 'list', li: 'listitem',
    dialog: 'dialog', summary: 'button', iframe: 'iframe', video: 'video', audio: 'audio',
  }
  const INPUT_ROLE = {
    checkbox: 'checkbox', radio: 'radio', submit: 'button', button: 'button',
    reset: 'button', range: 'slider', file: 'file', hidden: 'hidden',
  }
  const INTERACTIVE = new Set([
    'link', 'button', 'textbox', 'checkbox', 'radio', 'combobox', 'slider', 'file', 'tab',
    'menuitem', 'option', 'switch', 'searchbox', 'spinbutton',
  ])

  function roleOf(el) {
    const explicit = el.getAttribute('role')
    if (explicit) return explicit.trim().split(/\s+/)[0]
    const tag = el.tagName.toLowerCase()
    if (tag === 'input') return INPUT_ROLE[(el.type || '').toLowerCase()] || 'textbox'
    if (el.isContentEditable) return 'textbox'
    return ROLE_BY_TAG[tag] || null
  }

  function clean(s) {
    return (s || '').replace(/\s+/g, ' ').trim().slice(0, 200)
  }

  function nameOf(el) {
    const aria = el.getAttribute('aria-label')
    if (aria) return clean(aria)
    const by = el.getAttribute('aria-labelledby')
    if (by) {
      const parts = by.split(/\s+/).map((id) => document.getElementById(id)).filter(Boolean)
      if (parts.length) return clean(parts.map((p) => p.innerText || p.textContent).join(' '))
    }
    const tag = el.tagName.toLowerCase()
    if (tag === 'img') return clean(el.getAttribute('alt'))
    if (tag === 'input' || tag === 'textarea') {
      const ph = el.getAttribute('placeholder')
      if (ph) return clean(ph)
      if (el.labels && el.labels.length) return clean(el.labels[0].innerText)
      if (el.value && el.type !== 'password') return clean(el.value)
    }
    const title = el.getAttribute('title')
    if (title) return clean(title)
    const own = ownText(el)
    if (own) return own
    return ''
  }

  // 자식 컨테이너의 긴 텍스트를 이름으로 삼지 않도록 직계 텍스트를 우선한다.
  function ownText(el) {
    let direct = ''
    for (const n of el.childNodes) {
      if (n.nodeType === 3) direct += n.nodeValue
    }
    direct = clean(direct)
    if (direct) return direct
    const all = clean(el.innerText || el.textContent)
    return all.length <= 80 ? all : ''
  }

  function visible(el) {
    if (!el.getClientRects().length) return false
    const cs = getComputedStyle(el)
    if (cs.visibility === 'hidden' || cs.display === 'none' || cs.opacity === '0') return false
    return true
  }

  function refFor(el) {
    if (el.__ccRef && S.refs.get(el.__ccRef) === el) return el.__ccRef
    const ref = `e${++S.seq}`
    el.__ccRef = ref
    S.refs.set(ref, el)
    return ref
  }

  function resolve(ref) {
    const el = S.refs.get(ref)
    if (!el || !el.isConnected) throw new Error(`REF_STALE: ${ref} 가 더 이상 페이지에 없습니다. read_page 나 find 로 다시 잡으세요.`)
    return el
  }

  function snapshot({ filter = 'interactive', maxChars = 40000 } = {}) {
    const lines = []
    let truncated = false

    function walk(node, depth) {
      if (truncated) return
      for (const el of node.children) {
        if (el.tagName === 'SCRIPT' || el.tagName === 'STYLE' || el.tagName === 'NOSCRIPT') continue
        const isVisible = visible(el)
        const role = roleOf(el)
        const interactive = role && (INTERACTIVE.has(role) || el.onclick || el.getAttribute('tabindex') !== null)
        const wanted = filter === 'all' ? isVisible : isVisible && interactive
        let nextDepth = depth
        if (wanted && role !== 'hidden') {
          const name = nameOf(el)
          const state = []
          if (el.disabled) state.push('disabled')
          if (el.checked) state.push('checked')
          if (el.getAttribute('aria-expanded')) state.push(`expanded=${el.getAttribute('aria-expanded')}`)
          if (el.value && (role === 'textbox' || role === 'searchbox') && el.type !== 'password') state.push(`value="${clean(el.value)}"`)
          const url = el.tagName === 'A' && el.href ? ` -> ${el.href.slice(0, 120)}` : ''
          lines.push(`${'  '.repeat(depth)}- ${role || el.tagName.toLowerCase()}${name ? ` "${name}"` : ''} [ref=${refFor(el)}]${state.length ? ` (${state.join(', ')})` : ''}${url}`)
          nextDepth = depth + 1
          if (lines.join('\n').length > maxChars) { truncated = true; return }
        }
        if (el.shadowRoot) walk(el.shadowRoot, nextDepth)
        walk(el, nextDepth)
      }
    }

    walk(document.body, 0)
    let text = lines.join('\n')
    if (truncated) text += '\n... (maxChars 초과로 잘림 — filter 나 maxChars 를 조정하세요)'
    return {
      url: location.href,
      title: document.title,
      // 숨은 탭에서는 rAF·스크롤·미디어가 전부 멈춘다. 오진을 막으려고 항상 함께 돌려준다.
      visibilityState: document.visibilityState,
      scroll: { y: Math.round(window.scrollY), height: Math.round(document.documentElement.scrollHeight), viewport: window.innerHeight },
      snapshot: text || '(보이는 요소 없음)',
    }
  }

  function find(query) {
    const q = query.toLowerCase()
    const out = []
    const seen = new Set()
    const all = document.querySelectorAll('*')
    for (const el of all) {
      if (out.length >= 20) break
      if (!visible(el)) continue
      const role = roleOf(el)
      const name = nameOf(el)
      const hay = `${role || ''} ${name} ${el.getAttribute('placeholder') || ''} ${el.id || ''}`.toLowerCase()
      if (!hay.includes(q)) continue
      // 같은 텍스트를 감싸는 조상 체인 중 가장 안쪽만 남긴다.
      const key = `${role}|${name}`
      if (seen.has(key)) continue
      if (name && [...out].some((o) => o.el.contains(el))) {
        const idx = out.findIndex((o) => o.el.contains(el))
        out.splice(idx, 1)
      }
      seen.add(key)
      const r = el.getBoundingClientRect()
      out.push({ el, ref: refFor(el), role: role || el.tagName.toLowerCase(), name, box: { x: Math.round(r.x + r.width / 2), y: Math.round(r.y + r.height / 2), w: Math.round(r.width), h: Math.round(r.height) } })
    }
    return { visibilityState: document.visibilityState, matches: out.map(({ el, ...rest }) => rest) }
  }

  // 조용한 경로가 no-op 이었는지 판정한다. 아무것도 안 변했으면 background 가 CDP 로 재시도한다.
  // 준비동작(focus 등)은 반드시 이 함수 밖에서 끝내야 한다 — 안에서 하면 activeElement 가 늘 바뀌어
  // 모든 클릭이 "변화 있음"으로 읽히고 승격이 영원히 안 돈다.
  function observeChange(fn, waitMs = 250) {
    const before = { url: location.href, active: document.activeElement, scrollY: window.scrollY }
    let mutated = false
    const mo = new MutationObserver(() => { mutated = true })
    mo.observe(document.documentElement, { childList: true, subtree: true, attributes: true, characterData: true })
    fn()
    return new Promise((resolve) => {
      setTimeout(() => {
        mo.disconnect()
        const changed = mutated
          || location.href !== before.url
          || document.activeElement !== before.active
          || window.scrollY !== before.scrollY
        resolve({ changed })
      }, waitMs)
    })
  }

  function centerOf(el) {
    el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'auto' })
    const r = el.getBoundingClientRect()
    return { x: Math.round(r.x + r.width / 2), y: Math.round(r.y + r.height / 2), w: Math.round(r.width), h: Math.round(r.height) }
  }

  // React·Vue 는 value 프로퍼티를 가로채므로 네이티브 setter 로 써야 상태가 따라온다.
  function setNativeValue(el, value) {
    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype
    const setter = Object.getOwnPropertyDescriptor(proto, 'value')?.set
    if (setter) setter.call(el, value)
    else el.value = value
  }

  function ensureOverlay() {
    if (!document.documentElement) return null
    let host = document.getElementById('__cc_overlay')
    // 확장을 리로드·업데이트하면 isolated world 는 새로 뜨지만 페이지에 붙여둔 이 DOM 은 남는다.
    // 마크업이 갈린 채 재사용하면 새 코드가 없는 요소를 찾다 죽으므로, 판이 다르면 통째로 새로 만든다.
    if (host && (!host.shadowRoot || host.dataset.v !== OVERLAY_V)) {
      host.remove()
      host = null
    }
    if (host) return host.shadowRoot
    host = document.createElement('div')
    host.id = '__cc_overlay'
    host.dataset.v = OVERLAY_V
    host.style.cssText = 'position:fixed;inset:0;pointer-events:none;z-index:2147483647;opacity:0;transition:opacity .25s ease'
    const root = host.attachShadow({ mode: 'open' })
    root.innerHTML = `
      <style>
        :host { all: initial; }
        /* 첫 버전의 글로우 테두리를 그대로 기본값으로 쓴다 — 3px 선에 안쪽으로 번지는 글로우.
           조작이 끝나도 남아 "이 창은 누가 잡고 있다"를 계속 알린다. 조작 중임은 글로우의
           숨쉬기와 픽셀 러너로 알리므로 선 자체는 두 상태에서 똑같다.
           안쪽 1px 어두운 선은 밝은 배경에서 밝은 학생색이 묻히지 않게 하는 대비용.
           ⚠️글로우를 .frame 의 box-shadow 에 두고 숨쉬기를 걸면 선과 러너(자식)까지 함께
           흐려진다. 글로우만 별도 층으로 떼면 숨쉬는 것이 그 층뿐이라 둘 다 늘 또렷하다. */
        .frame {
          position: fixed; inset: 0; pointer-events: none;
          border: 3px solid var(--cc, #6BCF7F);
          box-shadow: inset 0 0 0 1px rgba(0,0,0,.15);
        }
        .frame::before {
          content: ''; position: absolute; inset: 0;
          box-shadow: inset 0 0 32px -6px var(--cc, #6BCF7F);
        }
        :host([data-mode="active"]) .frame::before { animation: breathe 2.4s ease-in-out infinite; }
        @keyframes breathe { 0%,100% { opacity: 1 } 50% { opacity: .45 } }
        /* brand-skip — 창 테두리를 도는 픽셀 러너. 흰 머리 한 칸과 옅어지는 꼬리 네 칸이 같은
           경로를 한 칸씩 시차를 두고 돌아 지나간 자취처럼 보인다. 다섯 칸의 크기를 똑같이 두는
           것이 중요하다 — 뒤로 갈수록 작게 만들면 자취가 아니라 따로 노는 점 다섯 개가 된다.
           96칸 × 100ms 라 한 칸이 60px 을 넘어 이동이 확실히 끊긴다.
           ⚠️anchor 를 기본(중앙)으로 두면 러너 절반이 창 밖으로 잘린다. 위쪽 가장자리를 경로에
           맞추고 offset-rotate: auto 로 진행 방향을 따르게 하면 네 변 모두에서 안쪽에 온전히 놓인다.
           ⚠️active 규칙에서 animation 단축을 쓰면 칸마다 준 delay 가 0 으로 덮여 다섯이
           겹쳐 돈다. 꼬리를 만드는 것이 그 delay 이므로 여기서는 개별 속성으로 쓴다. */
        .frame i {
          position: absolute; display: block; width: 10px; height: 10px;
          offset-path: border-box; offset-distance: 0%;
          offset-rotate: auto; offset-anchor: 50% 0%;
          opacity: 0; transition: opacity .3s ease;
        }
        :host([data-mode="active"]) .frame i {
          opacity: 1;
          animation-name: runner; animation-duration: 9.6s;
          animation-timing-function: steps(96); animation-iteration-count: infinite;
        }
        .frame i:nth-child(1) {
          background: #fff;
          box-shadow: 0 0 6px var(--cc, #6BCF7F), 0 0 18px var(--cc, #6BCF7F);
          animation-delay: -.4s;
        }
        .frame i:nth-child(2) {
          background: var(--cc, #6BCF7F); box-shadow: 0 0 10px var(--cc, #6BCF7F);
          animation-delay: -.3s;
        }
        .frame i:nth-child(3) {
          background: color-mix(in srgb, var(--cc, #6BCF7F) 55%, transparent);
          animation-delay: -.2s;
        }
        .frame i:nth-child(4) {
          background: color-mix(in srgb, var(--cc, #6BCF7F) 32%, transparent);
          animation-delay: -.1s;
        }
        .frame i:nth-child(5) {
          background: color-mix(in srgb, var(--cc, #6BCF7F) 16%, transparent);
        }
        /* /brand-skip */
        .chip {
          position: fixed; top: 12px; right: 12px; display: flex; align-items: center; gap: 8px;
          padding: 5px 12px 5px 5px; border-radius: 999px;
          background: rgba(20,22,26,.88); color: #fff;
          font: 500 12px/1.4 -apple-system, BlinkMacSystemFont, "Apple SD Gothic Neo", sans-serif;
          box-shadow: 0 2px 12px rgba(0,0,0,.35), 0 0 0 2px var(--cc, #6BCF7F), 0 0 16px -4px var(--cc, #6BCF7F);
          white-space: nowrap;
          transform-origin: 100% 0;
          transition: opacity .3s ease, transform .3s ease, box-shadow .3s ease;
        }
        /* brand-skip — 칩 둘레를 도는 픽셀 러너. offset-path: border-box 가 칩의 pill 모양을 그대로
           경로로 쓰므로 글자 길이가 바뀌어도 따로 계산할 게 없다. steps() 로 칸칸이 끊어 옮겨야
           픽셀처럼 보인다 — 부드럽게 미끄러지면 그냥 도는 점이다. */
        .chip::after {
          content: ''; position: absolute; width: 5px; height: 5px;
          background: var(--cc, #6BCF7F); box-shadow: 0 0 6px var(--cc, #6BCF7F);
          offset-path: border-box; offset-distance: 0%; offset-rotate: 0deg;
          animation: runner 3.6s steps(28) infinite;
        }
        @keyframes runner { to { offset-distance: 100% } }
        /* /brand-skip */
        /* 조작 중에만 나타나 숨쉬는 바깥 링. box-shadow 를 직접 애니메이션하면 매 프레임 다시
           그려야 하지만, 링을 따로 두면 opacity·transform 만 움직여 compositor 가 처리한다. */
        .chip::before {
          content: ''; position: absolute; inset: -4px; border-radius: 999px;
          border: 2px solid var(--cc, #6BCF7F); opacity: 0;
          transition: opacity .3s ease;
        }
        .ava { position: relative; width: 22px; height: 22px; flex: none; }
        .chip img { width: 22px; height: 22px; border-radius: 50%; display: block; }
        .dot {
          position: absolute; right: -1px; bottom: -1px; width: 8px; height: 8px; border-radius: 50%;
          background: var(--cc, #6BCF7F); box-shadow: 0 0 0 2px rgba(20,22,26,.92);
        }
        .cursor {
          position: fixed; top: 0; left: 0; width: 30px; height: 30px; margin: -15px 0 0 -15px;
          opacity: 0; transition: transform .16s cubic-bezier(.22,1,.36,1), opacity .2s ease;
        }
        .cursor img {
          width: 30px; height: 30px; border-radius: 50%; display: block;
          box-shadow: 0 0 0 2px var(--cc, #6BCF7F), 0 3px 10px rgba(0,0,0,.4);
        }
        .cursor::after {
          content: ''; position: absolute; inset: -6px; border-radius: 50%;
          border: 2px solid var(--cc, #6BCF7F); opacity: 0; transform: scale(.6);
        }
        .cursor.tap::after { animation: tap .45s ease-out; }
        @keyframes tap {
          0% { opacity: .9; transform: scale(.6) }
          100% { opacity: 0; transform: scale(1.9) }
        }
        /* 조작 중 — 칩 바깥 링이 숨쉬고 상태 점이 뛴다. 러너는 상시 돌되 여기서 속도만 올린다. */
        :host([data-mode="active"]) .chip::before { opacity: 1; animation: halo 2.4s ease-in-out infinite; }
        /* brand-skip */
        :host([data-mode="active"]) .chip::after { animation-duration: 1.6s; }
        /* /brand-skip */
        @keyframes halo {
          0%,100% { opacity: .9; transform: scale(1) }
          50% { opacity: .2; transform: scale(1.08) }
        }
        :host([data-mode="active"]) .dot { background: #f08c00; animation: blip 1.1s ease-in-out infinite; }
        @keyframes blip { 0%,100% { transform: scale(1); opacity: 1 } 50% { transform: scale(.6); opacity: .5 } }
        /* 대기 — 커서만 걷고 테두리·칩은 남긴다. 칩은 작고 얌전해지되 **테두리는 그대로**.
           ⚠️칩 전체에 opacity 를 걸면 테두리까지 같이 흐려져 밝은 페이지에서 통째로 사라진다(실측).
           투명도는 배경과 글자에만 주고 테두리는 불투명하게 남긴다 — 그게 "누가 잡고 있다"는 신호다. */
        :host([data-mode="idle"]) .chip {
          transform: scale(.9);
          background: rgba(20,22,26,.62);
          color: rgba(255,255,255,.82);
          box-shadow: 0 1px 8px rgba(0,0,0,.24), 0 0 0 2px var(--cc, #6BCF7F), 0 0 12px -5px var(--cc, #6BCF7F);
        }
        :host([data-mode="idle"]) .chip img { opacity: .82; }
        :host([data-mode="idle"]) .cursor { opacity: 0 !important; }
      </style>
      <div class="frame"></div>
      <div class="chip"><span class="ava"><img alt=""><i class="dot"></i></span><span class="label"></span></div>
      <div class="cursor"><img alt=""></div>`
    /* brand-skip — 러너 다섯 칸. 마크업이 아니라 여기서 만드는 이유는 배포판에서 이 블록만
       들어내면 쓰이지 않는 빈 요소도 함께 사라지기 때문이다. */
    const fr = root.querySelector('.frame')
    for (let i = 0; i < 5; i++) fr.appendChild(document.createElement('i'))
    /* /brand-skip */
    document.documentElement.appendChild(host)
    return root
  }

  const ops = {
    ping: () => ({ ok: true, url: location.href, visibilityState: document.visibilityState }),
    snapshot: (a) => snapshot(a),
    find: (a) => find(a.query),
    // name·role 을 함께 주는 이유: 활동 기록에 "클릭 — 로그인" 처럼 사람이 읽을 대상을 남기려면
    // ref(e12) 가 아니라 요소의 이름이 필요하다.
    box: (a) => {
      const el = resolve(a.ref)
      return { box: centerOf(el), name: nameOf(el), role: roleOf(el) }
    },
    text: (a) => ({
      visibilityState: document.visibilityState,
      url: location.href,
      title: document.title,
      text: (document.body.innerText || '').slice(0, a.maxChars || 30000),
    }),
    click: async (a) => {
      const el = resolve(a.ref)
      const box = centerOf(el)
      el.focus?.()
      await new Promise((r) => setTimeout(r, 0))
      // 느린 프레임워크 갱신을 no-op 으로 오판하면 재시도가 두 번째 실행이 된다. 넉넉히 본다.
      const { changed } = await observeChange(() => el.click(), a.waitMs ?? 350)
      return { changed, box, visibilityState: document.visibilityState }
    },
    fill: async (a) => {
      const el = resolve(a.ref)
      const box = centerOf(el)
      el.focus?.()
      if (el.isContentEditable) {
        el.textContent = a.value
        el.dispatchEvent(new InputEvent('input', { bubbles: true }))
      } else if (el.tagName === 'SELECT') {
        const opt = [...el.options].find((o) => o.value === a.value || o.text.trim() === a.value)
        if (!opt) throw new Error(`OPTION_NOT_FOUND: "${a.value}" 가 선택지에 없습니다. 있는 것: ${[...el.options].map((o) => o.text.trim()).join(' / ')}`)
        el.value = opt.value
        el.dispatchEvent(new Event('change', { bubbles: true }))
      } else if (el.type === 'checkbox' || el.type === 'radio') {
        const want = a.value === true || a.value === 'true'
        if (el.checked !== want) el.click()
      } else {
        setNativeValue(el, a.value)
        el.dispatchEvent(new Event('input', { bubbles: true }))
        el.dispatchEvent(new Event('change', { bubbles: true }))
      }
      const actual = el.isContentEditable ? el.textContent : el.value
      const want = String(a.value)
      // name 은 활동 기록용 — 값(비밀번호일 수 있다)이 아니라 필드 이름만 남긴다
      return { box, name: nameOf(el), applied: el.type === 'checkbox' || el.type === 'radio' ? el.checked : actual, matched: String(actual) === want || el.type === 'checkbox' || el.type === 'radio' }
    },
    scroll: async (a) => {
      const target = a.ref ? resolve(a.ref) : null
      const amount = (a.amount ?? 3) * 100
      const dx = a.direction === 'left' ? -amount : a.direction === 'right' ? amount : 0
      const dy = a.direction === 'up' ? -amount : a.direction === 'down' ? amount : 0
      const before = target ? target.scrollTop : window.scrollY
      if (target) target.scrollBy({ left: dx, top: dy, behavior: 'auto' })
      else window.scrollBy({ left: dx, top: dy, behavior: 'auto' })
      await new Promise((r) => setTimeout(r, 120))
      const after = target ? target.scrollTop : window.scrollY
      return { changed: after !== before, from: Math.round(before), to: Math.round(after), visibilityState: document.visibilityState }
    },
    scroll_to: (a) => ({ box: centerOf(resolve(a.ref)), visibilityState: document.visibilityState }),
    press: async (a) => {
      const el = document.activeElement || document.body
      const key = a.key
      const init = { key, code: key, bubbles: true, cancelable: true }
      const { changed } = await observeChange(() => {
        el.dispatchEvent(new KeyboardEvent('keydown', init))
        el.dispatchEvent(new KeyboardEvent('keyup', init))
      }, a.waitMs ?? 200)
      return { changed }
    },
    type: async (a) => {
      const el = document.activeElement
      if (!el) throw new Error('NO_FOCUS: 포커스된 요소가 없습니다. 먼저 click 이나 fill 로 대상을 잡으세요.')
      const { changed } = await observeChange(() => {
        if (el.isContentEditable) {
          el.textContent += a.text
          el.dispatchEvent(new InputEvent('input', { bubbles: true }))
        } else if ('value' in el) {
          setNativeValue(el, (el.value || '') + a.text)
          el.dispatchEvent(new Event('input', { bubbles: true }))
        }
      }, a.waitMs ?? 200)
      return { changed }
    },
    // 누가 이 탭을 맡고 있는지 페이지 위에 직접 보여준다. 색·아바타는 그 신원 것.
    // state: 'on'(조작 중 — 테두리 글로우 + 칩) / 'idle'(대기 — 칩만) / 'off'(세션 끝, 걷어냄).
    // shadow DOM 에 넣어 사이트 CSS 와 섞이지 않게 하고, body 가 갈려도 살아남게 documentElement 에 붙인다.
    overlay: (a) => {
      const root = ensureOverlay()
      if (!root) return { applied: false }
      const frame = root.querySelector('.frame')
      const chip = root.querySelector('.chip')
      const cursor = root.querySelector('.cursor')

      if (a.state === 'off') {
        root.host.dataset.on = '0'
        clearTimeout(window.__ccOverlayTimer)
        window.__ccOverlayTimer = setTimeout(() => {
          if (root.host.dataset.on === '0') root.host.style.opacity = '0'
        }, a.lingerMs ?? 1200)
        return { applied: true, state: 'off' }
      }

      const mode = a.state === 'idle' ? 'idle' : 'active'
      root.host.dataset.on = '1'
      root.host.dataset.mode = mode
      clearTimeout(window.__ccOverlayTimer)
      root.host.style.opacity = '1'
      if (a.color) {
        frame.style.setProperty('--cc', a.color)
        cursor.style.setProperty('--cc', a.color)
        chip.style.setProperty('--cc', a.color)
      }
      if (a.avatar) {
        for (const img of root.querySelectorAll('img')) if (img.src !== a.avatar) img.src = a.avatar
      }
      const label = chip.querySelector('.label')
      const text = a.task ? `${a.name} · ${a.task}` : a.name
      if (label.textContent !== text) label.textContent = text
      return { applied: true, state: mode }
    },

    // 조작 지점을 사람이 눈으로 따라갈 수 있게 아바타 커서를 옮긴다.
    cursor: (a) => {
      const root = ensureOverlay()
      if (!root) return { applied: false }
      const cursor = root.querySelector('.cursor')
      cursor.style.transform = `translate(${Math.round(a.x)}px, ${Math.round(a.y)}px)`
      cursor.style.opacity = '1'
      if (a.click) {
        cursor.classList.remove('tap')
        void cursor.offsetWidth
        cursor.classList.add('tap')
      }
      return { applied: true }
    },

    file_input_ref: (a) => {
      const el = a.ref ? resolve(a.ref) : document.querySelector('input[type=file]')
      if (!el) throw new Error('FILE_INPUT_NOT_FOUND: 페이지에 파일 입력이 없습니다.')
      return { found: true, ref: refFor(el) }
    },
  }

  chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
    if (!msg || msg.__cc !== true) return
    const fn = ops[msg.op]
    if (!fn) { sendResponse({ ok: false, error: `UNKNOWN_OP: ${msg.op}` }); return }
    Promise.resolve()
      .then(() => fn(msg.args || {}))
      .then((result) => sendResponse({ ok: true, result }))
      .catch((e) => sendResponse({ ok: false, error: String(e && e.message ? e.message : e) }))
    return true
  })
}
