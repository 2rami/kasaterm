import { Assets, type Texture } from 'pixi.js';
import { fetchCharacters } from '@/lib/mcp';

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

/** 캐릭터명 → 스프라이트 시트 텍스처 맵. characters.json 의 sprite 필드 경로를
 *  (상대면 ROOT 기준) 로드. 필드 없거나 로드 실패한 캐릭터는 맵에서 빠져 색블록 폴백. */
export async function loadCharacterSprites(): Promise<Map<string, Texture>> {
  const out = new Map<string, Texture>();
  const chars = await fetchCharacters();
  if (!chars) return out;
  const defs = [chars.leader, ...(chars.members ?? [])].filter(Boolean);
  await Promise.all(
    defs.map(async (d) => {
      if (!d?.sprite) return;
      const url = /^https?:|^\//.test(d.sprite) ? d.sprite : `${ROOT}${d.sprite}`;
      const tex = await loadOrNull(url);
      if (tex) out.set(d.name, tex);
    })
  );
  return out;
}
