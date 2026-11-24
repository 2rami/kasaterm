import { useEffect, useRef } from 'react';
import { Application, Container, Texture } from 'pixi.js';
import { TiledMapRenderer, type TiledMap } from './TiledMapRenderer';

const ROOT = import.meta.env.BASE_URL || '/';

// office.tmj 의 외부 .tsx 타일셋 참조(a5/interiors)를 렌더러가 필요한 inline 메타로
// 패치 — munder OfficeFloor.resolveMap 이식. 순서 = 로드하는 텍스처 순서.
function resolveMap(m: TiledMap): TiledMap {
  return {
    ...m,
    tilesets: [
      m.tilesets[0], // office-tileset.png (embedded, firstgid 1, cols 16)
      { firstgid: 513, columns: 16, tilewidth: 16, tileheight: 16, tilecount: 512 } as TiledMap['tilesets'][number],
      { firstgid: 1025, columns: 16, tilewidth: 16, tileheight: 16, tilecount: 1424 } as TiledMap['tilesets'][number],
    ],
  };
}

// <img> 로 텍스처 로드 + nearest(픽셀 또렷). Pixi Assets.load 대신(확장자 없는 URL 대응).
function loadTexture(url: string): Promise<Texture> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => { const t = Texture.from(img); t.source.scaleMode = 'nearest'; resolve(t); };
    img.onerror = () => reject(new Error('texture load failed: ' + url));
    img.src = url;
  });
}

// 포켓몬 Gen4식 타일맵 룸(munder office 이식, Phase 1 = 정적 룸만). PixiJS 로 Tiled
// 맵(floor/walls/furniture 레이어)을 렌더하고 컨테이너에 맞춰 스케일. 캐릭터/이동은 후속.
export function TileRoom() {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const appRef = useRef<Application | null>(null);
  const mountRef = useRef(0);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const mountId = ++mountRef.current;
    const app = new Application();
    appRef.current = app;

    const init = async () => {
      await app.init({
        background: 0xeef3fb, antialias: false, roundPixels: true,
        resolution: Math.max(window.devicePixelRatio || 1, 2), autoDensity: true,
        width: host.clientWidth || 800, height: host.clientHeight || 520,
      });
      if (mountRef.current !== mountId) { try { app.destroy(true); } catch { /* noop */ } return; }
      while (host.firstChild) host.removeChild(host.firstChild);
      host.appendChild(app.canvas);

      const mapData = (await fetch(`${ROOT}assets/maps/office.tmj`).then((r) => r.json())) as TiledMap;
      const [t0, t1, t2] = await Promise.all([
        loadTexture(`${ROOT}assets/tilesets/office-tileset.png`),
        loadTexture(`${ROOT}assets/tilesets/a5-office-floors-walls.png`),
        loadTexture(`${ROOT}assets/tilesets/interiors.png`),
      ]);
      if (mountRef.current !== mountId) { try { app.destroy(true); } catch { /* noop */ } return; }

      const world = new Container();
      app.stage.addChild(world);
      const renderer = new TiledMapRenderer(resolveMap(mapData), [t0, t1, t2]);
      world.addChild(renderer.getContainer());

      const mapW = renderer.width * renderer.tileSize;
      const mapH = renderer.height * renderer.tileSize;
      const fit = () => {
        const sw = app.screen.width, sh = app.screen.height;
        const s = Math.max(0.01, Math.min(sw / mapW, sh / mapH));
        world.scale.set(s);
        world.position.set((sw - mapW * s) / 2, (sh - mapH * s) / 2);
      };
      fit();
      const ro = new ResizeObserver(() => {
        if (mountRef.current !== mountId) return;
        app.renderer.resize(host.clientWidth, host.clientHeight);
        fit();
      });
      ro.observe(host);
      (app as unknown as { __ro?: ResizeObserver }).__ro = ro;
    };
    void init();

    return () => {
      mountRef.current++;
      const a = appRef.current;
      if (a) {
        (a as unknown as { __ro?: ResizeObserver }).__ro?.disconnect();
        try { a.destroy(true); } catch { /* noop */ }
        appRef.current = null;
      }
    };
  }, []);

  return (
    <div ref={hostRef} style={{
      width: '100%', aspectRatio: '34 / 22', maxWidth: 960, margin: '0 auto',
      borderRadius: 16, overflow: 'hidden', imageRendering: 'pixelated',
      boxShadow: '0 6px 20px rgba(21,41,74,0.12), inset 0 0 0 1px var(--cth-cream-200)',
    }} />
  );
}
