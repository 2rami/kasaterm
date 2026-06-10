import type { TiledMap, TiledLayer } from './TiledMapRenderer';

// 샬레 교실 맵을 **코드로 생성**한다(tmj 파일 손코딩 대신 — 자체 맵 요건 충족,
// LimeZu 0). 14×10 그리드, 32px. gid: 1=바닥 2=벽 3=책상 4=칠판 5=의자.
// TiledMapRenderer 가 그대로 먹는 TiledMap 형태로 반환한다.
export const TS = 32;
export const MAP_W = 14;
export const MAP_H = 10;

export const TILE = { FLOOR: 1, WALL: 2, DESK: 3, BOARD: 4, CHAIR: 5 } as const;
export const TILESET_COLS = 8; // placeholderTileset 과 맞춤

// 책상(상단)·의자(그 아래) 자리. desk-N spawn-point = 의자 칸(캐릭터가 앉는 곳).
// characters.json 순서대로 leader→members 가 desk-0..N 에 고정 배치된다.
export const DESK_CELLS = [
  { x: 3, y: 4 }, { x: 6, y: 4 }, { x: 9, y: 4 },
  { x: 3, y: 7 }, { x: 6, y: 7 }, { x: 9, y: 7 }
];

export function buildClassroomMap(): TiledMap {
  const n = MAP_W * MAP_H;
  const floor = new Array<number>(n).fill(TILE.FLOOR);
  const walls = new Array<number>(n).fill(0);
  const furniture = new Array<number>(n).fill(0);
  const collision = new Array<number>(n).fill(0);
  const at = (x: number, y: number) => y * MAP_W + x;

  // 테두리 벽 (+collision)
  for (let x = 0; x < MAP_W; x++) {
    walls[at(x, 0)] = TILE.WALL; walls[at(x, MAP_H - 1)] = TILE.WALL;
    collision[at(x, 0)] = TILE.WALL; collision[at(x, MAP_H - 1)] = TILE.WALL;
  }
  for (let y = 0; y < MAP_H; y++) {
    walls[at(0, y)] = TILE.WALL; walls[at(MAP_W - 1, y)] = TILE.WALL;
    collision[at(0, y)] = TILE.WALL; collision[at(MAP_W - 1, y)] = TILE.WALL;
  }
  // 칠판 (상단 중앙)
  for (let x = 4; x <= 9; x++) furniture[at(x, 1)] = TILE.BOARD;

  // 책상 + 의자 + spawn-point
  const spawnObjs = DESK_CELLS.map((d, i) => {
    furniture[at(d.x, d.y)] = TILE.DESK;
    collision[at(d.x, d.y)] = TILE.DESK; // 책상은 비통과
    furniture[at(d.x, d.y + 1)] = TILE.CHAIR;
    return { name: `desk-${i}`, x: d.x * TS, y: (d.y + 1) * TS };
  });

  const layers: TiledLayer[] = [
    { name: 'floor', type: 'tilelayer', data: floor },
    { name: 'furniture-below', type: 'tilelayer', data: furniture },
    { name: 'walls', type: 'tilelayer', data: walls },
    { name: 'collision', type: 'tilelayer', data: collision },
    { name: 'spawn-points', type: 'objectgroup', objects: spawnObjs }
  ];

  return {
    width: MAP_W, height: MAP_H, tilewidth: TS, tileheight: TS,
    layers,
    tilesets: [{ firstgid: 1, columns: TILESET_COLS, tilewidth: TS, tileheight: TS, tilecount: TILESET_COLS }]
  };
}
