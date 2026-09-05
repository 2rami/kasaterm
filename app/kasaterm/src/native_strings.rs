//! 네이티브 설정·온보딩의 화면 문구.
//!
//! 렌더러는 파일을 읽지 않고 설정 캐시에 든 언어를 프레임 시작에 한 번 건넨다.
//! 두 화면의 낮은 단계 텍스트 함수가 모두 이 계층을 지나므로 새 문구가 한쪽 언어에만
//! 남지 않는다. 동적 데이터(테마명·계정 이메일·경로)는 번역하지 않는다.

use std::borrow::Cow;
use std::cell::Cell;

thread_local! {
    static ENGLISH: Cell<bool> = const { Cell::new(false) };
}

pub(crate) fn set_language(language: &str) {
    ENGLISH.with(|english| english.set(language == "en"));
}

pub(crate) fn text(value: &str) -> Cow<'_, str> {
    if !ENGLISH.with(Cell::get) {
        return Cow::Borrowed(value);
    }
    if let Some(translated) = english(value) {
        return Cow::Borrowed(translated);
    }
    if let Some(name) = value.strip_suffix(" 명단") {
        return Cow::Owned(format!("{name} roster"));
    }
    if let Some(name) = value.strip_suffix(" 이름") {
        return Cow::Owned(format!("{name} name"));
    }
    if let Some(count) = value.strip_prefix("캐릭터 ").and_then(|s| s.strip_suffix('명')) {
        return Cow::Owned(format!("{count} characters"));
    }
    if let Some(name) = value.strip_suffix(" · 기본") {
        return Cow::Owned(format!("{name} · default"));
    }
    if let Some(name) = value.strip_suffix(" · 준비 안 됨") {
        return Cow::Owned(format!("{name} · not ready"));
    }
    Cow::Borrowed(value)
}

fn english(value: &str) -> Option<&'static str> {
    Some(match value {
        "설정 방" => "Settings room",
        "앱의 작업 환경" => "App workspace",
        "작업 방으로" => "Back to workspace",
        "일반" => "General",
        "모양" => "Appearance",
        "셸" => "Shell",
        "테마" => "Theme",
        "캐릭터" => "Characters",
        "피드백" => "Feedback",
        "시작 위치와 파일, 스크롤의 기본값을 정합니다" => "Choose defaults for startup, files, and scrolling",
        "커서와 색, 글자 크기를 한 화면에서 맞춥니다" => "Tune cursor, colors, and text size together",
        "새 pane이 어떤 셸로 시작할지 정합니다" => "Choose the shell used by new panes",
        "모델과 계정, 협업 연결의 기본값을 정합니다" => "Choose model, account, and collaboration defaults",
        "캐릭터 명단과 그림을 한 벌로 갈아낍니다" => "Switch character rosters and artwork as a set",
        "한 명씩 이름과 성격, 모델을 고칩니다" => "Edit each character's name, persona, and model",
        "불편한 점을 이 기기에 기록합니다" => "Save feedback on this device",
        "언어" => "Language",
        "설정과 안내 화면에서 쓸 말을 고릅니다" => "Choose the language for settings and onboarding",
        "한국어" => "Korean",
        "시작과 파일" => "Startup and files",
        "새 작업 방과 파일을 여는 기본 동작입니다" => "Defaults for new rooms and opening files",
        "마지막 위치" => "Last location",
        "홈" => "Home",
        "직접 지정" => "Custom",
        "시작 폴더" => "Startup folder",
        "카사텀" => "kasaterm",
        "앱" => "App",
        "터미널" => "Terminal",
        "기본 앱" => "Default app",
        "편집기 명령" => "Editor command",
        "파일 트리 기본으로 열기" => "Open file tree by default",
        "pane 하단바 기본으로 켜기" => "Show pane footer by default",
        "편집과 스크롤" => "Editing and scrolling",
        "자주 바꾸지 않는 입력 감각만 모았습니다" => "Input behavior you rarely need to change",
        "끔" => "Off",
        "차분하게" => "Trackpad",
        "보통" => "Normal",
        "빠르게" => "Mouse",
        "아주 빠르게" => "Fast",
        "창 상태줄 높이" => "Window status bar height",
        "pane 하단바 높이" => "Pane footer height",
        "커서 작업대" => "Cursor workshop",
        "여덟 모양을 같은 전각 셀에서 비교합니다" => "Compare eight shapes in the same full-width cell",
        "블록" => "Block",
        "빔" => "Beam",
        "밑줄" => "Underline",
        "프레임" => "Frame",
        "괄호" => "Brackets",
        "쌍선" => "Double line",
        "윗줄" => "Overline",
        "모서리" => "Corners",
        "실제 깜빡임" => "Live blink",
        "마우스 포인터" => "Mouse pointer",
        "텍스트 입력 캐럿과 터미널 위 포인터는 서로 다른 설정입니다" => "Text caret and terminal pointer are separate settings",
        "화살표" => "Arrow",
        "I-빔" => "I-beam",
        "색과 형태" => "Color and shape",
        "현재 테마 토큰을 모든 네이티브 화면이 함께 씁니다" => "All native screens share the current theme tokens",
        "시스템 밝기별 테마" => "Themes by system appearance",
        "운영체제가 밝음/어두움을 바꿀 때 입을 팔레트입니다" => "Palettes used when the OS switches appearance",
        "밝은 화면" => "Light",
        "어두운 화면" => "Dark",
        "현재 색으로 복제" => "Duplicate current colors",
        "커스텀 팔레트 이름" => "Custom palette name",
        "초기화" => "Reset",
        "팔레트 치우기" => "Remove palette",
        "팔레트 색" => "Palette colors",
        "색 칸을 고른 뒤 휠이나 #rrggbb 값으로 바꿉니다" => "Pick a slot, then use the wheel or a #rrggbb value",
        "화면에서 색 집기" => "Pick from screen",
        "글자 크기" => "Font size",
        "UI 배율" => "UI zoom",
        "탭을 위에" => "Tabs on top",
        "탭을 옆에" => "Tabs on side",
        "배율 1:1로 되돌리기" => "Reset scale",
        "새 pane의 셸" => "Shell for new panes",
        "이미 열린 pane은 그대로 두고 다음 pane부터 적용합니다" => "Applies to new panes; open panes stay unchanged",
        "시스템 기본" => "System default",
        "직접 경로" => "Custom path",
        "셸 경로는 실행 파일 하나만 적습니다. 명령 옵션은 각 pane에서 직접 붙여 주세요." => "Enter one shell executable; add options inside each pane",
        "Agent 기본값" => "Agent defaults",
        "새로 띄우는 Claude와 Codex 작업대에 적용됩니다" => "Applies to newly launched Claude and Codex workbenches",
        "캐릭터 성격 넣기" => "Inject character persona",
        "협업 연결 넣기" => "Enable collaboration",
        "기본" => "Default",
        "낮게" => "Low",
        "높게" => "High",
        "아주 높게" => "Extra high",
        "추가 인자" => "Extra arguments",
        "계정 작업대" => "Account workbench",
        "고르면 실행 중인 작업을 확인한 뒤 안전하게 갈아낍니다" => "Checks running work before switching safely",
        "기본 로그인" => "Default login",
        "계정 추가" => "Add account",
        "로그인 진행 중" => "Signing in",
        "로그인 취소" => "Cancel sign-in",
        "로그인 필요" => "Sign-in required",
        "확인 중…" => "Checking…",
        "로그인을 마쳤어요" => "Signed in",
        "별명" => "Label",
        "한도에 맞춰 자동 전환" => "Auto-switch at usage limit",
        "상태줄에 다른 계정도 표시" => "Show other accounts in status bar",
        "캐릭터 테마" => "Character themes",
        "명단과 그림, 성격을 한 벌로 갈아낍니다" => "Switch roster, art, and personas together",
        "전부 고르기" => "Select all",
        "기본값으로" => "Use default",
        "현재 테마 복제" => "Duplicate current theme",
        "목록 새로고침" => "Refresh list",
        "ZIP 테마 파일은 이 설정 방 어디에든 놓아 가져올 수 있어요." => "Drop a ZIP theme anywhere in this room to import it",
        "그림 생성 엔진" => "Image generation engine",
        "준비되지 않은 엔진은 이유를 함께 표시합니다" => "Unavailable engines show why",
        "목록으로" => "Back to list",
        "화면으로" => "Rendered",
        "원본" => "Raw",
        "원본 저장" => "Save raw",
        "정의 전체" => "Full definition",
        "이름" => "Name",
        "모델" => "Model",
        "이 캐릭터만 다른 실행 통로를 쓸 수 있습니다" => "This character can use a different backend",
        "성격" => "Persona",
        "다른 칸으로 나가거나 목록으로 돌아갈 때 저장합니다" => "Saves when focus leaves or you return to the list",
        "그림 생성" => "Image generation",
        "참조 그림을 이 화면에 놓고 모든 기본 동작을 한 번에 굽습니다" => "Drop a reference here to generate every basic motion",
        "그림 살펴보는 중" => "Inspecting reference",
        "굽는 중" => "Generating",
        "설치하는 중" => "Installing",
        "완성" => "Done",
        "실패" => "Failed",
        "참조 그림 준비됨" => "Reference ready",
        "참조 그림을 놓아 주세요" => "Drop a reference image",
        "그림 굽기" => "Generate artwork",
        "모션 그림" => "Motion artwork",
        "프레임 칸을 고르고 그림 파일을 놓으면 그 한 장만 바뀝니다" => "Select a frame and drop an image to replace only that frame",
        "대기" => "Idle",
        "걷기" => "Walk",
        "손 흔들기" => "Wave",
        "완료" => "Complete",
        "프로필" => "Profile",
        "대기 GIF" => "Idle GIF",
        "기본으로" => "Reset",
        "캐릭터 폴더 열기" => "Open character folder",
        "정의 파일 열기" => "Open roster file",
        "그림 새로고침" => "Refresh artwork",
        "그림 파일을 이 화면에 놓으면 이 캐릭터의 참조로 저장합니다." => "Drop an image here to use it as this character's reference",
        "한 명을 골라 이름과 성격, 모델을 고칩니다" => "Choose a character to edit name, persona, and model",
        "새 캐릭터는 먼저 테마를 복제한 뒤 그림 파일을 이 화면에 놓아 만듭니다." => "Duplicate a theme, then drop an image here to add a character",
        "새 캐릭터 그림을 이 화면에 놓으면 파일 이름으로 명단에 추가합니다." => "Drop a character image here to add it using the filename",
        "무엇이 불편했나요" => "What felt inconvenient?",
        "보내지 않고 이 기기의 피드백 폴더에 한 장씩 저장합니다" => "Saved locally as individual notes; nothing is sent",
        "진단 정보 함께 남기기" => "Include diagnostics",
        "피드백 저장" => "Save feedback",
        "저장 폴더 열기" => "Open saved feedback",
        "입력하세요" => "Enter text",
        "처음 설정" => "First setup",
        "외형" => "Appearance",
        "건너뛰고 터미널 열기" => "Skip and open terminal",
        "익숙한 터미널 모습으로 시작하세요" => "Start with a familiar terminal",
        "기존 설정을 가져오거나 색과 글꼴을 직접 고릅니다" => "Import existing settings or choose colors and fonts",
        "이미 로그인한 Agent를 그대로 씁니다" => "Use your existing Agent sign-ins",
        "Claude Code와 Codex의 기존 인증만 확인합니다" => "Only existing Claude Code and Codex credentials are checked",
        "새 pane의 환경을 확인하세요" => "Review the environment for new panes",
        "운영체제에 맞는 셸과 외형을 마지막으로 확인합니다" => "Review shell and appearance for this platform",
        "준비가 끝났어요" => "You're ready",
        "지금 고른 값은 나중에도 설정 방에서 바꿀 수 있습니다" => "You can change these choices later in Settings",
        "설치 환경을 확인하고 있어요" => "Checking your environment",
        "이전" => "Back",
        "다음" => "Next",
        "kasaterm 열기" => "Open kasaterm",
        "기존 설정 가져오기" => "Import existing settings",
        "직접 설정" => "Set up manually",
        "Mac 터미널에서 가져오기" => "Import from Mac terminals",
        "색상과 글꼴만 읽고 원본 프로필은 바꾸지 않습니다" => "Reads colors and fonts without changing the source profile",
        "가져오기" => "Import",
        "가져올 수 없음" => "Unavailable",
        "가져올 Apple Terminal 또는 iTerm2 프로필을 찾지 못했어요" => "No Apple Terminal or iTerm2 profile was found",
        "색상 테마" => "Color theme",
        "앱과 터미널 ANSI 색이 함께 바뀝니다" => "App and terminal ANSI colors change together",
        "터미널 글꼴" => "Terminal font",
        "설치된 고정폭 글꼴과 글자 크기를 고릅니다" => "Choose an installed monospace font and size",
        "시스템 글꼴" => "System font",
        "감지한 고정폭 글꼴이 없어 현재 시스템 글꼴을 유지합니다" => "No monospace font was detected; keeping the system font",
        "강조색" => "Accent color",
        "선택 영역과 커서, 링크에 함께 씁니다" => "Used for selection, cursor, and links",
        "첫 터미널에서 먼저 쓸 Agent" => "Preferred Agent for the first terminal",
        "로그인된 항목을 기본으로 고를 수 있습니다" => "A signed-in Agent can be selected as default",
        "로그인" => "Sign in",
        "로그인됨" => "Signed in",
        "로그인이 필요해요" => "Sign-in required",
        "설치되지 않았어요" => "Not installed",
        "인증정보는 각 도구가 보관하고, kasaterm은 토큰이나 비밀번호를 저장하지 않아요." => "Each tool stores its own credentials; kasaterm stores no tokens or passwords",
        "Windows 기본 셸" => "Default Windows shell",
        "셸 경로 직접 입력" => "Enter shell path",
        "새 pane을 열 때 시작할 셸입니다" => "The shell used when opening a new pane",
        "경로를 입력하세요" => "Enter a path",
        "Mac 터미널 설정" => "Mac terminal settings",
        "기본 셸" => "Default shell",
        "지금까지 고른 값을 첫 pane에 그대로 사용합니다" => "Use these choices in the first pane",
        "가져온 곳" => "Imported from",
        "kasaterm에서 직접 설정" => "Configured in kasaterm",
        "현재 테마" => "Current theme",
        "현재 글꼴" => "Current font",
        "나중에 연결" => "Connect later",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_switch_changes_real_settings_and_onboarding_copy() {
        set_language("en");
        assert_eq!(text("설정 방"), "Settings room");
        assert_eq!(text("준비가 끝났어요"), "You're ready");
        assert_eq!(text("테마 이름"), "테마 name");
        set_language("ko");
        assert_eq!(text("설정 방"), "설정 방");
    }
}
