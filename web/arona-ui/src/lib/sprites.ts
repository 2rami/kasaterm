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
