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
