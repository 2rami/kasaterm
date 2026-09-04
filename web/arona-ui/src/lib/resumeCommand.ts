export type Harness = 'claude' | 'codex' | 'agy';

/** 오프라인 세션을 현재 터미널에서 이어가는 명령.
 * Codex의 시작 업데이트 화면은 resume 입력을 버리고 설치 뒤 종료하므로, 앱이
 * 자동 생성하는 resume에만 검사를 끈다. bare Codex와 사용자 설정은 건드리지 않는다. */
export function resumeCommand(id: string, harness?: Harness): string {
  switch (harness) {
    case 'codex': return `codex resume ${id} -c check_for_update_on_startup=false`;
    case 'agy': return `agy --conversation ${id}`;
    default: return `claude --resume ${id}`;
  }
}
