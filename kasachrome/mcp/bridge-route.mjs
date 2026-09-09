// Selection policy is separate from sockets so it can be tested without Chrome.
export function bridgeRoute(settings, envUrls, localUrl) {
  const raw = settings?.kasachrome_bridge_urls
  const configured = (Array.isArray(raw) ? raw : typeof raw === 'string' ? raw.split(',') : [])
    .map((url) => String(url).trim()).filter(Boolean)
  const selected = typeof settings?.kasachrome_machine === 'string'
    ? settings.kasachrome_machine.trim() : null
  let urls
  if (selected === '') urls = [localUrl]
  else if (selected !== null) {
    // A chosen remote device must never silently become the local browser.
    urls = configured.length && configured[0] !== localUrl ? [configured[0]] : []
  } else urls = configured.length ? configured : envUrls
  return { selected, urls, allowLocalStart: selected === null || selected === '',
    key: JSON.stringify([selected, urls]) }
}

export function needsFreshBrowserHandles(args, tool) {
  return args?.tabId !== undefined || args?.windowId !== undefined || args?.tabIds !== undefined
    || (tool !== undefined && !['status', 'list_tabs', 'new_tab', 'new_window'].includes(tool))
}
