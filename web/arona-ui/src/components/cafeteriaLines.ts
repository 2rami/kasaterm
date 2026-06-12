// munder cafeteriaLines 이식(한국어 SCHALE 버전) — idle 학생들이 카페에 모이면
// 둘이 짝지어 주고받는 잡담. 각 교환은 화자 교대(짝수=A, 홀수=B).

const EXCHANGES: string[][] = [
  ['선생님 또 야근하시려나…', '그러게, 커피 한 잔 더 타둘까?', '히힛 좋아요'],
  ['이번 빌드 통과했어?', '응 방금 초록불 떴어', '역시 빠르다'],
  ['나 잠깐 쉬는 중', '나도 컨텍스트 꽉 찼었어', '리셋되니까 살 것 같다'],
  ['여기 소파 자리 좋다', '맞아 창밖도 보이고', '선생님 오면 자랑해야지'],
  ['그 버그 결국 잡았어?', '응 한 줄이더라…', '항상 한 줄이지'],
  ['오늘 의뢰 몇 개 남았지?', '두 개? 금방 끝나', '끝나면 같이 쉬자'],
  ['커밋 메시지 또 길게 썼네', '선생님이 좋아하셔', '그럼 됐지 뭐'],
  ['서브에이전트 잘 돌아가?', '응 셋이 나눠서 하니까 빨라', '분업 최고'],
  ['배경 새로 바뀐 거 봤어?', '응 책상까지 따로 생겼더라', '이제 안 부딪혀서 좋아'],
];

const SOLO: string[] = [
  '커피 한 잔의 여유…',
  '잠깐 숨 돌리는 중',
  '창밖 날씨 좋다',
  '다음 의뢰 뭐려나',
  '선생님 어디 가셨지?',
];

export interface ChatLine { who: 'a' | 'b'; text: string; }

// seed(인덱스)로 교환 하나를 골라 화자 교대로 매핑.
export function pickExchange(seed: number): ChatLine[] {
  const ex = EXCHANGES[Math.abs(seed) % EXCHANGES.length];
  return ex.map((text, i) => ({ who: i % 2 === 0 ? 'a' : 'b', text }));
}

export function pickSolo(seed: number): string {
  return SOLO[Math.abs(seed) % SOLO.length];
}
