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
    pub(crate) scroll: f32,
    /// 매 paint 재생성되는 hit target. 행은 `view` 인덱스로 되짚는다.
    pub(crate) row_rects: Vec<(usize, (f32, f32, f32, f32))>,
    /// 행별 지우기 버튼. 행 hit 보다 먼저 본다 — 겹쳐 있다.
    pub(crate) del_rects: Vec<(usize, (f32, f32, f32, f32))>,
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
            scroll: 0.0,
            row_rects: Vec::new(),
            del_rects: Vec::new(),
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
    Ports,
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
    /// 포트 전용 — 브라우저로 `http://localhost:<port>`.
    OpenPort,
    CopyUrl,
}

/// 우클릭 메뉴가 겨눈 대상. 포트도 결국 프로세스를 죽여서 닫으므로 pid 를 함께
/// 들고 다닌다 — 메뉴가 열린 뒤 목록이 갱신돼도 겨눈 대상이 흔들리지 않는다.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum InfoTarget {
    Proc(u32),
    Port(u16, u32),
}

impl InfoTarget {
    pub(crate) fn pid(self) -> u32 {
        match self {
            Self::Proc(pid) | Self::Port(_, pid) => pid,
        }
    }
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
    pub(crate) ports_collapsed: bool,
    /// 우클릭 메뉴 — `(화면 좌표, 대상)`.
    pub(crate) ctx_menu: Option<(f32, f32, InfoTarget)>,
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
    pub(crate) port_rects: Vec<(u16, u32, (f32, f32, f32, f32))>,
    pub(crate) port_kill_rects: Vec<(u16, u32, (f32, f32, f32, f32))>,
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
            ports_collapsed: false,
            ctx_menu: None,
            group_collapsed: std::collections::HashSet::new(),
            pane_expanded: std::collections::HashSet::new(),
            closed_rects: Vec::new(),
            closed_kill_rects: Vec::new(),
            last_group_click: None,
            tab_rects: Vec::new(),
            port_rects: Vec::new(),
            port_kill_rects: Vec::new(),
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
    #[allow(dead_code)] // board badge count — bumped, render/clear lands with sidebar work
    pub(crate) unread: u32,
}
