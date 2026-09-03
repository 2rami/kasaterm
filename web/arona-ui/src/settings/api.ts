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
  /// 위 두 문구의 **언어 없는 이름**. 있으면 화면이 사전에서 그 나라 말로 만들고,
  /// 없으면 위의 한국어 문구를 그대로 쓴다(서버 쪽 코드화가 진행 중이라 둘 다
  /// 올 수 있다 — 노아와 합의, 2026-08-15).
  error_code?: string | null;
  error_args?: Record<string, string | number>;
  message_code?: string | null;
  message_args?: Record<string, string | number>;
  confirm?: AccountSwitchConfirmation;
};

export type AccountSwitchConfirmation = {
  provider: 'claude' | 'codex';
  id: string;
  nonce: string;
  title: string;
  lines: string[];
  dangerous: boolean;
};

import type { Character, OnboardingState, SettingsValues } from './types';

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

export async function fetchOnboardingState(): Promise<OnboardingState> {
  const res = await fetch('/onboarding/state');
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return (await res.json()) as OnboardingState;
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
  const out = (await res.json()) as SettingsActionResult;
  if (out.ok && !out.error && TOKEN_ACTIONS.has(action)) {
    void window.__ktRefreshTokens?.();
  }
  return out;
}

const TOKEN_ACTIONS = new Set([
  'terminal-profile-import',
  'theme-mode',
  'theme-system-light',
  'theme-system-dark',
  'start-custom-theme',
  'delete-custom-theme',
  'reset-custom-theme',
  'palette-hex',
  'accent',
  'shape',
  'min-contrast',
  'select-theme',
]);

/// `GET /theme-roster?id=<테마id|__base>` — 그 테마 하나의 명단.
///
/// `/settings/characters` 가 **활성 테마 명단만** 싣기 때문에 필요하다. 11테마
/// 300명을 매번 함께 실으면 설정을 열 때마다 그게 다 오가므로, 접힌 묶음을
/// 펼치는 그 순간에만 받는다.
///
/// 응답은 로스터 원본 형태(`leader`/`leaders`/`members`)라 여기서 한 줄로 편다 —
/// 리더 특권은 2026-07-13 에 폐기됐고 배정도 이 합집합에서 나온다.
export async function fetchThemeRoster(id: string): Promise<Character[]> {
  const res = await fetch(`/theme-roster?id=${encodeURIComponent(id)}`);
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  const v = (await res.json()) as {
    leader?: Character;
    leaders?: Character[];
    members?: Character[];
  };
  const pool = [...(v.leaders ?? (v.leader ? [v.leader] : [])), ...(v.members ?? [])];
  const seen = new Set<string>();
  // 이름이 겹치는 항목(리더가 members 에도 있는 로스터)을 한 번만 남긴다 —
  // 배정 쪽 `assignable_names` 도 같은 규칙이라 화면과 실제가 어긋나지 않는다.
  return pool.filter((m) => m?.name && !seen.has(m.name) && seen.add(m.name));
}
