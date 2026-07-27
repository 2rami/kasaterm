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

/// 우측 칼럼의 활성 탭. 칼럼은 원래 git 전용이었고 Info 가 나중에 붙었다 —
/// 둘은 폭·스크롤·닫기 버튼을 공유하고 본문만 갈린다.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SideTab {
    #[default]
    Git,
    Info,
}

/// Info 탭 — 활성 pane 셸 아래 프로세스 + listen 포트. `rows` 는 워커 스레드가
/// 채우는 스냅샷이고 `busy`/`last_refresh` 가 그 워커를 스로틀한다(수집이 `ps` +
/// `lsof` fork 라 렌더 스레드에서 돌릴 수 없다 — info.rs 참고). `shell_pid` 는
/// 현재 목록이 어느 pane 것인지로, pane 을 옮기면 즉시 갱신을 트리거한다.
pub(crate) struct InfoState {
    pub(crate) tab: SideTab,
    pub(crate) rows: std::sync::Arc<std::sync::Mutex<Vec<crate::info::ProcRow>>>,
    pub(crate) busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(crate) last_refresh: Option<std::time::Instant>,
    pub(crate) shell_pid: Option<u32>,
    pub(crate) scroll: f32,
    /// 매 paint 재생성되는 hit target. 탭 머리 / 포트 칩(→ 브라우저로 열기).
    pub(crate) tab_rects: Vec<(SideTab, (f32, f32, f32, f32))>,
    pub(crate) port_rects: Vec<(u16, (f32, f32, f32, f32))>,
}

impl Default for InfoState {
    fn default() -> Self {
        Self {
            tab: SideTab::Git,
            rows: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            busy: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            last_refresh: None,
            shell_pid: None,
            scroll: 0.0,
            tab_rects: Vec::new(),
            port_rects: Vec::new(),
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
    pub(crate) rename_row_rect: (f32, f32, f32, f32),
    /// 새 항목 생성 부모(우클릭한 폴더). None 이면 트리 root.
    pub(crate) new_parent: Option<std::path::PathBuf>,
    pub(crate) search_active: bool,
    pub(crate) search_query: String,
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
