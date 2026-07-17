import type { Agent } from '../store';

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
