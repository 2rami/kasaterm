import { useEffect, useState } from 'react';
import { fetchCharacters, fetchThemeRoster, fetchThemesList, characterPool } from './mcp';
import type { Agent } from '../store';

// 한글 캐릭터 ↔ 로마자 slug. teammate agent-name("shiroko-twgz")·에셋 파일명
// (sheet-shiroko.png)이 로마자라 UI 표시엔 역매핑이 필요하다. (SpritePortrait/
// SpriteWalk 의 SLUG 와 같은 표 — 향후 그 둘도 이 정본을 import 하도록 통합.)
export const CHARACTER_SLUG: Record<string, string> = {
  아로나: 'arona', 프라나: 'prana',
  미도리: 'midori', 모모이: 'momoi', 유즈: 'yuzu', 아리스: 'arisu',
  유우카: 'yuuka', 시로코: 'shiroko', 호시노: 'hoshino', 코하루: 'koharu',
  히마리: 'himari', 아루: 'aru',
};
export const SLUG_TO_CHARACTER: Record<string, string> = Object.fromEntries(
  Object.entries(CHARACTER_SLUG).map(([ko, slug]) => [slug, ko]),
);

// 위 표는 **번들 도트 초상이 있는 12명**뿐이다. 활성 로스터만 해도 79명이라, 표에
// 없는 학생은 프사 자리가 통째로 이니셜 상자가 됐다(세이아·코유키… 목록 절반).
// 게다가 pane 에 배정된 학생이 **다른 테마** 것일 수 있다 — 하츠네 미쿠(보컬로이드)·
// 은랑(스타레일)·우사기(치이카와)·펠리카(엔드필드)가 그랬다. 그래서 두 단계다.
//
//   1단계  활성 로스터 한 번(요청 1개). 대개 여기서 다 풀린다.
//   2단계  그래도 못 찾은 이름이 화면에 있을 때만 설치된 테마를 전부 훑는다.
//
// 2단계를 처음부터 돌리지 않는 이유는 요청 열댓 개가 첫 렌더에 딸려 나가서다.
export interface CharacterRef {
  slug: string;
  /** 활성 테마가 아닌 곳에서 왔으면 그 테마 id — /character-face 에 같이 넘겨야 한다. */
  theme?: string;
}

let refs: Record<string, CharacterRef> | null = null;
let deepDone = false;
let shallowPending: Promise<void> | null = null;
let deepPending: Promise<void> | null = null;
const subs = new Set<() => void>();
const notify = () => subs.forEach((f) => f());

function harvest(c: Awaited<ReturnType<typeof fetchCharacters>>, theme?: string) {
  const into = (refs ??= {});
  for (const m of characterPool(c)) {
    // 활성 로스터가 먼저 이긴다 — 같은 이름이 두 테마에 있으면 지금 쓰는 쪽이 정본이다.
    if (m.slug && !into[m.name]) into[m.name] = theme ? { slug: m.slug, theme } : { slug: m.slug };
  }
}

async function loadDeep() {
  const meta = await fetchThemesList();
  const ids = meta.themes.map((t) => t.id).filter((id) => id && id !== meta.active);
  const rosters = await Promise.all(ids.map((id) => fetchThemeRoster(id).catch(() => null)));
  rosters.forEach((r, i) => harvest(r, ids[i]));
  deepDone = true;
}

/** 캐릭터 이름 → slug(+테마). 하드코딩 표가 먼저고, 없으면 로스터에서 찾는다. */
export function useCharacterSlug(name: string): CharacterRef | undefined {
  // v 는 「로스터가 한 번 더 들어왔다」는 신호다. 이게 deps 에 없으면 1단계가
  // 끝나도 name·hard·found 가 그대로라 effect 가 안 깨어나고, 2단계가 영영 안 돈다.
  const [v, bump] = useState(0);
  const hard = CHARACTER_SLUG[name];
  const found = hard ? { slug: hard } : refs?.[name];
  useEffect(() => {
    if (hard || found) return;
    const fn = () => bump((n) => n + 1);
    subs.add(fn);
    if (!refs) {
      // 빈 표로 굳히지 않는다 — 실패해도 다음 이름이 다시 시도할 수 있어야 한다.
      shallowPending ??= fetchCharacters()
        .then((c) => harvest(c))
        .catch(() => { refs ??= {}; })
        .finally(notify);
    } else if (!deepDone) {
      deepPending ??= loadDeep().catch(() => { deepDone = true; }).finally(notify);
    }
    return () => { subs.delete(fn); };
  }, [name, hard, found, v]);
  return found;
}

// 게임개발부 학생 — 작업명 학생에게 외형을 폴백 배정할 순서(거노 2026-06-17 로스터).
// 미도리/모모이/유즈/아리스 + 아로나/프라나 모두 walk 시트(sheet-<slug>.png) 보유.
const BA_STUDENTS = ['미도리', '모모이', '유즈', '아리스'];

// SpriteWalk/SpritePortrait SLUG 키 — 이 이름이면 외형 에셋을 직접 갖고 있으니 그대로 둔다.
const KNOWN = new Set([
  '아로나', '프라나', '미도리', '모모이', '유즈', '아리스',
  '유우카', '시로코', '호시노', '코하루', '히마리', '아루',
]);

/** character 가 작업명(BA 학생명이 아님)인 학생에게 게임개발부 외형을
 *  순서대로 폴백 배정한다. 백엔드가 character 마커로 BA 학생명을 주면(KNOWN) 그게 우선.
 *  이름표는 character 그대로 — 외형(spriteChar)만 바꿔 교실에서 도트 스프라이트가 뜬다. */
export function assignSprites(agents: Agent[]): Agent[] {
  let i = 0;
  return agents.map((a) => {
    if (KNOWN.has(a.character)) return { ...a, spriteChar: a.character };
    const spriteChar = BA_STUDENTS[i % BA_STUDENTS.length];
    i += 1;
    return { ...a, spriteChar };
  });
}
