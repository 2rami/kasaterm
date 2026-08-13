/// `GET /design-tokens` 응답. 색은 CSS hex 문자열이라 그대로 var() 에 꽂힌다
/// (알파가 불투명하지 않으면 8자리로 온다 — 테두리가 그렇다).
export type DesignTokens = {
  theme: string;
  accent_name: string;
  palette: Record<string, string>;
  ansi: string[];
  shape: {
    radius_sm: number;
    radius_md: number;
    border_w: number;
    shadow_offset: number;
    roundness: number;
    pixel_chrome: boolean;
  };
};

/// `GET /settings/characters` 응답.
export type SettingsCharacters = {
  /// 활성 테마 폴더 이름. 번들을 쓰는 중이면 빈 문자열이다.
  active_theme: string;
  /// 캐릭터 말투로 대답하는지(`claude_persona`). 새로 여는 pane 부터 적용된다.
  persona_enabled: boolean;
  themes: ThemeCard[];
  roster: Character[];
};

export type ThemeCard = {
  /// 번들은 빈 문자열 — 폴더가 없다.
  id: string;
  label: string;
  count: number;
  /// 미리보기 얼굴의 slug. 경로가 아니라 slug 라서 프사는 `/character-face` 로
  /// 다시 물어본다.
  faces: string[];
};

export type Character = {
  name: string;
  slug: string;
  school: string;
  header_color: string;
  persona: string;
};

/// 프사 URL. `theme` 을 주면 그 테마 폴더의 그림(카드 미리보기), 안 주면 활성
/// 폴더 → 번들 순으로 찾는다.
export function faceUrl(slug: string, theme?: string): string {
  const q = new URLSearchParams({ slug });
  if (theme) q.set('theme', theme);
  return `/character-face?${q}`;
}
