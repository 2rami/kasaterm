import { useEffect, useRef } from 'react';
import { Application } from 'pixi.js';
import { TiledMapRenderer } from '@/scene/TiledMapRenderer';
import { buildClassroomMap, DESK_CELLS, MAP_W, MAP_H, TS } from '@/scene/classroomMap';
import { makePlaceholderTileset } from '@/scene/placeholderTileset';
import { ClassroomCharacter, type CharStatus } from '@/scene/ClassroomCharacter';
import { useStore } from '@/store';
import { focusPane } from '@/lib/mcp';
import { accentByName, type AccentColorName } from '@/design/tokens';

// 샬레 교실 — pixi 로 맵을 그리고, board 의 학생들을 책상에 앉혀 상태대로 움직인다.
// 배치: isGod(아로나) 먼저, 그 다음 board 순서로 desk-0..N. 클릭 → 그 pane 포커스.
export function ClassroomView() {
  const hostRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let app: Application | undefined;
    let unsub: (() => void) | undefined;
    let destroyed = false;
    // app.init() 은 async — StrictMode(dev)가 init 완료 전에 cleanup 의 destroy 를
    // 부르면 pixi 내부 resize plugin 미설정 상태라 `_cancelResize` 크래시. ready 로
    // 게이트해 init 끝난 인스턴스만 destroy 한다.
    let ready = false;
    const chars = new Map<string, ClassroomCharacter>();

    void (async () => {
      app = new Application();
      await app.init({ background: 0xfcfaf0, width: MAP_W * TS, height: MAP_H * TS, antialias: false });
      if (destroyed || !hostRef.current) { app.destroy(true); return; }
      ready = true;
      hostRef.current.appendChild(app.canvas);

      const renderer = new TiledMapRenderer(buildClassroomMap(), [makePlaceholderTileset()]);
      app.stage.addChild(renderer.getContainer());
      const charLayer = renderer.getCharacterContainer();

      const sync = () => {
        const agents = [...useStore.getState().agents].sort((a, b) => Number(b.isGod) - Number(a.isGod));
        agents.forEach((a, i) => {
          if (i >= DESK_CELLS.length) return;
          let c = chars.get(a.id);
          if (!c) {
            const color = accentByName[a.accent as AccentColorName] ?? 0xa899b5;
            c = new ClassroomCharacter(a.id, a.name, color);
            c.view.on('pointertap', () => { void focusPane(a.id); });
            chars.set(a.id, c);
            charLayer.addChild(c.view);
          }
          const desk = DESK_CELLS[i];
          // 의자 칸(책상 위쪽, 칠판 향함) 중앙에 앉힌다.
          c.setPos((desk.x + 0.5) * TS, (desk.y - 1 + 0.5) * TS);
          c.setStatus(a.status as CharStatus);
        });
        // board 에서 사라진 학생 정리
        for (const [id, c] of chars) {
          if (!agents.some((a) => a.id === id)) {
            c.destroy();
            chars.delete(id);
          }
        }
      };

      sync();
      unsub = useStore.subscribe(sync);
      app.ticker.add((ticker) => {
        chars.forEach((c) => c.tick(ticker.deltaTime));
      });
    })();

    return () => {
      destroyed = true;
      unsub?.();
      if (ready) app?.destroy(true);
    };
  }, []);

  return (
    <div
      ref={hostRef}
      style={{
        display: 'inline-block',
        imageRendering: 'pixelated',
        boxShadow: 'var(--cth-panel-border)',
        lineHeight: 0
      }}
    />
  );
}
