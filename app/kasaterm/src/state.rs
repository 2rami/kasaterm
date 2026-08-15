//! Domain sub-structs split out of the mega `struct App` (main.rs).
//!
//! Why: `App` carried ~148 flat fields spanning three layers (terminal /
//! workspace / collab), so two workers touching different features both edited
//! the same struct definition → guaranteed git conflicts. Grouping a domain's
//! fields into a sub-struct here means the App definition holds one line
//! (`statusbar: state::StatusbarState`) and the field-level churn lives in this
//! file, per CLAUDE.md 병렬 작업 충돌 회피. Fields are `pub(crate)` because the
//! render/handler/input/chrome modules read them as `self.statusbar.<field>`.
use super::*;
use std::collections::HashMap;

/// Per-pane status bar (cwd/branch/diff chips at each pane's foot) plus the
/// open dropdown's state. Visibility = the global `set_footer_default` default,
/// flipped per pane by two exception sets: `hidden` (ids explicitly collapsed
/// while the default is on) and `shown` (ids explicitly opened while the default
/// is off). Toggling the global default clears both so panes re-unify. The
/// `*_rects` are per-frame hit targets rebuilt each paint; `menu*` back the open
/// path/branch dropdown.
#[derive(Default)]
pub(crate) struct StatusbarState {
    pub(crate) hidden: std::collections::HashSet<String>,
    pub(crate) shown: std::collections::HashSet<String>,
    pub(crate) path_rects: Vec<(String, (f32, f32, f32, f32))>,
    pub(crate) branch_rects: Vec<(String, (f32, f32, f32, f32))>,
    pub(crate) toggle_rects: Vec<(String, (f32, f32, f32, f32))>,
    pub(crate) diff_rects: Vec<(String, (f32, f32, f32, f32))>,
    pub(crate) menu: Option<(String, StatusbarMenu)>,
    pub(crate) menu_dir_rects: Vec<(std::path::PathBuf, (f32, f32, f32, f32))>,
    pub(crate) menu_branch_rects: Vec<(String, (f32, f32, f32, f32))>,
    pub(crate) menu_dirs: Vec<std::path::PathBuf>,
    pub(crate) menu_branches: Vec<String>,
    pub(crate) menu_scroll: f32,
    pub(crate) menu_rect: Option<(f32, f32, f32, f32)>,
    pub(crate) menu_search: String,
    /// `menu_search` 안 커서(문자 단위). 검색칸도 가운데를 고칠 수 있어야 한다 —
    /// 끝에서만 지워지면 오타 하나에 뒤를 다 날려야 했다. 드롭다운을 닫을 때
    /// 따로 되돌리지 않는다: 버퍼가 비면 `lineedit` 이 다음 조작에서 0 으로
    /// 클램프하므로 남은 값이 화면에 나올 일이 없다.
    pub(crate) menu_search_cursor: usize,
    /// 바깥주소(터널) 칩 — **창** 우하단 고정(2026-08-15 지시). pane 소속이
    /// 아니지만 발판 chips 와 같은 프레임에 그려지고 같은 손이 클릭을 가르므로
    /// 여기 얹는다. `tunnel_on` 은 마지막 폴 결과(None=아직 모름), `tunnel_checked`
    /// 는 폴 박자 게이트 — 상태 조회에 pgrep 이 들어가 매 프레임은 못 돈다.
    pub(crate) tunnel_on: Option<bool>,
    pub(crate) tunnel_checked: Option<std::time::Instant>,
    /// 원격 주소(cloudflared config 의 hostname). 같은 5초 폴에 얹는다 — 읽는 일이
    /// **파일 IO** 라, 팝오버가 그릴 때마다 부르면 열어 둔 동안 매 프레임
    /// `config.yml` 을 읽게 된다.
    pub(crate) tunnel_host: Option<String>,
    /// 자원을 많이 쓰는 순으로 상위 몇 개 — (pid, CPU%, RSS KB, 프로세스 이름).
    /// `res` 와 **같은 표본**이라 둘의 수치가 어긋나지 않는다.
    pub(crate) usage_top: Vec<(u32, f32, u64, String)>,
    pub(crate) tunnel_rect: Option<(f32, f32, f32, f32)>,
    /// 하단바 왼쪽에 적는 웹터미널 포트(Orca 하단바처럼 — 2026-08-15 지시).
    /// 포트 파일은 bind 뒤에 써지므로 부팅 직후 조회는 폴백(8765)일 수 있어
    /// 터널 폴과 같은 5초 박자로 읽어 여기 캐시한다. 클릭 = /term 열기.
    pub(crate) port: Option<String>,
    pub(crate) port_rect: Option<(f32, f32, f32, f32)>,
    /// 리소스 사용량 — kasaterm 자신 + 자식 트리(PTY 셸·claude 들) 합.
    /// (CPU %, RSS bytes). ps 폴이라 5초 박자.
    pub(crate) res: Option<(f32, u64)>,
    pub(crate) res_rect: Option<(f32, f32, f32, f32)>,
    /// 지금 펼쳐진 팝오버와 그것을 연 칩의 자리(앵커). 한 번에 하나만 — 하단바
    /// 칩들이 서로 8px 안에 붙어 있어 둘이 겹치면 어느 쪽 행을 눌렀는지 사람도
    /// 코드도 못 가른다.
    ///
    /// 즉시 실행하던 칩(바깥 토글·포트 열기)을 전부 이 안으로 넣은 것은 거노
    /// 지시다(2026-08-15: 「누르면 좌측 사용량처럼 펼쳐져서 거기서 조작하게
    /// 하자」). 하단바는 좁아서 라벨이 한 낱말로 줄고, 그러면 누르기 전에 무슨
    /// 일이 일어날지 알 수가 없다.
    pub(crate) popover: Option<(StatusbarPopover, (f32, f32, f32, f32))>,
    /// 팝오버 바깥 사각형 — 바깥을 눌렀을 때 닫으려면 안쪽이 어디까지인지
    /// 알아야 한다. **닫혀 있으면 `None`** 이고, 렌더가 매 프레임 다시 채운다.
    pub(crate) popover_rect: Option<(f32, f32, f32, f32)>,
    /// 팝오버 안 행들의 클릭 자리. 닫히면 비운다 — 남겨 두면 안 보이는 행이
    /// 눌린다(렌더 카탈로그의 「시저는 픽셀만 자르지 클릭은 안 자른다」와 같은
    /// 부류인데, 여긴 아예 그려지지도 않은 행이라 더 나쁘다).
    pub(crate) popover_hits: Vec<(StatusbarHit, (f32, f32, f32, f32))>,
    /// 팝오버 세로 스크롤(px). 포트가 스무 개면 창 높이를 넘는다.
    pub(crate) popover_scroll: f32,
}

/// 하단바에서 펼쳐지는 것들.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum StatusbarPopover {
    Ports,
    Tunnel,
    Usage,
}

/// 팝오버 행을 눌렀을 때 할 일.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum StatusbarHit {
    /// 행 클릭 — `http://localhost:<port>`.
    OpenPort(u16),
    /// 호버 시 나오는 ×. 포트는 그 자체로 못 닫으니 쥔 프로세스를 죽인다.
    KillPort(u32),
    /// 맨 윗줄 — 이 앱의 웹터미널(`/term`).
    OpenWebTerm,
    /// 원격 접속 문을 여닫는다.
    ToggleTunnel,
    /// 열려 있을 때의 주소를 클립보드로.
    CopyTunnelHost,
}

/// Right-hand git column + commit modal + path/branch dropdowns (the in-window
/// replacement for the old floating webview git panel). `col_*` are the column
/// (visibility, width, scroll, per-frame file-row/button hit rects, the parsed
/// diff cache, and the off-thread `git status` snapshot + its cwd). `commit_*`
/// back the VSCode-style message box + commit modal. `path_*`/`branch_*` are the
/// header dropdowns; `op` labels the in-flight push/pull for the spinner.
#[derive(Default)]
pub(crate) struct GitState {
    pub(crate) col_visible: bool,
    pub(crate) col_w_logical: f32,
    pub(crate) col_resize: Option<(f32, f32)>,
    pub(crate) col_scroll: f32,
    /// 직전 프레임의 변경 목록 기하 — `(보이는 높이, 내용 높이)`, LOGICAL px.
    /// 휠이 스크롤 상한을 여기서 읽는다.
    ///
    /// 그리기 쪽이 써 주는 이유: 휠은 이 값을 자기 힘으로 알 수 없다. 목록 높이는
    /// 파일 수만이 아니라 섹션 머리 두 개와 **펼친 diff 줄 수**로 정해지는데, 그건
    /// 캐시를 뒤져야 나온다. 예전엔 휠이 `파일 수 × 22` 로 어림했고 그래서 diff 를
    /// 펼치면 끝까지 스크롤이 안 됐다 — 목록은 화면 몇 배로 길어졌는데 상한은
    /// 파일 몇 개 몫 그대로였다. 보이는 높이도 `헤더 68px · 버튼 44px` 로 박혀 있어
    /// 최근 커밋 미리보기가 자리를 얼마나 먹는지를 아예 못 봤다.
    pub(crate) col_list_extent: (f32, f32),
    pub(crate) col_file_rects: Vec<(bool, String, (f32, f32, f32, f32))>,
    pub(crate) col_btn_rects: Vec<(GitColBtn, (f32, f32, f32, f32))>,
    pub(crate) col_expanded: std::collections::HashSet<(bool, String)>,
    pub(crate) col_diff_cache: HashMap<(bool, String), Vec<kasa_mcp::git::DiffLine>>,
    /// 최근 커밋 더블클릭 인라인 펼침(GitLens 그래프식): 펼친 커밋 hash(하나만),
    /// 그 변경 파일 목록 캐시, 다시 펼친 파일 diff 집합/캐시, 행 hit, 더블클릭 감지.
    pub(crate) col_commit_expanded: Option<String>,
    pub(crate) col_commit_files_cache: HashMap<String, Vec<(String, u32, u32)>>,
    pub(crate) col_commit_file_expanded: std::collections::HashSet<(String, String)>,
    pub(crate) col_commit_diff_cache: HashMap<(String, String), Vec<kasa_mcp::git::DiffLine>>,
    pub(crate) col_commit_rects: Vec<(String, (f32, f32, f32, f32))>,
    pub(crate) col_commit_file_rects: Vec<(String, String, (f32, f32, f32, f32))>,
    pub(crate) last_commit_click: Option<(std::time::Instant, String)>,
    /// 「최근 커밋」 구역에 사용자가 잡아 준 높이(LOGICAL px). `None` 이면 가져온
    /// 커밋 수에 맞춘 자동. 구역 머리의 가로선을 위아래로 끌면 잡힌다.
    pub(crate) col_commits_h: Option<f32>,
    /// 그 가로선 드래그 중 — `(드래그 시작 커서 y, 시작 높이)`. 시작 높이를 함께
    /// 쥐는 건 폭 드래그(`col_resize`)와 같은 이유다: 매 이동마다 델타를 누적하면
    /// clamp 에 걸린 뒤 커서를 되돌려도 값이 안 따라온다.
    pub(crate) col_commits_resize: Option<(f32, f32)>,
    /// 그 가로선의 hit rect(직전 프레임). 렌더가 써 주고 마우스가 읽는다 — 자리가
    /// 변경 목록 길이·펼침 상태에 따라 매 프레임 달라져 handler 가 자기 힘으로는
    /// 못 구한다.
    pub(crate) col_commits_grip: Option<(f32, f32, f32, f32)>,
    /// 폴러에게 건네는 「커밋 몇 개까지 가져와라」. 렌더가 구역 높이에서 계산해
    /// 쓰고 폴러 스레드가 읽는다. 0 은 「아직 안 정해짐」이라 폴러가 기본값을 쓴다.
    pub(crate) col_commit_want: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pub(crate) col_close_rect: Option<(f32, f32, f32, f32)>,
    pub(crate) col_expand_rect: Option<(f32, f32, f32, f32)>,
    pub(crate) commit_btn_rect: Option<(f32, f32, f32, f32)>,
    pub(crate) commit_caret_rect: Option<(f32, f32, f32, f32)>,
    pub(crate) commit_menu_open: bool,
    pub(crate) commit_menu_rects: Vec<(GitCommitAction, (f32, f32, f32, f32))>,
    pub(crate) commit_modal_open: bool,
    pub(crate) commit_modal_include_unstaged: bool,
    pub(crate) commit_modal_rects: Vec<(GitModalBtn, (f32, f32, f32, f32))>,
    pub(crate) col_stage_rects: Vec<(bool, String, (f32, f32, f32, f32))>,
    pub(crate) col_discard_rects: Vec<(String, bool, (f32, f32, f32, f32))>,
    pub(crate) col_open_rects: Vec<(String, (f32, f32, f32, f32))>,
    pub(crate) commit_msg: String,
    pub(crate) commit_cursor: usize,
    pub(crate) commit_focused: bool,
    pub(crate) commit_input_rect: Option<(f32, f32, f32, f32)>,
    pub(crate) col_data: std::sync::Arc<std::sync::Mutex<GitColView>>,
    pub(crate) op: Option<&'static str>,
    pub(crate) col_cwd: std::sync::Arc<std::sync::Mutex<Option<std::path::PathBuf>>>,
    pub(crate) col_pinned_cwd: Option<std::path::PathBuf>,
    pub(crate) path_menu_open: bool,
    pub(crate) branch_menu_open: bool,
    pub(crate) path_hdr_rect: Option<(f32, f32, f32, f32)>,
    pub(crate) branch_hdr_rect: Option<(f32, f32, f32, f32)>,
    pub(crate) path_menu_rects: Vec<(Option<std::path::PathBuf>, (f32, f32, f32, f32))>,
    pub(crate) branch_menu_rects: Vec<(String, (f32, f32, f32, f32))>,
}

/// 우측 칼럼의 활성 탭. 칼럼은 원래 git 전용이었고 Info·Sessions 가 나중에
/// 붙었다 — 셋은 폭·스크롤·닫기 버튼을 공유하고 본문만 갈린다.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SideTab {
    #[default]
    Git,
    Info,
    /// 과거 세션 기록(claude·codex·agy)을 골라 그 자리에서 잇는다.
    Sessions,
    /// 하네스별 MCP 서버와 스킬 — 무엇이 붙어 있고 무엇이 꺼져 있나.
    Mcp,
}

/// 「+」로 여는 URL 서버 추가 칸. 이름·주소 두 줄뿐이다.
#[derive(Default)]
pub(crate) struct McpAddForm {
    /// 어느 하네스에 더하나 — `"claude"` | `"codex"`. 누른 섹션이 정한다.
    pub(crate) harness: &'static str,
    pub(crate) name: String,
    pub(crate) name_cursor: usize,
    pub(crate) url: String,
    pub(crate) url_cursor: usize,
    /// 참이면 주소 칸에 커서가 있다. Tab 으로 오간다.
    pub(crate) on_url: bool,
    /// 마지막 시도가 남긴 한 줄. 칸 아래에 그대로 뜬다 — 토스트로 띄우면 칸을
    /// 보고 있는 눈에서 멀어지고, 다음 토스트에 밀려 사라진다.
    pub(crate) err: Option<String>,
    pub(crate) name_rect: Option<(f32, f32, f32, f32)>,
    pub(crate) url_rect: Option<(f32, f32, f32, f32)>,
    pub(crate) ok_rect: Option<(f32, f32, f32, f32)>,
    pub(crate) cancel_rect: Option<(f32, f32, f32, f32)>,
}

/// 우측 칼럼의 「MCP·Skill」 탭 상태.
///
/// 설정 두 벌(`~/.claude.json` json · `~/.codex/config.toml` toml)을 파싱하고 스킬
/// 폴더를 훑는 일이라 수집을 워커로 뺀다 — `SessionsColState` 와 같은 얼개다.
pub(crate) struct McpColState {
    pub(crate) snap: std::sync::Arc<std::sync::Mutex<Vec<crate::mcpcol::McpRow>>>,
    pub(crate) view: Vec<crate::mcpcol::McpRow>,
    pub(crate) rev: std::sync::Arc<std::sync::atomic::AtomicU64>,
    pub(crate) seen_rev: u64,
    pub(crate) busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(crate) last_refresh: Option<std::time::Instant>,
    /// 다음 pump 에서 주기를 무시하고 다시 읽는다. 우리가 설정을 고친 직후에 세운다 —
    /// 방금 누른 토글이 6초 뒤에 반영되면 눌린 건지 아닌지를 알 수 없다.
    pub(crate) stale: bool,
    /// 워커가 끝내 놓고 가는 한 줄. GUI 스레드에서만 토스트를 띄울 수 있어서,
    /// 지우기처럼 CLI 를 부르는 일은 결과를 여기 두고 다음 pump 가 집어 간다.
    pub(crate) notice: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// 지우기를 한 번 눌러 확인을 기다리는 행(`view` 인덱스)과 그 시각. 삭제는
    /// 되돌릴 수 없어 두 번 누르게 한다 — 다이얼로그를 띄우면 목록에서 눈이 떠난다.
    pub(crate) confirm_delete: Option<(usize, std::time::Instant)>,
    /// 이 창이 보고 있는 폴더. claude 쪽은 꺼짐도 `.mcp.json` 도 폴더마다 달라서,
    /// 여기가 바뀌면 주기를 기다리지 않고 다시 읽는다.
    pub(crate) cwd: Option<std::path::PathBuf>,
    /// URL 서버를 더하는 칸. 열려 있으면 키가 PTY 대신 이리로 온다.
    ///
    /// URL 만 받는 이유는 stdio 서버가 커맨드·인자·환경변수를 다 받아야 해서다. 그
    /// 환경변수 자리엔 대개 API 키가 들어가는데, 그걸 여기서 받으면 우리가 토큰을
    /// 평문으로 설정 파일에 적는 셈이 된다 — 2026-08-11 확정("URL 서버 추가까지").
    pub(crate) add: Option<McpAddForm>,
    pub(crate) scroll: f32,
    /// 매 paint 재생성되는 hit target. 행은 `view` 인덱스로 되짚는다.
    pub(crate) row_rects: Vec<(usize, (f32, f32, f32, f32))>,
    /// 행별 지우기 버튼. 행 hit 보다 먼저 본다 — 겹쳐 있다.
    pub(crate) del_rects: Vec<(usize, (f32, f32, f32, f32))>,
    /// 섹션 머리의 더하기 버튼과 그게 여는 하네스.
    pub(crate) add_rects: Vec<(&'static str, (f32, f32, f32, f32))>,
    /// 접힌 머리들. 하네스는 `"claude"`, 종류는 `"claude/skill"`.
    ///
    /// 접힌 쪽을 담는다(펼친 쪽이 아니라) — 기본이 「다 펼침」이라 빈 집합이 곧
    /// 기본값이고, 새 종류가 늘어도 저절로 보인다. 2026-08-11 지시 "다 뜨게하고
    /// 접기도 가능하게".
    pub(crate) collapsed: std::collections::HashSet<String>,
    /// 섹션·종류 머리의 클릭 자리와 그 접힘 키.
    pub(crate) head_rects: Vec<(String, (f32, f32, f32, f32))>,
    pub(crate) refresh_rect: Option<(f32, f32, f32, f32)>,
    pub(crate) body_rect: (f32, f32, f32, f32),
    pub(crate) content_h: f32,
}

impl Default for McpColState {
    fn default() -> Self {
        Self {
            snap: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            view: Vec::new(),
            rev: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            seen_rev: 0,
            busy: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            last_refresh: None,
            stale: false,
            notice: std::sync::Arc::new(std::sync::Mutex::new(None)),
            confirm_delete: None,
            cwd: None,
            add: None,
            scroll: 0.0,
            row_rects: Vec::new(),
            del_rects: Vec::new(),
            add_rects: Vec::new(),
            collapsed: Default::default(),
            head_rects: Vec::new(),
            refresh_rect: None,
            body_rect: (0.0, 0.0, 0.0, 0.0),
            content_h: 0.0,
        }
    }
}

/// 우측 칼럼의 「세션 기록」 탭 — 과거 대화를 골라 잇는 레일.
///
/// 살아 있는 tmux 세션 목록(`/sessions` 패널)과 다르다. 이쪽은 각 하네스가
/// 디스크에 남긴 기록이라 목록을 만들려면 저장소를 통째로 stat 해야 하고,
/// 그래서 `snap` 을 워커 스레드가 채운다(Info 탭이 `ps`/`lsof` 를 GUI 스레드
/// 밖으로 뺀 것과 같은 이유). 렌더는 `rev` 가 올라갔을 때만 `view` 로 옮겨
/// 담는다 — 매 프레임 Vec<String> 을 clone 하면 목록 길이만큼 할당이 돈다.
pub(crate) struct SessionsColState {
    pub(crate) snap: std::sync::Arc<std::sync::Mutex<Vec<kasa_socket::backend::RecentSession>>>,
    pub(crate) view: Vec<kasa_socket::backend::RecentSession>,
    pub(crate) rev: std::sync::Arc<std::sync::atomic::AtomicU64>,
    pub(crate) seen_rev: u64,
    pub(crate) busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(crate) last_refresh: Option<std::time::Instant>,
    /// `false` = 이 방(활성 pane 의 cwd) 세션만, `true` = 하네스 전부.
    /// 기본이 방인 건 목록을 여는 대부분의 이유가 "방금 하던 그거"라서다.
    pub(crate) scope_all: bool,
    /// 지금 목록이 어느 cwd 의 것인지 — pane 을 옮겨 cwd 가 달라지면 재수집을
    /// 트리거한다(`scope_all` 일 땐 무의미하므로 비교에서 뺀다).
    pub(crate) cwd: Option<std::path::PathBuf>,
    pub(crate) scroll: f32,
    /// 매 paint 재생성되는 hit target. 행은 `view` 인덱스로 되짚는다.
    pub(crate) row_rects: Vec<(usize, (f32, f32, f32, f32))>,
    pub(crate) scope_rects: Vec<(bool, (f32, f32, f32, f32))>,
    pub(crate) refresh_rect: Option<(f32, f32, f32, f32)>,
    /// 직전 프레임의 본문 영역 — 스크롤 clamp 가 실제 그려진 높이를 알아야 한다.
    pub(crate) body_rect: (f32, f32, f32, f32),
    pub(crate) content_h: f32,
}

impl Default for SessionsColState {
    fn default() -> Self {
        Self {
            snap: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            view: Vec::new(),
            rev: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            seen_rev: 0,
            busy: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            last_refresh: None,
            scope_all: false,
            cwd: None,
            scroll: 0.0,
            row_rects: Vec::new(),
            scope_rects: Vec::new(),
            refresh_rect: None,
            body_rect: (0.0, 0.0, 0.0, 0.0),
            content_h: 0.0,
        }
    }
}

/// Info 패널의 접히는 섹션. 접힘 상태는 pane 을 옮겨도 유지된다 — 포트만 보려고
/// 프로세스를 접어둔 사람이 탭을 옮길 때마다 다시 접을 이유가 없다.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum InfoSection {
    Dir,
    Procs,
    /// 닫아서 물러난 pane. 되살릴 게 있을 때만 나타나는 섹션이라, 다른 셋과 달리
    /// 자리를 상시 차지하지 않는다.
    Closed,
}

/// Info 탭 머리의 앱 전역 진입점. 우상단 아이콘 클러스터에 흩어져 있던 것들이라
/// 프로세스·포트와 달리 pane 상태와 무관하다 — 스크롤 위, 탭 머리 바로 아래 고정.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum InfoAction {
    /// SCHALE OS(아로나) 패널 토글.
    Arona,
    /// 설정 화면.
    Settings,
    /// 피드백 작성(설정 창의 Feedback 페이지). 사이드바 트레이에도 있지만,
    /// 사이드바를 접으면 트레이째 사라져 여기가 유일한 진입점이 된다.
    Feedback,
}

/// 프로젝트 디렉터리 섹션의 액션 버튼.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum InfoDirBtn {
    /// Finder(macOS) · 탐색기 · 파일 관리자.
    Reveal,
    /// 설치된 에디터 중 첫 번째(`proc::open_with_apps`).
    Editor,
    CopyPath,
}

/// 프로세스·포트 행 우클릭 메뉴 항목.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum InfoMenuAction {
    Terminate,
    ForceKill,
    CopyPid,
    CopyCmd,
}

/// Info 탭 — 활성 pane 셸 아래 프로세스 + listen 포트. `snap` 은 워커 스레드가
/// 채우는 스냅샷이고 `busy`/`last_refresh` 가 그 워커를 스로틀한다(수집이 `ps` +
/// `lsof` fork 라 렌더 스레드에서 돌릴 수 없다 — info.rs 참고). `shell_pid` 는
/// 현재 목록이 어느 pane 것인지로, pane 을 옮기면 즉시 갱신을 트리거한다.
/// `root`/`root_is_repo` 는 파일트리가 앵커한 디렉터리로, 그게 git 레포라서
/// 골라진 것인지를 패널이 정직하게 밝히는 데 쓴다.
pub(crate) struct InfoState {
    pub(crate) tab: SideTab,
    pub(crate) snap: std::sync::Arc<std::sync::Mutex<crate::info::InfoSnap>>,
    /// 렌더가 읽는 사본. 매 프레임 `snap` 을 잠가 통째로 clone 하면 프로세스가
    /// 수십이면 프레임마다 그만큼의 String 할당이 도는데, 실제 내용은 1.5초에
    /// 한 번만 바뀐다 — `rev` 가 올라갔을 때만 옮겨 담는다.
    pub(crate) view: crate::info::InfoSnap,
    /// 워커가 새 스냅샷을 넣을 때마다 증가. GUI 는 `seen_rev` 와 비교만 한다.
    pub(crate) rev: std::sync::Arc<std::sync::atomic::AtomicU64>,
    pub(crate) seen_rev: u64,
    /// 포트가 응답한 제목 캐시(워커가 채운다).
    pub(crate) sites: crate::info::SiteCache,
    pub(crate) busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(crate) last_refresh: Option<std::time::Instant>,
    /// 지금 목록이 어느 pane 집합의 것인지 — pane 이 열리거나 닫히면 달라져
    /// 즉시 재수집을 트리거한다.
    pub(crate) key: String,
    pub(crate) scroll: f32,
    pub(crate) root: Option<std::path::PathBuf>,
    pub(crate) root_is_repo: bool,
    pub(crate) dir_collapsed: bool,
    pub(crate) procs_collapsed: bool,
    pub(crate) closed_collapsed: bool,
    /// 우클릭 메뉴 — `(화면 좌표, 대상)`.
    /// 열렸으면 (좌상단 x, y, 겨눈 프로세스 pid). pid 를 들고 다니는 건 메뉴가
    /// 열린 뒤 목록이 갱신돼도 겨눈 대상이 흔들리지 않게 하려는 것이다.
    pub(crate) ctx_menu: Option<(f32, f32, u32)>,
    /// 직전 프레임에 이 패널이 차지한 영역 `(x, y, w, h)`. 커서가 여기 있는 동안은
    /// 새 스냅샷을 렌더 사본으로 옮기지 않는다 — 항목이 생기거나 사라지면 아래
    /// 행들이 밀려 **누르려던 것이 손가락 밑에서 달아나기** 때문이다. 한 프레임
    /// 늦은 값이지만 패널 위치는 거의 안 변해 판정에 충분하다.
    pub(crate) panel_rect: Option<(f32, f32, f32, f32)>,
    /// 동결이 시작된 시각. 커서가 패널을 떠나면 비운다. `CursorLeft` 를 안 받으므로
    /// 커서 좌표는 창을 떠나도 마지막 자리에 남는다 — 패널 위에 마우스를 얹어 둔 채
    /// 자리를 뜨면 목록이 영영 굳는다. 그래서 동결에 시한을 둔다.
    pub(crate) frozen_since: Option<std::time::Instant>,
    /// 접어둔 **방**(`win:N`). 방은 펴진 게 기본이라 여기 담긴 것만 접힌다.
    pub(crate) group_collapsed: std::collections::HashSet<String>,
    /// 펴 둔 **학생 그룹**(surface id). 학생은 접힌 게 기본이라 여기 담긴 것만
    /// 펴진다 — 방과 기본값이 반대여서 한 집합으로는 표현할 수 없다.
    ///
    /// 기본을 접힘으로 둔 건 목록 길이 때문이다. pane 마다 프로세스 나무를 다 펴면
    /// 방 몇 개만 돼도 화면을 넘겨야 "누가 무슨 포트를 쥐었나"에 닿는다. 접힌 학생도
    /// 포트를 쥔 줄은 남으므로(그리기 참조) 접는다고 서버를 놓치지는 않는다.
    pub(crate) pane_expanded: std::collections::HashSet<String>,
    /// 닫힌 pane 줄의 hit rect `(스택 인덱스, rect)` — 누르면 그 줄만 되살린다.
    /// 매 paint 재생성(다른 hit target 과 같은 규칙).
    pub(crate) closed_rects: Vec<(usize, (f32, f32, f32, f32))>,
    /// 그 줄의 × `(스택 인덱스, rect)` — 되살리기를 포기하고 **프로세스까지** 끈다.
    /// 커서가 얹힌 줄에만 생기므로 `closed_rects` 보다 성기고, 겹치는 자리라 클릭
    /// 판정을 먼저 받아야 한다.
    pub(crate) closed_kill_rects: Vec<(usize, (f32, f32, f32, f32))>,
    /// 그룹 머리 직전 클릭 `(시각, 열쇠)` — 더블클릭(=그 학생으로 포커스) 판정용.
    /// 한 번 클릭은 접기라, 두 번째 클릭이 접기를 되돌리고 포커스까지 옮긴다.
    pub(crate) last_group_click: Option<(std::time::Instant, String)>,
    /// 매 paint 재생성되는 hit target. 탭 머리 / 포트 행(→ 브라우저로 열기) /
    /// 프로세스 행(우클릭 대상) / 종료 버튼 / 섹션 머리 / 디렉터리 버튼.
    pub(crate) tab_rects: Vec<(SideTab, (f32, f32, f32, f32))>,
    /// `(포트, 소유 pid, rect)` — 종료가 붙으면서 pid 없이는 행을 다룰 수 없다.
    /// pane 그룹 머리 — 클릭하면 그 그룹만 접힌다.
    pub(crate) group_rects: Vec<(String, (f32, f32, f32, f32))>,
    pub(crate) proc_rects: Vec<(u32, (f32, f32, f32, f32))>,
    pub(crate) kill_rects: Vec<(u32, (f32, f32, f32, f32))>,
    pub(crate) sec_rects: Vec<(InfoSection, (f32, f32, f32, f32))>,
    pub(crate) dir_btn_rects: Vec<(InfoDirBtn, (f32, f32, f32, f32))>,
    /// 머리의 전역 진입점 버튼. 스크롤 밖(고정)이라 본문 rect 들과 달리
    /// `draw_info_col` 이 아니라 그 위 블록이 채운다.
    pub(crate) action_rects: Vec<(InfoAction, (f32, f32, f32, f32))>,
    pub(crate) ctx_menu_rects: Vec<(InfoMenuAction, (f32, f32, f32, f32))>,
    pub(crate) refresh_rect: Option<(f32, f32, f32, f32)>,
}

impl Default for InfoState {
    fn default() -> Self {
        Self {
            tab: SideTab::Git,
            snap: std::sync::Arc::new(std::sync::Mutex::new(crate::info::InfoSnap::default())),
            view: crate::info::InfoSnap::default(),
            rev: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            seen_rev: 0,
            sites: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            busy: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            last_refresh: None,
            panel_rect: None,
            frozen_since: None,
            key: String::new(),
            scroll: 0.0,
            root: None,
            root_is_repo: false,
            dir_collapsed: false,
            procs_collapsed: false,
            closed_collapsed: false,
            ctx_menu: None,
            group_collapsed: std::collections::HashSet::new(),
            pane_expanded: std::collections::HashSet::new(),
            closed_rects: Vec::new(),
            closed_kill_rects: Vec::new(),
            last_group_click: None,
            tab_rects: Vec::new(),
            group_rects: Vec::new(),
            proc_rects: Vec::new(),
            kill_rects: Vec::new(),
            sec_rects: Vec::new(),
            dir_btn_rects: Vec::new(),
            action_rects: Vec::new(),
            ctx_menu_rects: Vec::new(),
            refresh_rect: None,
        }
    }
}

/// Sidebar file-tree column. `root` follows the active pane's cwd; `nodes` is
/// the flattened expanded tree (rebuilt only on root/expand change). `drag`/
/// `new`/`selected`/`search_*` carry the in-flight tree interactions; `fs_dirty`
/// /`watch`/`ignored` are the off-GUI-thread live-refresh + gitignore-dim Arcs.
/// `visible`/`w_logical`/`resize` are the column chrome. The `*_rect` fields are
/// per-frame hit targets rebuilt each paint.
#[derive(Default)]
pub(crate) struct FileTreeState {
    pub(crate) drag: Option<FileTreeDrag>,
    pub(crate) new: Option<(bool, String)>,
    pub(crate) new_folder_rect: (f32, f32, f32, f32),
    pub(crate) new_file_rect: (f32, f32, f32, f32),
    pub(crate) new_row_rect: (f32, f32, f32, f32),
    pub(crate) selected: Option<std::path::PathBuf>,
    /// Cmd/Shift-click 다중선택분 — `selected`(primary/anchor)와 합쳐 "현재 선택
    /// 전체". 일괄 휴지통 삭제·우클릭 메뉴 대상. 일반 클릭이면 clear.
    pub(crate) selected_more: std::collections::HashSet<std::path::PathBuf>,
    /// 우클릭 컨텍스트 메뉴 — 열렸으면 메뉴 좌상단(px). `ctx_menu_rects` 는 항목 hit.
    pub(crate) ctx_menu: Option<(f32, f32)>,
    pub(crate) ctx_menu_rects: Vec<(FtMenuAction, (f32, f32, f32, f32))>,
    /// 인라인 이름변경 — (대상 경로, 편집 중 텍스트). `new` 와 상호배타.
    pub(crate) rename: Option<(std::path::PathBuf, String)>,
    /// 인라인 입력행(`new`/`rename` 중 열린 쪽) 안 커서(문자 단위). 두 모드가
    /// 동시에 열리지 않으므로 커서도 한 벌이면 된다. `new` 는 늘 빈 버퍼로
    /// 열려 다음 조작에서 0 으로 클램프되지만, `rename` 은 기존 이름을 싣고
    /// 열리므로 그 자리에서 이름 끝을 찍어 준다(`run_ft_menu_action`).
    pub(crate) edit_cursor: usize,
    pub(crate) rename_row_rect: (f32, f32, f32, f32),
    /// 새 항목 생성 부모(우클릭한 폴더). None 이면 트리 root.
    pub(crate) new_parent: Option<std::path::PathBuf>,
    pub(crate) search_active: bool,
    pub(crate) search_query: String,
    /// `search_query` 안 커서(문자 단위).
    pub(crate) search_cursor: usize,
    pub(crate) search_rect: (f32, f32, f32, f32),
    /// 트리 본문의 실제 geometry: (x, start_y, w, visible_h). start_y 는 검색박스
    /// + 빠른파일 섹션(항목 수만큼 동적) 아래로 밀린 트리 첫 행 y. visible_h 는
    /// dock 을 뺀 창 끝까지의 본문 높이. 렌더가 매 paint 갱신 → 스크롤 처리가
    /// 이걸로 clamp 해야 동적 헤더 높이를 정확히 반영(하드코딩하면 max_scroll 틀림).
    pub(crate) body_rect: (f32, f32, f32, f32),
    pub(crate) root: Option<std::path::PathBuf>,
    /// git 레포 앵커 계산의 1-엔트리 캐시: `(pane cwd, 그 cwd 를 감싸는 레포 루트)`.
    /// 앵커는 cwd 부터 위로 `.git` 을 훑으므로 깊이만큼 stat 이 든다 —
    /// `refresh_file_tree` 가 매 프레임 불리니 cwd 가 그대로면 재계산하지 않는다.
    pub(crate) anchor_cache: Option<(std::path::PathBuf, Option<std::path::PathBuf>)>,
    pub(crate) expanded: std::collections::HashSet<std::path::PathBuf>,
    pub(crate) nodes: Vec<FileNode>,
    pub(crate) fs_dirty: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(crate) watch: std::sync::Arc<std::sync::Mutex<Vec<std::path::PathBuf>>>,
    pub(crate) watch_started: bool,
    pub(crate) ignored: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    pub(crate) hover: Option<std::path::PathBuf>,
    pub(crate) scroll: f32,
    pub(crate) rects: Vec<(std::path::PathBuf, (f32, f32, f32, f32))>,
    /// "빠른 파일" 고정 섹션(트리 최상단)의 행 hit rect: (파일 경로, 논리 rect).
    /// 개인/프로젝트 CLAUDE.md·프로젝트 MEMORY.md 로의 원클릭. 스크롤과 무관하게
    /// 고정 위치라 별도 벡터로 둔다(스크롤하는 `rects` 와 분리). 매 paint 재생성.
    pub(crate) quick_rects: Vec<(std::path::PathBuf, (f32, f32, f32, f32))>,
    pub(crate) visible: bool,
    pub(crate) w_logical: f32,
    pub(crate) resize: Option<(f32, f32)>,
}

/// 한 pane 이 **지금 돌리고 있는** 서브에이전트·백그라운드 셸. 훅이 시작·종료를
/// 직접 보고한 것이라, 화면에도 transcript 에도 안 물어보고 안다.
///
/// 원래는 transcript 꼬리 64KB 를 읽어 `tool_use`(런치)와 `tool_result`(회수)를
/// 짝지어 알아냈다. 그 방식은 **세션이 커지면 조용히 눈이 먼다** — 런치 기록이
/// 창 밖으로 밀려나면 짝이 안 맞아 in-flight 인 줄을 모른다. 실측(2026-08-11):
/// 3.8MB 세션은 7건이 잡혔는데 8.3MB·24MB 세션은 0건이었다. 하필 **오래 기다리는
/// 작업일수록 안 보이는** 쪽으로 틀리니, 진행 표시가 가장 필요한 자리에서 꺼졌다.
/// 훅은 그 순간 한 번 오고 끝이라 세션 크기와 무관하다(Orca 의 「상태는 훅에서
/// 온다」와 같은 자리).
///
/// 값이 `(라벨, 겹친 수)` 인 이유: 설명이 같은 작업을 동시에 여럿 띄울 수 있어서다
/// (`Task` 셋을 같은 프롬프트로 부르는 fan-out). 키만 지우면 아직 도는 형제까지
/// 함께 사라지므로 세어서 0 일 때만 뺀다.
#[derive(Default, Clone)]
pub(crate) struct HookActivity {
    pub(crate) subagents: HashMap<String, (String, u32)>,
    pub(crate) background: HashMap<String, (String, u32)>,
}

impl HookActivity {
    /// 라벨을 화면 순서대로 — 맵은 순서가 없으니 정렬해 프레임마다 흔들리지 않게.
    pub(crate) fn labels(map: &HashMap<String, (String, u32)>) -> Vec<String> {
        let mut v: Vec<String> = map.values().map(|(l, _)| l.clone()).collect();
        v.sort();
        v.dedup();
        v
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.subagents.is_empty() && self.background.is_empty()
    }

    /// 훅 한 건을 반영한다. `phase` 는 `start`|`end`|`clear`, `kind` 는
    /// `subagent`|`background` — 값은 소켓 입구(`methods.rs`)에서 이미 좁혔다.
    pub(crate) fn apply(&mut self, phase: &str, kind: &str, key: &str, label: &str) {
        let slot = match kind {
            "subagent" => &mut self.subagents,
            _ => &mut self.background,
        };
        match phase {
            "start" => {
                // 라벨은 시작할 때만 온다(종료 훅은 설명을 안 실어 준다). 같은 키가
                // 다시 시작하면 세기만 올리고 처음 라벨을 지킨다 — 두 번째가 비어도
                // 화면에서 이름이 사라지지 않게.
                let e = slot
                    .entry(key.to_string())
                    .or_insert_with(|| (String::new(), 0));
                if e.0.is_empty() {
                    e.0 = if label.is_empty() {
                        key.to_string()
                    } else {
                        label.to_string()
                    };
                }
                e.1 += 1;
            }
            "end" => {
                // 모르는 키의 `end` 는 흘린다 — 앱이 도중에 떠서 시작을 못 본 경우가
                // 정상적으로 있다. 여기서 만들어 두면 없는 작업이 화면에 남는다.
                if let Some(e) = slot.get_mut(key) {
                    e.1 = e.1.saturating_sub(1);
                    if e.1 == 0 {
                        slot.remove(key);
                    }
                }
            }
            _ => slot.clear(), // "clear" — 그 kind 통째. `end` 를 놓친 것까지 함께 걷힌다.
        }
    }
}

#[cfg(test)]
mod hook_activity_tests {
    use super::*;

    #[test]
    fn start_and_end_pair_up_by_key() {
        let mut a = HookActivity::default();
        a.apply("start", "subagent", "toolu_1", "진행 조사");
        assert_eq!(HookActivity::labels(&a.subagents), vec!["진행 조사"]);
        a.apply("end", "subagent", "toolu_1", "");
        assert!(a.is_empty(), "짝이 맞으면 아무것도 안 남는다");
    }

    #[test]
    fn same_key_twice_needs_two_ends() {
        // 설명이 같은 작업을 동시에 여럿 띄우는 fan-out. 키만 지우면 아직 도는
        // 형제까지 사라지므로 세어야 한다.
        let mut a = HookActivity::default();
        a.apply("start", "subagent", "toolu_x", "코드 훑기");
        a.apply("start", "subagent", "toolu_x", "코드 훑기");
        a.apply("end", "subagent", "toolu_x", "");
        assert_eq!(
            HookActivity::labels(&a.subagents),
            vec!["코드 훑기"],
            "하나 끝났다고 둘 다 사라지면 안 된다"
        );
        a.apply("end", "subagent", "toolu_x", "");
        assert!(a.is_empty());
    }

    #[test]
    fn unknown_end_is_ignored() {
        // 앱이 나중에 떠서 시작을 못 본 작업의 종료. 여기서 항목을 만들면 이미 끝난
        // 일이 「도는 중」으로 화면에 남는다.
        let mut a = HookActivity::default();
        a.apply("end", "subagent", "toolu_ghost", "");
        assert!(a.is_empty());
    }

    #[test]
    fn clear_wipes_only_its_kind() {
        // 턴이 끝나면(Stop) 서브에이전트는 안 남지만 백그라운드 셸은 계속 산다.
        let mut a = HookActivity::default();
        a.apply("start", "subagent", "toolu_1", "조사");
        a.apply("start", "background", "toolu_2", "cargo build");
        a.apply("clear", "subagent", "-", "");
        assert!(a.subagents.is_empty());
        assert_eq!(HookActivity::labels(&a.background), vec!["cargo build"]);
    }

    #[test]
    fn label_falls_back_to_key_and_survives_a_blank_restart() {
        let mut a = HookActivity::default();
        a.apply("start", "background", "toolu_9", "");
        assert_eq!(HookActivity::labels(&a.background), vec!["toolu_9"]);
        let mut b = HookActivity::default();
        b.apply("start", "subagent", "toolu_8", "첫 라벨");
        b.apply("start", "subagent", "toolu_8", "");
        assert_eq!(HookActivity::labels(&b.subagents), vec!["첫 라벨"]);
    }
}

/// Collab completion toast + munder-style approval card. `toast` is the
/// "✓ %3 완료" message for a sibling pane's working→idle flip (faded by
/// `collab_toast_alpha`); `toast_action` = Some(pane id) pins it as an
/// approve/deny card whose chip rects (`toast_approve_rect`/`toast_deny_rect`)
/// route a response key to that pane. `attention` is the board `waiting` flag
/// map, shared (Arc) with the socket `PtyBackend`. `unread` badges the board.
#[derive(Default)]
pub(crate) struct CollabState {
    pub(crate) toast: Option<(String, std::time::Instant)>,
    pub(crate) toast_rect: Option<(f32, f32, f32, f32)>,
    pub(crate) toast_action: Option<String>,
    pub(crate) toast_approve_rect: Option<(f32, f32, f32, f32)>,
    pub(crate) toast_deny_rect: Option<(f32, f32, f32, f32)>,
    pub(crate) attention: std::sync::Arc<std::sync::Mutex<HashMap<String, String>>>,
    /// pane → 훅이 보고한 in-flight. `attention` 과 같이 socket `PtyBackend` 와 Arc
    /// 공유 — 쓰는 쪽은 훅(소켓 스레드), 읽는 쪽은 진행 표시(GUI 스레드)다.
    pub(crate) hook_activity: std::sync::Arc<std::sync::Mutex<HashMap<String, HookActivity>>>,
    #[allow(dead_code)] // board badge count — bumped, render/clear lands with sidebar work
    pub(crate) unread: u32,
}
