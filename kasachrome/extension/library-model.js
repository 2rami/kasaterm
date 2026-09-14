const collator = new Intl.Collator('ko', { numeric: true, sensitivity: 'base' })
export const byName = (a, b) => collator.compare(a.title, b.title)
export const matches = (query, ...values) => !query || values.some(value => String(value || '').toLocaleLowerCase().includes(query.toLocaleLowerCase().trim()))

export function tabSections(tabs, groups, sort = 'name', query = '') {
  const map = new Map(groups.map(group => [group.id, group]))
  const sections = new Map()
  for (const tab of [...tabs].sort((a, b) => a.index - b.index)) {
    const key = tab.pinned ? 'pinned' : tab.groupId >= 0 ? String(tab.groupId) : 'loose'
    const group = map.get(tab.groupId)
    if (!sections.has(key)) sections.set(key, {
      key, id: group?.id, color: group?.color || 'grey',
      title: key === 'pinned' ? '고정 탭' : key === 'loose' ? '그룹 없는 탭' : group?.title || '이름 없는 그룹',
      index: tab.index, tabs: [],
    })
    sections.get(key).tabs.push(tab)
  }
  const result = [...sections.values()]
  result.sort((a, b) => {
    const rank = section => section.key === 'pinned' ? 0 : section.key === 'loose' ? 2 : 1
    return rank(a) - rank(b) || (sort === 'name' ? byName(a, b) : a.index - b.index)
  })
  return result.map(section => ({ ...section, total: section.tabs.length,
    tabs: section.tabs.filter(tab => matches(query, section.title, tab.title, tab.url)),
  })).filter(section => section.tabs.length)
}

export function bookmarkMatches(node, query) {
  return matches(query, node.title, node.url) || (node.children || []).some(child => bookmarkMatches(child, query))
}

export function bookmarkCount(node) {
  return node.url ? 1 : (node.children || []).reduce((sum, child) => sum + bookmarkCount(child), 0)
}

export function bookmarkResults(nodes, query, path = []) {
  return nodes.flatMap(node => node.url
    ? (matches(query, node.title, node.url, ...path) ? [{ ...node, path: path.join(' / ') }] : [])
    : bookmarkResults(node.children || [], query, [...path, node.title || '북마크']))
}

export function canOpenBookmark(url) {
  try { return ['http:', 'https:', 'file:', 'ftp:', 'chrome:', 'about:'].includes(new URL(url).protocol) }
  catch { return false }
}

export async function sortWindowGroups(api, windowId) {
  const [tabs, groups] = await Promise.all([api.tabs.query({ windowId }), api.tabGroups.query({ windowId })])
  const ordered = tabSections(tabs, groups, 'name').filter(section => section.id != null)
  let index = Math.min(...ordered.map(section => section.index))
  for (const section of ordered) {
    // A user can move a group to another window while earlier group moves are pending.
    if ((await api.tabGroups.get(section.id)).windowId !== windowId) throw new Error('Group changed windows')
    await api.tabGroups.move(section.id, { index })
    index += section.tabs.length
  }
}
