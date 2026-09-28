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

// 모르는 이름에 다른 캐릭터의 그림을 붙이면 세션 정체성을 잘못 표시한다.
export function assignSprites(agents: Agent[]): Agent[] {
  return agents.map((a) => ({ ...a, spriteChar: a.character }));
}
