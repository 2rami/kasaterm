import { hostOf } from './url.js'
import { tabSections, bookmarkMatches, bookmarkCount, bookmarkResults, byName, matches, canOpenBookmark } from './library-model.js'

const COLORS = { grey: '#aeb4bd', blue: '#8ab4f8', red: '#f28b82', yellow: '#fdd663', green: '#81c995', pink: '#ff8bcb', purple: '#c58af9', cyan: '#78d9ec', orange: '#fcad70' }
const PREF_KEY = 'browserLibrary'

function node(tag, className, text) {
  const element = document.createElement(tag)
  if (className) element.className = className
  if (text != null) element.textContent = text
  return element
}

function icon(kind) {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg')
  svg.setAttribute('viewBox', '0 0 20 20')
  svg.setAttribute('aria-hidden', 'true')
  svg.classList.add('library-icon')
  const path = document.createElementNS(svg.namespaceURI, 'path')
  path.setAttribute('d', {
    search: 'M13.5 13.5 18 18M15 8.5a6.5 6.5 0 1 1-13 0 6.5 6.5 0 0 1 13 0',
    folder: 'M2 6V4h6l2 2h8v11H2V6Z',
    page: 'M5 2h7l4 4v12H5V2Zm7 0v5h4',
    chevron: 'm7 5 5 5-5 5',
    close: 'm5 5 10 10M15 5 5 15',
  }[kind])
  svg.append(path)
  return svg
}

export function mountLibrary(students, connection) {
  const shell = node('section', 'library-shell')
  const controls = node('div', 'library-controls')
  const searchBox = node('div', 'library-search')
  const search = node('input')
  search.type = 'search'
  search.autocomplete = 'off'
  search.setAttribute('aria-label', '탭과 그룹 검색')
  const clear = node('button', 'library-clear')
  clear.type = 'button'
  clear.setAttribute('aria-label', '검색 지우기')
  clear.append(icon('close'))
  clear.hidden = true
  searchBox.append(icon('search'), search, clear)
  const nav = node('div', 'library-nav')
  nav.setAttribute('role', 'tablist')
  nav.setAttribute('aria-label', '브라우저 탐색')
  const content = node('div', 'library-content')
  content.id = 'library-content'
  content.setAttribute('role', 'tabpanel')
  const status = node('div', 'library-status')
  status.setAttribute('role', 'status')
  status.hidden = true
  const toolbar = node('div', 'library-toolbar')
  const summary = node('span', 'library-summary')
  const sort = node('select', 'library-sort')
  sort.setAttribute('aria-label', '목록 정렬')
  for (const [value, title] of [['name', '이름순'], ['chrome', '크롬 순서']]) {
    const option = node('option', null, title)
    option.value = value
    sort.append(option)
  }
  const actions = node('div', 'library-actions')
  const align = node('button', 'library-action', '탭바 정렬')
  align.title = '현재 창의 그룹을 이름순으로 정렬합니다. 고정 탭과 그룹 밖 탭은 서로의 순서를 유지합니다.'
  const recolor = node('button', 'library-action', '색 정리')
  recolor.title = '이 창에서 겹치는 그룹색을 비어 있는 색으로 바꿉니다.'
  actions.append(align, recolor)
  toolbar.append(summary, sort)
  controls.append(searchBox, nav, toolbar, actions)
  shell.append(controls, status, content)
  students.before(shell)
  students.setAttribute('role', 'tabpanel')
  students.setAttribute('aria-labelledby', 'library-nav-students')

  let prefs = { view: 'tabs', sort: 'name', open: {} }
  let tabs = [], groups = [], bookmarks = []
  let tabsLoaded = false, booksLoaded = false, permitted = false
  let tabsError = false, booksError = false
  let windowId = null, tabGeneration = 0, bookGeneration = 0
  let actionRunning = false
  let prefsTouched = false
  const navButtons = new Map()

  const save = () => {
    prefsTouched = true
    chrome.storage.local.set({ [PREF_KEY]: prefs }).catch(() => notify('보기 설정을 저장하지 못했어요. 다시 열면 초기화될 수 있어요.'))
  }
  function notify(text) { status.textContent = text; status.hidden = !text }
  function query() { return search.value.trim() }

  for (const [view, label] of [['tabs', '탭'], ['bookmarks', '북마크'], ['students', '학생']]) {
    const button = node('button', 'library-nav-item', label)
    button.id = `library-nav-${view}`
    button.setAttribute('role', 'tab')
    button.setAttribute('aria-controls', view === 'students' ? students.id : content.id)
    button.addEventListener('click', () => selectView(view))
    nav.append(button)
    navButtons.set(view, button)
  }
  nav.addEventListener('keydown', event => {
    const keys = [...navButtons.keys()]
    const current = keys.indexOf(prefs.view)
    let next
    if (event.key === 'ArrowRight') next = (current + 1) % keys.length
    if (event.key === 'ArrowLeft') next = (current + keys.length - 1) % keys.length
    if (event.key === 'Home') next = 0
    if (event.key === 'End') next = keys.length - 1
    if (next == null) return
    event.preventDefault()
    selectView(keys[next])
    navButtons.get(keys[next]).focus()
  })

  function selectView(view, persist = true) {
    prefs.view = view
    const isStudents = view === 'students'
    students.hidden = !isStudents
    connection.hidden = !isStudents
    content.hidden = isStudents
    searchBox.hidden = isStudents
    toolbar.hidden = isStudents
    actions.hidden = view !== 'tabs'
    for (const [key, button] of navButtons) {
      button.setAttribute('aria-selected', String(key === view))
      button.tabIndex = key === view ? 0 : -1
    }
    content.setAttribute('aria-labelledby', `library-nav-${view}`)
    search.placeholder = view === 'bookmarks' ? '북마크와 폴더 검색' : '탭, 주소, 그룹 검색'
    search.setAttribute('aria-label', search.placeholder)
    sort.lastElementChild.textContent = view === 'bookmarks' ? '저장된 순서' : '크롬 순서'
    sort.value = prefs.sort
    notify('')
    if (persist) save()
    render()
    if (view === 'bookmarks') loadBookmarks()
  }

  search.addEventListener('input', () => { clear.hidden = !search.value; render() })
  clear.addEventListener('click', () => { search.value = ''; clear.hidden = true; search.focus(); render() })
  sort.addEventListener('change', () => { prefs.sort = sort.value; save(); render() })

  function empty(title, detail, action, handler) {
    const wrap = node('div', 'library-empty')
    wrap.append(node('strong', null, title), node('p', null, detail))
    if (action) {
      const button = node('button', 'library-primary', action)
      button.addEventListener('click', handler)
      wrap.append(button)
    }
    return wrap
  }

  function disclosure(key, title, count, color, defaultOpen) {
    const wrap = node('section', 'library-section')
    const button = node('button', 'library-section-head')
    const open = query() ? true : prefs.open[key] ?? defaultOpen
    const body = node('div', 'library-section-body')
    body.hidden = !open
    button.dataset.focus = key
    button.setAttribute('aria-expanded', String(open))
    button.append(icon('chevron'))
    if (color) {
      const dot = node('span', 'library-color')
      dot.style.backgroundColor = COLORS[color] || COLORS.grey
      button.append(dot)
    } else button.append(icon('folder'))
    const label = node('span', 'library-section-title', title)
    label.title = title
    button.append(label, node('span', 'library-count', String(count)))
    button.addEventListener('click', () => {
      if (query()) {
        body.hidden = !body.hidden
        button.setAttribute('aria-expanded', String(!body.hidden))
        return
      }
      prefs.open[key] = !open
      save()
      render()
    })
    wrap.append(button, body)
    return { wrap, body }
  }

  function linkRow(title, url, active, key, onClick) {
    const button = node('button', active ? 'library-link active' : 'library-link')
    button.dataset.focus = key
    button.title = `${title}\n${url || ''}`
    if (active) button.setAttribute('aria-current', 'page')
    const text = node('span', 'library-link-text')
    text.append(node('span', 'library-link-title', title || url || '제목 없음'), node('span', 'library-domain', hostOf(url) || url || '주소 없음'))
    button.append(icon('page'), text)
    if (active) button.append(node('span', 'library-active-label', '현재'))
    button.addEventListener('click', onClick)
    return button
  }

  function renderTabs() {
    summary.textContent = tabsLoaded ? `이 창 · ${tabs.length}개 탭` : '탭 불러오는 중'
    align.disabled = actionRunning || !tabsLoaded || groups.length < 2
    recolor.disabled = actionRunning || !tabsLoaded || groups.length < 2
    if (tabsError) return content.append(empty('탭을 불러오지 못했어요', '잠시 후 다시 불러와 주세요.', '다시 시도', loadTabs))
    if (!tabsLoaded) return content.append(empty('탭을 불러오는 중', '현재 크롬 창의 탭을 확인하고 있어요.'))
    const sections = tabSections(tabs, groups, prefs.sort, query())
    if (!sections.length) return content.append(empty(query() ? '찾는 탭이 없어요' : '열린 탭이 없어요', query() ? '다른 제목이나 주소로 검색해 보세요.' : '이 창에서 탭을 열면 여기에 나타나요.'))
    if (query()) summary.textContent = `${sections.reduce((n, section) => n + section.tabs.length, 0)}개 탭 찾음`
    for (const section of sections) {
      const { wrap, body } = disclosure(`tab:${windowId}:${section.key}`, section.title, section.tabs.length, section.color, section.tabs.some(tab => tab.active) || section.key === 'pinned')
      for (const tab of section.tabs) {
        body.append(linkRow(tab.title, tab.url, tab.active, `tab-link:${tab.id}`, async () => {
          try {
            await chrome.tabs.update(tab.id, { active: true })
            if (document.body.classList.contains('popup')) window.close()
          } catch { notify('이 탭이 닫혔거나 이동했어요. 목록을 새로 불러올게요.'); loadTabs() }
        }))
      }
      content.append(wrap)
    }
  }

  function bookmarkBranch(nodes, parentMatched = false, depth = 0) {
    const fragment = document.createDocumentFragment()
    const ordered = prefs.sort === 'name' ? [...nodes].sort((a, b) => Number(!!a.url) - Number(!!b.url) || byName(a, b)) : nodes
    for (const bookmark of ordered) {
      if (!parentMatched && !bookmarkMatches(bookmark, query())) continue
      if (bookmark.url) {
        const row = linkRow(bookmark.title, bookmark.url, false, `bookmark-link:${bookmark.id}`, async () => {
          if (!canOpenBookmark(bookmark.url)) return notify('이 북마크는 여기서 열 수 없는 주소예요. 크롬 북마크 메뉴에서 열어 주세요.')
          try {
            await chrome.tabs.create({ url: bookmark.url, windowId: (await chrome.windows.getCurrent()).id })
            if (document.body.classList.contains('popup')) window.close()
          } catch { notify('북마크를 열지 못했어요. 주소가 유효한지 크롬 북마크 메뉴에서 확인해 주세요.') }
        })
        if (bookmark.path) {
          const path = node('span', 'library-bookmark-path', bookmark.path)
          path.title = bookmark.path
          row.querySelector('.library-link-text').append(path)
        }
        fragment.append(row)
      } else {
        const children = bookmark.children || []
        const { wrap, body } = disclosure(`bookmark:${bookmark.id}`, bookmark.title || '북마크', bookmarkCount(bookmark), null, depth === 0)
        wrap.classList.add('library-folder')
        body.append(children.length ? bookmarkBranch(children, parentMatched || (!!query() && matches(query(), bookmark.title)), depth + 1) : node('p', 'library-folder-empty', '비어 있는 폴더'))
        fragment.append(wrap)
      }
    }
    return fragment
  }

  function renderBookmarks() {
    summary.textContent = '이 크롬 프로필의 북마크'
    if (booksError) return content.append(empty('북마크를 불러오지 못했어요', '잠시 후 다시 불러와 주세요.', '다시 시도', loadBookmarks))
    if (!booksLoaded) return content.append(empty('북마크를 확인하는 중', '이 프로필의 접근 권한을 확인하고 있어요.'))
    if (!permitted) return content.append(empty('내 북마크를 여기서 보기', '한 번 연결하면 폴더와 저장한 페이지를 볼 수 있어요. 북마크는 이 크롬 안에서만 읽어요.', '북마크 연결', async () => {
      try {
        const granted = await chrome.permissions.request({ permissions: ['bookmarks'] })
        if (!granted) return notify('북마크 접근을 허용하지 않았어요. 연결 버튼으로 다시 시도할 수 있어요.')
        await loadBookmarks()
      } catch { notify('연결하지 못했어요. 확장 프로그램을 새로고침한 뒤 다시 시도해 주세요.') }
    }))
    if (!bookmarks.length) return content.append(empty('저장한 북마크가 없어요', '크롬에서 페이지를 북마크에 저장하면 여기에 나타나요.'))
    if (!bookmarks.some(bookmark => bookmarkMatches(bookmark, query()))) return content.append(empty('찾는 북마크가 없어요', '다른 제목, 폴더 이름이나 주소로 검색해 보세요.'))
    const results = query() ? bookmarkResults(bookmarks, query()) : []
    if (query() && results.length) {
      summary.textContent = `${results.length}개 북마크 찾음`
      content.append(bookmarkBranch(results, true))
    } else content.append(bookmarkBranch(bookmarks))
  }

  function render() {
    const focus = content.contains(document.activeElement) ? document.activeElement.dataset.focus : null
    content.replaceChildren()
    if (prefs.view === 'tabs') renderTabs()
    if (prefs.view === 'bookmarks') renderBookmarks()
    if (focus) [...content.querySelectorAll('[data-focus]')].find(element => element.dataset.focus === focus)?.focus({ preventScroll: true })
  }

  async function loadTabs() {
    const generation = ++tabGeneration
    try {
      const currentWindowId = (await chrome.windows.getCurrent()).id
      const [nextTabs, nextGroups] = await Promise.all([chrome.tabs.query({ windowId: currentWindowId }), chrome.tabGroups.query({ windowId: currentWindowId })])
      if (generation !== tabGeneration) return
      windowId = currentWindowId
      tabs = nextTabs; groups = nextGroups; tabsLoaded = true; tabsError = false
    } catch { if (generation !== tabGeneration) return; tabsError = true }
    if (prefs.view === 'tabs') render()
  }

  async function loadBookmarks() {
    const generation = ++bookGeneration
    try {
      const allowed = await chrome.permissions.contains({ permissions: ['bookmarks'] })
      const tree = allowed ? await chrome.bookmarks.getTree() : []
      if (generation !== bookGeneration) return
      permitted = allowed; bookmarks = tree.flatMap(root => root.children || []); booksLoaded = true; booksError = false
      if (allowed) listenToBookmarks()
    } catch { if (generation !== bookGeneration) return; booksError = true }
    if (prefs.view === 'bookmarks') render()
  }

  async function runAction(button, action) {
    if (actionRunning) return
    actionRunning = true; align.disabled = true; recolor.disabled = true
    const text = button.textContent
    button.textContent = '정리 중'
    try { await action() }
    catch { notify('일부 그룹을 정리하지 못했어요. 탭 이동이 끝난 뒤 다시 눌러 주세요.') }
    finally { actionRunning = false; button.textContent = text; await loadTabs() }
  }

  align.addEventListener('click', () => runAction(align, async () => {
    const currentTabs = await chrome.tabs.query({ windowId })
    const currentGroups = await chrome.tabGroups.query({ windowId })
    const ordered = tabSections(currentTabs, currentGroups, 'name').filter(section => section.id != null)
    // Group moves keep membership and internal order intact, including collapsed groups.
    let index = Math.min(...ordered.map(section => section.index))
    for (const section of ordered) {
      await chrome.tabGroups.move(section.id, { index })
      index += section.tabs.length
    }
    notify('그룹을 이름순으로 정렬했어요. 그룹 안의 탭 순서는 그대로예요.')
  }))
  recolor.addEventListener('click', () => runAction(recolor, async () => {
    const result = await chrome.runtime.sendMessage({ __ccPopup: true, op: 'recolorGroups', windowId })
    if (!result?.ok) throw new Error('Group colors could not be updated')
    notify(result.overflow ? '색을 고르게 나눴어요. 크롬은 9가지 색이라 그룹이 더 많으면 일부 색이 겹쳐요.' : result.changed ? '겹치던 그룹색을 정리했어요.' : '이미 모든 그룹색이 달라요.')
  }))

  let tabsTimer, bookmarksTimer, bookmarkListeners = false
  const scheduleTabs = () => { clearTimeout(tabsTimer); tabsTimer = setTimeout(loadTabs, 100) }
  const scheduleBookmarks = () => { clearTimeout(bookmarksTimer); bookmarksTimer = setTimeout(loadBookmarks, 100) }
  function listenToBookmarks() {
    if (bookmarkListeners || !chrome.bookmarks) return
    bookmarkListeners = true
    for (const name of ['onCreated', 'onRemoved', 'onChanged', 'onMoved', 'onChildrenReordered', 'onImportEnded']) chrome.bookmarks[name]?.addListener(scheduleBookmarks)
  }
  for (const name of ['onCreated', 'onRemoved', 'onUpdated', 'onMoved', 'onActivated', 'onAttached', 'onDetached', 'onReplaced']) chrome.tabs[name]?.addListener(scheduleTabs)
  for (const name of ['onCreated', 'onRemoved', 'onUpdated', 'onMoved']) chrome.tabGroups[name]?.addListener(scheduleTabs)
  chrome.permissions.onAdded.addListener(scheduleBookmarks)
  chrome.permissions.onRemoved.addListener(scheduleBookmarks)
  selectView('tabs', false)
  chrome.storage.local.get(PREF_KEY).then(saved => {
    if (prefsTouched) return
    const value = saved[PREF_KEY]
    if (value && ['tabs', 'bookmarks', 'students'].includes(value.view)) prefs = { view: value.view, sort: value.sort === 'chrome' ? 'chrome' : 'name', open: value.open || {} }
    selectView(prefs.view, false)
  }).catch(() => {})
  loadTabs()
}
