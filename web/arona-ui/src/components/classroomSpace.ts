// munder Office 의 "타일맵 + collision + BFS pathfinding" 을 순수 DOM(%) 좌표계로
// 이식. 배경은 빈 바닥 한 장이고, 가구는 개별 스프라이트로 좌표 배치한다 — 그림에
// 박힌 게 아니라 오브젝트라 캐릭터가 가구를 피해 다니고(충돌) y-zindex 로 앞뒤가
// 가려진다. 좌표는 전부 % (배경 위 절대 위치, x=중심 / y=발밑 바닥).

export type Facing = 'up' | 'down' | 'left' | 'right';
export type FurnitureKind = 'desk' | 'sofa' | 'coffee' | 'shelf' | 'plant';

export interface Furniture {
  id: string;
  kind: FurnitureKind;
  /** % 중심 x, 발밑(바닥) y, 스프라이트 폭. */
  x: number;
  y: number;
  w: number;
  /** assets/ 하위 파일명. */
  sprite: string;
  /** 충돌 사각형 [x0,y0,x1,y1] (%). 이 안은 못 지나감. */
  block: [number, number, number, number];
  /** 책상류: 캐릭터가 앉는 자리(발밑 %) + 바라보는 방향. */
  seat?: { x: number; y: number; facing: Facing };
}

export interface CafeSpot {
  x: number;
  y: number;
  facing: Facing;
}

// 바닥 보행 영역 — 이 사각형 밖(벽/창/위쪽 원경)은 못 감. 빈 바닥 배경 보고 보정.
export const FLOOR_BOUNDS = { x0: 10, y0: 48, x1: 90, y1: 96 };

// 교실 가구 배치(빈 바닥 furn-floor 위). 책상 6개 2열(자리) + 좌하단 카페(소파·커피)
// + 벽쪽 책장/화분(장식·충돌만). 모든 좌표 % (x=중심, y=발밑).
export const CLASSROOM_FURNITURE: Furniture[] = [
  // 뒷줄 책상(창가 쪽)
  deskAt('desk-0', 32, 62),
  deskAt('desk-1', 52, 62),
  deskAt('desk-2', 72, 62),
  // 앞줄 책상(앞쪽 — 더 큼/가까움)
  deskAt('desk-3', 32, 84),
  deskAt('desk-4', 52, 84),
  deskAt('desk-5', 72, 84),
  // 카페 구역(좌하단) — 소파 + 커피
  { id: 'sofa', kind: 'sofa', x: 17, y: 93, w: 20, sprite: 'furn-sofa.png', block: [9, 87, 25, 93] },
  { id: 'coffee', kind: 'coffee', x: 13, y: 66, w: 11, sprite: 'furn-coffee.png', block: [9, 59, 18, 67] },
  // 장식 — 충돌만(자리 없음)
  { id: 'shelf', kind: 'shelf', x: 87, y: 56, w: 11, sprite: 'furn-shelf.png', block: [83, 49, 92, 57] },
  { id: 'plant', kind: 'plant', x: 89, y: 90, w: 7, sprite: 'furn-plant.png', block: [86, 86, 92, 91] },
];

function deskAt(id: string, x: number, y: number): Furniture {
  // 책상 스프라이트는 x 중심, y 발밑. 자리는 책상 바로 앞(아래)에 둬서 캐릭터가
  // 책상에 붙어 앉은 것처럼 보이고(발밑 y 가 더 커 책상 앞에 렌더), 위(책상)를 봄.
  const seat = { x, y: y + 4, facing: 'up' as Facing };
  return { id, kind: 'desk', x, y, w: 14, sprite: 'furn-desk.png', block: [x - 7, y - 12, x + 7, y - 2], seat };
}

export function deskSeats(furniture: Furniture[]): Furniture['seat'][] {
  return furniture.filter((f) => f.kind === 'desk' && f.seat).map((f) => f.seat!);
}

// 카페 머무름 지점 — 소파/커피 앞. idle 캐릭터가 여기로 가서 어슬렁.
export function cafeSpots(furniture: Furniture[]): CafeSpot[] {
  const spots: CafeSpot[] = [];
  for (const f of furniture) {
    if (f.kind === 'sofa') {
      spots.push({ x: f.x - 5, y: f.y + 1, facing: 'up' });
      spots.push({ x: f.x + 5, y: f.y + 1, facing: 'up' });
    } else if (f.kind === 'coffee') {
      spots.push({ x: f.x, y: f.y + 5, facing: 'up' });
    }
  }
  return spots;
}

// ── 충돌 그리드 + BFS 길찾기 ────────────────────────────────────────────────
export const GRID_COLS = 44;
export const GRID_ROWS = 30;

export interface Cell { c: number; r: number; }

export function pctToCell(x: number, y: number): Cell {
  return {
    c: Math.max(0, Math.min(GRID_COLS - 1, Math.floor((x / 100) * GRID_COLS))),
    r: Math.max(0, Math.min(GRID_ROWS - 1, Math.floor((y / 100) * GRID_ROWS))),
  };
}
export function cellToPct(cell: Cell): { x: number; y: number } {
  return { x: ((cell.c + 0.5) / GRID_COLS) * 100, y: ((cell.r + 0.5) / GRID_ROWS) * 100 };
}

// walkable[r][c] — 바닥 영역 안 && 가구 충돌 사각형 밖.
export function buildGrid(furniture: Furniture[]): boolean[][] {
  const grid: boolean[][] = [];
  for (let r = 0; r < GRID_ROWS; r++) {
    grid[r] = [];
    for (let c = 0; c < GRID_COLS; c++) {
      const { x, y } = cellToPct({ c, r });
      let ok = x >= FLOOR_BOUNDS.x0 && x <= FLOOR_BOUNDS.x1 && y >= FLOOR_BOUNDS.y0 && y <= FLOOR_BOUNDS.y1;
      if (ok) {
        for (const f of furniture) {
          const [x0, y0, x1, y1] = f.block;
          if (x >= x0 && x <= x1 && y >= y0 && y <= y1) { ok = false; break; }
        }
      }
      grid[r][c] = ok;
    }
  }
  return grid;
}

function walkable(grid: boolean[][], cell: Cell): boolean {
  return cell.r >= 0 && cell.r < GRID_ROWS && cell.c >= 0 && cell.c < GRID_COLS && grid[cell.r][cell.c];
}

// 막힌 목표(자리=책상 충돌칸일 수 있음)는 가장 가까운 보행칸으로 스냅.
export function nearestWalkable(grid: boolean[][], cell: Cell): Cell {
  if (walkable(grid, cell)) return cell;
  for (let rad = 1; rad < Math.max(GRID_COLS, GRID_ROWS); rad++) {
    for (let dr = -rad; dr <= rad; dr++) {
      for (let dc = -rad; dc <= rad; dc++) {
        if (Math.abs(dr) !== rad && Math.abs(dc) !== rad) continue;
        const n = { c: cell.c + dc, r: cell.r + dr };
        if (walkable(grid, n)) return n;
      }
    }
  }
  return cell;
}

const DIRS = [{ c: 1, r: 0 }, { c: -1, r: 0 }, { c: 0, r: 1 }, { c: 0, r: -1 }];

// BFS — start→goal 셀 경로(start 제외, goal 포함). 직선 구간은 합쳐 waypoint 수를 줄인다.
export function findPath(grid: boolean[][], startPct: { x: number; y: number }, goalPct: { x: number; y: number }): { x: number; y: number }[] {
  const start = nearestWalkable(grid, pctToCell(startPct.x, startPct.y));
  const goal = nearestWalkable(grid, pctToCell(goalPct.x, goalPct.y));
  if (start.c === goal.c && start.r === goal.r) return [goalPct];

  const key = (x: Cell) => x.r * GRID_COLS + x.c;
  const queue: Cell[] = [start];
  const parent = new Map<number, Cell | null>();
  parent.set(key(start), null);

  while (queue.length) {
    const cur = queue.shift()!;
    if (cur.c === goal.c && cur.r === goal.r) break;
    for (const d of DIRS) {
      const n = { c: cur.c + d.c, r: cur.r + d.r };
      if (!walkable(grid, n) || parent.has(key(n))) continue;
      parent.set(key(n), cur);
      queue.push(n);
    }
  }

  if (!parent.has(key(goal))) return [goalPct]; // 도달 불가 — 그냥 직행(근사)

  const cells: Cell[] = [];
  let node: Cell | null | undefined = goal;
  while (node) { cells.push(node); node = parent.get(key(node)); }
  cells.reverse(); // start … goal

  // 직선 구간 압축 — 방향 바뀌는 지점만 waypoint 로.
  const pts: { x: number; y: number }[] = [];
  for (let i = 1; i < cells.length; i++) {
    const prev = cells[i - 1], cur = cells[i], nxt = cells[i + 1];
    const turn = !nxt || (nxt.c - cur.c) !== (cur.c - prev.c) || (nxt.r - cur.r) !== (cur.r - prev.r);
    if (turn) pts.push(cellToPct(cur));
  }
  // 마지막 칸은 실제 목표 %로 치환(셀 중심 대신 정확한 자리).
  if (pts.length) pts[pts.length - 1] = goalPct; else pts.push(goalPct);
  return pts;
}
