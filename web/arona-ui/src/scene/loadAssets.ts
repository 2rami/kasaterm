import { Assets, type Texture } from 'pixi.js';

// 아트 교체 슬롯: 정적 경로에 PNG 가 있으면 로드, 없으면 null → 호출부가 placeholder
// /색블록으로 폴백. 경로는 arona-ui 서빙 루트(BASE_URL) 기준(dist=/arona-ui/).
const ROOT = import.meta.env.BASE_URL || '/';

async function loadOrNull(url: string): Promise<Texture | null> {
  try {
    const tex = (await Assets.load(url)) as Texture;
    return tex ?? null;
  } catch {
    return null; // 파일 없음/디코드 실패 → placeholder 폴백
  }
}

/** 교실 타일셋 PNG(ROOT/tileset.png, 256×32·8칸). 없으면 null → placeholderTileset. */
export function loadTilesetTexture(): Promise<Texture | null> {
  return loadOrNull(`${ROOT}tileset.png`);
}

// 캐릭터명 → ASCII 슬러그(SpritePortrait 와 동일 — 한글 파일명은 Rust 정적서빙
// percent-decode 404 회피). char-<slug>.png 단일 전신 누끼를 로드한다.
const CHAR_SLUG: Record<string, string> = {
  '아로나': 'arona', '유우카': 'yuuka', '시로코': 'shiroko',
  '아리스': 'arisu', '호시노': 'hoshino', '코하루': 'koharu',
};

/** 캐릭터명 → 전신 PNG 텍스처 맵(`assets/char-<slug>.png`). 파일 없는 캐릭터는
 *  맵에서 빠져 색블록 폴백. ClassroomCharacter 가 Sprite 로 그린다. */
export async function loadCharacterSprites(): Promise<Map<string, Texture>> {
  const out = new Map<string, Texture>();
  await Promise.all(
    Object.entries(CHAR_SLUG).map(async ([name, slug]) => {
      const tex = await loadOrNull(`${ROOT}assets/char-${slug}.png`);
      if (tex) out.set(name, tex);
    })
  );
  return out;
}
