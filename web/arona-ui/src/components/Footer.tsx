// Footer props 인터페이스 — 시로코 정의, 아리스가 내부를 채운다.
// 목업 하단 바: 학생 관리 | 인연 레벨 보너스 | (spacer) | 재화(크레딧/골드) | 새 의뢰 작성 CTA
export interface FooterProps {
  onManage?: () => void;       // "학생 관리" 버튼
  onNewRequest?: () => void;   // "+ 새 의뢰 작성" CTA
  credits?: number;
  gold?: number;
  bondBonus?: string;          // "전체 학년 상위 +12%" 등 텍스트
}

// stub — 아리스가 내부 구현을 채울 때까지 빈 fragment.
export function Footer(_props: FooterProps) {
  return null;
}
