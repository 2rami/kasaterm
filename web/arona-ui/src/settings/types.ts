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

/// `GET /settings/values` 응답 — 캐릭터 탭 밖의 설정 값 전부.
///
/// 카테고리마다 하위 객체 하나씩이라 탭이 늘어도 라우트가 늘지 않는다. 값의 정본은
/// 앱의 메모리다(파일이 아니다) — UI 배율처럼 저장되지 않는 값이 섞여 있다.
export type SettingsValues = {
  general: GeneralValues;
  appearance: AppearanceValues;
  shell: { shell: string };
  claude: ClaudeValues;
  feedback: { diag: string; diag_on: boolean };
};

export type GeneralValues = {
  /// `"last"` · `"home"` 이거나, 그 밖이면 사용자가 고른 경로 그 자체다.
  cwd_mode: string;
  /// `"builtin"` · `"app"` · `"terminal"`. `"system"` 은 `"app"` 의 옛 저장값.
  file_open_mode: string;
  /// 빈 문자열 = OS 연결 프로그램.
  file_open_app: string;
  file_open_cmd: string;
  /// 설치된 것만 온다 — 목록에 없는 앱을 쓰는 사람의 탈출구가 빈 문자열이다.
  apps: { name: string; short: string }[];
  file_tree_default: boolean;
  footer_default: boolean;
  autosave_ms: number;
  tabs_on_top: boolean;
  cursor_shape: string;
  cursor_thickness: number;
  mouse_cursor: string;
  wheel_gain_x100: number;
};

export type AppearanceValues = {
  theme: string;
  themes: ThemePreset[];
  /// system 모드가 밝기별로 입을 테마 — 프리셋 키 또는 `"custom"`.
  theme_system_light: string;
  theme_system_dark: string;
  has_custom_theme: boolean;
  palette_keys: string[];
  /// UI 색(palette_keys 순서) 뒤에 ANSI 16색이 이어 붙는다.
  palette_hex: string[];
  accent: string;
  accents: { name: string; hex: string }[];
  shape: string;
  shapes: ShapePreset[];
  min_contrast: number;
  /// `sample` 은 그 임계로 끌어올린 글자색. 대비 계산을 웹으로 옮기면 두 화면의
  /// 판정이 갈리므로 결과만 받는다.
  contrasts: { label: string; value: number; sample: string }[];
  font_size: number;
  font_size_default: number;
  ui_zoom: number;
};

/// 형태 프리셋 한 벌. 카드가 **자기 실루엣으로** 그려져야 고르기 전에 형태가
/// 보이므로, 라벨만이 아니라 그리기에 필요한 값이 함께 온다.
export type ShapePreset = {
  key: string;
  label: string;
  radius_md: number;
  border_w: number;
  shadow_offset: number;
  /// 점·캡슐이 원에서 사각으로 얼마나 기우는지. 1 = 정원, 0 = 사각.
  roundness: number;
};

/// 테마 카드 한 장의 미리보기 재료. 색은 CSS hex 라 그대로 style 에 꽂힌다.
export type ThemePreset = {
  key: string;
  label: string;
  bg: string;
  text: string;
  dim: string;
  /// ANSI 1..6 (red green yellow blue magenta cyan) — 카드의 색 점.
  ansi: string[];
};

export type ClaudeValues = {
  shim_inject: boolean;
  persona: boolean;
  accounts: AccountRow[];
  account: string;
  codex_accounts: AccountRow[];
  codex_account: string;
  autoswitch: boolean;
  autoswitch_pct: number;
  model: string;
  effort: string;
  extra: string;
};

/// 계정 한 행. 이름·부제는 서버가 조립해서 준다 — 그 규칙(라벨이 없으면 이메일,
/// 팀 조직이면 이어 붙이기)이 네이티브 폼에 있어서, 여기서 다시 짜면 두 화면이
/// 같은 계정을 다르게 부른다.
export type AccountRow = {
  id: string;
  label: string;
  name: string;
  sub: string;
  /// 부제를 어떤 색으로 읽을지 — `danger`(로그인 필요) · `mute` · `faint`(확인 중).
  sub_kind: string;
  /// false = 첫 행(지금 로그인). 지울 것도 이름 붙일 것도 없다.
  slot: boolean;
  /// 위 `name`·`sub` 의 **언어 없는 이름**. 있으면 화면이 사전에서 그 나라 말로
  /// 만들고, `null` 이면 그 자리는 옮길 말이 아니라 **데이터**다 — 사용자가 붙인
  /// 별명, 이메일, 팀 조직명. 부제에 조직명이 이어 붙는 경우도 코드가 빠진다
  /// (코드로 갈면 조직이 사라진다). 노아와 합의, 2026-08-15.
  name_code?: string | null;
  name_args?: { n: number } | null;
  sub_code?: string | null;
};

/// 프사 URL. `theme` 을 주면 그 테마 폴더의 그림(카드 미리보기), 안 주면 활성
/// 폴더 → 번들 순으로 찾는다.
export function faceUrl(slug: string, theme?: string): string {
  const q = new URLSearchParams({ slug });
  if (theme) q.set('theme', theme);
  return `/character-face?${q}`;
}
