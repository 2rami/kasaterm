import { Texture } from 'pixi.js';
import { TILE, TILESET_COLS, TS } from './classroomMap';

// placeholder 타일셋 — canvas 로 단색 + 픽셀 보더 타일 시트를 런타임 생성한다.
// LimeZu 픽셀 하나도 안 쓴다. 거노가 진짜 타일셋 PNG 를 채우면 이 함수를 Texture
// 로더로 갈아끼우면 된다(TiledMapRenderer 는 art-agnostic 이라 무수정).
// SCHALE 블루 톤 placeholder — 화이트 책상 + 연하늘 바닥 + 네이비 스크린.
// (거노 교실 배경 일러 PNG 오면 이 타일맵은 그 위 캐릭터 오버레이로 물러난다.)
const FILL: Record<number, { fill: string; border: string }> = {
  [TILE.FLOOR]: { fill: '#E8F1FB', border: '#D6E6F5' },
  [TILE.WALL]:  { fill: '#C3D8EE', border: '#8AA6C8' },
  [TILE.DESK]:  { fill: '#FFFFFF', border: '#B9D3ED' },
  [TILE.BOARD]: { fill: '#16294A', border: '#15294A' },
  [TILE.CHAIR]: { fill: '#9DB8D8', border: '#4A638F' }
};

export function makePlaceholderTileset(): Texture {
  const canvas = document.createElement('canvas');
  canvas.width = TILESET_COLS * TS;
  canvas.height = TS;
  const ctx = canvas.getContext('2d')!;
  ctx.imageSmoothingEnabled = false;

  // localId 0..N → gid 1..N (firstgid=1). 색 지정 없는 칸은 투명.
  for (let gid = 1; gid <= TILESET_COLS; gid++) {
    const c = FILL[gid];
    if (!c) continue;
    const x = (gid - 1) * TS;
    ctx.fillStyle = c.fill;
    ctx.fillRect(x, 0, TS, TS);
    ctx.strokeStyle = c.border;
    ctx.lineWidth = 2;
    ctx.strokeRect(x + 1, 1, TS - 2, TS - 2);
    // 칠판엔 흰 분필선 한 줄(교실 느낌)
    if (gid === TILE.BOARD) {
      ctx.strokeStyle = '#FCFAF0';
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(x + 5, 11); ctx.lineTo(x + TS - 6, 11);
      ctx.moveTo(x + 5, 18); ctx.lineTo(x + TS - 12, 18);
      ctx.stroke();
    }
  }
  return Texture.from(canvas);
}
