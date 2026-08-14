/// 설정 화면의 버튼 하나 = 네이티브 액션 하나. 라우트가 액션마다 갈리지 않고
/// 이름으로 갈리는 이유는 네이티브가 이미 액션 enum 하나로 모여 있어서다 —
/// 1:1 이라 구현이 둘로 갈릴 수가 없다.
export type SettingsActionResult = {
  ok: boolean;
  /// 네이티브가 토스트로 띄우려던 문구. 웹뷰 창에선 그 토스트가 안 보이므로
  /// 이걸 화면에 옮겨 놔야 「테마를 바꿨어요 — 새로 여는 pane 부터」 같은
  /// 중요한 단서가 사라지지 않는다.
  message?: string | null;
  /// 요청 자체가 거부된 이유(모르는 액션·잘못된 이름 등).
  error?: string;
};

import type { SettingsValues } from './types';

/// 캐릭터 탭 밖의 설정 값 전부. 탭마다 따로 묻지 않는 이유는 액션과 같다 — 값의
/// 정본이 앱 한 곳이라 조회도 한 번이면 되고, 탭이 늘어도 여기 손댈 게 없다.
export async function fetchValues(): Promise<SettingsValues> {
  const res = await fetch('/settings/values');
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  const v = (await res.json()) as SettingsValues | null;
  // 설정 개념이 없는 백엔드는 오류가 아니라 null 을 답한다 — 그걸 그대로 화면에
  // 넘기면 탭마다 undefined 를 읽다 터지므로 여기서 한 번에 가른다.
  if (!v) throw new Error('이 인스턴스는 설정 값을 안 알려 줘요');
  return v;
}

export async function postAction(
  action: string,
  args?: { id?: string; label?: string }
): Promise<SettingsActionResult> {
  const res = await fetch('/settings/action', {
    method: 'POST',
    // text/plain 은 CORS simple request 라 preflight(OPTIONS)가 안 뜬다.
    // application/json 이면 preflight 가 붙고, post() 만 걸린 라우트는 OPTIONS 에
    // 405 를 답해 요청이 조용히 죽는다.
    headers: { 'Content-Type': 'text/plain' },
    body: JSON.stringify({ action, ...args }),
  });
  return (await res.json()) as SettingsActionResult;
}
