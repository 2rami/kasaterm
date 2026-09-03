// 학생 활동 가시화 공용 — 빌드 명령 인식 + 상태 아이콘(SVG, 이모지 금지).
// board.action(=intent "Bash <cmd>")에서 빌드를 일반 Bash 와 구분한다.

const BUILD_RE = /\b(cargo (build|check|test|run|clippy)|npm (run )?(build|ci|install)|pnpm \w*(build|install)|yarn( (build|install))?|make\b|tsc\b|vite build|webpack|next build|go build|mvn\b|gradle|nohup .*relaunch)/i;

export function isBuildCmd(s?: string): boolean {
  return !!s && BUILD_RE.test(s);
}

export const BUILD_COLOR = 'var(--cth-attention-text-surface)';
export const BUILD_COLOR_BG = 'var(--cth-attention-text-bg)';

// 톱니바퀴 — 빌드/컴파일 진행. currentColor 로 색 상속.
export function GearIcon({ size = 12 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" style={{ display: 'block' }}>
      <path d="M8 1.4l1 1.7 1.9-.4.5 1.9 1.7 1-1 1.7 1 1.7-1.7 1-.5 1.9-1.9-.4-1 1.7-1-1.7-1.9.4-.5-1.9-1.7-1 1-1.7-1-1.7 1.7-1 .5-1.9 1.9.4 1-1.7Z"
        fill="currentColor" stroke="currentColor" strokeWidth="0.5" strokeLinejoin="round" />
      <circle cx="8" cy="8" r="2.4" fill="var(--cth-cream-50)" />
    </svg>
  );
}

// 회전 화살표 — 백그라운드 지속 실행.
export function SpinIcon({ size = 12 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" style={{ display: 'block' }}>
      <path d="M13 8a5 5 0 1 1-1.7-3.8" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" />
      <path d="M13 2.5v2.7h-2.7" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

// 갈래 — 서브에이전트.
export function ForkIcon({ size = 12 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" style={{ display: 'block' }}>
      <circle cx="4" cy="3.5" r="1.8" fill="none" stroke="currentColor" strokeWidth="1.4" />
      <circle cx="4" cy="12.5" r="1.8" fill="none" stroke="currentColor" strokeWidth="1.4" />
      <circle cx="12" cy="8" r="1.8" fill="none" stroke="currentColor" strokeWidth="1.4" />
      <path d="M5.6 4.4 10.4 7M5.6 11.6 10.4 9" fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" />
    </svg>
  );
}
