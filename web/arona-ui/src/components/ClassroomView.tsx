import { useEffect, useRef } from 'react';
import { Application } from 'pixi.js';
import { TiledMapRenderer } from '@/scene/TiledMapRenderer';
import { buildClassroomMap, DESK_CELLS, MAP_W, MAP_H, TS } from '@/scene/classroomMap';
import { makePlaceholderTileset } from '@/scene/placeholderTileset';
import { loadTilesetTexture, loadCharacterSprites } from '@/scene/loadAssets';
import { ClassroomCharacter, type CharStatus } from '@/scene/ClassroomCharacter';
import { useStore, type Agent } from '@/store';
import { focusPane } from '@/lib/mcp';
import { accentByName, type AccentColorName } from '@/design/tokens';

// 도트칩 이니셜 충돌 해소: 같은 첫글자를 가진 캐릭터가 둘 이상이면 그 캐릭터들만
// 2글자(아로나·아리스 → 아로·아리), 첫글자가 유일하면 1글자. 이름 집합 전체를
// 보고 결정해야 해서 ClassroomView(전 캐릭터 조망) 레벨에서 계산한다.
function initialResolver(names: string[]): (n: string) => string {
  const firstCount = new Map<string, number>();
  for (const n of names) {
    const f = (n || '?').trim().charAt(0) || '?';
    firstCount.set(f, (firstCount.get(f) ?? 0) + 1);
  }
  return (n: string) => {
    const t = (n || '?').trim();
    const f = t.charAt(0) || '?';
    return (firstCount.get(f) ?? 0) > 1 ? t.slice(0, 2) : f;
  };
}

// 생각 구름 첫마디 — 마지막 답변/질문의 첫 문장만(줄바꿈·문장부호 경계). 대기·완료
// 시 길게 늘어진 last_reply 를 학생 머리 위에 한 줄로 압축한다.
function firstLine(s?: string): string {
  if (!s) return '';
  const head = s.trim().split(/[\n。.!?！？]/)[0].trim();
  return head.length > 40 ? head.slice(0, 39).trimEnd() + '…' : head;
}

// intent 의 긴 절대경로는 노이즈 — 마지막 세그먼트만 남겨 "…/folder" 로 축약.
function shortenAction(s?: string): string {
  if (!s) return '';
  return s.replace(/\/\S*\/([^\s/]+)/g, '…/$1');
}

// 상태별 구름 텍스트(아로나 ① 우선순위): working = 행동(intent, 빈값이면 "…"
// 사고중), waiting/blocked = 직전 질문/제안(뭘 기다리는지 사용자가 봐야 함),
// idle = 마지막 한마디 or 비표시.
function thoughtFor(a: Agent): string {
  switch (a.status) {
    // 선생님 ①: 말풍선엔 "무슨 tool 쓰는지"(Bash/Read/Edit)만 — 명령어/인자 없이.
    // 서브에이전트 돌리는 중이면 "· 서브 N" 을 덧댄다(선생님 ④).
    case 'working': {
      const tool = a.currentTool || shortenAction(a.action);
      const n = a.subagents?.length ?? 0;
      return n > 0 ? `${tool} · 서브 ${n}` : tool;
    }
    case 'waiting':
    case 'blocked': return firstLine(a.lastReply) || shortenAction(a.action);
    case 'idle': return firstLine(a.lastReply);
    default: return '';
  }
}

// 샬레 교실 — pixi 로 맵을 그리고, board 의 학생들을 책상에 앉혀 상태대로 움직인다.
// 배치: isGod(아로나) 먼저, 그 다음 board 순서로 desk-0..N. 클릭 → 터미널 뷰 패널.
export interface ClassroomViewProps {
  onSelect?: (surfaceId: string, title: string) => void;
}

export function ClassroomView({ onSelect }: ClassroomViewProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  // pixi useEffect 는 한 번만 돌아 prop 을 클로저로 가두므로, 최신 onSelect 를
  // ref 로 흘려 stale 콜백을 피한다.
  const onSelectRef = useRef(onSelect);
  onSelectRef.current = onSelect;

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
      await app.init({ background: 0xeaf3fc, width: MAP_W * TS, height: MAP_H * TS, antialias: false });
      if (destroyed || !hostRef.current) { app.destroy(true); return; }
      ready = true;
      hostRef.current.appendChild(app.canvas);

      // 아트 슬롯: 타일셋 PNG 있으면 그걸로, 없으면 placeholder. 캐릭터 시트는
      // 이름→텍스처 맵으로 미리 로드(없는 캐릭터는 색블록 폴백). 둘 다 폴백 안전.
      const [tileTex, spriteMap] = await Promise.all([loadTilesetTexture(), loadCharacterSprites()]);
      if (destroyed) { app.destroy(true); return; }

      const renderer = new TiledMapRenderer(buildClassroomMap(), [tileTex ?? makePlaceholderTileset()]);
      app.stage.addChild(renderer.getContainer());
      const charLayer = renderer.getCharacterContainer();
      charLayer.sortableChildren = true; // thought.zIndex(100000)가 다른 학생 위로

      const sync = () => {
        const agents = [...useStore.getState().agents].sort((a, b) => Number(b.isGod) - Number(a.isGod));
        const initialOf = initialResolver(agents.map((a) => a.character || a.name));
        agents.forEach((a, i) => {
          if (i >= DESK_CELLS.length) return;
          let c = chars.get(a.id);
          if (!c) {
            const color = accentByName[a.accent as AccentColorName] ?? 0xa899b5;
            c = new ClassroomCharacter(a.id, a.name, color, spriteMap.get(a.character || a.name));
            const aid = a.id;
            c.view.on('pointertap', () => {
              const name = useStore.getState().agents.find((x) => x.id === aid)?.character || aid;
              if (onSelectRef.current) onSelectRef.current(aid, name);
              else void focusPane(aid);
            });
            c.setBounds(MAP_W * TS, MAP_H * TS); // 구름이 맵 밖으로 안 넘치게
            chars.set(a.id, c);
            charLayer.addChild(c.view);
            charLayer.addChild(c.thought.container); // 캐릭터 위 레이어(절대좌표)
          }
          const desk = DESK_CELLS[i];
          // 의자 칸(책상 위쪽, 칠판 향함) 중앙에 앉힌다.
          c.setPos((desk.x + 0.5) * TS, (desk.y - 1 + 0.5) * TS);
          c.setInitial(initialOf(a.character || a.name));
          c.setStatus(a.status as CharStatus);
          c.setThought(thoughtFor(a));
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
        borderRadius: 16,
        overflow: 'hidden',
        boxShadow: '0 6px 20px rgba(21, 41, 74, 0.10), inset 0 0 0 1px var(--cth-cream-200)',
        lineHeight: 0
      }}
    />
  );
}
