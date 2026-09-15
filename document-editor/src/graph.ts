export type GraphNode = { id: string; name: string };
export type VaultGraph = { vaultId: string; requestId: string; nodes: GraphNode[]; edges: { source: string; target: string }[]; truncated: boolean; error?: string };
type Point = GraphNode & { x: number; y: number; neighbors: Set<string> };

export function graphLayout(data: Pick<VaultGraph, 'nodes' | 'edges'>): Point[] {
  const points = new Map<string, Point>();
  for (const node of data.nodes) if (!points.has(node.id)) points.set(node.id, { ...node, x: 0, y: 0, neighbors: new Set() });
  for (const edge of data.edges) {
    if (edge.source === edge.target || !points.has(edge.source) || !points.has(edge.target)) continue;
    points.get(edge.source)!.neighbors.add(edge.target); points.get(edge.target)!.neighbors.add(edge.source);
  }
  // Connected components stay together without a continuously running force simulation.
  const groups: Point[][] = [], visited = new Set<string>();
  for (const start of points.values()) {
    if (visited.has(start.id)) continue;
    const group = [start]; visited.add(start.id);
    for (let i = 0; i < group.length; i++) for (const id of group[i].neighbors) if (!visited.has(id)) { visited.add(id); group.push(points.get(id)!); }
    groups.push(group);
  }
  const columns = Math.ceil(Math.sqrt(groups.length));
  const sizes = groups.map(group => Math.max(110, Math.sqrt(group.length) * 32));
  const cell = Math.max(220, ...sizes.map(size => size * 2 + 80));
  groups.forEach((group, index) => group.forEach((point, i) => {
    const angle = i * 2.399963229728653, radius = 26 * Math.sqrt(i);
    point.x = index % columns * cell + Math.cos(angle) * radius;
    point.y = Math.floor(index / columns) * cell + Math.sin(angle) * radius;
  }));
  return [...points.values()];
}

export function mountGraph(open: (id: string) => void, refresh: () => void) {
  const root = document.createElement('section'); root.className = 'vault-graph'; root.hidden = true; root.setAttribute('aria-label', '문서 연결 그래프');
  const toolbar = document.createElement('div'); toolbar.className = 'graph-toolbar';
  const search = document.createElement('input'); search.type = 'search'; search.placeholder = '문서 찾기'; search.setAttribute('aria-label', '그래프 문서 검색');
  const button = (label: string, fn: () => void) => { const b = document.createElement('button'); b.type = 'button'; b.className = 'document-button'; b.textContent = label; b.addEventListener('click', fn); return b; };
  const reload = button('새로고침', refresh);
  toolbar.append(search, button('전체 보기', fit), button('확대', () => zoom(1.3)), button('축소', () => zoom(1 / 1.3)), reload);
  const status = document.createElement('p'); status.className = 'graph-status'; status.setAttribute('role', 'status');
  const stage = document.createElement('div'); stage.className = 'graph-stage';
  const canvas = document.createElement('canvas'); canvas.setAttribute('aria-label', '문서 연결 지도. 아래 문서 목록에서도 문서를 열 수 있어요.'); canvas.setAttribute('role', 'img');
  const tooltip = document.createElement('div'); tooltip.className = 'graph-tooltip'; tooltip.hidden = true;
  stage.append(canvas, tooltip);
  const details = document.createElement('details'); details.className = 'graph-documents';
  const summary = document.createElement('summary'); summary.textContent = '문서 목록';
  const list = document.createElement('div'); list.className = 'graph-list';
  details.append(summary, list);
  const hint = document.createElement('p'); hint.className = 'graph-hint'; hint.textContent = '드래그로 이동 · 스크롤로 확대 · 문서를 눌러 열기';
  root.append(toolbar, status, stage, hint, details);
  let points: Point[] = [], byId = new Map<string, Point>(), edges: VaultGraph['edges'] = [], hovered: Point | undefined;
  let scale = 1, offsetX = 0, offsetY = 0, frame = 0, active = false, result: VaultGraph | undefined, page = 100;
  let drag: { x: number; y: number; startX: number; startY: number; moved: boolean } | undefined;
  function matching(point: Point) { const q = search.value.trim().toLocaleLowerCase(); return !q || point.name.toLocaleLowerCase().includes(q) || point.id.toLocaleLowerCase().includes(q); }
  function invalidate() { if (!active || document.hidden || frame) return; frame = requestAnimationFrame(() => { frame = 0; draw(); }); }
  function fit() {
    const selected = points.filter(matching); if (!selected.length) { invalidate(); return; }
    let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
    for (const p of selected) { minX = Math.min(minX, p.x); minY = Math.min(minY, p.y); maxX = Math.max(maxX, p.x); maxY = Math.max(maxY, p.y); }
    const w = stage.clientWidth || 600, h = stage.clientHeight || 500;
    scale = Math.min(2, (w - 80) / Math.max(80, maxX - minX), (h - 80) / Math.max(80, maxY - minY));
    scale = Math.max(.01, scale); offsetX = w / 2 - (minX + maxX) / 2 * scale; offsetY = h / 2 - (minY + maxY) / 2 * scale; invalidate();
  }
  function zoom(factor: number, x = stage.clientWidth / 2, y = stage.clientHeight / 2) { const next = Math.max(.01, Math.min(8, scale * factor)); offsetX = x - (x - offsetX) * next / scale; offsetY = y - (y - offsetY) * next / scale; scale = next; invalidate(); }
  function draw() {
    const ctx = canvas.getContext('2d'); if (!ctx) return;
    const w = stage.clientWidth, h = stage.clientHeight, ratio = Math.min(devicePixelRatio || 1, 2);
    canvas.width = Math.round(w * ratio); canvas.height = Math.round(h * ratio); ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
    const style = getComputedStyle(root), ink = style.getPropertyValue('--ink').trim() || '#333', muted = style.getPropertyValue('--muted').trim() || '#777', line = style.getPropertyValue('--line').trim() || '#ccc';
    ctx.clearRect(0, 0, w, h); ctx.lineWidth = 1;
    for (const edge of edges) { const a = byId.get(edge.source), b = byId.get(edge.target); if (!a || !b) continue; const highlighted = hovered && (a === hovered || b === hovered); ctx.globalAlpha = hovered && !highlighted ? .12 : .55; ctx.strokeStyle = highlighted ? ink : line; ctx.beginPath(); ctx.moveTo(a.x * scale + offsetX, a.y * scale + offsetY); ctx.lineTo(b.x * scale + offsetX, b.y * scale + offsetY); ctx.stroke(); }
    const labels: { point: Point; x: number; y: number }[] = [];
    for (const p of points) {
      const x = p.x * scale + offsetX, y = p.y * scale + offsetY; if (x < -100 || y < -30 || x > w + 100 || y > h + 30) continue;
      const highlight = p === hovered || hovered?.neighbors.has(p.id), match = matching(p);
      ctx.globalAlpha = !match || hovered && !highlight ? .2 : 1; ctx.fillStyle = highlight ? ink : muted;
      ctx.beginPath(); ctx.arc(x, y, p === hovered ? 6 : 3.5, 0, Math.PI * 2); ctx.fill();
      if (highlight || match && (points.length <= 8 || scale >= 1.25 && points.length < 60 || search.value.trim())) labels.push({ point: p, x, y });
    }
    const boxes: { x: number; y: number; width: number }[] = [];
    labels.sort((a, b) => Number(b.point === hovered) - Number(a.point === hovered));
    ctx.font = '12px -apple-system, sans-serif'; ctx.globalAlpha = 1; ctx.fillStyle = ink;
    for (const { point, x, y } of labels) {
      if (boxes.length >= 80 || y < 12 || y > h - 12) continue;
      let label = point.name.length > 28 ? point.name.slice(0, 27) + '…' : point.name;
      while (label.length > 2 && ctx.measureText(label).width > w - 32) label = label.slice(0, -2) + '…';
      const width = ctx.measureText(label).width, left = Math.max(8, Math.min(w - width - 8, x + 9));
      if (boxes.some(box => Math.abs(box.y - y) < 18 && left < box.x + box.width + 6 && left + width + 6 > box.x)) continue;
      boxes.push({ x: left, y, width }); ctx.fillText(label, left, y + 4);
    }
    ctx.globalAlpha = 1;
  }
  function hit(x: number, y: number) { let found: Point | undefined, distance = 144; for (const p of points) { const d = (p.x * scale + offsetX - x) ** 2 + (p.y * scale + offsetY - y) ** 2; if (d < distance && matching(p)) { found = p; distance = d; } } return found; }
  function renderList() {
    list.replaceChildren(); const matches = points.filter(matching); summary.textContent = `문서 목록 · ${matches.length}`;
    for (const point of matches.slice(0, page)) { const b = button(point.name, () => open(point.id)); b.title = point.id; list.append(b); }
    if (matches.length > page) list.append(button('더 보기', () => { page += 100; renderList(); }));
    if (!matches.length && points.length) list.append('일치하는 문서가 없어요.');
  }
  search.addEventListener('input', () => { page = 100; details.open = Boolean(search.value); renderList(); fit(); });
  canvas.addEventListener('wheel', event => { event.preventDefault(); const bounds = canvas.getBoundingClientRect(); zoom(Math.exp(-Math.max(-100, Math.min(100, event.deltaY)) * .008), event.clientX - bounds.left, event.clientY - bounds.top); }, { passive: false });
  canvas.addEventListener('pointerdown', event => { if (event.button !== 0) return; canvas.setPointerCapture(event.pointerId); drag = { x: event.clientX, y: event.clientY, startX: event.clientX, startY: event.clientY, moved: false }; });
  canvas.addEventListener('pointermove', event => {
    const bounds = canvas.getBoundingClientRect(), x = event.clientX - bounds.left, y = event.clientY - bounds.top;
    if (drag) { drag.moved ||= Math.hypot(event.clientX - drag.startX, event.clientY - drag.startY) > 4; offsetX += event.clientX - drag.x; offsetY += event.clientY - drag.y; drag.x = event.clientX; drag.y = event.clientY; hovered = undefined; } else hovered = hit(x, y);
    tooltip.hidden = !hovered; tooltip.textContent = hovered?.id ?? ''; canvas.style.cursor = drag ? 'grabbing' : hovered ? 'pointer' : 'grab'; invalidate();
  });
  canvas.addEventListener('pointerup', event => { if (drag && !drag.moved) { const bounds = canvas.getBoundingClientRect(); const p = hit(event.clientX - bounds.left, event.clientY - bounds.top); if (p) open(p.id); } drag = undefined; });
  canvas.addEventListener('pointercancel', () => { drag = undefined; });
  canvas.addEventListener('pointerleave', () => { hovered = undefined; tooltip.hidden = true; invalidate(); });
  const resize = new ResizeObserver(() => invalidate()); resize.observe(stage);
  const theme = new MutationObserver(invalidate); theme.observe(document.documentElement, { attributes: true }); theme.observe(document.body, { attributes: true });
  const visibility = () => { if (document.hidden) { cancelAnimationFrame(frame); frame = 0; } else invalidate(); }; document.addEventListener('visibilitychange', visibility);
  return {
    root,
    show(show: boolean) { active = show; root.hidden = !show; if (show) { if (!result) fit(); else invalidate(); } else { cancelAnimationFrame(frame); frame = 0; } },
    loading() { result = undefined; points = []; byId.clear(); edges = []; reload.disabled = true; status.textContent = '문서 사이의 연결을 읽고 있어요…'; renderList(); invalidate(); },
    set(data: VaultGraph) {
      result = data; reload.disabled = false; points = graphLayout(data); byId = new Map(points.map(p => [p.id, p])); edges = data.edges;
      status.textContent = data.error ? `그래프를 읽지 못했어요. ${data.error} 새로고침으로 다시 시도하세요.` : !points.length ? '이 볼트에는 마크다운 문서가 없어요.' : `${points.length}개 문서 · ${edges.length}개 연결${!edges.length ? ' · 문서에 링크를 넣으면 연결돼요.' : ''}${data.truncated ? ' · 일부 문서만 표시했어요.' : ''}`;
      page = 100; renderList(); fit();
    },
    destroy() { cancelAnimationFrame(frame); resize.disconnect(); theme.disconnect(); document.removeEventListener('visibilitychange', visibility); root.remove(); },
  };
}
