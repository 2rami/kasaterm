//! GPU 렌더 패스 — App 렌더 메서드(cell-renderer 파이프라인 + chrome 오버레이).
//! main.rs 의 impl App 에서 분리. struct App·자유함수·타입은 crate root 그대로 참조.
use super::*;
pub(crate) use crate::screenread::*;
pub(crate) use crate::sprites::*;

#[path = "pane_identity.rs"]
pub(crate) mod pane_identity;
pub(crate) use pane_identity::machine_tint;
use pane_identity::{MachineIdentity, PaneIdentity};
#[path = "terminal_scene.rs"]
pub(crate) mod terminal_scene;
#[path = "account_popover.rs"]
mod account_popover;
mod file_tree_column;
mod side_column;
mod sidebar;
mod status_bar;

#[derive(Clone, Copy)]
struct AccountSettings<'a> {
    claude: &'a String,
    claude_list: &'a [socket::ClaudeAccount],
    codex: &'a String,
    codex_list: &'a [socket::CodexAccount],
}

/// 슬롯 id → (한도 배지, 로그인 상태). `&self` 메서드로 구하므로 GPU 를 빌리기 전에 읽어 둔다.
type ClaudeObservations = HashMap<String, (Option<crate::UsageBadge>, &'static str)>;

/// ⋮ 메뉴 칸에 올리면 뜨는 이름. `view` 는 보기 전환의 `(셸 칸인가, 켜졌나)`.
fn handle_menu_tip(action: ActionKind, view: (bool, bool)) -> &'static str {
    match action {
        ActionKind::ChatView if view.1 => "터미널로 보기",
        ActionKind::ChatView if view.0 => "명령으로 보기",
        ActionKind::ChatView => "대화로 보기",
        ActionKind::NewTab => "새 탭",
        ActionKind::SplitH => "좌우로 나누기",
        ActionKind::SplitV => "위아래로 나누기",
        ActionKind::ToggleHeader => "상단바",
        ActionKind::ToggleStatusbar => "하단바",
        ActionKind::ToggleZoom => "크게 보기",
        ActionKind::RefreshRenderer => "화면 새로고침",
        ActionKind::Undock => "별도 창으로",
        ActionKind::Close => "닫기",
        _ => "",
    }
}

fn terminal_preedit_for_active<'a>(
    preedit: &'a str,
    owner_surface: Option<&str>,
    active_surface: Option<&str>,
) -> &'a str {
    match owner_surface {
        Some(owner) if active_surface == Some(owner) => preedit,
        _ => "",
    }
}

/// 펼친 방의 pane 줄 하나를 그리는 데 필요한 것 전부 — 페인트 루프가 `g`
/// (=&mut self.gpu) 를 잡고 있어 `self` 를 다시 읽을 수 없으므로 미리 뜬 스냅샷이다.
/// 튜플로 두다 필드가 여섯이 되면서 `.3`/`.4` 가 무엇인지 호출부에서 안 읽혀 이름을 달았다.
/// 카드 덱 한 장이 **누구의 무슨 자리**인지. 덱은 지금까지 장 수만 말하고 누가
/// 들어있는지는 못 말했다(2026-08-24 지시). 계단이 1.5~4.5px 라 얼굴은 못 넣고
/// 색만 들어가므로, 이름은 마우스를 올렸을 때 목록으로 편다.
#[derive(Clone)]
struct TabPeek {
    /// 그 탭의 학생. claude 가 실제로 도는 탭만 갖는다(`display_tab_char` 관문).
    who: Option<String>,
    /// 그 탭의 이름 — 붙인 제목, 없으면 OSC, 없으면 프로세스 이름.
    label: String,
    /// 지금 보이는 탭. 덱의 앞장이자 목록에서 표시할 줄.
    active: bool,
}

struct SidebarRowInfo {
    /// 목록 줄(두 줄)의 글 — 세션 이름과 차례. 둘째 줄 띠·시간은 아래 칸들에서 온다.
    student: crate::sidebar_pulse::RowStudent,
    /// 배정 학생명(얼굴용). claude 가 안 붙은 pane 은 빈 문자열.
    who: String,
    /// 이 칸에서 도는 학생 하네스(claude·codex) — 학생 얼굴이 없을 때 그 자리에 로고를 놓는다.
    agent: String,
    /// 칸 id. 로고 칸이 여럿일 때 색 순번을 가르는 열쇠.
    pane: String,
    /// 줄에 적는 것 — 그 pane 이 지금 무엇인가(claude · zsh · 편집기…).
    label: String,
    /// 오른쪽 끝 상태 점 색(`pane_state_color`).
    color: [u8; 4],
    /// 지금 보고 있는 pane.
    is_cur: bool,
    /// 못 본 완료 — 줄 끝 점이 accent 로 깜빡인다.
    alert: bool,
    /// 승인·입력을 기다리는 중 — 줄 끝 점이 주황으로, 두 배 빠르게 깜빡인다.
    waiting: bool,
    /// 사람이 이 방을 보러 와서 깜빡임을 멈춘 pane — 위 두 표시가 깜빡이지 않고 멈춰 선다.
    quiet: bool,
    /// 에이전트가 오류 복구를 기다리는 중 — 미니맵에만 빨간 삼각형으로 표시한다.
    error: bool,
    /// 지금 도는 중 — 학생이 걷는다. 기다리는 중은 여기 안 든다(그건 멈춘 것이다).
    busy: bool,
    /// 사이드바에서 숨긴 pane — 화면엔 없지만 PTY 는 돈다. 흐리게 + 아이콘으로
    /// 그려 「없는 것」이 아니라 「치워 둔 것」임을 말한다.
    stashed: bool,
    /// 학생 얼굴이 없을 때 칸이 무엇인지 말하는 아이콘 — 웹 pane 은 globe,
    /// 이미지는 image, md 는 file-text, 그 외 terminal. 이게 없으면 미니맵이
    /// 웹 pane 도 터미널이라고 거짓말한다.
    icon: &'static str,
    /// pane **안의 탭 수**. 배치도 칸은 pane 하나당 하나뿐이고 활성 탭만
    /// 대표하므로, 이게 없으면 한 pane 에 학생이 셋 들어 있어도 화면 어디에도
    /// 흔적이 없다(사용자 2026-08-20 「탭 안에 있으면 … 미니맵에 겹친다든지」).
    /// 1 이면 지금까지와 완전히 같은 그림 — 거의 모든 pane 이 그렇다.
    ///
    /// **모든 탭이 한 자리씩 차지한다.** 학생을 못 찾는 탭(이미지·md·웹, 아직
    /// claude 가 안 뜬 자리)도 `who: None` 으로 남는다 — 빼면 장 수가 틀어지고,
    /// 덱은 장 수부터 말하는 그림이다.
    tab_peeks: Vec<TabPeek>,
    /// 눈에 보이는 일은 없는데 백그라운드 셸·Monitor 가 도는 중.
    /// **`busy` 와 배타적이다** — `refresh_pane_activity` 가 `busy` 면 아예
    /// false 를 넣는다. 그래서 이걸 배치도 칸에 그려도 걷기와 겹칠 수가 없다.
    bg_active: bool,
    /// compact 진행률(%). 걷기로는 「얼마나 남았나」가 절대 안 읽히므로
    /// 이것만은 걷는 칸에도 함께 그린다 — 되풀이가 아니라 걷기가 못 하는 말이다.
    compact_pct: Option<u8>,
    /// 지금 도는 일이 시작된 뒤로 흐른 초(`busy_since`, input.rs 가 잡는다).
    /// 걷기·쓸림바는 **도는 중이라는 사실**만 말해서, 2분짜리와 40분짜리가
    /// 화면에서 완전히 같아 보였다(사용자 2026-08-24). 기다리는 중은 여기 안 든다
    /// — 그건 멈춘 것이고, 멈춘 시간이 차오르면 「일하는 줄」 알고 지나친다.
    ///
    /// Instant 가 아니라 **초로 접어** 싣는다. 이 구조체는 페인트 직전 스냅샷이라
    /// 한 프레임 안에서 두 번 재면 두 값이 나오고, 그러면 같은 pane 의 칸과 줄이
    /// 서로 다른 시간을 말할 수 있다.
    busy_secs: Option<u64>,
    /// 원격 pane 이면 그 기계 이름. 페인트 루프는 self 를 못 읽으므로(2887 주석)
    /// 여기서 프레임당 1회 떠 둔다.
    machine: Option<String>,
    /// 배치도 칸을 물들일 기기 이름 — 로컬 pane 도 든다. 헤더 칩과 **같은 이름
    /// 풀이**(`MachineIdentity::for_pane`)를 쓴다: 링크에 적힌 이름과 명부 이름이
    /// 다르면 같은 기계가 헤더와 배치도에서 다른 색이 된다(2026-09-10 지적).
    device: Option<String>,
}

/// 도는 시간을 칸에 얹을 짧은 말로. **1분 미만은 None** — 잠깐 도는 일에까지 숫자가
/// 붙으면 배치도가 시계판이 되고, 정작 갈라 보이고 싶던 「오래 도는 것」이 그 숫자들
/// 사이에 묻힌다(사용자 2026-08-24 「오래 걸릴수록 눈에 띄게」).
///
/// **두 시간까지 분으로 버틴다.** 「90분」이 「1시간」보다 정보가 많고, 무엇보다
/// 한 시간에서 단위를 갈면 99분 다음이 「1시간」이 되어 **화면의 숫자가 거꾸로
/// 간다** — 오래 도는 것을 갈라 보려고 붙인 표시가 정작 그 자리에서 뒤집힌다.
/// 두 시간에서 갈면 「119분 → 2시간」이라 값이 줄지 않는다.
///
/// 시간대는 내림이다(올림하면 2시간 1분이 「3시간」이 되어 실제보다 오래 도는
/// 것처럼 읽힌다).
fn elapsed_label(secs: u64) -> Option<String> {
    match secs {
        s if s < 60 => None,
        s if s < 7200 => Some(format!("{}분", s / 60)),
        s => Some(format!("{}시간", s / 3600)),
    }
}

/// 배치도 칸 바닥의 진행 띠 — 높이와 그 아래 여백.
pub(crate) const MINI_BAR_H: f32 = 2.0;
pub(crate) const MINI_BAR_PAD: f32 = 2.0;
/// 걷는 학생 상자가 얼굴보다 큰 몫(위 2 + 아래 2). 원본이 정사각 전신이라
/// `draw_student_walk` 은 얼굴 상자를 4px 키워 그린다 — 자리를 셈할 때도 그만큼 본다.
const MINI_WALK_PAD: f32 = 4.0;

/// 그 칸에 띠를 그릴 수 있나. **얼굴 배치와 띠 그리기가 같은 답을 써야 한다** —
/// 갈리면 자리를 안 비운 칸에 띠가 서거나(발밑에 겹친다) 빈 자리에 아무것도 안 온다.
///
/// 높이 하한이 띠·여백·걷기 상자(얼굴 최소 10 + 4)의 합이다. 그보다 좁은 칸은
/// 띠를 포기한다 — 2px 띠를 우겨넣느니 학생이 온전한 쪽이 낫고, 그런 칸은 얼굴이
/// 이미 칸을 꽉 채워 띠가 읽히지도 않는다.
pub(crate) fn minimap_has_bar(mw: f32, mh: f32) -> bool {
    mw > 10.0 && mh > MINI_BAR_H + MINI_BAR_PAD + 10.0 + MINI_WALK_PAD
}

/// 칸 안 얼굴 상자 `(x, y, 변)` — 띠 자리를 뺀 나머지의 중앙.
///
/// 띠가 **실제로 서는 동안만** 자리를 빼면 걷기 시작·끝마다 얼굴이 튀므로, 띠를
/// 그릴 수 있는 칸이면 도는 중이든 아니든 늘 뺀다.
/// 점선 사각 — 배치도의 「별도창」 칸 테두리. 실선 칸(트리 안)과 한눈에 갈리도록
/// 3px 긋고 2px 쉰다. 얇은 1px 선을 gpu.rect 조각으로 잇는다.
pub(crate) fn dashed_rect(g: &mut gpu::GpuRenderer, x: f32, y: f32, w: f32, h: f32, col: [u8; 4]) {
    const ON: f32 = 3.0;
    const OFF: f32 = 2.0;
    let mut t = 0.0;
    while t < w {
        let seg = ON.min(w - t);
        g.rect(x + t, y, seg, 1.0, col);
        g.rect(x + t, y + h - 1.0, seg, 1.0, col);
        t += ON + OFF;
    }
    let mut t = 0.0;
    while t < h {
        let seg = ON.min(h - t);
        g.rect(x, y + t, 1.0, seg, col);
        g.rect(x + w - 1.0, y + t, 1.0, seg, col);
        t += ON + OFF;
    }
}

pub(crate) fn minimap_face_box(mx: f32, my: f32, mw: f32, mh: f32) -> (f32, f32, f32) {
    let room = if minimap_has_bar(mw, mh) {
        MINI_BAR_H + MINI_BAR_PAD
    } else {
        0.0
    };
    let face = (mw.min(mh - room) - 8.0).clamp(10.0, 26.0);
    (mx + (mw - face) / 2.0, my + (mh - room - face) / 2.0, face)
}

/// 오래 도는 것일수록 눈에 띄게 — 색과 굵기로 세 단(사용자 2026-08-24 조건).
/// 흐린 회색으로 시작해 accent 굵은 글씨로 끝난다. 크기를 키우는 길도 있었지만
/// 칸이 40px 대라 한 단만 키워도 얼굴을 밀어낸다.
fn elapsed_style(secs: u64) -> ([u8; 4], bool) {
    match secs {
        s if s < 600 => (theme::text_mute(), false),
        s if s < 1800 => (theme::text_dim(), false),
        _ => (theme::accent(), true),
    }
}

/// 진행 띠 끝에 붙는 경과 시간 `(글, 색, 굵게)` — 도는 중이거나 백그라운드가 돌 때만.
/// 배치도 칸과 사이드바 목록 줄이 같은 값을 같은 단계로 보이게 한 곳에서 판다.
pub(crate) fn elapsed_mark(busy_secs: Option<u64>, running: bool) -> Option<(String, [u8; 4], bool)> {
    let secs = busy_secs.filter(|_| running)?;
    let (color, bold) = elapsed_style(secs);
    Some((elapsed_label(secs)?, color, bold))
}

/// 배치도 칸 신호 채움의 진하기 — 깜빡이거나(`period` 초), 사람이 그 방을 보러 와 멈췄으면 깜빡임의
/// 가운데쯤에 멈춰 선다. 상태(기다림·못 본 완료)는 남고 움직임만 멎는다.
fn signal_alpha(quiet: bool, period: f32) -> u8 {
    if quiet {
        90
    } else {
        (30.0 + 120.0 * blink(anim_phase_secs(), period)) as u8
    }
}

/// compact 진행 띠 한 줄 — 배치도 칸·별도창 칸·사이드바 목록 줄이 같은 모양을 쓴다. 끝이 있는 일이라
/// 차오르는 눈금으로 말한다. 끝을 모르는 「도는 중」은 띠가 아니라 `activity_mark` 의 움직이는 테두리다.
pub(crate) fn progress_bar(g: &mut gpu::GpuRenderer, x: f32, y: f32, w: f32, compact_pct: Option<u8>) {
    if let Some(pct) = compact_pct {
        g.rect(x, y, w, MINI_BAR_H, theme::with_alpha(theme::accent(), 0x3a));
        let done = w * (pct as f32 / 100.0).clamp(0.0, 1.0);
        if done > 0.5 {
            g.rect(x, y, done, MINI_BAR_H, theme::accent());
        }
    }
}

/// 배치도 칸·별도창 칸·사이드바 목록 줄의 「도는 중」 — 그 칸(줄) 윤곽을 pane 테두리와 같은 결로 빛 조각이
/// 돌거나(일하는 중) 점선이 흐른다(뒤에서 도는 중). 고른 칸은 또렷하게, 나머지는 옅게. `inside` 는 칸 가장자리에
/// 이미 그어 둔 멈춘 테두리(고른 칸 표시)의 굵기 — 움직이는 윤곽은 그 안쪽을 돈다. 같은 색 실선 위에 겹치면
/// 꼬리가 묻혀 「여기」와 「도는 중」이 한 줄로 뭉친다.
///
/// `bg_active` 는 `refresh_pane_activity` 가 busy 일 때 false 로 넣으니(input.rs) busy 와 겹칠 일이 없다.
/// 대기 중은 부르지 않는다 — 멈춘 것인데 움직이면 「일하는 줄」 알고 지나친다.
pub(crate) fn activity_mark(
    g: &mut gpu::GpuRenderer,
    (x, y, w, h): (f32, f32, f32, f32),
    radius: f32,
    busy: bool,
    bg_active: bool,
    focused: bool,
    inside: f32,
) {
    let kind = if busy {
        theme::Activity::Working
    } else if bg_active {
        theme::Activity::Background
    } else {
        return;
    };
    let rect = (x + inside, y + inside, w - 2.0 * inside, h - 2.0 * inside);
    let col = theme::beside_still_edge(theme::accent(), inside > 0.0);
    g.activity_outline(rect, (radius - inside).max(0.0), col, theme::activity_edge(kind, focused, true));
}

impl App {

    /// Phase 2a path. Collects every pane's live cell grid and hands
    /// it to the cell-renderer pipeline. Chrome (sidebar, tabs,
    /// headers, cursor block, selection, preedit) is intentionally
    /// not drawn yet — Phase 2b+ will reattach those via the same
    /// pipeline / atlas.
    /// Self-only snapshot used by `kasa_gridview::overlay::paint`. Built before
    /// we borrow `self.gpu` mutably so the renderer pass can run
    /// without a re-entrant `&self` read. All coordinates here are
    /// already cell-space — the renderer-side helper applies cell
    /// metric multiplication.
    fn gpu_overlay_snapshot(&self) -> GpuOverlay {
        // 터미널 오버레이는 조합기 주인이 터미널일 때만 그린다. 편집기가 조합 중인데
        // 포커스만 터미널로 옮겨진 순간(클릭 직후, 아직 아무 키도 안 친 상태)
        // 남의 조합 글자를 터미널 커서에 그리게 된다.
        //
        // **화이트리스트로 둔다.** 크롬 쪽 입력칸(git 커밋·파일트리·방 이름·경로 검색)은
        // 전부 자기 자리에 프리에딧을 그리므로, 빠뜨린 갈래가 하나라도 있으면 같은 글자가
        // 그 칸과 터미널 커서에 이중으로 뜬다. 목록에 없는 새 필드가 생겨도 안 새게.
        let active_surface = self.target_surface();
        let terminal_owner = self.os_ime_surface.as_deref().or_else(|| {
            self.ime_focus
                .as_ref()
                .and_then(crate::ImeFocus::terminal_surface)
        });
        let preedit_text = terminal_preedit_for_active(
            &self.preedit,
            terminal_owner,
            active_surface.as_deref(),
        )
        .to_string();
        let commit_overlay = self.commit_overlay.clone();
        // Active pane's font multiplier — the overlay anchors to this same
        // pane (see pane_origin below), so its cell size must match the
        // pane's zoomed glyphs, not the base grid.
        let pane_font_scale = self
            .target_pane()
            .map(|id| self.pane_display_scale(&self.ws.lock().unwrap(), &id))
            .unwrap_or(1.0);
        let snap = {
            let ws = self.ws.lock().unwrap();
            // Active pane's top-left in cell units. When the workspace is
            // split the cursor/preedit overlay must anchor to THIS pane,
            // not the global origin (which is the left/top pane).
            //
            // 원점은 **실제로 그린 rect**(effective_leaf_rects)에서 온다 —
            // ws.layout 의 split 좌표는 줌을 모르기 때문이다. 줌한 pane 은 원래
            // 자리가 아니라 작업영역 한가운데 들여 그려지므로, split 좌표를 쓰면
            // 커서·조합 오버레이만 옛 자리에 남아 화면 어딘가로 사라진다.
            // (아래쪽 pane 을 줌하면 예전에도 어긋났고, inset 이 생기며 항상
            // 드러났다.) leaf_rects 의 id 는 이미 `%n` 꼴이라 변환이 없다.
            let (gcols, grows) = self.window_cells();
            let pane_origin = ws
                .active_pane
                .as_ref()
                .and_then(|aid| {
                    self.effective_leaf_rects(gcols, grows)
                        .into_iter()
                        .find(|(id, ..)| id == aid)
                        .map(|(_, x, y, _, _)| (x, y))
                })
                .unwrap_or((0u16, 0u16));
            ws.active_pane.clone().and_then(|id| {
                ws.panes.get(&id).map(|pane| {
                    let tab_pid = ws.active_tab_pid(&id);
                    let cursor_color = ws
                        .pane_character
                        .get(&tab_pid)
                        .and_then(|name| {
                            theme::character_accent_n(
                                name,
                                theme::character_ordinal(&ws.pane_character, &tab_pid),
                            )
                        })
                        .unwrap_or_else(theme::cursor);
                        let cursor_color = theme::cursor_on_pane(cursor_color);
                    // Preedit sits exactly on the reported PTY cursor —
                    // that's where the next char lands. We used to bump
                    // the column to the row's last filled cell to dodge
                    // tail padding, but a TUI's grey placeholder ("Type
                    // something") counts as filled, so that dragged the
                    // composing syllable past it to the line's end. The
                    // cursor column is already correct (incl. trailing
                    // spaces the PTY echoes), so trust it directly.
                    // Image/markdown panes have no PTY cursor — their terminal
                    // block cursor stays hidden (the Raw editor draws its own).
                    let (raw_row, raw_col, source_vis, source_cols, cur_w) = match pane.term() {
                        Some(t) => (
                            t.cursor_row,
                            t.cursor_col,
                            t.cursor_visible,
                            t.cells.first().map(|r| r.len()).unwrap_or(80) as u16,
                            cursor_cell_width(&t.cells, t.cursor_row, t.cursor_col),
                        ),
                        None => (0, 0, false, 80, 1),
                    };
                    let shift = self.pane_view_shift.get(&id);
                    let position = shift.map(|view| view.display_pos(raw_row as usize, raw_col as usize))
                        .unwrap_or(Some((raw_row as usize, raw_col as usize)));
                    let (cur_row, cur_col) = position.map(|(row, col)| (row as u16, col as u16))
                        .unwrap_or((0, 0));
                    // 대화로 보는 pane 은 격자를 안 그린다 — 커서 블록만 허공에 뜨면 안 된다.
                    let cur_vis = source_vis && position.is_some() && !self.chat_view_showing(&id)
                        && !self.shell_view_showing(&id);
                    let cols = shift.and_then(|view| view.projection.as_ref())
                        .and_then(|projection| projection.rows.first())
                        .map(|row| row.len().min(u16::MAX as usize) as u16)
                        .unwrap_or(source_cols);
                    let (base_row, base_col) = (cur_row, cur_col);
                    // Until the committed syllable's echo lands (cursor
                    // still where it was at commit time), draw the
                    // committed text in front of the preedit at that spot.
                    //
                    // PTY 가 없는 pane(편집기·이미지)은 위에서 (0,0) 을 기본으로
                    // 받는다. 거기에 조합 문자열을 그리면 **줄번호 거터 위에
                    // 유령**이 뜨고, raw 편집기는 자기 문서 캐럿에 preedit 을
                    // 따로 그리므로 같은 글자가 두 군데 보인다(사용자: "입력이
                    // 동시에 되고"). 터미널 오버레이는 터미널일 때만 그린다.
                    let (display, prow, pcol) = match &commit_overlay {
                        _ if pane.term().is_none() || position.is_none() => (String::new(), base_row, base_col),
                        Some((ctext, before, owner))
                            if active_surface.as_deref() == Some(owner.as_str())
                                && *before == (raw_row, raw_col) =>
                        {
                            (format!("{ctext}{preedit_text}"), base_row, base_col)
                        }
                        _ => (preedit_text.clone(), base_row, base_col),
                    };
                    (
                        cur_row,
                        cur_col,
                        cur_vis,
                        cols,
                        cur_w,
                        prow,
                        pcol,
                        display,
                        pane_origin.0,
                        pane_origin.1,
                        pane.header_px(),
                        cursor_color,
                    )
                })
            })
        };
        let (
            cursor_row,
            cursor_col,
            cursor_visible,
            cols,
            cursor_w,
            preedit_row,
            preedit_col,
            preedit,
            pane_x,
            pane_y,
            header_shift,
            cursor_color,
        ) = snap.unwrap_or((
            0,
            0,
            false,
            80,
            1,
            0,
            0,
            preedit_text.clone(),
            0,
            0,
            0.0,
            theme::cursor(),
        ));
        // When split OR any pane is multi-tab, every pane body is pushed
        // down by its header band. The cursor / preedit / selection
        // overlays anchor off the same origin as the cells, so they must
        // apply the identical shift — otherwise the cursor floats up into
        // the header row (which is exactly what made it appear one line
        // above the actual prompt after a cross-pane tab drop).
        // header_shift = active pane 의 header_px(snap 에서 가져옴). 헤더 있는
        // pane 은 커서/조합(IME)/선택 오버레이도 셀과 똑같이 헤더만큼 내려간다.
        GpuOverlay {
            cell_w: self.cell.w,
            cell_h: self.cell.h,
            pad_x: WINDOW_PADDING
                + self.effective_sidebar_w()
                + pane_x as f32 * self.cell.w
                + PANE_INNER_X,
            pad_y: TITLE_HEIGHT + pane_y as f32 * self.cell.h + header_shift + PANE_INNER_Y,
            cursor_row,
            cursor_col,
            cursor_w,
            cursor_shape: self.cursor_shape,
            cursor_thickness: self.cursor_thickness,
            cursor_color,
            cursor_visible,
            cols,
            blink_on: self.cursor_blink_on(Instant::now()),
            preedit,
            preedit_row,
            preedit_col,
            font_scale: pane_font_scale,
            selection: self.selection,
            suggestion: if active_surface.as_deref().is_some_and(|id| self.chat_view_showing(id) || self.shell_view_showing(id))
                || self.target_pane().is_some_and(|id| self.chat_view_showing(&id) || self.shell_view_showing(&id))
            {
                String::new()
            } else {
                self.current_suggestion.clone().unwrap_or_default()
            },
        }
    }

    /// OS IME 후보창을 커서 칸에 붙인다. macOS 는 플랫폼 IME 자체를 끄고 우리가
    /// 조합하므로 해당 없고(`set_ime_allowed(false)`), 켜 두는 Windows·Linux 만
    /// 대상이다. 한 번도 안 부르면 좌표가 클라이언트 영역 원점이라 한자 변환·
    /// 후보 목록이 창 왼쪽 위 구석에 뜬다 — 조합 중인 글자와 딴 데.
    ///
    /// 좌표는 물리 픽셀(`self.cell` 이 이미 scale 반영). 프레임마다 Win32 를
    /// 때리지 않도록 칸이 바뀔 때만 보낸다.
    #[cfg(target_os = "macos")]
    fn sync_ime_cursor_area(&mut self, _ov: &GpuOverlay) {}

    #[cfg(not(target_os = "macos"))]
    fn sync_ime_cursor_area(&mut self, ov: &GpuOverlay) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let cw = ov.cell_w * ov.font_scale;
        let ch = ov.cell_h * ov.font_scale;
        // 조합 중이면 조합 시작 칸, 아니면 커서 칸.
        let (row, col) = if ov.preedit.is_empty() {
            (ov.cursor_row, ov.cursor_col)
        } else {
            (ov.preedit_row, ov.preedit_col)
        };
        let x = (ov.pad_x + col as f32 * cw).round() as i32;
        let y = (ov.pad_y + row as f32 * ch).round() as i32;
        if self.ime_cursor_px == Some((x, y)) {
            return;
        }
        self.ime_cursor_px = Some((x, y));
        window.set_ime_cursor_area(
            winit::dpi::PhysicalPosition::new(x, y),
            winit::dpi::PhysicalSize::new(cw.round() as u32, ch.round() as u32),
        );
    }


    /// `surface.capture` — pane 한 칸만 잘라 다음 프레임에 PNG 로 굽도록 무장한다.
    ///
    /// 좌표는 렌더가 셀을 놓는 식 그대로다(`render_frame_gpu` 의 pad_x/pad_y 와 같은
    /// 원점, 안쪽 여백 PANE_INNER 만 빼고 pane 상자 전체). 헤더 띠는 폐기돼(ghostty식,
    /// `layout.rs` 히트테스트 참조) 세로 보정이 없다. 프레임버퍼는 물리 픽셀이라
    /// 마지막에 scale 을 곱한다 — 안 곱하면 HiDPI 에서 좌상단 1/4 만 잘린다.
    ///
    /// ★ `pane` 이 **빈 문자열이면 창 전체**다. 프레임버퍼에는 사이드바·탭바·우측 칼럼이
    /// 이미 다 그려져 있고 pane 캡처가 위 오프셋으로 **일부러 잘라내던 것**이라, 크롭을
    /// 안 세우면(`capture_crop = None`) 그대로 창 한 장이 나온다. 에이전트가 제 UI 를
    /// 보려면 이 길이 필요하다 — pane 만 찍혀서는 사이드바가 어떻게 보이는지 알 수 없다.
    ///
    /// 알림 배너처럼 자체 렌더러를 가진 임시 창은 이 서피스에 없다.
    pub(crate) fn arm_pane_capture(
        &mut self,
        pane: &str,
        path: Option<String>,
        max_w: u32,
        reply: std::sync::mpsc::Sender<std::result::Result<serde_json::Value, String>>,
    ) {
        if self.gpu.is_none() {
            let _ = reply.send(Err("capture needs the gpu renderer".into()));
            return;
        }
        // 프레임당 한 장만 읽는다. 헤드리스 autocapture 와 겹치면 그쪽이 먼저다 —
        // 덮어쓰면 그 검증이 엉뚱한 pane 을 찍고 조용히 통과한다.
        if self.gpu.as_ref().is_some_and(|g| g.capture_next.is_some()) {
            let _ = reply.send(Err("another capture is already armed; retry".into()));
            return;
        }
        // 창 전체는 크롭을 안 세운다 — 아래 pane 갈래가 잘라내던 것을 안 자르는 것뿐이다.
        let crop = if pane.is_empty() {
            None
        } else {
            let (gcols, grows) = self.window_cells();
            let Some((_, cx, cy, cw, ch)) = self
                .effective_leaf_rects(gcols, grows)
                .into_iter()
                .find(|(id, ..)| id == pane)
            else {
                // 줌 중이면 가려진 pane 은 rect 가 아예 없다. 「없는 pane」과 구분해
                // 답해야 부른 쪽이 줌을 풀 생각을 한다.
                let zoomed = self.zoomed_pane.is_some();
                // 활성 배치에 없어도 격자가 살아 있으면(다른 방의 pane) 화면 밖
                // 렌더로 **진짜 그림**을 찍는다 — termshot 폴백으로 밀리던 자리
                // (2026-09-02 나쵸 알림 사진 건). 줌은 기존 안내가 더 정확하다.
                if !zoomed {
                    if let Some(res) = self.capture_pane_offscreen(pane, path.clone(), max_w) {
                        let _ = reply.send(res);
                        return;
                    }
                }
                let _ = reply.send(Err(if zoomed {
                    format!("{pane} is hidden behind a zoomed pane")
                } else {
                    format!("no such pane: {pane}")
                }));
                return;
            };
            let s = self.effective_scale();
            let px = (WINDOW_PADDING + self.effective_sidebar_w() + cx as f32 * self.cell.w) * s;
            let py = (TITLE_HEIGHT + cy as f32 * self.cell.h) * s;
            let pw = (cw as f32 * self.cell.w * s).round().max(1.0) as u32;
            let ph = (ch as f32 * self.cell.h * s).round().max(1.0) as u32;
            Some((px.max(0.0) as u32, py.max(0.0) as u32, pw, ph))
        };
        let path = path.unwrap_or_else(|| {
            let name = if pane.is_empty() {
                "kasaterm-window.png".to_string()
            } else {
                format!("kasaterm-capture-{}.png", pane.trim_start_matches('%'))
            };
            std::env::temp_dir()
                .join(name)
                .to_string_lossy()
                .into_owned()
        });
        if let Some(g) = self.gpu.as_mut() {
            g.capture_crop = crop;
            g.capture_max_w = max_w;
            g.capture_next = Some(path.clone());
        }
        self.pending_capture_reply.push((path, reply));
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// 무장한 캡처가 실제로 파일로 떨어졌는지 확인하고 회신한다(렌더 직후 호출).
    pub(crate) fn settle_pane_captures(&mut self) {
        if self.pending_capture_reply.is_empty() {
            return;
        }
        // 아직 안 그려진 요청이 남아 있으면 이번엔 건너뛴다 — 무장이 그대로면
        // 렌더가 소비하지 않은 것이다.
        if self.gpu.as_ref().is_some_and(|g| g.capture_next.is_some()) {
            return;
        }
        for (path, reply) in std::mem::take(&mut self.pending_capture_reply) {
            let msg = match std::fs::metadata(&path) {
                Ok(m) if m.len() > 0 => Ok(serde_json::json!({
                    "path": path,
                    "bytes": m.len(),
                })),
                Ok(_) => Err(format!("capture wrote an empty file: {path}")),
                Err(e) => Err(format!("capture produced no file ({path}): {e}")),
            };
            let _ = reply.send(msg);
        }
    }

    /// 다른 방(비활성 window) pane 의 격자를 화면 밖에서 그려 PNG 로 — 기존
    /// capture(활성 프레임 크롭)가 원리적으로 못 찍는 자리의 진짜 촬영.
    /// `None` = 이 경로도 모른다(격자 없음·gpu 없음) — 부른 쪽이 기존 오류로.
    pub(crate) fn capture_pane_offscreen(
        &mut self,
        pane: &str,
        path: Option<String>,
        max_w: u32,
    ) -> Option<std::result::Result<serde_json::Value, String>> {
        let (snap, images) = {
            let ws = self.ws.lock().unwrap();
            ws.panes
                .get(pane)
                .and_then(|p| p.term().map(|t| (t.cells.clone(), t.inline_images.clone())))
        }?;
        let rows = snap.len() as u32;
        let cols = snap.iter().map(Vec::len).max().unwrap_or(0) as u32;
        if rows == 0 || cols == 0 {
            return Some(Err(format!("{pane} 의 격자가 비어 있다")));
        }
        // gpu 를 빌리기 전에 스칼라를 다 떠 둔다(차용 규칙).
        let fs = self.pane_font_scales.get(pane).copied().unwrap_or(1.0);
        let s = self.effective_scale();
        let w = (cols as f32 * self.cell.w * fs * s).round().max(1.0) as u32;
        let h = (rows as f32 * self.cell.h * fs * s).round().max(1.0) as u32;
        let path = path.unwrap_or_else(|| {
            std::env::temp_dir()
                .join(format!(
                    "kasaterm-capture-{}.png",
                    pane.trim_start_matches('%')
                ))
                .to_string_lossy()
                .into_owned()
        });
        let default_fg = crate::cells::default_fg();
        let source = crate::mirror_theme::pane_source_palette(pane);
        // 화면 밖 렌더는 원본 글자판 그대로라 입력창 옮김이 없다(dy 0). 키 머리를 따로 두어
        // 본 화면 캐시와 섞지 않고 캡처가 끝나면 통째로 놓는다.
        const KEY_PREFIX: &str = "offscreen-inline:";
        let cell = (self.cell.w * fs, self.cell.h * fs);
        let inline: Vec<_> = images
            .iter()
            .map(|v| {
                let key = format!("{KEY_PREFIX}{}:{}", v.id, v.path);
                terminal_scene::inline_slot(v, key, (0.0, 0.0), cell, rows as usize, 0)
            })
            .collect();
        let g = self.gpu.as_mut()?;
        let slot = gpu::PaneSlot {
            rows: &snap,
            origin_px: (0.0, 0.0),
            font_scale: fs,
            dim: false,
            links: Vec::new(),
            default_fg,
            source,
        };
        let rendered = g.render_cells_offscreen(&[slot], w, h, &path, max_w, |g| {
            kasa_gridview::images::paint_once(g, &inline)
        });
        if !inline.is_empty() {
            g.drop_images_with_prefix(KEY_PREFIX);
        }
        Some(match rendered {
            Ok((ow, oh)) => {
                let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                Ok(serde_json::json!({
                    "path": path,
                    "bytes": bytes,
                    "offscreen": true,
                    "width": ow,
                    "height": oh,
                }))
            }
            Err(e) => Err(e),
        })
    }

    fn render_frame_gpu(&mut self, scale: f32, time_secs: f32) {
        // 두 하단바 높이는 설정값이라 프레임 내내 여러 번 읽힌다. 여기서 한 번
        // 뽑아 두는 건 값이 도중에 바뀔 일이 없어서이기도 하지만, 아래쪽 대부분이
        // `self.gpu` 를 빌린 안쪽이라 거기서 `&self` 메서드를 다시 못 부르기 때문이다.
        let status_h = self.status_h();
        let pane_footer_h = self.pane_footer_h();
        // Glyph-atlas repack, if one is pending. This is the only safe point
        // for it: from here on the frame emits quads whose UVs index the
        // current packing, so a repack mid-frame would show them whatever
        // texels land in those slots instead. Everything this frame needs
        // re-bakes below.
        if let Some(g) = self.gpu.as_mut() {
            g.maintain_atlas();
        }
        // Keep the header breadcrumb's cwd cache fresh (self-rate-limited).
        self.refresh_pane_cwds();
        // File-tree column follows the active pane's cwd (rebuild on change).
        if self.file_tree.visible {
            self.refresh_file_tree();
        }
        // Git column follows the same active-pane cwd; publish it so the
        // off-thread poller refreshes the right repo.
        self.publish_git_col_cwd();
        // Info 탭이 열려 있으면 프로세스·포트 스냅샷을 갱신(스로틀은 내부에서).
        self.pump_info();
        self.pump_sessions_col();
        self.pump_mcp_col();
        // Every pane's status bar wants its own repo badge — feed all pane cwds
        // to the same poller.
        self.publish_pane_git_cwds();
        // Mirror each pane's cwd+badge for the BA GUI's `/layout` (Warp bar on
        // plain terminal tiles). Reads the caches above; no extra git/lsof.
        self.publish_pane_status();
        let Some(window) = self.window.as_ref() else {
            return;
        };
        // Snapshot for the launch banner before the &mut self.gpu borrow
        // below (which rules out re-borrowing &self inside that block).
        let win_size = window.inner_size();
        let win_px = (win_size.width as f32, win_size.height as f32);
        let version_alpha = self.version_alpha();
        // URL under the mouse right now (pane id + cell range). Hovering it
        // draws a blue underline; computed before the workspace lock below so
        // it doesn't re-enter it. None when the cursor isn't over a link.
        let hovered_link = self.link_hit(self.cursor_px.0, self.cursor_px.1);
        let cell_w_px = self.cell.w * scale;
        let cell_h_px = self.cell.h * scale;
        // Snapshot per-pane cell grids while we hold the workspace
        // lock so the render call below can run without re-locking
        // (matches the sugarloaf path's design).
        struct PaneSlot {
            rows: Vec<Vec<GridCell>>,
            origin_px: (f32, f32),
            dim: bool,
            font_scale: f32,
            /// The single URL range under the mouse, if it's in this pane —
            /// drawn as a blue hover underline. Empty otherwise (links only
            /// show on hover, not always-on).
            links: Vec<crate::links::LinkSpan>,
            /// pane 기본 전경색(tmux window-style fg 등가) — 학생 pane 은 accent
            /// 틴트, 무배정은 테마 default fg. slot 빌드 시 pane 당 1회 결정.
            default_fg: [u8; 4],
            /// 거울 pane 의 원본 팔레트(없으면 None) — gpu::PaneSlot::source.
            source: Option<crate::cells::SourcePalette>,
        }
        // Header chrome carried in LOGICAL px — gpu.rect/draw_text
        // promote to physical internally, matching the cell pass.
        #[allow(dead_code)]
        struct HeaderInfo {
            /// BSP leaf used for layout, tabs, and header geometry.
            id: String,
            /// The actual PTY behind the currently visible tab. Process-bound
            /// state such as Codex rollout and account restart status uses it,
            /// rather than the enclosing BSP leaf id.
            active_tab_pid: String,
            x: f32,
            y: f32,
            w: f32,
            /// Full pane box height (header + body) in logical px, used
            /// to draw the divider / active-focus ring around the pane.
            box_h: f32,
            label: String,
            is_active: bool,
            color: Option<[u8; 4]>,
            /// Markdown panes get Render/Raw toggle pills in the header.
            is_markdown: bool,
            /// Markdown, code and plain-text panes share the source editor.
            is_editor: bool,
            /// Current markdown mode (true = Raw editor) for pill highlighting.
            md_raw_mode: bool,
            /// The editor buffer differs from its last successful save.
            md_modified: bool,
            /// Image panes get zoom/rotate buttons instead of the terminal-action cluster.
            is_image: bool,
            /// Web panes get back/forward/reload/open-external instead of the
            /// terminal cluster — split/statusbar buttons don't fit a browser.
            is_web: bool,
            /// 웹 pane 의 현재 주소(활성 탭) — 헤더 주소 pill 표시용. 페이지
            /// 이동을 따라간다(webpane 의 500ms 주소 폴링이 WebPane.url 갱신).
            web_url: Option<String>,
            /// 웹 pane 페이지 로딩 중 — 헤더 작업 바를 켜고 리로드 버튼을
            /// ×(정지)로 바꾼다.
            web_loading: bool,
            /// In-pane tab labels (empty = single-tab; header shows `label`).
            tabs: Vec<String>,
            /// Active tab index into `tabs`.
            active_tab: usize,
            /// Overflow windowing: first tab drawn (pane.tab_first snapshot).
            tab_first: usize,
            /// `active_tab` at the previous frame — a mismatch means a tab
            /// switch happened and the strip must reveal the new active tab.
            tab_last_active: usize,
            /// True while this pane is working (daemon transcript watcher sees a
            /// running tool, cross-window). Draws the flowing bar along the
            /// header bottom; idle panes draw nothing.
            busy: bool,
            /// True when a background shell / Monitor is in-flight but no spinner
            /// shows (from the transcript tail, not the glyph scan). When `busy`
            /// is false, this draws the slower pulse bar instead.
            bg_active: bool,
            /// True while claude compacts its conversation. `busy` 도 같이 참이지만
            /// 이쪽이 이기고 채워지는 바를 그린다 — compact 는 끝이 있는 작업이라
            /// 쓸림바로는 「얼마나 남았나」가 안 읽히고, 화면에 뜨는 알림은 teammate
            /// 메시지 오버레이에 가려질 수 있어 헤더가 그 신호를 들어야 한다.
            compacting: bool,
            /// 화면의 `▰▰▱ N%` 에서 읽은 compact 진행률. Some 이면 바를 이 값으로
            /// 채우고(진짜 진행률), None 이면 시간 루프로 폴백한다.
            compact_pct: Option<u8>,
            /// Codex rollout이 보고한 추론 강도·협업 모드. 모델은 상태줄에 있어
            /// 헤더에서는 지금 고른 두 값을 짧게 보인다.
            codex_status: Option<String>,
        }
        // Captured once so the &mut self.gpu block below (which can't
        // re-borrow &self) can still see the collapsed/expanded width.
        // `sidebar_w` = full left chrome (tabs + tree) for the cell-grid
        // origin; the tab strip and tree column have their own widths so
        // each paints into its own band.
        let sidebar_w = self.effective_sidebar_w();
        let tab_strip_w = self.tab_strip_w();
        let tree_col_x = self.file_tree_col_x();
        let tree_col_w = self.file_tree_col_w();
        // Right-hand git column geometry (logical px) + this frame's status
        // snapshot, all captured before the &mut self.gpu block (which can't
        // re-borrow &self). `git_reserve` is what the rightmost pane's stretch
        // must leave free on the right: the column plus one window padding, or
        // 0 when the column is hidden (so the pane keeps hugging the edge).
        let git_col_w = self.git_col_w();
        let git_col_x = (win_px.0 / scale - git_col_w).max(0.0);
        // 다른 기기 레포를 보는 중이면 칼럼이 그 기기색을 문다 — 어느 기계 것인지 배경이 말한다.
        let git_col_bg = self
            .git
            .col_remote
            .lock()
            .ok()
            .and_then(|r| r.as_ref().map(|(label, _)| label.clone()))
            .map_or(theme::side_panel_bg(), |label| {
                pane_identity::panel_background(theme::side_panel_bg(), Some(&label))
            });
        let tree_col_bg = self.file_tree.remote.as_ref().map_or(theme::panel_bg(), |(label, _)| {
            pane_identity::panel_background(theme::panel_bg(), Some(label))
        });
        let git_reserve = if git_col_w > 0.0 {
            git_col_w + WINDOW_PADDING
        } else {
            0.0
        };
        let git_view = self
            .git
            .col_data
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();
        let git_repo_choice = self.git_active_pane().and_then(|id| self.git.col_repo_choice.get(&id).cloned());
        // Remote paths must never become local repository picker targets.
        let git_repo_list: Vec<std::path::PathBuf> = {
            let mut set: std::collections::BTreeSet<std::path::PathBuf> = self.pane_cwd_cache.iter()
                .filter(|(id, _)| !kasa_mcp::remote::is_remote_pane(id))
                .map(|(_, cwd)| cwd.clone()).collect();
            if let Some(cur) = git_view.cwd.clone().filter(|_| git_view.remote.is_none()) {
                set.insert(cur);
            }
            set.into_iter().collect()
        };
        let pad_px = (WINDOW_PADDING + sidebar_w) * scale;
        let title_px = TITLE_HEIGHT * scale;
        // Per-pane font multipliers (keyed by pty/leaf id), so each pane's
        // glyphs can be sized independently of the shared base cell.
        let mut pane_scales = self.pane_font_scales.clone();
        // Code-block copy buttons (text + logical rect), filled per pane in
        // the loop below and handed to both the mouse handler and overlay.
        // Image panes collected here (id, pixels, body box in LOGICAL px) so
        // the gpu block below can upload + queue them after the cell pass.
        // (pid, image_data, body_box, zoom, rotation_quarters, pan_xy)
        let mut image_slots: Vec<(
            String,
            Arc<ImagePane>,
            (f32, f32, f32, f32),
            f32,
            u8,
            (f32, f32),
        )> = Vec::new();
        // Claude Code 시작 배너의 Clawd 아트 자리에 그릴 학생 도트:
        // (에셋 슬러그, 배너 박스 LOGICAL px). 셀 스냅샷에서 감지·수집.
        // (slug, 배너 박스 rect, pane 세로 클립(y0, y1)) — 박스는 스크롤로
        // pane 밖까지 이어질 수 있고, 그리기는 클립 범위 안만.
        let mut banner_slots: Vec<(&'static str, (f32, f32, f32, f32), (f32, f32))> = Vec::new();
        // agents 뷰 SCHALE 로고 자리(Clawd 마스코트 위치 / 헤더 왼쪽 여백) — 위치만.
        let mut schale_logo_slots: Vec<(f32, f32, f32, f32)> = Vec::new();
        // agents 목록·resume 피커 화면의 교실 배경(셀 뒤 cover-fit). pane 본문 rect.
        let mut classroom_slots: Vec<(f32, f32, f32, f32)> = Vec::new();
        // /rename 세션명 아웃라인 (x,y,w,h,color) — 입력박스 위 구분선 이름을 사각 테두리로.
        let mut title_outline_slots: Vec<(f32, f32, f32, f32, [u8; 4])> = Vec::new();
        // Claude Code 스크롤 sticky prompt → 웹뷰풍 pill: (px, py, pw, ph, text,
        // pane_id). logical px. 스캔 루프에서 감지·수집, chrome 패스에서 그린다.
        // (px, py, pw, ph, 텍스트, pane_id, ↑ rect, ↓ rect, ↡ rect). 화살표 자리는 셀
        // 폭을 아는 스캔 루프에서 미리 재 둔다 — chrome 패스에서 되재면 어긋난다.
        type StickySlot = (
            f32,
            f32,
            f32,
            f32,
            String,
            String,
            Option<(f32, f32, f32, f32)>,
            Option<(f32, f32, f32, f32)>,
            Option<(f32, f32, f32, f32)>,
        );
        let mut sticky_pill_slots: Vec<StickySlot> = Vec::new();
        // claude 칸의 스크롤바 — (pane, 백엔드 pid, 정본, 격자 오른쪽 끝, 대화 위, 대화 높이,
        // 대화 행 수, 상태). logical px.
        type NavSlot = (String, String, crate::prompt_nav::NavSource, f32, f32, f32, i64, crate::prompt_nav::NavState);
        let mut nav_slots: Vec<NavSlot> = Vec::new();
        // 대화 턴 헤더 — (pane_id, 바 rect, ↑ rect, ↓ rect, ↡ rect, 헤더 내용). logical px.
        // 화살표 rect 는 갈 곳이 있을 때만 담긴다(흐린 화살표는 눌러도 무반응).
        type TurnSlot = (
            String,
            (f32, f32, f32, f32),
            Option<(f32, f32, f32, f32)>,
            Option<(f32, f32, f32, f32)>,
            Option<(f32, f32, f32, f32)>,
            crate::turnjump::TurnHeader,
        );
        let mut turn_header_slots: Vec<TurnSlot> = Vec::new();
        // pane 별 헤더는 **여기서** 지어 둔다. 짓는 데 `&mut self`(앵커 캐시)가 필요한데
        // 아래 pane 루프는 ws 락을 쥔 채 돌아, 그 안에서 `pty_for_pane`(자체 락)을
        // 부르면 같은 뮤텍스를 두 번 잡는다. 락을 잡기 전에 끝내는 것이 그 함정을
        // 구조적으로 없앤다.
        let turn_headers: std::collections::HashMap<String, crate::turnjump::TurnHeader> = {
            let ids: Vec<String> = self
                .ws
                .lock()
                .ok()
                .map(|ws| ws.panes.keys().cloned().collect())
                .unwrap_or_default();
            self.turn.retain_panes(|id| ids.iter().any(|k| k == id) || self.pty.contains_key(id));
            let mut out = std::collections::HashMap::new();
            for id in ids {
                // Arc 를 복제해 self 빌림을 끊는다 — 참조를 든 채로는 캐시를 못 고친다.
                let Some(sess) = self.pty_for_pane(&id).cloned() else {
                    continue;
                };
                let viewer_top = self.pane_view_shift.get(&id).and_then(|s| s.projection.as_ref())
                    .filter(|p| p.scroll_from_bottom > 0 || sess.view_state().0 > 0)
                    .and_then(|p| p.top_abs);
                let pid = self.ws.lock().unwrap().active_tab_pid(&id);
                let claude = matches!(sess.active_agent(), Some(kasa_pty::AgentKind::Claude));
                if let Some(h) = self.turn.header_at(&pid, &sess, viewer_top, claude) {
                    out.insert(id, h);
                }
            }
            out
        };
        // classic claude 칸의 스크롤바 — 앵커 캐시가 `&mut self` 라 헤더처럼 락 전에 짓는다.
        let scrollback_navs: std::collections::HashMap<String, crate::prompt_nav::NavState> = {
            let ids: Vec<String> =
                self.ws.lock().ok().map(|ws| ws.panes.keys().cloned().collect()).unwrap_or_default();
            ids.into_iter().filter_map(|id| self.scrollback_nav(&id).map(|s| (id, s))).collect()
        };
        // 인라인 이미지 이번 프레임 배치(`InlineSlot`).
        let mut inline_slots: Vec<crate::render::terminal_scene::InlineSlot> = Vec::new();
        // 커서가 멎은 `[Image #N]` — (pane, 번호, 그 글자의 화면 박스). 박스는
        // 툴팁을 글자 바로 옆에 붙이는 데 쓴다.
        let mut tip_hit: Option<(String, u32, (f32, f32, f32, f32))> = None;
        // working 스피너(✻/braille) 자리 학생 도트(제자리 걸음): 같은 형태.
        let mut spinner_slots: Vec<(&'static str, (f32, f32, f32, f32))> = Vec::new();
        // 승인 대기(approval prompt) 학생 도트(폴짝 바운스): 같은 형태.
        let mut waiting_slots: Vec<(&'static str, (f32, f32, f32, f32))> = Vec::new();
        // statusline 자리표시자(U+FFFC) → 학생 프사(bust, 정적 1프레임).
        let mut profile_slots: Vec<(&'static str, (f32, f32, f32, f32))> = Vec::new();
        // 상태줄 모델 표식 → Claude/OpenAI 공식 SVG. 셀 글리프 대신 같은 크기의
        // 오버레이를 써 Claude·Codex의 모델 시작점과 색을 정확히 맞춘다.
        let mut status_model_icons: Vec<StatusModelIconSlot> = Vec::new();
        // statusline 프사의 hover 확대·클릭용 (학생이름, slug, rect). profile_slots
        // 와 달리 학생 이름을 들고 있어(hover 큰 bust·클릭→학생설정 딥링크) — /resume
        // 피커 프사는 이름을 모르므로 여기 안 담고 statusline 프사만.
        // 입력박스 위 스페이서 행(effort 칩 자리)에 서 있는 학생(전신 애니).
        // 두 번째 필드 = 모션: "cheer"(턴 완료 직후) 또는 "idle"(대기).
        let mut standing_slots: Vec<(&'static str, &'static str, (f32, f32, f32, f32))> =
            Vec::new();
        // Markdown panes: (id, doc, body box, scroll px, raw_mode, edit lines,
        // cursor, selection, h_scroll px, syntax lang). Render mode draws
        // blocks; Raw mode draws the editor buffer.
        #[allow(clippy::type_complexity)]
        let mut md_slots: Vec<(
            String,
            Arc<MarkdownDoc>,
            (f32, f32, f32, f32),
            f32,
            bool,
            Option<Arc<Vec<String>>>,
            (usize, usize),
            Option<((usize, usize), (usize, usize))>,
            f32,
            &'static str,
            Option<FindState>,
            Option<(Vec<String>, usize, usize)>,
            Vec<crate::lsp::Diag>,
            crate::markdown::Folds,
            bool,
            Vec<crate::markdown::Caret>,
            // HEAD 대비 변경 + 펼친 헝크. 락 안에서 통째로 떠 온다 — 그리는 쪽은
            // `g` 를 가변으로 잡아 락을 든 채로 못 간다(이 vec 전체가 그 이유다).
            Option<(crate::gitdiff::BufferDiff, Option<usize>)>,
        )> = Vec::new();
        // Per-pane body rect (header-excluded) in logical px, collected for
        // every pane so in-pane WebViews and other overlays can be snapped
        // to their pane after the borrow scope ends.
        let mut body_rects: Vec<(String, (f32, f32, f32, f32))> = Vec::new();
        let mut chat_slots: Vec<crate::chat_view::Slot> = Vec::new();
        let mut shell_slots: Vec<crate::shell_view::Slot> = Vec::new();
        // pane 마다 화면을 어떻게 옮겨 그렸는지. 락 안에서는 `self` 가 불변이라
        // 여기 모아 두었다가 블록이 끝난 뒤 한 번에 옮긴다(body_rects 와 같은 이유).
        let mut view_shifts: Vec<(String, crate::PaneViewShift)> = Vec::new();
        // 날씨: 초점 창과 그 입력줄(논리 px) — 물방울을 올리지 않는 자리. 같은 이유로 밖에 모은다.
        let mut weather_focus: Option<String> = None;
        let mut weather_guard: Option<[f32; 4]> = None;
        let (slots, headers, footer_slots, agents_view_panes, mirror_claude_panes): (
            Vec<PaneSlot>,
            Vec<HeaderInfo>,
            Vec<(String, f32, f32, f32, f32)>,
            std::collections::HashSet<String>,
            std::collections::HashSet<String>,
        ) = {
            let ws = self.ws.lock().unwrap();
            let active_id = ws.active_pane.clone();
            // Total grid rows/cols — used to detect the bottom-row / right-col
            // pane so it can stretch to the window's true edge (window_cells
            // floors both, leaving a sub-cell remainder otherwise).
            let (grid_cols, grid_rows) = self.window_cells();
            // tmux-style zoom: render only the zoomed pane, filling the grid and
            // hiding the rest. Skips when the zoomed pane isn't in this window's
            // map (closed / moved out) so no phantom paints.
            let zoom_leaves: Option<Vec<(String, u16, u16, u16, u16)>> =
                match self.zoomed_pane.as_deref() {
                    Some(z) if ws.panes.contains_key(z) => {
                        // 사방을 들여 「떠 있는 카드」로 — inset 규칙은
                        // effective_leaf_rects(PTY resize·히트테스트)와 같은
                        // 함수를 써야 그린 칸과 PTY 가 어긋나지 않는다.
                        let (ix, iy) = self.zoom_inset_cells(grid_cols, grid_rows);
                        Some(vec![(
                            z.to_string(),
                            ix,
                            iy,
                            grid_cols - ix * 2,
                            grid_rows - iy * 2,
                        )])
                    }
                    _ => None,
                };
            let leaves: Vec<(String, u16, u16, u16, u16)> = if let Some(z) = zoom_leaves {
                z
            } else if let Some(layout) = ws.layout.as_ref() {
                layout
                    .leaves()
                    .into_iter()
                    .filter_map(|n| match n {
                        Layout::Pane { id, x, y, w, h } => Some((format!("%{id}"), *x, *y, *w, *h)),
                        _ => None,
                    })
                    .collect()
            } else {
                // Single-pane fallback (no split tree). `ws.panes` holds EVERY
                // window's pane (a session shares one pane map across its
                // windows), so an arbitrary entry would draw another
                // window's/session's pane here — the dead-pane "resurrection"
                // in an emptied window. Honor ONLY the active pane; if it's
                // unset/gone, draw nothing and let the next State broadcast set
                // the right one. Never fall back to an arbitrary HashMap entry.
                let active = active_id
                    .as_ref()
                    .filter(|id| ws.panes.contains_key(*id))
                    .cloned();
                match active {
                    Some(id) => vec![(id, 0, 0, 0, 0)],
                    None => Vec::new(),
                }
            };
            // Header bar when split OR when any pane carries multiple tabs.
            // A lone pane with a single tab stays header-less so the first
            // session reads as a plain terminal; but a lone pane with two or
            // more tabs (after a cross-pane drag, or a +button add) MUST
            // keep its strip so the tabs stay reachable.
            // ghostty식: 상시 헤더 띠 폐기 → 셀 시프트 0, 헤더 paint 없음.
            // 비활성 pane dim(흐림)만 split 여부로 유지. pane 컨트롤은
            // hover ⋮ 핸들로 이관(Phase 2~4).
            let is_split = leaves.len() > 1;
            let mut slots = Vec::new();
            let mut headers = Vec::new();
            // Box geometry per leaf (id, x, y, w, h) in logical px — collected
            // for EVERY pane, headered or not, so the per-pane status bar can
            // anchor to the box bottom even on a lone unsplit pane.
            let mut footer_slots: Vec<(String, f32, f32, f32, f32)> = Vec::new();
            // claude agents(에이전트 목록 뷰)로 판정된 pane 집합 — 개별 학생 대신
            // SCHALE 조직 정체성(타이틀·테두리)으로 표시한다. 판정은 루프 안에서
            // argv(is_claude_agents) + statusline 프사 슬롯(U+FFFC) 부재로 하고,
            // 루프 뒤 타이틀바·테두리 패스가 이 집합을 읽는다.
            let mut agents_view_panes: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            // 이사 간 거울 pane 중 저쪽에서 claude 가 도는 것으로 판정된 집합 —
            // 판정(원격 링크 + 화면의 statusline 표식)은 루프 안 agent_kind 폴백이
            // 하고, 루프 뒤 타이틀바 패스가 학생 이름을 올릴 때 다시 읽는다.
            let mut mirror_claude_panes: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            // 포인터와 IME도 같은 관문에서 최종 셀 배율을 얻는다.
            for (id, ..) in &leaves {
                pane_scales.insert(id.clone(), self.pane_display_scale(&ws, id));
            }
            for (id, x_cells, y_cells, w_cells, h_cells) in leaves {
                let Some(pane) = ws.panes.get(&id) else {
                    continue;
                };
                // pane.cells already holds the correct view: the PTY
                // backend snapshots through alacritty's display_offset,
                // so a scrolled-up frame arrives here pre-composed with
                // real scrollback (scroll-region TUIs included). Just
                // normalise each row to the current width so the GPU
                // pipeline emits exactly `cols` cells per row.
                // During a divider drag we DEFER the PTY reshape (SIGWINCH +
                // shell repaint is what causes the flicker), so the PTY's
                // reported cols/rows are stale. Clip the rendered cells to
                // the layout's CURRENT pane rect — overflow gets dropped at
                // the new edge instead of bleeding into the neighbouring
                // pane. After release, the final resize_backend lets the
                // shell catch up and the clip is a no-op.
                //
                // Single-pane fallback path (no layout tree yet) passes
                // (0,0,0,0) as a placeholder. A mirror must use the full local
                // window in that case, never the source terminal dimensions.
                let pty_cols = pane.term().map_or(1, |t| t.cols).max(1) as usize;
                let pty_rows = pane.term().map_or(0, |t| t.cells.len());
                let independent_view = kasa_mcp::remote::is_view_pane(&ws.active_tab_pid(&id));
                let (cols_now, rows_now) = if !independent_view && (w_cells == 0 || h_cells == 0) {
                    (pty_cols, pty_rows)
                } else {
                    // Mirror resize_backend EXACTLY: pane box in base-grid px,
                    // minus real insets/header, divided by the ZOOMED cell.
                    // The clip has to land on the same count the PTY was sized
                    // to, or a zoomed-out pane (more cols/rows in the PTY) gets
                    // truncated back to the base-grid count and the TUI's
                    // layout tears.
                    let fs = pane_scales
                        .get(id.as_str())
                        .copied()
                        .unwrap_or(1.0);
                    let cw = self.cell.w.max(1.0);
                    let ch = self.cell.h.max(1.0);
                    let scaled_cw = cw * fs;
                    let scaled_ch = ch * fs;
                    let header_px_now = pane.header_px();
                    let footer_px_now = self.statusbar_px(id.as_str());
                    let local_w = if w_cells == 0 { grid_cols } else { w_cells };
                    let local_h = if h_cells == 0 { grid_rows } else { h_cells };
                    let usable_w = (local_w as f32 * cw - 2.0 * PANE_INNER_X).max(scaled_cw);
                    let usable_h =
                        (local_h as f32 * ch - header_px_now - footer_px_now - 2.0 * PANE_INNER_Y)
                            .max(scaled_ch);
                    let layout_cols = (usable_w / scaled_cw).floor() as usize;
                    let layout_rows = (usable_h / scaled_ch).floor() as usize;
                    if independent_view {
                        (layout_cols.max(2), layout_rows.max(1))
                    } else {
                        (layout_cols.min(pty_cols).max(1), layout_rows.min(pty_rows))
                    }
                };
                // Image/markdown panes carry no PTY grid; an empty rows vec
                // makes draw_cells a no-op and the content (texture or laid-out
                // document) is painted into the pane box instead (queued below).
                let img = pane.image().cloned();
                let img_zoom = pane.image_view_zoom();
                let img_rot = pane.image_rot % 4;
                let img_pan = (pane.image_pan_x, pane.image_pan_y);
                // Snapshot markdown render data: (doc, raw_mode, edit lines if
                // raw, cursor, normalized selection, scroll px, h_scroll px,
                // syntax lang).
                let md: Option<(
                    Arc<MarkdownDoc>,
                    bool,
                    Option<Arc<Vec<String>>>,
                    (usize, usize),
                    Option<((usize, usize), (usize, usize))>,
                    f32,
                    f32,
                    &'static str,
                    Option<FindState>,
                    Option<(Vec<String>, usize, usize)>,
                    crate::markdown::Folds,
                    bool,
                    Vec<crate::markdown::Caret>,
                    Option<(crate::gitdiff::BufferDiff, Option<usize>)>,
                )> = pane.markdown().map(|m| {
                    (
                        m.doc.clone(),
                        m.raw_mode,
                        // 프레임마다 도는 자리다 — Arc 라 포인터 하나 복사.
                        m.raw_mode.then(|| Arc::clone(&m.edit_lines)),
                        (m.cur_line, m.cur_col),
                        m.sel_range(),
                        m.scroll,
                        m.h_scroll,
                        code_lang_for_path(std::path::Path::new(&m.doc.path)),
                        m.find.clone(),
                        // 팝업이 열렸을 때만 후보를 복사한다(최대 8개).
                        m.complete
                            .as_ref()
                            .map(|c| (c.items.clone(), c.sel, c.from_col)),
                        m.folds.clone(),
                        m.wrap,
                        m.extra.clone(),
                        // 변경 없는 파일이면 `is_empty` 라 아예 안 싣는다 — 그
                        // 경우 아래 그리기는 이 기능이 없던 때와 완전히 같다.
                        m.diff
                            .as_ref()
                            .filter(|d| !d.is_empty())
                            .map(|d| (d.clone(), m.diff_peek)),
                    )
                });
                let header_shift_px = pane.header_px() * scale;
                let header_shift_logical = pane.header_px();
                let origin_px = (
                    pad_px + x_cells as f32 * cell_w_px + PANE_INNER_X * scale,
                    title_px + y_cells as f32 * cell_h_px + header_shift_px + PANE_INNER_Y * scale,
                );
                let body_left =
                    WINDOW_PADDING + sidebar_w + x_cells as f32 * self.cell.w + PANE_INNER_X;
                let body_top = TITLE_HEIGHT
                    + y_cells as f32 * self.cell.h
                    + header_shift_logical
                    + PANE_INNER_Y;
                let pane_font_scale = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
                // 대화로 보는 학생 pane — 격자와 그 위의 장식(학생 그림·배너)은 빼고
                // 본문 자리에 말풍선을 그린다. 학생이 나가 셸만 남았으면 터미널 그대로다.
                let chat_slot = (self.chat_view_on(&id) && self.pane_can_chat(&ws, &id)).then(|| {
                    let tab = ws.active_tab_pid(&id);
                    let state = self.pane_activity.get(&id).map(|a| &a.state);
                    crate::chat_view::Slot {
                        pane: id.clone(),
                        rect: (0.0, 0.0, 0.0, 0.0),
                        name: ws
                            .pane_character
                            .get(&tab)
                            .cloned()
                            .or_else(|| self.pty.get(tab.as_str()).and_then(|p| p.active_agent()).map(|k| k.as_str().to_string()))
                            .unwrap_or_else(|| "학생".to_string()),
                        working: state.is_some_and(|s| s.is_busy()),
                        needs_you: state.is_some_and(|s| s.needs_you()),
                        focused: active_id.as_deref() == Some(id.as_str()),
                        mirror: kasa_mcp::remote::remote_info(&tab).is_some()
                            || kasa_mcp::remote::remote_info(&id).is_some(),
                        preedit: self.chat_view_preedit(&id),
                        caret_on: self.cursor_blink_on(Instant::now()),
                        bg: theme::pane_bg(),
                        zoom: self.pane_font_scales.get(&id).copied().unwrap_or(1.0).max(0.1),
                    }
                });
                // 거울 셸 칸 — 원본이 낸 명령 묶음을 카드로. 원본 PTY 크기는 그대로다.
                let shell_slot = chat_slot.is_none().then(|| {
                    let tab = ws.active_tab_pid(&id);
                    let alt_now = pane.term().is_some_and(|t| t.alt_screen);
                    self.shell_view_ready(&id, &tab, alt_now).then(|| crate::shell_view::Slot {
                        pane: id.clone(),
                        rect: (0.0, 0.0, 0.0, 0.0),
                        focused: active_id.as_deref() == Some(id.as_str()),
                        prompt: self.shell_view_prompt(&ws, &id),
                        caret_on: self.cursor_blink_on(Instant::now()),
                        now_ms: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(0, |d| d.as_millis() as u64),
                        local: !kasa_mcp::remote::is_view_pane(&tab),
                        bg: theme::pane_bg(),
                        zoom: self.pane_font_scales.get(&id).copied().unwrap_or(1.0).max(0.1),
                    })
                }).flatten();
                let body_view = chat_slot.is_some() || shell_slot.is_some();
                let composition = self.compose_terminal_pane(
                    &ws, pane, pane.term(), &id, ws.active_tab_pid(&id), cols_now, rows_now,
                    body_left, body_top, pane_font_scale, true, &turn_headers,
                );
                let composed = composition.rows;
                if self.weather.settings.enabled && active_id.as_deref() == Some(id.as_str()) {
                    weather_focus = Some(id.clone());
                    let fs = pane_font_scale;
                    let (ch, cw) = (self.cell.h * fs, self.cell.w * fs);
                    let cols = composed.first().map_or(0, |r| r.len()) as f32;
                    let rows = match crate::screenread::prompt_box(&composed) {
                        Some(crate::screenread::PromptBox::Bordered { top, bottom, .. }) => Some(top..bottom + 1),
                        Some(pb) => Some(pb.rows()),
                        None => pane.term().map(|t| t.cursor_row as usize..t.cursor_row as usize + 1),
                    };
                    weather_guard = rows.map(|r| {
                        [body_left, body_top + r.start as f32 * ch, cols * cw, (r.end - r.start) as f32 * ch]
                    });
                }
                let agents_view = composition.agents_view;
                let runs_claude = composition.runs_claude;
                let true_char = composition.true_char;
                let tab_pid = composition.tab_pid;
                // 칸 오른쪽 여백의 스크롤바(prompt_nav.rs). 대체 화면 claude 는 스크롤을 자기가 쥐어
                // 터미널 스크롤백이 없으니 칸 안의 mod 가 잰 위치로, classic 은 이 터미널의 스크롤백으로.
                if runs_claude && !independent_view && chat_slot.is_none() {
                    let ch = self.cell.h * pane_font_scale;
                    let right = body_left + cols_now as f32 * self.cell.w * pane_font_scale;
                    if pane.term().is_some_and(|t| t.alt_screen) {
                        if let Some(state) = crate::prompt_nav::live_state(&tab_pid) {
                            // 입력 상자 위 빈 줄 하나까지는 대화가 아니다.
                            let rows = crate::screenread::pinned_input_top(&composed)
                                .unwrap_or(composed.len())
                                .saturating_sub(1);
                            nav_slots.push((
                                id.clone(),
                                tab_pid.clone(),
                                crate::prompt_nav::NavSource::Mod,
                                right,
                                body_top,
                                rows as f32 * ch,
                                rows as i64,
                                state,
                            ));
                        }
                    } else if let Some(state) = scrollback_navs.get(id.as_str()) {
                        // 화면 전체가 스크롤백의 창이다 — 입력 상자도 대화와 함께 흘러간다.
                        let rows = composed.len();
                        nav_slots.push((
                            id.clone(),
                            tab_pid.clone(),
                            crate::prompt_nav::NavSource::Scrollback,
                            right,
                            body_top,
                            rows as f32 * ch,
                            rows as i64,
                            state.clone(),
                        ));
                    }
                }
                // 학생 판정(제목줄 이름)은 보기와 상관없이 산다.
                agents_view_panes.extend(composition.agents_view_panes);
                mirror_claude_panes.extend(composition.mirror_claude_panes);
                if !body_view {
                    banner_slots.extend(composition.banner_slots);
                    spinner_slots.extend(composition.spinner_slots);
                    waiting_slots.extend(composition.waiting_slots);
                    standing_slots.extend(composition.standing_slots);
                    profile_slots.extend(composition.profile_slots);
                    inline_slots.extend(composition.inline_slots);
                    schale_logo_slots.extend(composition.schale_logo_slots);
                    title_outline_slots.extend(composition.title_outline_slots);
                    status_model_icons.extend(composition.status_model_icons);
                    sticky_pill_slots.extend(composition.sticky_pill_slots);
                    turn_header_slots.extend(composition.turn_header_slots);
                    view_shifts.extend(composition.view_shifts);
                    if tip_hit.is_none() {
                        tip_hit = composition.tip_hit;
                    }
                }
                let hover_links = hovered_link
                    .as_ref()
                    .filter(|(pid, _, _)| pid.as_str() == id.as_str())
                    .map(|(_, spans, _)| spans.clone())
                    .unwrap_or_default();
                slots.push(PaneSlot {
                    rows: if body_view { Vec::new() } else { composed },
                    origin_px,
                    // Unfocused panes dim their text only (no box veil). Single
                    // un-split pane is never dimmed.
                    dim: is_split && active_id.as_deref() != Some(id.as_str()),
                    font_scale: pane_font_scale,
                    links: hover_links,
                    default_fg: cells::default_fg(),
                    source: crate::mirror_theme::pane_source_palette(id.as_str()),
                });
                // Body box (header band excluded, inset by the same
                // PANE_INNER margins the cell grid uses) in logical px.
                // Bottom-row stretch mirrors the header's box_h so the
                // content fills to the window edge with no seam.
                // Computed for EVERY pane (not just image/md) — in-pane
                // WebViews need it too.
                let bx = WINDOW_PADDING + sidebar_w + x_cells as f32 * self.cell.w + PANE_INNER_X;
                let by = TITLE_HEIGHT
                    + y_cells as f32 * self.cell.h
                    + header_shift_logical
                    + PANE_INNER_Y;
                // A single un-split pane reports 0 cells (the layout tree has no
                // split to divide by). The cell-grid clip above already falls
                // back to the full window in that case; mirror it here, or the
                // body box — and the Alt pane-number overlay drawn on it —
                // collapses to 1px (overlay then skipped by the rw<24 guard).
                let eff_w_cells = if w_cells == 0 { grid_cols } else { w_cells };
                let eff_h_cells = if h_cells == 0 { grid_rows } else { h_cells };
                let base_w = eff_w_cells as f32 * self.cell.w;
                let full_w = if x_cells + eff_w_cells >= grid_cols {
                    let extra = self.window.as_ref().map_or(0.0, |w| {
                        let s = w.scale_factor() as f32 * self.ui_zoom;
                        let raw_lw = w.inner_size().width as f32 / s;
                        // Stop at the git column's left padding when it's shown,
                        // else hug the true window edge (git_reserve == 0).
                        (raw_lw
                            - git_reserve
                            - (WINDOW_PADDING + sidebar_w + grid_cols as f32 * self.cell.w))
                            .max(0.0)
                    });
                    base_w + extra
                } else {
                    base_w
                };
                // An edge pane meets the window border, not a divider, so it
                // gets no inner inset on that side — otherwise the right/bottom
                // edge keeps an inner-pad-width empty strip (the "우측하단 빈칸"
                // a drag leaves when it puts a pane against the window edge).
                let right_inset = if x_cells + eff_w_cells >= grid_cols {
                    0.0
                } else {
                    PANE_INNER_X
                };
                let bw = (full_w - PANE_INNER_X - right_inset).max(1.0);
                let base_h = eff_h_cells as f32 * self.cell.h;
                let full_h = if y_cells + eff_h_cells >= grid_rows {
                    let extra = self.window.as_ref().map_or(0.0, |w| {
                        let s = w.scale_factor() as f32 * self.ui_zoom;
                        let raw_lh = w.inner_size().height as f32 / s;
                        (raw_lh
                            - self.bottom_reserve_h()
                            - (TITLE_HEIGHT + grid_rows as f32 * self.cell.h))
                            .max(0.0)
                    });
                    base_h + extra
                } else {
                    base_h
                };
                let bottom_inset = if y_cells + eff_h_cells >= grid_rows {
                    0.0
                } else {
                    PANE_INNER_Y
                };
                // 상태바 띠를 빼야 한다. PTY 그리드는 `resize_backend` 가 푸터만큼
                // 행을 줄여 바 위에서 멈추는데, 편집기·이미지처럼 PTY 없는 pane 은
                // 이 박스가 곧 본문 클립이라 여기서 빼지 않으면 마지막 줄이 바
                // 아래로 새어 창 끝까지 그려졌다(실측).
                let bh = (full_h
                    - header_shift_logical
                    - PANE_INNER_Y
                    - bottom_inset
                    - self.statusbar_px(&id))
                .max(1.0);
                body_rects.push((id.clone(), (bx, by, bw, bh)));
                if let Some(mut slot) = chat_slot {
                    slot.rect = (bx, by, bw, bh);
                    chat_slots.push(slot);
                }
                if let Some(mut slot) = shell_slot {
                    slot.rect = (bx, by, bw, bh);
                    shell_slots.push(slot);
                }
                if let Some(image) = img {
                    image_slots.push((
                        id.clone(),
                        image,
                        (bx, by, bw, bh),
                        img_zoom,
                        img_rot,
                        img_pan,
                    ));
                }
                if let Some((
                    doc,
                    raw_mode,
                    lines,
                    cursor,
                    sel,
                    scroll,
                    h_scroll,
                    lang,
                    find,
                    complete,
                    folds,
                    wrap,
                    extra,
                    diff,
                )) = md
                {
                    // 편집기 모드에서만 — 렌더 뷰엔 밑줄을 그릴 자리가 없다.
                    // 여기서 뽑는 이유는 아래 그리는 루프가 `&mut self.gpu` 를
                    // 잡고 있어 self 를 다시 빌릴 수 없기 때문이다.
                    let diags = if raw_mode {
                        self.lsp_diags(&doc.path)
                    } else {
                        Vec::new()
                    };
                    md_slots.push((
                        id.clone(),
                        doc,
                        (bx, by, bw, bh),
                        scroll,
                        raw_mode,
                        lines,
                        cursor,
                        sel,
                        h_scroll,
                        lang,
                        find,
                        complete,
                        diags,
                        folds,
                        wrap,
                        extra,
                        diff,
                    ));
                }
                // Box geometry (logical px). Right/bottom-edge panes stretch to
                // the window's true edge so the floored sub-cell remainder
                // doesn't read as a seam. Computed unconditionally — the status
                // bar anchors off box_y + box_h whether or not a header is drawn.
                let box_x = WINDOW_PADDING + sidebar_w + x_cells as f32 * self.cell.w;
                let box_y = TITLE_HEIGHT + y_cells as f32 * self.cell.h;
                // A lone unsplit pane arrives as a (0,0,0,0) placeholder (see the
                // clip note above), which would leave the box 0×0 and starve the
                // footer (`fbox_h < pane_footer_h()` → skipped). Treat a 0 span
                // as "fills the grid" so the box — and its status bar — spans the
                // whole pane area just like a real right/bottom-edge leaf.
                let box_w = {
                    let base = w_cells as f32 * self.cell.w;
                    if w_cells == 0 || x_cells + w_cells >= grid_cols {
                        let right_edge =
                            WINDOW_PADDING + sidebar_w + (x_cells + w_cells) as f32 * self.cell.w;
                        let extra = self.window.as_ref().map_or(0.0, |w| {
                            let s = w.scale_factor() as f32 * self.ui_zoom;
                            let raw_lw = w.inner_size().width as f32 / s;
                            (raw_lw - git_reserve - right_edge).max(0.0)
                        });
                        base + extra
                    } else {
                        base
                    }
                };
                let box_h = {
                    let base = h_cells as f32 * self.cell.h;
                    if h_cells == 0 || y_cells + h_cells >= grid_rows {
                        let bottom_edge = TITLE_HEIGHT + (y_cells + h_cells) as f32 * self.cell.h;
                        let extra = self.window.as_ref().map_or(0.0, |w| {
                            let s = w.scale_factor() as f32 * self.ui_zoom;
                            let raw_lh = w.inner_size().height as f32 / s;
                            // 상태줄·dock 예약을 빼야 한다 — body_rects 의 stretch 는
                            // 빼는데 여기만 안 빼서, 하단행 pane 의 박스가 창 끝까지
                            // 내려가 포커스 테두리 아랫변이 나중에 그려지는 전역
                            // 상태줄 뒤에 통째로 깔렸다(사용자 2026-08-15 「하단바때문에
                            // 포커스 테두리 밑에가 안보여」, 창 캡처 실측).
                            (raw_lh - self.bottom_reserve_h() - bottom_edge).max(0.0)
                        });
                        base + extra
                    } else {
                        base
                    }
                };
                footer_slots.push((id.clone(), box_x, box_y, box_w, box_h));
                // 기존 사용자 배경은 목록에서만 유지해 대화 화면을 가리지 않는다.
                if agents_view && theme::character_appearance() {
                    classroom_slots.push((box_x, box_y, box_w, box_h));
                }
                // image/md pane만 헤더 띠 데이터 생성(전용 컨트롤 자리). 일반
                // 터미널은 hover ⋮ 핸들로 — has_header()가 그 경계를 가른다.
                if pane.has_header() {
                    let shown_id = pane_identity::shown_pane_id(&tab_pid, pane.term().is_some());
                    // 캐릭터 배정 pane(학생)은 헤더에도 이름을 — "미도리 · 작업명"(작업명
                    // =OSC title). BA GUI board 라벨과 통일(사용자: 터미널 탭도 학생 이름).
                    // 비배정 pane 만 기존 "%N · 프로세스" 폴백.
                    let label = if agents_view {
                        // 목록 화면은 세션에 배정된 캐릭터와 구별되어야 한다.
                        match pane
                            .title
                            .clone()
                            .map(|t| crate::strip_activity_prefix(&t).to_string())
                            .filter(|t| !t.is_empty())
                        {
                            Some(t) => format!("KASA · {t}"),
                            None => "KASA".to_string(),
                        }
                    } else if let Some(c) = true_char.as_ref().filter(|_| runs_claude) {
                        // 헤더 학생명은 `display_pane_char`(=true_char) 정본을 쓴다 —
                        // raw pane.character 만 보면 claude agents 로 이어받은 백그라운드
                        // 세션은 ws.pane_character 가 비어(attach 스폰이 캐릭터 미배정)
                        // 이 분기를 못 타고 아래 폴백으로 흘러 "미도리 · 작업명" 대신
                        // 세션제목이 칩자리에 박혔다(사용자 Q1). session_character(bound sid)
                        // 로 해석하면 스프라이트·프사(둘 다 true_char)와 헤더가 일치한다.
                        match pane
                            .title
                            .clone()
                            .map(|t| crate::strip_activity_prefix(&t).to_string())
                            .filter(|t| !t.is_empty())
                        {
                            // pane 아이디를 캐릭터 뒤에 붙인다(사용자 2026-08-05: "칩 위치를
                            // 바꿔 pane아이디 이런데에, 거기는 /rename 들어갈 자리니까").
                            // 입력박스 보더 우측은 `/rename` 이름 자리로 비워 뒀으니
                            // (`inlay_prompt_box_right`) **이 pane 이 누구인가**는 헤더가
                            // 든다. 학생 pane 은 여태 캐릭터만 실어 아이디가 어디에도
                            // 없었다. 거울은 몸통이 있는 기기의 pane 번호를 표시한다.
                            //
                            // agent 이름(`midori-p1`)을 그대로 싣지 않는 이유: 그건 캐릭터
                            // 슬러그 + pane 번호라 `미도리 %1` 과 같은 정보인데, 로마자
                            // 슬러그는 스프라이트·board 의 한글 이름과 안 맞아 두 이름을
                            // 오가게 만든다. 정체 표시는 한 벌로 둔다.
                            Some(t) => format!("{c} {shown_id} · {t}"),
                            None => format!("{c} {shown_id}"),
                        }
                    } else {
                        // Custom title (rename / OSC) wins; otherwise the live
                        // foreground process (vim, claude, zsh …); fall back to
                        // the raw "%N" id only if both are empty.
                        let smart = self.pty.get(&id).and_then(|p| Self::smart_pane_label(p));
                        let base = pane
                            .title
                            .clone()
                            .filter(|t| !t.is_empty())
                            .or(smart)
                            .unwrap_or_else(|| shown_id.clone());
                        // Prefix the displayed source number; skip a duplicate fallback.
                        if base == id || base == shown_id {
                            shown_id
                        } else {
                            format!("{shown_id} · {base}")
                        }
                    };
                    // 웹 pane 로딩 상태 — host 실물(web_hosts)이 쥔다. 헤더 작업
                    // 바(busy)와 리로드↔정지 아이콘이 읽는다.
                    let web_loading = pane
                        .web()
                        .and_then(|w| self.web_hosts.get(&w.host_id))
                        .map(|h| h.loading)
                        .unwrap_or(false);
                    let codex_status = self
                        .socket_backend
                        .as_ref()
                        .and_then(|backend| backend.codex_rollout_snapshot(&tab_pid))
                        .and_then(|snapshot| codex_header_summary(&snapshot));
                    headers.push(HeaderInfo {
                        id: id.clone(),
                        active_tab_pid: tab_pid,
                        x: box_x,
                        y: box_y,
                        w: box_w,
                        box_h,
                        // ● = 미저장 편집(raw 편집기). 단일탭 폴백 라벨에도 붙어야
                        // 헤더 어디로 그려지든 저장 안 된 게 보인다.
                        label: if pane.markdown().map_or(false, |m| m.modified) {
                            format!("● {label}")
                        } else {
                            label
                        },
                        is_active: active_id.as_deref() == Some(id.as_str()),
                        // Busy = the daemon's transcript watcher sees this pane
                        // working (cross-window). Drives the header working bar.
                        // 웹 pane 은 페이지 로딩이 곧 「작업 중」이다.
                        busy: self.pane_is_busy(&id) || web_loading,
                        // A background shell / Monitor is running with no spinner —
                        // drives the slower header pulse bar when not busy.
                        bg_active: self
                            .pane_activity
                            .get(&id)
                            .map(|a| a.bg_active)
                            .unwrap_or(false),
                        compacting: matches!(
                            self.agent_state(&id),
                            crate::agent_state::AgentState::Compacting
                        ),
                        compact_pct: self.pane_activity.get(&id).and_then(|a| a.compact_pct),
                        codex_status,
                        color: pane.color,
                        is_markdown: pane.markdown().map_or(false, |m| m.is_md_doc),
                        is_editor: pane.markdown().is_some(),
                        md_raw_mode: pane.markdown().map_or(false, |m| m.raw_mode),
                        md_modified: pane.markdown().is_some_and(|m| m.modified),
                        is_image: pane.image().is_some(),
                        is_web: pane.web().is_some(),
                        web_url: pane.web().map(|w| w.url.clone()),
                        web_loading,
                        tabs: self.pane_tab_labels(&ws, &id, pane),
                        active_tab: pane.active_tab,
                        tab_first: pane.tab_first,
                        tab_last_active: pane.tab_last_active,
                    });
                }
            }
            // Fallback: if nothing is marked active (e.g. active_pane not yet
            // set right after a split), make the first header active so the
            // focused-tab box/accent always shows on exactly one pane.
            if !headers.is_empty() && !headers.iter().any(|h| h.is_active) {
                headers[0].is_active = true;
            }
            (
                slots,
                headers,
                footer_slots,
                agents_view_panes,
                mirror_claude_panes,
            )
        };
        // 이 프레임이 화면을 어떻게 옮겼는지 확정. 통째로 갈아끼워야 사라진 pane 의
        // 옛 옮김이 남아, 닫힌 창의 좌표로 복사가 어긋나는 일이 없다.
        self.pane_view_shift = view_shifts.into_iter().collect();
        // 메뉴에는 계정별 네트워크 조회값이 아니라, 현재 창(없으면 같은 방의 최근
        // 창)이 rollout에 직접 남긴 값을 쓴다. 그래서 다른 슬롯 수치를 현재 선택할
        // 계정의 한도로 잘못 읽히게 하지 않는다.
        let codex_rollout = self.socket_backend.as_ref().and_then(|backend| {
            let ids = self
                .ws
                .lock()
                .ok()
                .map(|ws| {
                    let mut ids = Vec::new();
                    if let Some(active) = ws.active_pane.as_deref() {
                        let tab = ws.active_tab_pid(active);
                        ids.push(tab.clone());
                        if tab != active {
                            ids.push(active.to_string());
                        }
                    }
                    for outer in ws.panes.keys() {
                        let tab = ws.active_tab_pid(outer);
                        if !ids.contains(&tab) {
                            ids.push(tab);
                        }
                        if !ids.contains(outer) {
                            ids.push(outer.clone());
                        }
                    }
                    ids
                })
                .unwrap_or_default();
            ids.into_iter()
                .find_map(|id| backend.codex_rollout_snapshot(&id))
        });
        // pane 하단바는 바깥 pane id로 그리지만 rollout은 현재 탭 pid에 묶인다.
        // GPU 대여 전 한 번 접어 두면 footer 루프에서 socket/워크스페이스를 다시
        // 잠글 필요가 없고, 탭을 바꾼 순간에도 현재 보이는 Codex 값만 쓴다.
        let codex_footer_status: std::collections::HashMap<String, String> = self
            .socket_backend
            .as_ref()
            .and_then(|backend| {
                self.ws.lock().ok().map(|ws| {
                    footer_slots
                        .iter()
                        .filter_map(|(outer, ..)| {
                            backend
                                .codex_rollout_snapshot(&ws.active_tab_pid(outer))
                                .and_then(|snapshot| codex_header_summary(&snapshot))
                                .map(|status| (outer.clone(), status))
                        })
                        .collect()
                })
            })
            .unwrap_or_default();
        // Top-right toast. Pre-read here so the render block below never
        // re-borrows self while g is held.
        let collab_toast_alpha = self.collab_toast_alpha();
        let collab_toast_msg = if self.lite { None } else { self.collab.toast.as_ref().map(|(m, _)| m.clone()) };
        let collab_toast_action_on = self.collab.toast_action.is_some();
        let collab_toast_elapsed_ms = self.collab_toast_elapsed_ms();
        // 새 판 알림이면 칩 라벨이 승인/거부 대신 업데이트/닫기, 뜻도 그쪽이 정한다.
        let update_chips = self.update_notice_chips().or_else(|| self.remote_notice_chips());
        let slot_views: Vec<gpu::PaneSlot<'_>> = slots
            .iter()
            .map(|s| gpu::PaneSlot {
                rows: &s.rows,
                origin_px: s.origin_px,
                dim: s.dim,
                font_scale: s.font_scale,
                links: s.links.clone(),
                default_fg: s.default_fg,
                source: s.source,
            })
            .collect();
        // Recompute the inline suggestion against the freshly-applied
        // grid before snapshotting it into the overlay.
        self.update_suggestion();
        let overlay = self.gpu_overlay_snapshot();
        self.sync_ime_cursor_area(&overlay);
        let chrome_font = 14.0_f32;
        // Markdown Render/Raw toggle lives in the pane action buttons
        // (drawn in the header loop), not a separate pill.
        // Session tabs live in a wry webview panel (like the git panel), not
        // the native title bar — drawing them here collided with the OSC title.
        // Drop-zone overlay: while a header drag is active, highlight the
        // half of the target pane the dragged pane would land in. Computed
        // here (immutable self borrow) so the gpu block below only touches
        // the cached rect.
        // Drop zone shows for BOTH header drags (whole pane → quadrant)
        // and tab drags whose cursor is over a pane BODY (split + place
        // moved tab as new pane). Tab drag over a strip is handled by
        // tab_drag_info's insertion bar instead.
        let header_drag_active = self
            .header_drag
            .as_ref()
            .map(|hd| hd.active)
            .unwrap_or(false);
        let tab_drag_active = self.tab_drag.as_ref().map(|d| d.active).unwrap_or(false);
        // The strip-only insertion bar gets replaced by the zone overlay
        // — without it the user sees no preview when hovering the header,
        // which is exactly the spot most people aim for when intending
        // "merge into this pane".
        // 라이브 드래그(실제 레이아웃이 재배치되는 케이스): header/handle 드래그는
        // 항상, tab 드래그는 단일탭 pane 일 때. 진짜 reflow 가 곧 피드백이므로 파란
        // drop-zone 박스를 띄우지 않는다 — 박스는 라이브가 아닌 tab 드래그(멀티탭
        // 탭 추출)에만 남긴다.
        let live_drag = header_drag_active
            || self
                .tab_drag
                .as_ref()
                .map(|t| {
                    t.active
                        && self
                            .ws
                            .lock()
                            .ok()
                            .and_then(|w| w.panes.get(&t.pane).map(|p| p.tabs.len() <= 1))
                            .unwrap_or(true)
                })
                .unwrap_or(false);
        // 중앙("안에 넣기") 프리뷰는 라이브 드래그에서도 박스를 띄운다 — 소스가
        // 그리드에서 빠지는 것만으론 *어느* pane 안으로 들어가는지 안 보인다.
        let live_center = self
            .drag_live_applied
            .as_ref()
            .is_some_and(|(_, z)| *z == DropZone::Center);
        let show_drop_zone = (tab_drag_active && !live_drag) || live_center;
        // Indicator policy:
        //   - header band (cursor_on_header) → strip insertion bar only
        //                                       (overlay 안 그림)
        //   - body Center / split            → rectangle overlay
        // 두 인디케이터가 동시에 뜨지 않게 mutually exclusive.
        let current_zone = self.drop_target_at(self.cursor_px.0, self.cursor_px.1);
        let cursor_on_header = matches!(current_zone, Some((_, DropZone::Center))) && {
            // 헤더 = pane_top ~ pane_top + header_band. body_top
            // 10px 위까지 관대 (좁은 헤더에서 마우스 못 맞추는 거 방지).
            let cur_y = self.cursor_px.1;
            let leaves = self
                .pty_layout
                .as_ref()
                .map(|t| t.leaves().len())
                .unwrap_or(1);
            let header_band = if leaves > 1 { PANE_HEADER_HEIGHT } else { 0.0 };
            current_zone
                .as_ref()
                .and_then(|(id, _)| {
                    let tree = self.pty_layout.as_ref()?;
                    let (cols, rows) = self.window_cells();
                    tree.leaf_rects(cols, rows)
                        .into_iter()
                        .find(|(i, ..)| i == id)
                        .map(|(_, _, cy, _, _)| TITLE_HEIGHT + cy as f32 * self.cell.h)
                })
                .map(|pane_top| cur_y < pane_top + header_band + 10.0)
                .unwrap_or(false)
        };
        // Overlay shows when cursor is over a pane BODY (split zone or
        // body-Center). Header-Center routes to the strip insertion bar.
        let zone_overlay_active = tab_drag_active && current_zone.is_some() && !cursor_on_header;
        let drop_zone_rect: Option<(f32, f32, f32, f32)> = show_drop_zone
            .then_some(current_zone)
            .flatten()
            .filter(|_| live_center || !cursor_on_header)
            .and_then(|(target, zone)| {
                let tree = self.pty_layout.as_ref()?;
                let leaves = tree.leaves().len();
                let (cols, rows) = self.window_cells();
                let pad = WINDOW_PADDING + self.effective_sidebar_w();
                let (_, cx, cy, cw, ch) = tree
                    .leaf_rects(cols, rows)
                    .into_iter()
                    .find(|(id, ..)| *id == target)?;
                let bx = pad + cx as f32 * self.cell.w;
                let pane_top = TITLE_HEIGHT + cy as f32 * self.cell.h;
                let bw = cw as f32 * self.cell.w;
                let bh = ch as f32 * self.cell.h;
                let header_band = if leaves > 1 { PANE_HEADER_HEIGHT } else { 0.0 };
                // Split overlay는 body 영역만 색칠 (헤더 띠 침범 X).
                let body_top = pane_top + header_band;
                let body_h = (bh - header_band).max(1.0);
                Some(match zone {
                    DropZone::Left => (bx, body_top, bw / 2.0, body_h),
                    DropZone::Right => (bx + bw / 2.0, body_top, bw / 2.0, body_h),
                    DropZone::Up => (bx, body_top, bw, body_h / 2.0),
                    DropZone::Down => (bx, body_top + body_h / 2.0, bw, body_h / 2.0),
                    // 반쪽이 아니라 body 통째 — "이 pane 안으로 들어간다"는 뜻이고,
                    // 어느 쪽으로도 갈리지 않는다는 것도 같이 읽힌다.
                    DropZone::Center => (bx, body_top, bw, body_h),
                })
            });
        // Ghostty-style split seams: one 1px hairline per interior split
        // boundary instead of a 4-side border around every pane (which
        // doubled up into a thick seam between abutting panes). Coords match
        // divider_at_px so drag hit-testing lines up with the drawn line.
        let pane_seams: Vec<(f32, f32, f32, f32)> = if self.zoomed_pane.is_some() {
            // Zoom 최대화 시 형제 pane이 숨겨지므로 분할선도 생략한다 — 안 그러면
            // 가려진 split 경계선이 최대화 화면 위에 1px 선으로 남는다(C 버그).
            Vec::new()
        } else {
            self.pty_layout
                .as_ref()
                .map(|tree| {
                    let (cols, rows) = self.window_cells();
                    let pad = WINDOW_PADDING + self.effective_sidebar_w();
                    // True window edges (logical). window_cells floors the grid,
                    // so a seam spanning the last row/col must reach past the grid
                    // to the real edge — otherwise it stops short like box_h did.
                    // ⚠️ 오른쪽 끝은 **창 끝이 아니라 우측 컬럼(Git·Info) 앞**이다.
                    // 격자는 `window_cells` 가 그 폭을 이미 접어 두는데 이 선만 창 끝을
                    // 써서, 마지막 열까지 걸친 가로선이 열려 있는 패널을 관통했다
                    // (사용자: "73·27 사이 선이 우측 패널까지 뚫어버려").
                    let (win_right, win_bottom) = self.window.as_ref().map_or(
                        (
                            pad + cols as f32 * self.cell.w,
                            TITLE_HEIGHT + rows as f32 * self.cell.h,
                        ),
                        |w| {
                            let s = w.scale_factor() as f32 * self.ui_zoom;
                            (
                                w.inner_size().width as f32 / s - self.effective_right_chrome_w(),
                                w.inner_size().height as f32 / s,
                            )
                        },
                    );
                    tree.dividers(cols, rows)
                        .into_iter()
                        .map(|d| match d.dir {
                            kasa_pty::SplitDir::Horizontal => {
                                let x = pad + d.edge as f32 * self.cell.w;
                                let y0 = TITLE_HEIGHT + d.span_start as f32 * self.cell.h;
                                let y1 = if d.span_start + d.span_len >= rows {
                                    win_bottom
                                } else {
                                    TITLE_HEIGHT + (d.span_start + d.span_len) as f32 * self.cell.h
                                };
                                (x, y0, 1.0, (y1 - y0).max(0.0))
                            }
                            kasa_pty::SplitDir::Vertical => {
                                let y = TITLE_HEIGHT + d.edge as f32 * self.cell.h;
                                let x0 = pad + d.span_start as f32 * self.cell.w;
                                let x1 = if d.span_start + d.span_len >= cols {
                                    win_right
                                } else {
                                    pad + (d.span_start + d.span_len) as f32 * self.cell.w
                                };
                                (x0, y, (x1 - x0).max(0.0), 1.0)
                            }
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        // Left window-tab sidebar geometry. Cache the hit rects for the
        // mouse handler; the gpu block below paints from the same numbers so
        // a click always lands on what the user sees.
        let sb_win_h = win_px.1 / scale;
        // 목록 배치를 재기 **전에** 동결을 한 번 재검사한다 — 커서가 떠났거나 시한이
        // 지났으면 여기서 녹고, 그 프레임의 배치가 곧 재정렬이 된다.
        self.tick_close_freeze();
        // 아래 `self.gpu.as_mut()` 블록 안에서는 `self` 를 다시 못 읽는다. 되살리기
        // 패널에 넘길 동결값은 그래서 여기서 미리 뽑아 둔다.
        let frozen_info = self
            .close_freeze
            .info_content
            .filter(|_| self.close_freeze.live());
        let frozen_tabs = self
            .close_freeze
            .live()
            .then(|| self.close_freeze.tab_slots.clone())
            .flatten();
        self.refresh_window_labels();
        let sb_labels = self.window_labels.clone();
        let sb_room_numbers: Vec<_> = (0..self.windows.len())
            .map(|i| self.room_number_for_window(i)).collect();
        let local_count = (0..self.windows.len())
            .filter(|&i| self.remote_view_of_window(i).is_none()).count();
        self.info.navigation.room_numbers = self.remote_room_navigation().into_iter().enumerate()
            .map(|(n, (label, window, room))| (label, window, room, local_count + n)).collect();
        self.info.navigation.local_content_h = self.sidebar_local_content_h();
        self.info.navigation.shared_scroll = self.sidebar_scroll_px.clamp(0.0, self.sidebar_max_scroll(sb_win_h));
        // 방마다 탭 글리프 — 내부 방(설정·보드)은 셸이 아니라 자기 아이콘을 단다.
        // 이름으로 고르는 `tab_icon_glyph` 는 「설정」에도 터미널 글리프를 줘서
        // 타이틀 알약이 `>_ Settings` 였다. 페인트 루프는 `&self` 를 못 빌리므로
        // 여기서 인덱스별로 늘어놓는다.
        let sb_icons: Vec<&'static str> = (0..sb_labels.len())
            .map(|i| match self.internal_room_kind_at(i) {
                Some(crate::internal_room::InternalRoomKind::Settings) => "settings-2",
                None => tab_icon_glyph(&sb_labels[i].0),
            })
            .collect();
        let (sb_tabs, sb_closes, sb_plus, sb_rows, sb_mini, sb_undock) =
            self.sidebar_layout(sb_win_h);
        // Windowed strip: publish the effective first/visible-count for the
        // wheel handler's clamp, and note per-side overflow for the chevron
        // hints painted with the tabs below.
        if self.tabs_on_top {
            self.win_tab_first = sb_tabs.first().map_or(0, |(i, _)| *i);
        }
        self.win_tab_vis = sb_tabs.len().max(1);
        // 막대는 `win_tab_first` 가 이 프레임 값으로 갱신된 **뒤에** 재야 한다.
        let sb_scroll = self.sidebar_scroll_geom(sb_win_h);
        // 방 목록이 실제로 보이는 세로 구간 `(top, height)`. 그리기의 시저와 아래
        // 히트렉트 자르기가 **같은 값**을 봐야 한다 — 갈리면 화면엔 없는 방이 눌린다.
        let sb_view = (self.sidebar_content_top(), self.sidebar_avail_h(sb_win_h));
        // 기기 절까지 합친 높이 — 절 배치는 그 안에서 스스로 나눈다.
        let sb_full_h = self.sidebar_full_avail_h(sb_win_h);
        // 지금 보는 창이 다른 기기 방의 보기 창이면 그 기계 절의 카드가 「보는 중」으로 선다.
        self.info.navigation.viewing = self.remote_view_of_window(self.active_window);
        // 그 방 안에서 지금 포커스한 거울의 원본 pane — 본기기 배치도의 「지금 보는 칸」 테두리.
        self.info.navigation.viewing_cur = self.ws.lock().ok().and_then(|ws| ws.active_pane.clone())
            .and_then(|pane| kasa_mcp::remote::remote_info(&pane)).map(|info| info.remote_id);
        let sb_over_before = self.win_tab_first > 0;
        let sb_over_after = sb_tabs
            .last()
            .is_some_and(|(i, _)| i + 1 < self.windows.len());
        // Only register hit-rects when tabs are actually painted (side strip
        // open, or top-tabs mode where they always live in the title bar). A
        // hidden sidebar (file-tree-only / collapsed) must not leave stale tab
        // rects that a header-drag would false-hit as a cross-window drop.
        let sidebar_shown = self.tabs_on_top || self.tab_strip_w() > 0.0;
        // 시저는 픽셀만 자른다. 잘려 안 보이는 부분이 눌리지 않도록 여기서 세로로
        // 교집합을 낸다 — 가로 탭 모드는 띠가 따로라 그대로 둔다.
        let clip_y = |r: (f32, f32, f32, f32)| -> Option<(f32, f32, f32, f32)> {
            if self.tabs_on_top {
                return Some(r);
            }
            let y0 = r.1.max(sb_view.0);
            let y1 = (r.1 + r.3).min(sb_view.0 + sb_view.1);
            (y1 > y0).then_some((r.0, y0, r.2, y1 - y0))
        };
        self.window_tab_rects = if sidebar_shown {
            sb_tabs
                .iter()
                .filter_map(|(i, r)| clip_y(*r).map(|c| (*i, c)))
                .collect()
        } else {
            Vec::new()
        };
        // 배치도 칸도 같은 히트 벡터에 넣는다 — 칸을 눌러도 목록 행을 누른 것과 똑같이
        // 포커스가 가고 드래그·우클릭까지 그대로 따라온다. 한 pane 이 rect 둘(칸·행)을
        // 갖지만 히트는 `find`(첫 매치)라 무해하다. **행이 먼저** 와야 좁은 칸보다
        // 누르기 쉬운 쪽이 이긴다.
        self.sidebar_row_rects = if sidebar_shown {
            sb_rows
                .iter()
                .chain(sb_mini.iter())
                .filter_map(|(i, id, r)| clip_y(*r).map(|c| (*i, id.clone(), c)))
                .collect()
        } else {
            Vec::new()
        };
        self.sidebar_mini_rects = if sidebar_shown {
            sb_mini
                .iter()
                .filter_map(|(i, id, r)| clip_y(*r).map(|c| (*i, id.clone(), c)))
                .collect()
        } else {
            Vec::new()
        };
        // 별도창 띠 칸은 따로 — 클릭은 그 OS 창을 앞으로, 우클릭은 되돌리기뿐.
        self.aux.undock_hits = if sidebar_shown {
            sb_undock
                .iter()
                .filter_map(|(i, id, r)| clip_y(*r).map(|c| (*i, id.clone(), c)))
                .collect()
        } else {
            Vec::new()
        };
        self.window_tab_close_rects = if sidebar_shown {
            sb_closes
                .iter()
                .filter_map(|(i, r)| clip_y(*r).map(|c| (*i, c)))
                .collect()
        } else {
            Vec::new()
        };
        self.room_list_add_rect = Some(sb_plus);
        // Shell picker popup layout, computed here (no GPU borrow) so the
        // click hit-list and the painted boxes share one source of truth.
        // Top tabs have room below the button, while the sidebar button lives
        // in the bottom tray and must open upward to stay inside the window.
        let menu_open = self.shell_menu_open;
        // (label, icon, cmd, chord). `chord` 는 맨 위 «기본 셸» 줄에만 붙는다 —
        // 그 줄은 새 탭 단축키가 어느 셸을 여는지 알려주는 자리라, 아래 목록에
        // 같은 셸이 또 나와도 중복이 아니라 안내다.
        let shell_items: Vec<(&'static str, &'static str, String, Option<&'static str>)> =
            if menu_open {
                let all = available_shells();
                let chord = if cfg!(target_os = "macos") {
                    "⌘T"
                } else {
                    "Ctrl Shift T"
                };
                let head = resolve_default_shell().map(|dc| {
                    // 설정에 적힌 경로가 이 기계에 없는 셸일 수 있다(다른 OS 에서
                    // 넘어온 값 등). 그때는 목록에서 못 찾으니 일반 라벨로 둔다 —
                    // 줄을 통째로 빼면 단축키를 알릴 자리가 사라진다.
                    all.iter()
                        .find(|(_, _, c)| c.eq_ignore_ascii_case(&dc))
                        .map(|(l, i, c)| (*l, *i, c.clone(), Some(chord)))
                        .unwrap_or(("기본 셸", "terminal", dc.clone(), Some(chord)))
                });
                head.into_iter()
                    .chain(all.into_iter().map(|(l, i, c)| (l, i, c, None)))
                    .collect()
            } else {
                Vec::new()
            };
        const SHELL_ITEM_H: f32 = 34.0;
        const SHELL_SEP_H: f32 = 9.0;
        let shell_has_head = shell_items
            .first()
            .is_some_and(|(_, _, _, chord)| chord.is_some());
        // 단축키가 라벨을 밀지 않도록 폭을 벌렸다.
        let menu_w_for_paint = sb_plus.2.max(240.0);
        let mut shell_sep_y: Option<f32> = None;
        #[allow(clippy::type_complexity)]
        let shell_menu_layout: Vec<(
            String,
            &'static str,
            &'static str,
            Option<&'static str>,
            (f32, f32, f32, f32),
        )> = {
            let (px, py, _, ph) = sb_plus;
            let menu_h = shell_items.len() as f32 * SHELL_ITEM_H
                + if shell_has_head { SHELL_SEP_H } else { 0.0 };
            // 위로 열 자리가 모자라면 아래로 뒤집고, 그래도 넘치면 창 안으로
            // 당긴다. 아래 트레이의 "+" 는 위로 여는 게 기본인데, 항목이 늘거나
            // 창이 짧으면 첫 줄들이 타이틀바 위로 잘려 나간다 — 잘린 줄은 클릭도
            // 안 되므로 «메뉴가 짧아 보이는» 게 아니라 항목이 사라진 것이 된다.
            let below = py + ph + 4.0;
            let above = py - menu_h - 4.0;
            let mut iy = if self.tabs_on_top || above < TITLE_HEIGHT {
                below
            } else {
                above
            };
            if iy + menu_h > sb_win_h - 4.0 {
                iy = (sb_win_h - 4.0 - menu_h).max(TITLE_HEIGHT);
            }
            shell_items
                .iter()
                .enumerate()
                .map(|(n, (label, icon, cmd, chord))| {
                    let r = (px, iy, menu_w_for_paint, SHELL_ITEM_H);
                    iy += SHELL_ITEM_H;
                    if n == 0 && shell_has_head {
                        shell_sep_y = Some((iy + SHELL_SEP_H / 2.0).round());
                        iy += SHELL_SEP_H;
                    }
                    (cmd.clone(), *label, *icon, *chord, r)
                })
                .collect()
        };
        self.shell_menu_hits = shell_menu_layout
            .iter()
            .map(|(cmd, _, _, _, r)| (cmd.clone(), *r))
            .collect();
        let sb_active = self.active_window;
        // 방 단위 "작업 중"·"방금 끝남" 플래그는 걷어냈다. 그 둘이 칩 모서리의 점
        // 쌍을 켜던 유일한 자리였는데, 상태가 바뀔 때마다 점이 두 모서리를 오가는
        // 게 정보보다 먼저 읽혔다(2026-08-11 지시). 지금은 방을 펴면 그 방의 줄이
        // 학생을 걷게 해서 도는 중임을 말하고, 카드 머리에 남은 건 손이 필요할 때만
        // 깜빡이는 동그라미 하나다.
        //
        // 방마다 pane 하나당 점 하나 — 방을 열지 않고도 "누가 나를 기다리는지"가
        // 보이게 한다(사용자). 색이 곧 상태다: 대기=danger(내가 엔터를 쳐야 풀린다) ·
        // 작업 중=accent · 방금 끝남=success · 쉬는 중=흐린 회색. 순서는 leaves
        // 순서라 pane 이 늘거나 줄기 전까진 점의 자리가 고정된다.
        //
        // `sb_busy` 하나로 뭉뚱그리던 것을 여기서 가른다 — 그건 `status != "idle"`
        // 이라 **엔터를 기다리는 pane 도 작업 중과 같은 파란 점**이었다. 정작 손이
        // 필요한 쪽이 바쁜 쪽과 구별되지 않던 게 이 화면의 가장 큰 거짓말이었다.
        let sb_dots: Vec<Vec<[u8; 4]>> = (0..sb_labels.len())
            .map(|i| {
                self.window_leaves(i)
                    .iter()
                    .chain(self.room_undocked(i).iter())
                    .map(|id| self.pane_state_color(id))
                    .collect()
            })
            .collect();
        let sb_expand_t: Vec<f32> = (0..sb_labels.len())
            .map(|i| self.expand_progress(i))
            .collect();
        let sb_row_drop: Option<(String, crate::DropZone, String)> = self
            .sidebar_row_drag
            .as_ref()
            .filter(|d| d.active)
            .and_then(|d| {
                d.target
                    .as_ref()
                    .map(|(t, z)| (t.clone(), *z, d.pane.clone()))
            });
        // 펼치기 버튼의 사각은 클릭 판정과 같은 것을 쓴다 — 페인트 루프는 `&self`
        // 를 다시 못 빌리므로(GPU 를 이미 빌렸다) 방 인덱스로 늘어놓고 들어간다.
        let sb_expand: Vec<Option<(f32, f32, f32, f32)>> = (0..sb_labels.len())
            .map(|i| {
                sb_tabs
                    .iter()
                    .find(|(ti, _)| *ti == i)
                    .and_then(|(_, r)| self.window_expand_rect(i, *r))
            })
            .collect();
        // 방 메뉴 문구도 **그 방** 기준이라 여기서 미리 읽는다 — 페인트 루프는 GPU 를
        // 이미 빌려 `&self` 메서드를 다시 못 부른다(위 `sb_expand` 와 같은 이유).
        let sb_menu_list = self
            .sidebar_menu
            .as_ref()
            .map(|(_, _, room, _)| self.room_body_is_list(*room))
            .unwrap_or(false);
        // 펼친 방의 pane 한 줄씩 — 이름·색을 여기서 뽑아 둔다. `pane_character_if_known`
        // 이 `ws` 를 잠그므로 GPU 를 빌린 페인트 루프 안에서 부르면 그 자리에서 멈춘다.
        // 줄에 적는 건 **그 pane 이 무엇을 하고 있나**(claude · zsh · 편집기…)다.
        // 학생 이름은 얼굴이 이미 말하고 있어, 글자로 한 번 더 쓰면 같은 말이 두 번
        // 나오고 정작 pane 을 가르는 정보가 자리를 잃는다(사용자: "학생이름은 빼고").
        // 배치도 칸에 쓸 활성 pane — 칸마다 락을 잡지 않게 여기서 한 번만 뜬다
        // (페인트 루프는 gpu 를 빌린 상태라 `&self` 메서드도 못 부른다).
        let sb_active_pane = self.ws.lock().unwrap().active_pane.clone();
        // 배치도 칸과 꼬리 줄이 **같은 것**을 말한다 — 한쪽만 고치면 같은 pane 이
        // 자리마다 다른 얼굴을 갖는다. 그래서 계산은 한 벌이다.
        let pane_info = |id: &String| -> SidebarRowInfo {
            {
                // 얼굴은 claude 가 붙은 pane 에만 — 셸만 도는 자리에 학생이 먼저 앉아
                // 있으면 목록이 "이미 일하는 중"이라고 거짓말한다.
                // 거울 pane 은 저쪽 claude 가 로컬 프로세스 표에 없어 `pane_claude_ready`
                // 도 `pane_character_if_known` 의 관문도 못 넘는다 — 그래서 맥미니 방의
                // 칸이 전부 빈 터미널 아이콘이었다(2026-09-07 「미니맵 테마 적용 안 돼」).
                // 화면의 statusline 표식으로 저쪽 claude 를 확인하고, 배정(unfold 가
                // 원격 명부에서 옮겨 적은 이름)을 그대로 얼굴로 쓴다.
                let who = {
                    let ws = self.ws.lock().unwrap();
                    self.display_pane_char(&ws, id)
                }
                .unwrap_or_default();
                let identity = MachineIdentity::for_pane(
                    Some(id.as_str()),
                    crate::info::cached_local_machine_name(),
                );
                let machine = identity.remote.then(|| identity.name.clone());
                // 기기색은 **남의 기계에서 온 pane 에만** 칠한다 — pane 배경(`pane_background`)과
                // 같은 규칙이다. 전에는 명부에 기계가 둘 이상이면 로컬 칸까지 이 기기색으로
                // 물들여, 배치도는 파란데 pane 배경은 그대로인 어긋남이 났다(2026-09-14 지적
                // 「연결된 기기 말고는 기본색이어야 하는 거 아니야」).
                let device = identity.remote.then_some(identity.label);
                let (is_cur, icon, tab_peeks) = {
                    let ws = self.ws.lock().unwrap();
                    let is_cur = ws.active_pane.as_deref() == Some(id.as_str());
                    // 활성 탭 기준(Deref) — 칸/줄은 pane 하나를 대표하므로
                    // 보이는 탭이 말하는 게 맞다.
                    let icon = ws
                        .panes
                        .get(id)
                        .map(|p| match &p.content {
                            PaneContent::Web(_) => "globe",
                            PaneContent::Image(_) => "image",
                            PaneContent::Markdown(_) => "file-text",
                            PaneContent::Settings => "settings",
                            _ => "terminal",
                        })
                        .unwrap_or("terminal");
                    // 배치도 칸이 「뒤에 누가 더 있다」를 말할 재료. 이미 잡은 락
                    // 통행에서 같이 꺼낸다 — 칸마다 다시 잠그면 매 프레임 pane 수만큼
                    // 늘어난다.
                    let tab_peeks = ws
                        .panes
                        .get(id)
                        .map(|p| {
                            let at = p.active_tab.min(p.tabs.len().saturating_sub(1));
                            p.tabs
                                .iter()
                                .enumerate()
                                .map(|(i, t)| {
                                    let pid = t.pid.as_deref();
                                    // 이름은 탭 알약과 같은 사슬이다(붙인 제목 →
                                    // OSC → 프로세스). 여기만 다른 규칙을 쓰면 같은
                                    // 탭이 자리마다 다른 이름으로 불린다.
                                    let label = t
                                        .title
                                        .clone()
                                        .filter(|s| !s.trim().is_empty())
                                        .or_else(|| {
                                            pid.and_then(|q| self.pty.get(q))
                                                .and_then(|p| p.osc_title())
                                                .filter(|s| !s.is_empty())
                                        })
                                        .unwrap_or_else(|| {
                                            pid.map(|q| {
                                                Self::resolve_pane_label(&self.pty, q, None)
                                            })
                                            .unwrap_or_default()
                                        });
                                    TabPeek {
                                        who: pid.and_then(|q| self.display_tab_char(&ws, q)),
                                        label,
                                        active: i == at,
                                    }
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    (is_cur, icon, tab_peeks)
                };
                let label = self.pane_row_label(id);
                let waiting = self.pane_needs_you(id);
                let act = self.pane_activity.get(id);
                // 걷게 할 조건은 헤더 진행 바와 **같은 한 벌**을 쓴다. 기다리는 중은
                // 빠진다 — 그건 도는 게 아니라 멈춘 것이고, 걸으면서 동시에 나를
                // 부르면 두 신호가 서로를 부정한다.
                let fixture_secs = crate::sidebar_pulse::fixture_busy_secs(id);
                let busy = self.pane_is_busy(id) || fixture_secs.is_some();
                let busy_secs = act
                    .and_then(|a| a.busy_since)
                    .map(|t| t.elapsed().as_secs())
                    .or(fixture_secs);
                let student = self.row_student(id);
                SidebarRowInfo {
                    student,
                    who,
                    agent: self.pty.get(id).and_then(|s| s.active_agent()).map(|k| k.as_str().to_string()).unwrap_or_default(),
                    pane: id.clone(),
                    label,
                    color: self.pane_state_color(id),
                    is_cur,
                    // 못 본 완료 — 방이 아니라 **이 줄** 이 숨쉰다(사용자: "숨쉬기효과
                    // 윈도우전체가 아니라 완료된세션하나만"). window_alert 가 아니라
                    // unread_panes 를 보는 이유: 전자는 배경 방에만 서고, 지금 보고
                    // 있는 방에서 옆 pane 이 끝난 것도 알려야 한다. 내가 그 pane 을
                    // 보는 순간 sync_dock_badge 가 지운다.
                    //
                    // 대기 중이면 양보한다 — `handle_attention` 이 unread 에도 넣기
                    // 때문에 둘이 같이 서고, 그러면 한 줄에 느린 숨과 빠른 깜빡임이
                    // 겹쳐 어느 쪽도 안 읽힌다. 급한 쪽이 이긴다.
                    alert: !waiting && self.unread_panes.contains(id),
                    waiting,
                    quiet: self.blink_quiet.contains(id),
                    error: act.is_some_and(|a| a.has_error),
                    busy,
                    stashed: self
                        .closed_panes
                        .iter()
                        .any(|c| c.stashed && c.alive && c.pane_id == *id),
                    icon,
                    tab_peeks,
                    // ⚠️ `pane_activity` 의 키는 **leaf id** 다(`refresh_pane_activity`
                    // 가 `ws.panes` 순회로 채운다). 얼굴·라벨과 달리 여기서 탭 pid 로
                    // 접으면 **오히려 어긋난다** — 함정이 반대 방향이다.
                    bg_active: act.map(|a| a.bg_active).unwrap_or(false),
                    compact_pct: act.and_then(|a| a.compact_pct),
                    busy_secs,
                    machine,
                    device,
                }
            }
        };
        let sb_row_info: Vec<SidebarRowInfo> =
            sb_rows.iter().map(|(_, id, _)| pane_info(id)).collect();
        let sb_mini_info: Vec<SidebarRowInfo> =
            sb_mini.iter().map(|(_, id, _)| pane_info(id)).collect();
        // 별도창 칸도 같은 정보로 — pane_info 는 트리를 안 보므로 트리 밖 pane 에도 된다.
        let sb_undock_info: Vec<SidebarRowInfo> =
            sb_undock.iter().map(|(_, id, _)| pane_info(id)).collect();
        // 펼친 방에서 그 방의 알림·대기를 **줄이 이미 말하고 있는가**. 말하고 있으면
        // 카드 머리는 조용히 둔다 — 같은 뜻을 두 겹으로 칠하면 결국 방 전체가 빛나
        // 고치기 전으로 돌아간다. 접힌 방은 줄이 없으니 여기에 안 들고, 머리가 계속
        // 말한다(그때는 그게 유일한 자리다).
        let mut sb_row_alert_win: std::collections::HashSet<usize> = Default::default();
        let mut sb_row_wait_win: std::collections::HashSet<usize> = Default::default();
        // 배치도 칸도 같은 말을 한다 — 칸이 통째로 숨쉬게 된 뒤로는 목록과 똑같은
        // 자격이다. 여기 안 넣으면 배치도 모드에서 칸과 머리 점이 같이 떠, 목록을
        // 고치며 없앴던 두 겹 칠하기가 그대로 되살아난다(실측: 캡처에 둘 다 떴다).
        let signalled = sb_rows
            .iter()
            .zip(sb_row_info.iter())
            .chain(sb_mini.iter().zip(sb_mini_info.iter()))
            .chain(sb_undock.iter().zip(sb_undock_info.iter()));
        for ((wi, _, _), info) in signalled {
            if info.alert {
                sb_row_alert_win.insert(*wi);
            }
            if info.waiting {
                sb_row_wait_win.insert(*wi);
            }
        }
        // 아이콘 칩 모서리의 작업 점도 같은 구분을 따른다 — 사이드바를 좁혀 두면
        // 그 점이 그 방의 유일한 표시라, 여기서만 뭉뚱그리면 좁은 모드에서 다시
        // 거짓말이 된다.
        let sb_wait: Vec<bool> = (0..sb_labels.len())
            .map(|i| {
                self.window_leaves(i)
                    .iter()
                    .chain(self.room_undocked(i).iter())
                    .any(|id| self.pane_needs_you(id))
            })
            .collect();
        // 그 기다림이 아직 깜빡일 자격이 있나 — 사람이 그 방을 보러 와 멈춘 칸만 기다리면 점은 서 있다.
        let sb_wait_loud: Vec<bool> = (0..sb_labels.len())
            .map(|i| {
                self.window_leaves(i)
                    .iter()
                    .chain(self.room_undocked(i).iter())
                    .any(|id| self.pane_needs_you(id) && !self.blink_quiet.contains(id))
            })
            .collect();
        // Per-window "unseen notification" flag: a pane finished / needs
        // attention while this window sat in the background. The tab pulses
        // (synced to the cursor blink) until the user switches to it. Unlike
        // sb_done's brief flash, this persists across the whole alert.
        let sb_alert: Vec<bool> = (0..sb_labels.len())
            .map(|i| self.window_alert.contains(&i))
            .collect();
        // 방 탭을 끌고 있는 동안 떨어질 자리. 탭 자체는 제자리에 두고 삽입선만
        // 그린다 — 실제 이동은 release 뿐이라, 놓기 전엔 "여기로 간다"만 알면 된다.
        let win_drag_target: Option<usize> = self
            .win_tab_drag
            .as_ref()
            .filter(|d| d.active)
            .map(|d| d.target);
        // Which tab the cursor is over (for hover affordance + showing × only
        // where the user is pointing, Warp-style).
        let sb_cursor = self.cursor_px;
        // ⋮ 메뉴의 대화 보기 칸 — 아래 그리기 패스는 gpu 를 빌린 채라 메서드를 못 부른다.
        let handle_chat = self.handle_menu.clone().map(|pid| {
            let ws = self.ws.lock().unwrap();
            (pid.clone(), self.pane_view_toggle(&ws, &pid))
        });
        let sb_hover = sb_tabs
            .iter()
            .find(|(_, r)| {
                sb_cursor.0 >= r.0
                    && sb_cursor.0 <= r.0 + r.2
                    && sb_cursor.1 >= r.1
                    && sb_cursor.1 <= r.1 + r.3
            })
            .map(|(i, _)| *i);
        // 조합 중인 글자는 **조합기 주인**에게만 그린다. 예전엔 pane 을 안 가려서,
        // 터미널에서 치는 한글이 열려 있는 편집기에도 같이 떴다(사용자: "입력이
        // 동시에 되고"). 주인은 `ime_focus` 가 이미 알고 있다.
        let md_preedit = self.preedit.clone();
        let ime_editor: Option<String> = match &self.ime_focus {
            Some(crate::ImeFocus::Editor(id)) => Some(id.clone()),
            _ => None,
        };
        // Raw-editor cursor blink phase (shared with the terminal cursor), read
        // before the gpu borrow so the editor cursor blinks in step.
        let raw_cursor_on = self.cursor_blink_on(std::time::Instant::now());
        // In-pane tab hit rects, collected during the header paint (needs the
        // measured tab widths) and published to self after the gpu borrow.
        let mut tab_hits: Vec<(String, usize, (f32, f32, f32, f32))> = Vec::new();
        let mut tab_close_hits: Vec<(String, usize, (f32, f32, f32, f32))> = Vec::new();
        // 계정이 바뀐 pane 의 「재시작」 칩 — gpu 대여가 끝난 뒤 self 로 옮긴다.
        let mut restart_chip_hits: Vec<(String, (f32, f32, f32, f32))> = Vec::new();
        let mut plus_hits: Vec<(String, (f32, f32, f32, f32))> = Vec::new();
        // Tab-overflow windowing per pane: (id, effective first, visible
        // count, active tab this frame) — written back to ws.panes after the
        // gpu borrow so the wheel handler and next frame's reveal check see
        // the clamped values.
        let mut pane_tab_windowing: Vec<(String, usize, usize, usize)> = Vec::new();
        let mut image_btn_hits: Vec<(String, ImageBtn, (f32, f32, f32, f32))> = Vec::new();
        // Terminal-pane right-action cluster hit rects. Rebuilt every frame
        // so a stale rect can't outlive its glyph after a layout change.
        let mut pane_action_hits: Vec<(String, ActionKind, (f32, f32, f32, f32))> = Vec::new();
        let mut confirm_btn_hits: Vec<(ConfirmBtn, (f32, f32, f32, f32))> = Vec::new();
        // `g` 대여가 끝난 뒤에 대기 상태로 되쓴다 — 그리는 동안은 `&self` 만 잡는다.
        let mut account_confirm_hits: Vec<(
            crate::session::AccountSwitchBtn,
            (f32, f32, f32, f32),
        )> = Vec::new();
        let mut swap_confirm_hits: Vec<(crate::session::CharacterSwapBtn, (f32, f32, f32, f32))> =
            Vec::new();
        let mut restore_btn_hits: Vec<(RestoreBtn, (f32, f32, f32, f32))> = Vec::new();
        let restore_toast_visible = self.restore_applying.is_some() || self.restore_progress.is_some();
        let restore_bottom_reserved = self.bottom_reserve_h();
        let win_h_logical = win_px.1 / scale;
        let settings_btn = self.settings_btn_rect(win_h_logical);
        self.settings_btn_rect = settings_btn;
        self.feedback_btn_rect = self.feedback_btn_rect(win_h_logical);
        // 트레이 기하는 `&self` 메서드라 아래 `self.gpu.as_mut()` 빌림 안에서는
        // 못 부른다 — 다른 chrome rect 들과 같이 여기서 미리 읽는다.
        let sidebar_tray = self.sidebar_tray_rects(win_h_logical);
        let file_tree_toggle = self.file_tree_toggle_rect();
        let sidebar_toggle = self.sidebar_toggle_rect();
        // 타이틀바 제목을 가운데 세울 때 오른쪽 한계로 쓴다 — 같은 이유로 여기서
        // 미리 읽는다(`g` 가 self.gpu 를 잡은 뒤엔 &self 메서드를 못 부른다).
        let git_col_toggle = self.git_col_toggle_rect();
        let settings_title = self.settings_title_rect();
        // Caret blink for the commit-modal message box, computed before `g`
        // borrows `self.gpu` (the blink helper takes `&self`).
        let commit_caret_on = self.cursor_blink_on(std::time::Instant::now());
        // Per-header completion-flash strength, sampled before `g` borrows
        // `self.gpu` (the header loop can't call `&self` while `g` is live).
        let header_flash: Vec<Option<f32>> = headers
            .iter()
            .map(|h| self.notify_flash_factor(&h.id))
            .collect();
        // "빠른 파일" 목록 — &self 메서드라 아래 &mut self.gpu 빌림 안에서는 못 부른다.
        // 빌림 전에 스냅샷(파일트리 렌더에서 로컬로 소비).
        self.track_instruction_pane();
        let quick_files_list = if self.file_tree.visible { self.quick_files() } else { Vec::new() };
        // 아래 &mut self.gpu 빌림 안에서 &self 메서드를 못 부른다 — 미리 스냅샷.
        // 학생 도트 배너 가시 상태 → 애니 타이머(handler.rs)와 damage 게이트
        // (render_frame)가 참조. 배너가 사라진 프레임에 false로 떨어져
        // 애니 redraw 펌프가 저절로 멈춘다.
        // 걷는 학생 표시는 이 프레임에서 `draw_student_walk` 가 다시 세운다 —
        // 여기서 내려야 걷던 pane 이 멈춘 뒤 타이머가 저절로 잠든다.
        STUDENT_WALK_ANIMATING.store(false, std::sync::atomic::Ordering::Relaxed);
        STUDENT_SPRITE_ANIMATING.store(
            // waiting(승인 대기)·standing(입력박스 위)은 렌더 펌프가 없는 정적
            // 상태에서도 idle 애니가 돌아야 해서 이 타이머에 의존한다. 스피너
            // 도트는 working 30fps 펌프가 있고, statusline 프사는 정적이라 불필요.
            !banner_slots.is_empty() || !waiting_slots.is_empty() || !standing_slots.is_empty(),
            std::sync::atomic::Ordering::Relaxed,
        );
        // `[Image #N]` 썸네일 — transcript 를 읽는 일이라 gpu 를 빌리기 전에 끝낸다.
        let tip_box = tip_hit.as_ref().map(|(_, _, b)| *b);
        let image_tip = self
            .pump_image_tip(tip_hit.map(|(pane, n, _)| (pane, n)))
            .zip(tip_box);
        // 헤더 드래그 pill 의 라벨 — `display_pane_char`(관문 有)를 gpu 를 빌리기
        // **전에** 떠 둔다. 아래 블록은 `g(=&mut self.gpu)` 를 잡고 있어 `&self`
        // 메서드를 못 부르는데, 그걸 인라인 사본으로 우회하던 동안 관문만 빠져
        // 셸 pane 을 끌어도 pill 에 남의 학생 이름이 따라왔다(2026-08-22). 사본을
        // 다시 만드는 대신 `footer_slots`·`claude_panes` 와 같은 스냅샷 방식으로.
        let drag_label: Option<String> = {
            let dragging = self
                .header_drag
                .as_ref()
                .filter(|hd| hd.active)
                .map(|hd| hd.pane.clone());
            dragging.map(|pane_id| {
                let ws = self.ws.lock().unwrap();
                self.display_pane_char(&ws, &pane_id).unwrap_or(pane_id)
            })
        };
        // 번호·제목·기기를 같은 활성 탭에서 떠야 탭 전환 때 서로 다른 창을
        // 가리키지 않는다. 기기 이름은 준비된 캐시만 읽어 GUI에서 fork하지 않는다.
        let pane_identities: HashMap<String, PaneIdentity> = {
            let ws = self.ws.lock().unwrap();
            let local_name = crate::info::cached_local_machine_name();
            footer_slots.iter()
                .filter_map(|(id, ..)| ws.panes.get(id).map(|pane| (id, pane)))
                .map(|(id, pane)| {
                    let tab = pane.tabs.get(pane.active_tab);
                    let tab_pid = ws.active_tab_pid(id);
                    let active_is_terminal = tab.is_some_and(|tab| tab.term().is_some());
                    let shown = pane_identity::shown_pane_id(&tab_pid, active_is_terminal);
                    let title = if self.show_pane_numbers {
                        tab.and_then(|t| t.title.clone())
                            .filter(|s| !s.trim().is_empty())
                            .or_else(|| (tab_pid == *id && pane.title_pinned)
                                .then(|| pane.title.clone()).flatten()
                                .filter(|s| !s.trim().is_empty()))
                            .or_else(|| self.display_tab_char(&ws, &tab_pid))
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    let machine = MachineIdentity::for_pane(
                        pane_identity::terminal_identity_pid(
                            &tab_pid,
                            active_is_terminal,
                        ),
                        local_name,
                    );
                    (id.clone(), PaneIdentity { shown, title, machine })
                })
                .collect()
        };
        // Resolve before borrowing the GPU; the titlebar uses the same active-tab
        // identity as terminal art, never a stale local session binding.
        let titlebar_character = {
            let ws = self.ws.lock().unwrap();
            ws.active_pane.as_ref().and_then(|id| {
                let pane = ws.panes.get(id)?;
                pane.term()?;
                let tab_pid = ws.active_tab_pid(id);
                if kasa_mcp::remote::is_remote_pane(&tab_pid) {
                    let (_, row) = crate::machinescol::remote_pane_facts(&tab_pid)?;
                    if row.get("harness").is_some_and(serde_json::Value::is_null)
                        || row.get("name").and_then(|name| name.as_str())
                            .is_none_or(|name| name.is_empty())
                    {
                        return None;
                    }
                }
                self.display_tab_char(&ws, &tab_pid)
            })
        };
        // Focus decoration uses the same current character as terminal art.
        // Resolve before borrowing the GPU, so mirrors never fall back to an
        // old local session binding or fail the local process-table gate.
        let (active_pane, pane_chars, claude_panes) = {
            let ws = self.ws.lock().unwrap();
            let chars: HashMap<String, String> = footer_slots.iter()
                .filter_map(|(id, ..)| self.display_pane_char(&ws, id).map(|name| (id.clone(), name)))
                .collect();
            let agents: std::collections::HashSet<String> = footer_slots.iter()
                .filter(|(id, ..)| {
                    let pid = ws.active_tab_pid(id);
                    self.pty.get(&pid).and_then(|p| p.active_agent()).is_some()
                        || mirror_claude_panes.contains(id)
                })
                .map(|(id, ..)| id.clone())
                .collect();
            (ws.active_pane.clone(), chars, agents)
        };
        let settings_room_active = self.settings_room_active();
        let pulse_h = self.sidebar_pulse_h();
        let sb_head_top = self.sidebar_head_top();
        // 본진 계정 조작은 백그라운드 스레드에서 끝나므로 그 자리에서 말풍선을
        // 못 띄운다. 계정 화면이 떠 있는 동안 여기서 받아 올린다 — 실패가 조용히
        // 사라지면 「눌렀는데 아무 일도 안 남」이 되고, 그 상태로 같은 버튼을
        // 반복해 누른 것이 애초에 이 기능이 생긴 이유다.
        if settings_room_active {
            self.drain_home_account_toasts();
        }
        let settings_snapshot = settings_room_active
            .then(|| {
                let left = self.effective_sidebar_w();
                // 패널 아래로 상태줄 자리를 남긴다 — 창 끝까지 차지하면 그 위에
                // 그려지는 상태줄이 패널 배경에 먹히거나 창 밖으로 밀린다.
                self.native_settings_snapshot(crate::native_settings::content_area(
                    (win_px.0 / scale, win_px.1 / scale), left, status_h,
                ))
            })
            .flatten();
        let mut settings_paint = None;
        // 원격 방 이름 편집칸 — GPU 를 빌리기 전에 읽어 둔다.
        self.info.navigation.rename = self.remote_rename_overlay();
        let claude_observations: ClaudeObservations = self.set_claude_accounts.iter()
            .map(|account| (account.id.clone(), (self.claude_account_usage(&account.id), self.claude_account_status(&account.id))))
            .collect();
        let weather_frame = self.weather_frame(
            scale,
            [win_px.0 / scale, win_px.1 / scale],
            &footer_slots,
            weather_focus.as_deref(),
            weather_guard,
        );
        let mut weather_spots = Vec::new();
        if let Some(g) = self.gpu.as_mut() {
            g.clear_chrome();
            // Upload any image pane's pixels once, then queue each for this
            // frame. The image pass (in g.render) paints under the chrome so
            // pane headers / focus ring / dim overlay land on top.
            for (id, image, _, _, rot, _) in &image_slots {
                // Per-(image, rotation, frame) cache key. The image pointer is in
                // the key because one pane can hold several image tabs — keying on
                // pane id alone made the 2nd image tab collide with the 1st's
                // texture (has_image hit → 2nd/switched image showed the 1st's
                // pixels, 사용자: 같은 pane에 이미지 띄우면 이전 게 덮어써짐).
                let cur = image.cur_idx();
                let key = format!("{id}-p{:x}-r{rot}-f{cur}", Arc::as_ptr(image) as usize);
                if !g.has_image(&key) {
                    let (rgba, w, h) = rotate_rgba_cw(image.cur_rgba(), image.w, image.h, *rot);
                    g.upload_image(&key, &rgba, w, h);
                }
            }
            if !classroom_slots.is_empty() && !g.has_image("schale:classroom") {
                if let Some((rgba, w, h)) = schale_classroom_rgba() {
                    g.upload_image("schale:classroom", &rgba, w, h);
                }
            }
            // Device identity belongs to the whole terminal, not only a header
            // that disappears for a single pane. Default terminal cells are
            // transparent, so paint below them; explicit syntax/diff/prompt
            // fills and character accents remain above this local-mode tint.
            let pane_bg_of = |id: &str| pane_identities.get(id)
                .and_then(|identity| identity.machine.pane_background(theme::pane_bg()))
                .unwrap_or_else(theme::pane_bg);
            for (id, x, y, w, h) in &footer_slots {
                // Only a decoded user background replaces the themed surface.
                if g.has_image("schale:classroom")
                    && classroom_slots.contains(&(*x, *y, *w, *h)) { continue; }
                g.rect(*x, *y, *w, *h, pane_bg_of(id));
            }
            for slot in &mut chat_slots {
                slot.bg = pane_bg_of(&slot.pane);
            }
            for slot in &mut shell_slots {
                slot.bg = pane_bg_of(&slot.pane);
            }
            g.draw_cells(&slot_views);
            paint_status_model_icons(g, &status_model_icons);
            for (id, image, (bx, by, bw, bh), zoom, rot, (pan_x, pan_y)) in &image_slots {
                let key = format!(
                    "{id}-p{:x}-r{rot}-f{}",
                    Arc::as_ptr(image) as usize,
                    image.cur_idx()
                );
                g.queue_image(&key, *bx, *by, *bw, *bh, *zoom, *pan_x, *pan_y);
            }
            paint_inline_images(g, &inline_slots);
            // 학생 도트 — Clawd 배너 자리. idle 4프레임을 캐릭터당 1회 일괄
            // 업로드해 모든 pane이 공유하고, 매 프레임 시간 기반으로 현재
            // 프레임만 queue한다(재렌더는 배너 애니 타이머가 깨워줌).
            // 디코딩 실패 시 queue_image가 조용히 skip.
            let anim_ms = self.version_anim_start.elapsed().as_millis() as u64;
            // 그리기와 이미지 키는 `paint_student_overlays` 한 곳에 있고, 여기선
            // 이 창의 좌표로 모은 자리만 넘긴다.
            let student_slots = StudentOverlays {
                banner: std::mem::take(&mut banner_slots),
                spinner: std::mem::take(&mut spinner_slots),
                waiting: std::mem::take(&mut waiting_slots),
                standing: std::mem::take(&mut standing_slots),
                profile: std::mem::take(&mut profile_slots),
            };
            paint_student_overlays(g, &student_slots, anim_ms);
            let nav_active = crate::prompt_nav::hover_pane();
            let mut nav_hits = Vec::new();
            for (pane_id, pid, source, right, top, track_h, rows, state) in &nav_slots {
                let Some(geo) = crate::prompt_nav::geometry(state, *top, *track_h, *rows) else {
                    continue;
                };
                // 격자 오른쪽 여백(PANE_INNER_X) 가운데 — 글자 칸을 덮지 않는다.
                let x = right + PANE_INNER_X / 2.0;
                let active = nav_active.as_deref() == Some(pane_id.as_str());
                crate::prompt_nav::paint(g, x, &geo, (*top, *track_h), state.current, active);
                nav_hits.push(crate::prompt_nav::NavHit {
                    pane: pane_id.clone(),
                    pid: pid.clone(),
                    source: *source,
                    // 누름 자리는 막대보다 넓되 칸 경계(나누기 손잡이)는 남긴다.
                    track: (right - 4.0, *top, PANE_INNER_X + 3.0, *track_h),
                    thumb: geo.thumb,
                    ticks: geo.ticks,
                    total: state.total,
                    visible: *rows,
                });
            }
            crate::prompt_nav::set_hits(nav_hits);
            // Claude Code 스크롤 sticky prompt: 텍스트·흰 배경은 위 스캔에서 원본
            // 셀을 선명화(등폭 유지)해 이미 그려졌다. 여기선 클릭 rect(셀 영역)만
            // STICKY_PILLS 로 mouse handler·seek 에 넘긴다 — 클릭 = "그 프롬프트가
            // 화면에 들어올 때까지 위로 스크롤"(begin_sticky_seek).
            STICKY_PILLS.with(|s| s.borrow_mut().clear());
            // 클릭 영역은 **여기서 한 번만** 비운다. 아래 sticky 루프와 그보다 뒤의
            // 턴 헤더 루프가 같은 통에 담으므로, 뒤쪽에서 또 비우면 앞에서 담은
            // 화살표가 통째로 사라진다 — 화면은 멀쩡한데 안 눌리는, 스크린샷이
            // 절대 못 잡는 부류다(실제로 그렇게 짰다가 여기서 잡았다).
            crate::turnjump::TURN_HITS.with(|s| s.borrow_mut().clear());
            for (px, py, pw, ph, text, pane_id, a_up, a_down, a_bottom) in &sticky_pill_slots {
                STICKY_PILLS.with(|s| {
                    s.borrow_mut()
                        .push((pane_id.clone(), (*px, *py, *pw, *ph), text.clone()))
                });
                // pill 에 얹은 ↑↓ — 바 클릭(위로 되짚기)보다 **나중에** 담아야
                // 겹치는 자리에서 화살표가 이긴다(조회가 역순이다).
                crate::turnjump::TURN_HITS.with(|s| {
                    let mut v = s.borrow_mut();
                    if let Some(r) = a_up {
                        v.push((pane_id.clone(), *r, crate::turnjump::TurnHit::SeekPrev));
                    }
                    if let Some(r) = a_down {
                        v.push((pane_id.clone(), *r, crate::turnjump::TurnHit::SeekNext));
                    }
                    if let Some(r) = a_bottom {
                        v.push((pane_id.clone(), *r, crate::turnjump::TurnHit::SeekBottom));
                    }
                });
            }
            // 대화 턴 헤더의 클릭 영역. 그림은 이미 셀로 그려졌고 여기선 자리만
            // 넘긴다. **바를 먼저, 화살표를 나중에** 담는 순서가 곧 우선순위다 —
            // 조회가 역순이라 겹치는 자리에서 화살표가 이긴다(화면에서 위에 있는
            // 것이 클릭도 가져간다).
            for (pane_id, bar, up, down, bottom, h) in &turn_header_slots {
                crate::turnjump::TURN_HITS.with(|s| {
                    let mut v = s.borrow_mut();
                    v.push((
                        pane_id.clone(),
                        *bar,
                        crate::turnjump::TurnHit::Jump(h.cur_abs),
                    ));
                    if let (Some(r), Some(a)) = (up, h.prev_abs) {
                        v.push((pane_id.clone(), *r, crate::turnjump::TurnHit::Prev(a)));
                    }
                    if let (Some(r), Some(a)) = (down, h.next_abs) {
                        v.push((pane_id.clone(), *r, crate::turnjump::TurnHit::Next(a)));
                    }
                    // 맨 아래로는 **갈 곳 조건이 없다** — 헤더가 떠 있다는 것이 이미
                    // 스크롤이 올라가 있다는 뜻이라, 앞뒤 화살표처럼 흐려질 일이 없다.
                    if let Some(r) = bottom {
                        v.push((pane_id.clone(), *r, crate::turnjump::TurnHit::Bottom));
                    }
                });
            }
            if g.has_image("schale:classroom") {
                for (bx, by, bw, bh) in &classroom_slots {
                    g.queue_image_cover("schale:classroom", *bx, *by, *bw, *bh);
                }
            }
            if !schale_logo_slots.is_empty() {
                if !g.has_image("schale:logo") {
                    if let Some((rgba, w, h)) = schale_logo_rgba() {
                        g.upload_image("schale:logo", &rgba, w, h);
                    }
                }
                for (bx, by, bw, bh) in &schale_logo_slots {
                    g.queue_image_above("schale:logo", *bx, *by, *bw, *bh);
                }
            }
            // /rename 세션명 아웃라인 — 입력박스 위 구분선 이름을 사각 테두리로(4변).
            for (x, y, w, h, col) in &title_outline_slots {
                let t = 1.5_f32;
                g.rect(*x, *y, *w, t, *col);
                g.rect(*x, *y + *h - t, *w, t, *col);
                g.rect(*x, *y, t, *h, *col);
                g.rect(*x + *w - t, *y, t, *h, *col);
            }
            // Markdown is laid out into chrome glyphs/rects here — after the
            // (empty) cell pass, before pane headers/borders so those land on
            // top. The returned content height feeds scroll clamping.
            // Rebuilt fresh each frame so a pane toggled out of raw mode (or
            // closed) drops its caret hit box.
            self.md_body_rects.clear();
            self.md_task_hits.clear();
            self.md_link_hits.clear();
            self.md_copy_hits.clear();
            let mut find_btn_hits: Vec<(String, FindBtn, (f32, f32, f32, f32))> = Vec::new();
            for (
                id,
                doc,
                (bx, by, bw, bh),
                scroll,
                raw_mode,
                lines,
                cursor,
                sel,
                h_scroll,
                lang,
                find,
                complete,
                diags,
                folds,
                wrap,
                extra,
                diff,
            ) in &md_slots
            {
                let content_h = if *raw_mode {
                    let lines = lines.as_ref().map_or(&[][..], |v| v.as_slice());
                    // Stash the body box so a mouse click can hit-test to a caret
                    // position (md_click_caret reads this).
                    self.md_body_rects.insert(id.clone(), (*bx, *by, *bw, *bh));
                    // 조합 중인 한글은 포커스를 가진 쪽에 그린다 — 찾기 바가
                    // 열려 있는데 문서 캐럿에 preedit 이 뜨면 어디에 쓰고 있는지
                    // 화면이 거짓말을 한다.
                    let mine = ime_editor.as_deref() == Some(id.as_str());
                    let pe = if mine { md_preedit.as_str() } else { "" };
                    let (body_pe, bar_pe): (&str, &str) = match find {
                        Some(_) => ("", pe),
                        None => (pe, ""),
                    };
                    // 펼친 헝크는 **그 줄을 덮는 헝크가 아직 있을 때만** 그린다 —
                    // 편집으로 자리를 잃은 펼침은 `store_diff` 가 닫지만, 그 사이
                    // 한 프레임을 엉뚱한 줄에 붙은 판으로 보내지 않는다.
                    let dv = diff.as_ref().map(|(d, peek)| crate::gitdiff::DiffView {
                        marks: &d.marks,
                        dels: &d.dels,
                        peek: peek
                            .and_then(|l| d.hunk_at(l).map(|h| (l, h.old.as_slice())))
                            .filter(|(_, old)| !old.is_empty()),
                    });
                    let h = g.draw_raw_editor(
                        lines,
                        *cursor,
                        *sel,
                        *bx,
                        *by,
                        *bw,
                        *bh,
                        *scroll,
                        *h_scroll,
                        lang,
                        body_pe,
                        raw_cursor_on,
                        find.as_ref().map(|f| (f.hits.as_slice(), f.idx)),
                        complete.as_ref().map(|(i, s, c)| (i.as_slice(), *s, *c)),
                        diags,
                        folds,
                        *wrap,
                        extra,
                        dv.as_ref(),
                    );
                    if let Some(f) = find {
                        for (btn, r) in Self::draw_find_bar(
                            g,
                            f,
                            *bx,
                            *by,
                            *bw,
                            bar_pe,
                            raw_cursor_on,
                            sb_cursor,
                        ) {
                            find_btn_hits.push((id.clone(), btn, r));
                        }
                    }
                    h
                } else {
                    // Upload this doc's inline images once (keyed per block).
                    for im in &doc.images {
                        if !g.has_image(&im.key) {
                            g.upload_image(&im.key, &im.rgba, im.w, im.h);
                        }
                    }
                    // 이 pane 의 선택만 넘긴다 — 마크다운 pane 이 둘일 때 다른
                    // pane 의 범위로 띠를 깔면 안 된다.
                    let sel = self
                        .md_render_sel
                        .as_ref()
                        .filter(|s| s.pane == *id)
                        .map(|s| (s.anchor.0, s.anchor.1, s.end.0, s.end.1));
                    let h = g.draw_markdown(&doc.blocks, doc.gen, *bx, *by, *bw, *bh, *scroll, sel);
                    self.md_task_hits.insert(id.clone(), g.md_task_rects.clone());
                    self.md_link_hits.insert(id.clone(), g.md_link_rects.clone());
                    self.md_copy_hits.insert(id.clone(), g.md_copy_rects.clone());
                    // 이 pane 이 그린 낱말 사각형을 옮겨 둔다 — 복사·히트테스트가
                    // 읽고, block_ys 와 같은 이유로 pane 별로 갈라야 한다.
                    let words = std::mem::take(&mut g.md_word_rects);
                    self.md_word_rects.insert(id.clone(), words);
                    // 블록별 문서좌표 y 를 pane 별로 옮겨 둔다 — Gpu 쪽은 pane
                    // 을 모르고 매 프레임 덮어써서, 마크다운 pane 이 둘이면
                    // 마지막 것만 남는다.
                    let ys = std::mem::take(&mut g.md_block_ys);
                    // Raw→Render 토글이 남긴 앵커: 이제야 새 레이아웃의 y 가
                    // 생겼으니 보던 줄을 화면 맨 위로 되돌린다. 한 프레임 늦는
                    // 건 어쩔 수 없다 — 위치는 그려봐야 알 수 있어서다.
                    if let Some(line) = self.md_scroll_anchor.remove(id) {
                        let i = doc
                            .block_lines
                            .partition_point(|&l| l <= line)
                            .saturating_sub(1);
                        let want = ys.get(i).copied().unwrap_or(0.0).max(0.0);
                        if (want - *scroll).abs() > 0.5 {
                            if let Ok(mut ws) = self.ws.lock() {
                                if let Some(pane) = ws.panes.get_mut(id) {
                                    pane.dirty = true;
                                    if let Some(m) = pane.markdown_mut() {
                                        m.scroll = want;
                                    }
                                }
                            }
                        }
                    }
                    self.md_block_ys.insert(id.clone(), ys);
                    h
                };
                self.md_content_h.insert(id.clone(), content_h);
            }
            self.md_find_rects = find_btn_hits;
            // 대화 보기 — 마크다운과 같은 자리(빈 셀 패스 뒤, pane 머리 앞)에 크롬으로 그린다.
            Self::paint_chat_views(g, &mut self.chat_view, &chat_slots, sb_cursor);
            Self::paint_shell_views(g, &mut self.shell_view, &shell_slots, sb_cursor);
            // 호버 툴팁 — pane 을 다 그린 뒤에 얹는다. pane 안에서 그리면 툴팁이
            // 경계를 넘는 순간 다음 pane 이 위를 덮어 반쪽만 남는다.
            if let Some((tip, hx, hy)) = self
                .hover
                .as_ref()
                .and_then(|h| h.text.as_ref().map(|t| (t.clone(), h.at.0, h.at.1)))
            {
                Self::draw_hover_tip(g, &tip, hx, hy, win_px.0 / scale, win_px.1 / scale);
            }
            // `[Image #N]` 썸네일 — 같은 이유로 pane 을 다 그린 뒤다. 텍스처를
            // 놓는 일이 있어 툴팁이 없는 프레임에도 부른다.
            Self::paint_image_tip(g, image_tip, win_px.0 / scale, win_px.1 / scale);
            // 크롬 판 — 위 스트립과 사이드바 칼럼이 이어진 ㄴ 자다. 본문보다 한 톤
            // 들려 있어 터미널이 그 위에 얹힌 것처럼 읽힌다.
            //
            // 사이드바 칼럼을 여기서(스트립과 같은 시점에) 칠하는 건 신호등 때문이다.
            // 칼럼이 y=0 까지 올라와야 신호등이 사이드바 위에 앉는데, 아래쪽에서 칠하면
            // 스트립에 이미 그린 토글 아이콘을 덮어 버린다.
            g.rect(0.0, 0.0, win_px.0 / scale, TITLE_HEIGHT, theme::titlebar_bg());
            if tab_strip_w > 0.0 {
                g.rect(0.0, 0.0, tab_strip_w, sb_win_h, theme::panel_bg());
                g.rect(tab_strip_w - 1.0, 0.0, 1.0, sb_win_h, theme::border());
            }
            let switch_tip = if pulse_h > 0.0 {
                crate::sidebar_pulse::draw_view_switch(g, &mut self.pulse, sb_cursor, tab_strip_w, self.sidebar_list_body)
            } else {
                self.pulse.switch = None;
                None
            };
            self.info.navigation.list_rooms.default_list = self.sidebar_list_body;
            crate::sidebar_navigation::draw(g, &mut self.info, sb_cursor, tab_strip_w,
                sb_head_top, (0.0, sb_view.0, tab_strip_w, sb_full_h));
            // 사이드바 토글. 자리는 `sidebar_toggle_rect` 가 정한다 — 접혔으면
            // 신호등 오른쪽, 폈으면 사이드바 오른쪽 위. 글리프는 그대로다(왼쪽
            // 칼럼이 찬 판 모양). 탭이 위로 가면 토글할 세로 스트립이 없다.
            if !self.lite && !self.tabs_on_top {
                let (bx, by, bw, bh) = sidebar_toggle;
                let hover = sb_cursor.0 >= bx
                    && sb_cursor.0 <= bx + bw
                    && sb_cursor.1 >= by
                    && sb_cursor.1 <= by + bh;
                if hover {
                    hover_rect(g, bx, by, bw, bh, theme::radius_sm());
                }
                // Brighter when the sidebar is open (state indicator) or on
                // hover; the panel-left SVG shape stays constant.
                let active = tab_strip_w > 0.0;
                let fg = if hover || active {
                    theme::text()
                } else {
                    theme::text_dim()
                };
                let isz = theme::ICON_SIZE;
                g.queue_icon(
                    "panel-left",
                    bx + (bw - isz) / 2.0,
                    by + (bh - isz) / 2.0,
                    isz,
                    fg,
                );
            }
            // File-tree toggle, just right of the sidebar toggle. Same chip
            // treatment; lit when the tree column is shown.
            if !self.lite {
                let (bx, by, bw, bh) = file_tree_toggle;
                let hover = sb_cursor.0 >= bx
                    && sb_cursor.0 <= bx + bw
                    && sb_cursor.1 >= by
                    && sb_cursor.1 <= by + bh;
                if hover {
                    hover_rect(g, bx, by, bw, bh, theme::radius_sm());
                }
                let active = tree_col_w > 0.0;
                let fg = if hover || active {
                    theme::text()
                } else {
                    theme::text_dim()
                };
                let isz = theme::ICON_SIZE;
                g.queue_icon(
                    "folder-tree",
                    bx + (bw - isz) / 2.0,
                    by + (bh - isz) / 2.0,
                    isz,
                    fg,
                );
            }
            // Git-column toggle, parked at the right end of the title strip
            // (the column lives on the right). This used to be drawn inline from
            // rounded rects to avoid adding an asset; that predates the shape
            // axis, and a hand-drawn 3px radius can't follow a pixel silhouette
            // the way the icon set does. It's `panel-right` now — the mirror of
            // the sidebar toggle's `panel-left`, which is what it always meant.
            if let Some((bx, by, bw, bh)) = settings_title {
                let hover = sb_cursor.0 >= bx
                    && sb_cursor.0 <= bx + bw
                    && sb_cursor.1 >= by
                    && sb_cursor.1 <= by + bh;
                if hover {
                    hover_rect(g, bx, by, bw, bh, theme::radius_sm());
                }
                let fg = if hover || settings_room_active {
                    theme::text()
                } else {
                    theme::text_dim()
                };
                let isz = theme::ICON_SIZE;
                g.queue_icon(
                    "settings-2",
                    bx + (bw - isz) / 2.0,
                    by + (bh - isz) / 2.0,
                    isz,
                    fg,
                );
            }
            // lite 는 오른쪽 패널(git 컬럼)이 없다 — 토글도 안 그린다.
            if !self.lite {
                let bw = 26.0_f32;
                let bh = 22.0_f32;
                #[cfg(not(windows))]
                let bx = win_px.0 / scale - bw - 8.0;
                // Windows paints its own min/max/close at the right edge; shove
                // the git-column toggle left of that cluster so they don't stack.
                #[cfg(windows)]
                let bx = Self::win_control_rects(win_px.0 / scale)[0].0 - 2.0 - bw;
                let by = (TITLE_HEIGHT - bh) / 2.0;
                let hover = sb_cursor.0 >= bx
                    && sb_cursor.0 <= bx + bw
                    && sb_cursor.1 >= by
                    && sb_cursor.1 <= by + bh;
                if hover {
                    hover_rect(g, bx, by, bw, bh, theme::radius_sm());
                }
                let active = git_col_w > 0.0;
                let fg = if hover || active {
                    theme::text()
                } else {
                    theme::text_dim()
                };
                let gs = 15.0_f32;
                g.queue_icon(
                    "panel-right",
                    bx + (bw - gs) / 2.0,
                    by + (bh - gs) / 2.0,
                    gs,
                    fg,
                );
            }
            // Windows frameless window controls (min / max / close) at the
            // strip's right edge. Native decorations are off on Windows, so we
            // paint and route these ourselves — same chip family as the toggles.
            #[cfg(windows)]
            {
                let ctrls = Self::win_control_rects(win_px.0 / scale);
                let icons = ["minus", "maximize", "x"];
                for (i, &(bx, by, bw, bh)) in ctrls.iter().enumerate() {
                    let hover = sb_cursor.0 >= bx
                        && sb_cursor.0 <= bx + bw
                        && sb_cursor.1 >= by
                        && sb_cursor.1 <= by + bh;
                    if hover {
                        hover_rect(g, bx, by, bw, bh, theme::radius_sm());
                    }
                    let fg = if hover {
                        theme::text()
                    } else {
                        theme::text_dim()
                    };
                    let isz = theme::ICON_SIZE;
                    g.queue_icon(
                        icons[i],
                        bx + (bw - isz) / 2.0,
                        by + (bh - isz) / 2.0,
                        isz,
                        fg,
                    );
                }
            }
            // 위 스트립: 활성 세션 알약 + 현재 경로. 세로 탭 배치 전용 —
            // 탭이 위로 가면 탭들이 이 자리를 쓴다.
            if !self.tabs_on_top {
                let (tbx, _, tbw, _) = file_tree_toggle;
                let px0 = tbx + tbw + 12.0;
                let ty = (TITLE_HEIGHT - chrome_font) / 2.0;
                // 경로는 알약 뒤에 온다 — "무엇을 보고 있나" 다음이 "어디인가"다.
                // 폭은 알약을 그린 뒤에야 정해지므로 자리만 잡아 두고 아래에서 그린다.
                // Title-bar cwd chip follows the FOCUSED pane's shell cwd —
                // resolved through pane_current_cwd: the ~700ms cwd cache first
                // (which prefers the shell's OSC 9;9 report — the only accurate
                // source under PowerShell, whose process cwd never moves), then
                // the shell pid's real cwd. Falls back to kasaterm's own cwd
                // when the pane has no PTY (image / markdown) or nothing
                // resolved. Reading the cache also keeps this off the
                // per-frame lsof / ReadProcessMemory path it used to take.
                let cwd_str = {
                    let active = self.ws.lock().ok().and_then(|w| w.active_pane.clone());
                    // Borrow the two fields explicitly rather than calling
                    // `self.pane_current_cwd()` — `self.gpu` is already mutably
                    // borrowed above, so only a disjoint field borrow compiles.
                    let cache = &self.pane_cwd_cache;
                    let pty = &self.pty;
                    active
                        .and_then(|id| {
                            cache.get(&id).cloned().or_else(|| {
                                pty.get(&id).and_then(|p| {
                                    p.reported_cwd()
                                        .or_else(|| p.shell_pid().and_then(socket::pid_cwd))
                                })
                            })
                        })
                        .or_else(|| std::env::current_dir().ok())
                        .map(|p| Self::shorten_cwd(&p))
                        .unwrap_or_default()
                };
                // Active pane title (OSC 0/2 or shell process name).
                // Active pane's accent (surface.set_color) recolors the
                // title text too, so it matches the per-pane tabs.
                let title_color = {
                    let ws = self.ws.lock().unwrap();
                    ws.active_pane
                        .as_deref()
                        .and_then(|id| ws.panes.get(id))
                        .and_then(|p| p.color)
                        .unwrap_or_else(theme::text)
                };
                // active pane 의 claude 세션이 bg_agents(background kind)에 있으면
                // 포크/백그라운드 배지. pane_claude_sid = 실제 세션(fork 시 갈라진 것).
                let title_is_bg = self
                    .ws
                    .lock()
                    .ok()
                    .and_then(|w| w.active_pane.clone())
                    .and_then(|id| self.pane_claude_sid.get(&id).cloned())
                    .is_some_and(|sid| {
                        self.bg_agents
                            .lock()
                            .map(|m| m.contains_key(&sid))
                            .unwrap_or(false)
                    });
                // 원격 pane 이면 어느 기계인지 — 화면은 이 창이지만 학생은 저 기계에서
                // 돈다. 배지가 없으면 로컬과 겉이 똑같아 「왜 반응이 없지」가 된다
                // (2026-08-28 실측: 터널이 끊긴 원격 pane 을 로컬로 알고 입력했다).
                let title_machine: Option<(String, String)> = self
                    .ws
                    .lock()
                    .ok()
                    .and_then(|w| w.active_pane.clone())
                    .and_then(|id| pane_identities.get(&id))
                    .filter(|identity| identity.machine.remote)
                    .map(|identity| (identity.machine.name.clone(), identity.machine.label.clone()));
                let title_text: String = {
                    let ws = self.ws.lock().unwrap();
                    let active = ws.active_pane.clone();
                    // claude code 가 이 pane 의 foreground 프로세스면 타이틀바에
                    // "학생 이름 · 작업명"(사용자: claude code 일 때만 학생 이름). zsh 등
                    // 일반 셸은 기존 process · tty 폴백. session-id 매칭은 /resume 시
                    // 실제 sessionId 가 주입값과 어긋나 깨졌다(사용자) → foreground 프로세스명
                    // ("claude")으로 판정해 resume·--session-id 무관하게 견고하다.
                    let claude_char = titlebar_character.clone();
                    // 목록 화면은 세션에 배정된 캐릭터와 구별되어야 한다.
                    let agents_active = active
                        .as_deref()
                        .map_or(false, |id| agents_view_panes.contains(id));
                    if agents_active {
                        let work = active
                            .as_deref()
                            .and_then(|id| ws.panes.get(id).and_then(|p| p.title.clone()))
                            .map(|t| crate::strip_activity_prefix(&t).to_string())
                            .filter(|s| !s.is_empty());
                        match work {
                            Some(w) => format!("KASA  ·  {w}"),
                            None => "KASA".to_string(),
                        }
                    } else if let Some(c) = claude_char {
                        let work = active
                            .as_deref()
                            .and_then(|id| ws.panes.get(id).and_then(|p| p.title.clone()))
                            .map(|t| crate::strip_activity_prefix(&t).to_string())
                            .filter(|s| !s.is_empty());
                        // pane 아이디를 캐릭터 뒤에 (사용자 2026-08-05: "칩 위치를 바꿔
                        // pane아이디 이런데에, 거기는 /rename 들어갈 자리니까").
                        //
                        // 헤더 띠가 아니라 **타이틀바**에 붙이는 이유: 학생 헤더 띠는
                        // 사용자가 폐기했고(main.rs:2146) 학생 정체는 그때 타이틀바로
                        // 옮겨졌다. 단일 탭 pane 은 `has_header()` 가 false 라 띠 쪽에만
                        // 붙이면 **사용자 화면엔 안 보인다** — 하네스만 통과하는 그 모양이
                        // 오늘 두 번 물었다. 띠를 되살리면 사용자가 회수한 세로 공간이
                        // pane 마다 다시 나가므로 그건 그의 결정이다.
                        let with_id = match active.as_deref() {
                            Some(id) => {
                                let shown = pane_identities.get(id)
                                    .map(|identity| identity.shown.as_str()).unwrap_or(id);
                                format!("{c} {shown}")
                            }
                            None => c,
                        };
                        match work {
                            Some(w) => format!("{with_id}  ·  {w}"),
                            None => with_id,
                        }
                    } else {
                        let title = active
                            .as_deref()
                            .and_then(|id| {
                                ws.panes.get(id).map(|p| (id.to_string(), p.title.clone()))
                            })
                            .and_then(|(id, osc)| {
                                osc.filter(|s| !s.is_empty()).or_else(|| {
                                    self.pty
                                        .get(&id)
                                        .and_then(|p| p.active_process_name())
                                        .filter(|s| !s.is_empty())
                                })
                            })
                            .unwrap_or_default();
                        title
                    }
                };
                // The title is a label on the header, not a separate button.
                // Leave the strip's panel_bg visible so dark/light and custom
                // palette changes do not leave a differently coloured pill.
                //
                // 가운데에 세우는 것은 **이름 칩 하나**다. 경로는 파일트리 버튼
                // 오른쪽 제자리에 남는다(사용자) — 둘을 한 덩어리로 묶어 가운데를
                // 잡으면 뒤에 붙는 경로 길이만큼 이름이 왼쪽으로 밀려, 정작
                // 가운데 오는 것은 이름과 경로 사이 빈 자리가 된다. 칩이 경로와
                // 오른쪽 토글 사이에 안 들어갈 만큼 길면 중앙을 포기하고 경로
                // 오른쪽에 붙인다 — 잘리는 것보다 낫다.
                // 포크/백그라운드 세션이면 이름 뒤에 dim 배지(⑂ = 분기 기호).
                const BG_BADGE: &str = "  ⑂ bg";
                let machine_badge = title_machine.as_ref().map(|(name, _)| format!("  ⇄ {name}"));
                let gl = 14.0_f32;
                let pad = 10.0_f32;
                let ph = 26.0_f32;
                let py = (TITLE_HEIGHT - ph) / 2.0;
                let isz = theme::ICON_SIZE;
                let (tw, bw, mw) = if title_text.is_empty() {
                    (0.0, 0.0, 0.0)
                } else {
                    (
                        g.measure_chrome_text(&title_text, chrome_font, true),
                        if title_is_bg {
                            g.measure_chrome_text(BG_BADGE, chrome_font, false)
                        } else {
                            0.0
                        },
                        machine_badge
                            .as_deref()
                            .map(|b| g.measure_chrome_text(b, chrome_font, true))
                            .unwrap_or(0.0),
                    )
                };
                let pw = if title_text.is_empty() {
                    0.0
                } else {
                    pad + gl + 7.0 + tw + bw + mw + pad
                };
                // 경로는 파일트리 칼럼 오른쪽에 붙은 표시라, 그 칼럼이 좁아지면 함께
                // 줄어야 한다. 안 줄이면 640px 창에서 132px 칼럼 옆의 경로가 250px
                // 자리까지 뻗어, 가운데 서야 할 이름 칩을 오른쪽으로 밀어냈다.
                // 가장 좁을 때 아예 접는 것은 같은 경로가 파일트리 루트와 하단 칩에도
                // 있어서다 — 자리가 없으면 중복부터 버린다.
                let cwd_str = {
                    let text_x0 = px0 + 12.0 + isz + 6.0;
                    let cap = if tree_col_w <= 0.0 {
                        f32::MAX
                    } else if Density::of(tree_col_w, FILE_TREE_DENSE_FULL, FILE_TREE_DENSE_COMPACT)
                        .is_icon()
                    {
                        0.0
                    } else {
                        (tree_col_x + tree_col_w - 8.0 - text_x0).max(0.0)
                    };
                    crate::info::fit_text_tail(g, &cwd_str, cap, chrome_font, false)
                };
                let cwd_w = if cwd_str.is_empty() {
                    0.0
                } else {
                    12.0 + isz + 6.0 + g.measure_chrome_text(&cwd_str, chrome_font, false)
                };
                let win_w = win_px.0 / scale;
                let right_lim = git_col_toggle.map_or(win_w - 8.0, |(x, ..)| x - 12.0);
                // 칩은 왼쪽 칼럼(파일트리) 위로 내려앉지 않는다 — 좁은 창에서 가운데가
                // 마침 그 경계와 겹치면 파일 목록에 딱 붙어 튀어나온 것처럼 보였다
                // (640px 실측: 경계도 236, 중앙 시작도 236).
                let left_lim = (px0 + cwd_w).max(if tree_col_w > 0.0 {
                    tree_col_x + tree_col_w + 10.0
                } else {
                    0.0
                });
                // 남은 자리보다 칩이 크면 이름을 줄인다. 안 줄이면 clamp 가 왼쪽을
                // 우선해 칩이 오른쪽 버튼들 밑으로 뻗는다.
                let (title_text, tw, pw) = {
                    let avail = (right_lim - left_lim).max(0.0);
                    let fixed = pad + gl + 7.0 + bw + mw + pad;
                    if title_text.is_empty() || pw <= avail {
                        (title_text, tw, pw)
                    } else if avail - fixed <= 8.0 {
                        (String::new(), 0.0, 0.0)
                    } else {
                        let cap = avail - fixed;
                        let mut keep = String::new();
                        for ch in title_text.chars() {
                            let trial = format!("{keep}{ch}…");
                            if g.measure_chrome_text(&trial, chrome_font, true) > cap {
                                break;
                            }
                            keep.push(ch);
                        }
                        if keep.is_empty() {
                            (String::new(), 0.0, 0.0)
                        } else {
                            let t = format!("{keep}…");
                            let w = g.measure_chrome_text(&t, chrome_font, true);
                            (t, w, fixed + w)
                        }
                    }
                };
                let start = ((win_w - pw) / 2.0).clamp(left_lim, (right_lim - pw).max(left_lim));
                // 라이트: 창 제목 하나만 가운데 — 경로 칩·아이콘·알약·배지 없음. 셸이
                // 제목을 안 정했으면(OSC 없음) 경로가 제목이다(Ghostty 셸 통합과 같다).
                if self.lite {
                    let osc = {
                        let ws = self.ws.lock().unwrap();
                        ws.active_pane
                            .as_deref()
                            .and_then(|id| self.pty.get(id))
                            .and_then(|p| p.osc_title())
                            .filter(|t| !t.trim().is_empty())
                    };
                    let plain = osc.unwrap_or_else(|| cwd_str.clone());
                    if !plain.is_empty() {
                        let w = g.measure_chrome_text(&plain, chrome_font, false);
                        let x = ((win_w - w) / 2.0).max(TRAFFIC_LIGHT_WIDTH + 12.0);
                        g.draw_text(
                            x,
                            ty,
                            &plain,
                            gpu::DrawOpts {
                                font_size: chrome_font,
                                color: theme::text_dim(),
                                bold: false,
                                italic: false,
                            },
                        );
                    }
                }
                if !self.lite && !title_text.is_empty() {
                    let icon_name = sb_icons
                        .get(sb_active)
                        .copied()
                        .unwrap_or_else(|| tab_icon_glyph(&title_text));
                    g.queue_icon(
                        icon_name,
                        start + pad,
                        py + (ph - gl) / 2.0,
                        gl,
                        theme::text_dim(),
                    );
                    let tx = start + pad + gl + 7.0;
                    g.draw_text(
                        tx,
                        ty,
                        &title_text,
                        gpu::DrawOpts {
                            font_size: chrome_font,
                            color: title_color,
                            bold: true,
                            italic: false,
                        },
                    );
                    if title_is_bg {
                        g.draw_text(
                            tx + tw,
                            ty,
                            BG_BADGE,
                            gpu::DrawOpts {
                                font_size: chrome_font,
                                color: theme::text_mute(),
                                bold: false,
                                italic: false,
                            },
                        );
                    }
                    if let (Some(b), Some(machine)) =
                        (machine_badge.as_deref(), title_machine.as_ref().map(|(_, label)| label.as_str()))
                    {
                        // The title strip must use the same device identity as
                        // the pane header and minimap, not the theme accent.
                        g.draw_text(
                            tx + tw + bw,
                            ty,
                            b,
                            gpu::DrawOpts {
                                font_size: chrome_font,
                                color: theme::enforce_contrast_at(
                                    machine_tint(machine), theme::panel_bg(), 4.5,
                                ),
                                bold: true,
                                italic: false,
                            },
                        );
                    }
                }
                if !self.lite && !cwd_str.is_empty() {
                    let isz = theme::ICON_SIZE;
                    let cx0 = px0 + 12.0;
                    g.queue_icon(
                        "folder",
                        cx0,
                        (TITLE_HEIGHT - isz) / 2.0,
                        isz,
                        theme::text_mute(),
                    );
                    g.draw_text(
                        cx0 + isz + 6.0,
                        ty,
                        &cwd_str,
                        gpu::DrawOpts {
                            font_size: chrome_font,
                            color: theme::text_dim(),
                            bold: false,
                            italic: false,
                        },
                    );
                }
            }
            // Shell picker popup painter — stacked under the "+" button
            // (sb_plus) in either tab mode, so the side strip and the top-tab
            // bar share one popup. Layout + hit rects were computed before the
            // GPU borrow so clicks land on the same boxes we paint.
            let paint_shell_menu = |g: &mut gpu::GpuRenderer| {
                if !menu_open || shell_menu_layout.is_empty() {
                    return;
                }
                let (px, py, _, _) = sb_plus;
                let backdrop_h = shell_menu_layout.len() as f32 * SHELL_ITEM_H
                    + if shell_sep_y.is_some() { SHELL_SEP_H } else { 0.0 }
                    + 8.0;
                // 첫 항목에서 되짚는다 — 위/아래 뒤집기와 창 안 당김을 레이아웃이
                // 이미 정했는데 여기서 같은 분기를 또 쓰면 둘이 어긋난다.
                let backdrop_y = shell_menu_layout
                    .first()
                    .map(|(_, _, _, _, r)| r.1 - 4.0)
                    .unwrap_or(py);
                round_rect(
                    g,
                    px - 4.0,
                    backdrop_y,
                    menu_w_for_paint + 8.0,
                    backdrop_h,
                    theme::radius_md(),
                    theme::surface_active(),
                );
                // 기본 셸 줄과 «다른 셸로 열기» 목록을 가르는 선. 둘은 성격이
                // 달라서(하나는 안내, 아래는 선택지) 붙여 두면 첫 줄이 목록의
                // 일부로 읽힌다.
                if let Some(sy) = shell_sep_y {
                    g.rect(px + 12.0, sy, menu_w_for_paint - 24.0, 1.0, theme::border());
                }
                for (_, label, icon, chord, (ix, iy, iw, ih)) in &shell_menu_layout {
                    let hov = sb_cursor.0 >= *ix
                        && sb_cursor.0 <= *ix + *iw
                        && sb_cursor.1 >= *iy
                        && sb_cursor.1 <= *iy + *ih;
                    if hov {
                        hover_rect(g, *ix, *iy, *iw, *ih, theme::radius_md());
                    }
                    // 셸 아이콘은 브랜드색을 그대로 쓰는 filled SVG 다 — 모노크롬
                    // 틴트로 그리면 다섯 줄이 도로 같은 실루엣이 된다.
                    if icon.starts_with("sh/") {
                        g.queue_icon_colored(
                            icon,
                            *ix + 12.0,
                            *iy + (*ih - theme::ICON_SIZE) / 2.0,
                            theme::ICON_SIZE,
                            1.0,
                        );
                    } else {
                        g.queue_icon(
                            icon,
                            *ix + 12.0,
                            *iy + (*ih - theme::ICON_SIZE) / 2.0,
                            theme::ICON_SIZE,
                            theme::text_dim(),
                        );
                    }
                    g.draw_text(
                        *ix + 38.0,
                        *iy + (*ih - 14.0) / 2.0,
                        label,
                        gpu::DrawOpts {
                            font_size: 14.0,
                            color: theme::text(),
                            bold: false,
                            italic: false,
                        },
                    );
                    if let Some(chord) = chord {
                        let cw = g.measure_chrome_text(chord, 11.5, false);
                        g.draw_text(
                            *ix + *iw - 12.0 - cw,
                            *iy + (*ih - 11.5) / 2.0,
                            chord,
                            gpu::DrawOpts {
                                font_size: 11.5,
                                color: theme::text_mute(),
                                bold: false,
                                italic: false,
                            },
                        );
                    }
                }
            };
            // Horizontal window tabs in the title strip (Windows Terminal-
            // style). Same rects + per-window state as the side strip — only
            // the paint differs: compact one-line pills, active cue = box +
            // top accent stroke, status dots on the leading glyph.
            if self.tabs_on_top {
                for (i, (tx, ty, tw, th)) in &sb_tabs {
                    let is_active = *i == sb_active;
                    let is_hover = sb_hover == Some(*i);
                    // 이미 활성인 탭도 누를 수 있는 자리다 — 배경이 안 바뀌어도
                    // 커서는 손가락이어야 한다.
                    g.hover_pointer |= is_hover;
                    if is_active {
                        round_rect(
                            g,
                            *tx,
                            *ty,
                            *tw,
                            *th,
                            theme::radius_sm(),
                            theme::surface_active(),
                        );
                        g.rect(
                            *tx + 5.0,
                            *ty,
                            *tw - 10.0,
                            ACTIVE_ACCENT_STROKE,
                            theme::accent(),
                        );
                    } else if is_hover {
                        hover_rect(g, *tx, *ty, *tw, *th, theme::radius_sm());
                    } else {
                        // Faint resting fill so background tabs still read as
                        // tabs (Windows Terminal-style), not floating labels.
                        let resting = theme::lerp(theme::surface_hover(), theme::bg(), 0.55);
                        round_rect(g, *tx, *ty, *tw, *th, theme::radius_sm(), resting);
                    }
                    let (name, _cwd) = sb_labels
                        .get(*i)
                        .cloned()
                        .unwrap_or_else(|| (format!("win {}", i + 1), String::new()));
                    let isz = 14.0_f32;
                    let icon_x = *tx + 8.0;
                    let icon_y = *ty + (*th - isz) / 2.0;
                    g.queue_icon(
                        sb_icons.get(*i).copied().unwrap_or_else(|| tab_icon_glyph(&name)),
                        icon_x,
                        icon_y,
                        isz,
                        if is_active {
                            theme::text_dim()
                        } else {
                            theme::text_mute()
                        },
                    );
                    // 세로 사이드바와 같은 규칙 — 자리 고정, 색이 말하고, 깜빡인다.
                    // 여기도 모서리 두 곳을 오가던 점 쌍을 하나로 합쳤다.
                    if sb_wait.get(*i).copied().unwrap_or(false) {
                        if sb_wait_loud.get(*i).copied().unwrap_or(false) {
                            blink_dot(g, icon_x + isz - 3.0, icon_y - 3.0, 6.0, theme::attention(), 0.9);
                        } else {
                            circle_rect(g, icon_x + isz - 3.0, icon_y - 3.0, 6.0, theme::attention());
                        }
                    } else if sb_alert.get(*i).copied().unwrap_or(false) {
                        blink_dot(
                            g,
                            icon_x + isz - 3.0,
                            icon_y - 3.0,
                            6.0,
                            theme::accent(),
                            1.6,
                        );
                    }
                    let show_close = sb_tabs.len() > 1 && (is_active || is_hover);
                    let text_x = icon_x + isz + 6.0;
                    let avail = (*tx + *tw - text_x - if show_close { 24.0 } else { 8.0 }).max(0.0);
                    let budget = (avail / 7.8).floor().max(2.0) as usize;
                    g.draw_text(
                        text_x,
                        *ty + (*th - 12.5) / 2.0,
                        &clip_display_width(&name, budget),
                        gpu::DrawOpts {
                            font_size: 12.5,
                            color: if is_active {
                                theme::text()
                            } else {
                                theme::text_dim()
                            },
                            bold: is_active,
                            italic: false,
                        },
                    );
                    if show_close {
                        if let Some((_, (cx, cy, cw, ch))) =
                            sb_closes.iter().find(|(ci, _)| ci == i)
                        {
                            let x_hover = sb_cursor.0 >= *cx
                                && sb_cursor.0 <= *cx + *cw
                                && sb_cursor.1 >= *cy
                                && sb_cursor.1 <= *cy + *ch;
                            if x_hover {
                                hover_rect(g, *cx, *cy, *cw, *ch, theme::radius_sm());
                            }
                            let xcol = if x_hover {
                                theme::text()
                            } else {
                                theme::text_mute()
                            };
                            g.queue_icon(
                                "x",
                                *cx + (*cw - 12.0) / 2.0,
                                *cy + (*ch - 12.0) / 2.0,
                                12.0,
                                xcol,
                            );
                        }
                    }
                }
                // "+" new-tab button after the last tab.
                let (px, py, pw, ph) = sb_plus;
                let plus_hover = sb_cursor.0 >= px
                    && sb_cursor.0 <= px + pw
                    && sb_cursor.1 >= py
                    && sb_cursor.1 <= py + ph;
                if plus_hover {
                    hover_rect(g, px, py, pw, ph, theme::radius_sm());
                }
                g.queue_icon(
                    "plus",
                    px + (pw - theme::ICON_SIZE) / 2.0,
                    py + (ph - theme::ICON_SIZE) / 2.0,
                    theme::ICON_SIZE,
                    theme::text_mute(),
                );
                // Overflow chevrons in the strip's reserved 14px end slots —
                // more tabs exist past this edge, wheel over the strip scrolls.
                if let Some((_, (fx, fy, _, fh))) = sb_tabs.first() {
                    let cis = 12.0_f32;
                    let cy = fy + (fh - cis) / 2.0;
                    if sb_over_before {
                        g.queue_icon("chevron-left", fx - 14.0, cy, cis, theme::text_mute());
                    }
                    if sb_over_after {
                        g.queue_icon("chevron-right", px + pw + 3.0, cy, cis, theme::text_mute());
                    }
                }
                // 재배치 드래그 중이면 떨어질 자리에 세로 막대. 마지막 탭 뒤로
                // 미는 경우만 끝 모서리에 붙는다(target == 마지막 + 1).
                if let Some(t) = win_drag_target {
                    let bar_x = sb_tabs
                        .iter()
                        .find(|(i, _)| *i == t)
                        .map(|(_, r)| r.0 - 2.0)
                        .or_else(|| {
                            sb_tabs
                                .last()
                                .filter(|(i, _)| t == i + 1)
                                .map(|(_, r)| r.0 + r.2 + 2.0)
                        });
                    if let (Some(bx), Some((_, fr))) = (bar_x, sb_tabs.first()) {
                        g.rect(bx - 1.5, fr.1, 3.0, fr.3, theme::accent());
                    }
                }
            }
            // 배치도 칸의 hover 팝업(별도창 띠·탭 명단). 사이드바 안에서 정하고 칼럼들
            // 뒤에 그린다 — 안에서 그리면 파일트리가 위에 얹혀 가린다.
            let mut sb_tips: (Option<(f32, f32, String)>, Option<(f32, f32, Vec<TabPeek>)>) =
                (switch_tip, None);
            // Window-tab sidebar, Warp-style. Painted first so per-pane
            // headers / rings layer on top at the seam.
            if tab_strip_w > 0.0 {
                let strip = sidebar::Strip { w: tab_strip_w, view: sb_view, cursor: sb_cursor };
                sidebar::paint_rooms(
                    g,
                    &strip,
                    &sidebar::Rooms {
                        tabs: &sb_tabs,
                        labels: &sb_labels,
                        active: sb_active,
                        hover: sb_hover,
                        numbers: &sb_room_numbers,
                        wait: &sb_wait,
                        wait_loud: &sb_wait_loud,
                        alert: &sb_alert,
                        row_wait: &sb_row_wait_win,
                        row_alert: &sb_row_alert_win,
                        expand: &sb_expand,
                        expand_t: &sb_expand_t,
                        dots: &sb_dots,
                        closes: &sb_closes,
                    },
                );
                let panes = sidebar::Panes {
                    rows: &sb_rows,
                    row_info: &sb_row_info,
                    mini: &sb_mini,
                    mini_info: &sb_mini_info,
                    undock: &sb_undock,
                    undock_info: &sb_undock_info,
                    active_pane: sb_active_pane.as_deref(),
                    row_drop: sb_row_drop.as_ref(),
                };
                let (undock_tip, deck_tip) = sidebar::paint_layouts(g, &strip, &panes);
                // 팝업은 여기서 안 그린다 — 파일트리·git 칼럼이 뒤에 그려져 그 위를
                // 덮는다(2026-09-09 지적 「마우스오버 명단이 파일트리창에 가려져」).
                // 칼럼들 뒤에 한 번 그린다(`paint_shell_menu` 앞).
                sb_tips = (undock_tip.or(sb_tips.0.take()), deck_tip);
                sidebar::paint_rows(g, &strip, &panes);
                sidebar::paint_edges(
                    g,
                    &strip,
                    &sidebar::Edges {
                        tabs: &sb_tabs,
                        plus: sb_plus,
                        over_before: sb_over_before,
                        over_after: sb_over_after,
                        scroll: sb_scroll,
                        drag_target: win_drag_target,
                        menu_open,
                        tray: sidebar_tray.as_ref(),
                    },
                );
                // pane 행 우클릭 메뉴 — 이 칼럼에서 **마지막**에 그린다(다른 것 위에
                // 떠야 한다). 골격은 파일트리·Info 메뉴와 같은 것을 쓴다.
                if let Some((mx0, my0, _, pane)) = self.sidebar_menu.clone() {
                    let menu = sidebar::RowMenu {
                        anchor: (mx0, my0),
                        pane: &pane,
                        room_is_list: sb_menu_list,
                        pulse_label: crate::sidebar_pulse::menu_label(self.pulse.hidden),
                        hidden: self
                            .closed_panes
                            .iter()
                            .any(|c| c.stashed && c.alive && c.pane_id == pane),
                        undocked: self.aux.terminals.iter().any(|t| t.pane_id == pane),
                        weather_on: self.weather.settings.enabled,
                        weather_now: self.weather.pane_override(&pane),
                    };
                    sidebar::paint_row_menu(g, &strip, sb_win_h, &menu, &mut self.sidebar_menu_rects);
                }
            }
            // 다른 기기 방 카드의 우클릭 메뉴 — 본기기 방 메뉴와 같은 자리(칼럼의 맨 끝).
            crate::sidebar_navigation::draw_menu(g, &mut self.info, sb_cursor, tab_strip_w, sb_win_h,
                crate::sidebar_pulse::menu_label(self.pulse.hidden));
            // ── File-tree column ── independent of the tab strip, parked just
            // right of it (VSCode explorer). Root = active pane's cwd; folders
            // first — click a folder to expand, a file to preview. Rows laid
            // out + hit rects cached here (window-tab pattern); the read_dir
            // build lives in refresh_file_tree, never per-frame.
            if tree_col_w > 0.0 {
                // Inlined (not the `active_preview_path` helper) so it borrows
                // only `self.ws`, disjoint from the `g` mutable borrow alive here.
                let active_file: Option<std::path::PathBuf> = self.ws.lock().ok().and_then(|ws| {
                    ws.active_pane
                        .as_ref()
                        .and_then(|id| ws.panes.get(id).and_then(|p| p.preview_path.clone()))
                });
                let ft_cwd = self
                    .ws
                    .lock()
                    .ok()
                    .and_then(|w| w.active_pane.clone())
                    .and_then(|id| self.pane_cwd_cache.get(&id).cloned());
                file_tree_column::paint(
                    g,
                    &mut self.file_tree,
                    &file_tree_column::Frame {
                        x: tree_col_x,
                        w: tree_col_w,
                        bg: tree_col_bg,
                        viewport: (win_px.0 / scale, sb_win_h),
                        status_h,
                        cursor: self.cursor_px,
                        caret_on: commit_caret_on,
                        preedit: self.in_preedit.then_some(self.preedit.as_str()),
                        quick: &quick_files_list,
                        active_file: active_file.as_deref(),
                        cwd: ft_cwd.as_deref(),
                        git_badges: &self.window_git,
                    },
                );
            }
            // "+" 피커 팝업 — 사이드바 Settings 행·파일트리 위로 뜨는 오버레이라 그 뒤에
            // 한 번만 그린다(먼저 그리면 나중에 그린 chrome 텍스트가 팝업 위로 비친다).
            // 배치도 hover 팝업 — 파일트리 칼럼까지 그린 뒤라 그 위에 뜬다.
            if let Some((tx, ty, text)) = sb_tips.0 {
                Self::draw_hover_tip(g, &text, tx, ty, win_px.0 / scale, win_px.1 / scale);
            }
            if let Some((tx, ty, peeks)) = sb_tips.1 {
                let fs = 11.0;
                let (pad, line, dot) = (7.0, fs + 5.0, 5.0);
                let name_w = peeks
                    .iter()
                    .map(|t| g.measure_chrome_text(t.who.as_deref().unwrap_or("-"), fs, true))
                    .fold(0.0_f32, f32::max);
                let label_w = peeks
                    .iter()
                    .map(|t| g.measure_chrome_text(&t.label, fs, false))
                    .fold(0.0_f32, f32::max);
                let bw = pad * 2.0 + dot + 5.0 + name_w + 8.0 + label_w;
                let bh = pad * 2.0 + line * peeks.len() as f32;
                // 사이드바가 왼쪽이라 칸 오른쪽이 기본 자리지만, 창이 좁으면 팝업이
                // 화면 밖으로 나간다. 그땐 칸 왼쪽으로 접고, 아래로 넘치면 끌어올린다.
                let (vw, vh) = (win_px.0 / scale, win_px.1 / scale);
                let bx = if tx + bw + 4.0 > vw {
                    (tx - bw - 12.0).max(4.0)
                } else {
                    tx
                };
                let by = ty.min((vh - bh - 4.0).max(4.0));
                round_rect(g, bx, by, bw, bh, theme::radius_md(), theme::panel_bg());
                for (i, t) in peeks.iter().enumerate() {
                    let ly = by + pad + line * i as f32;
                    let who = t.who.as_deref();
                    // 점은 배치도 칸의 그 장과 **같은 색**이다. 둘을 잇는 것이 이
                    // 명단의 값이고, 색이 어긋나면 두 그림이 딴 말을 한다.
                    if let Some(c) = who.and_then(theme::character_accent) {
                        circle_rect(g, bx + pad, ly + fs / 2.0 - dot / 2.0, dot, c);
                    }
                    let (name_col, bold) = if t.active {
                        (theme::text(), true)
                    } else {
                        (theme::text_dim(), false)
                    };
                    g.draw_text(
                        bx + pad + dot + 5.0,
                        ly,
                        who.unwrap_or("-"),
                        gpu::DrawOpts {
                            font_size: fs,
                            color: name_col,
                            bold,
                            italic: false,
                        },
                    );
                    g.draw_text(
                        bx + pad + dot + 5.0 + name_w + 8.0,
                        ly,
                        &t.label,
                        gpu::DrawOpts {
                            font_size: fs,
                            color: theme::text_mute(),
                            bold: false,
                            italic: false,
                        },
                    );
                }
            }
            paint_shell_menu(g);
            // ── Git column ── right-hand chrome mirroring the file-tree column
            // on the left, but native instead of the old floating webview: the
            // poller fills `git_view` off-thread and this paints branch +
            // change list + Commit/Push, caching file-row / button hit rects
            // for the mouse handler. window_cells already reserved its width so
            // no pane overlaps it; it stops above the status bar.
            self.git.clear_panel_hit_targets();
            // 계정 칩 rect 는 Info 탭 블록에서만 채워진다 — 여기서 매 프레임 비우지
            // 않으면 다른 탭으로 옮기거나 칼럼을 닫아도 옛 좌표가 남아, 그 자리에
            // 무엇이 놓이든 클릭이 계정 드롭다운에 먼저 먹힌다(2026-08-11 지적:
            // 세션 탭의 「전체」칩이 안 눌리고 계정 메뉴가 열렸다).
            self.account_chip_rect = None;
            if git_col_w > 0.0 {
                side_column::paint(
                    g,
                    side_column::Panels {
                        git: &mut self.git,
                        info: &mut self.info,
                        sessions: &mut self.sessions_col,
                        mcp: &mut self.mcp_col,
                        closed_panes: &self.closed_panes,
                    },
                    &side_column::Frame {
                        x: git_col_x,
                        w: git_col_w,
                        bg: git_col_bg,
                        viewport: (win_px.0 / scale, win_px.1 / scale),
                        status_h,
                        cursor: self.cursor_px,
                        time_secs,
                        git_view: &git_view,
                        repo_list: &git_repo_list,
                        repo_choice: git_repo_choice.as_ref(),
                        frozen_info,
                    },
                );
            }
            // Per-pane header bar. The band is the unified BG (same as the
            // body) so there's no depth seam; a bottom hairline separates it
            // from the cell grid. The active tab is marked by a raised pill +
            // a top accent strip — not a darker "cage" — and only the active
            // tab carries a × (so clicking any inactive tab just switches).
            // (drop_pane, target) — drives the insertion bar; updated to
            // the pane the cursor is currently over (cross-pane drag).
            // Suppressed whenever the zone-overlay rectangle is showing
            // for the same drag — two simultaneous indicators is what
            // the "pane 이동이랑 같이 떠" report was about. Falls back
            // to the bar only when the cursor is outside every pane box
            // (gap / window edge).
            let tab_drag_info: Option<(String, usize)> = self
                .tab_drag
                .as_ref()
                .filter(|d| d.active && !zone_overlay_active)
                .map(|d| (d.drop_pane.clone(), d.target));
            // (source_pane, source_idx) — the tab being lifted. The source
            // tab is drawn at reduced alpha so it reads as "in transit"
            // while the user drags it into another strip.
            let tab_drag_src: Option<(String, usize)> = self
                .tab_drag
                .as_ref()
                .filter(|d| d.active)
                .map(|d| (d.pane.clone(), d.from));
            let hover_info: Option<(String, usize)> = self.pane_tab_hover.clone();
            // Active-tab top accents we need to repaint after the pane
            // dividers (BORDER) draw, so a horizontal split's seam doesn't
            // wipe the accent of the lower pane's active tab.
            let mut deferred_accents: Vec<(f32, f32, f32, [u8; 4])> = Vec::new();
            for (hi, h) in headers.iter().enumerate() {
                // Completion flash: a finished pane's header pulses SUCCESS for
                // ~1.8s (notify_flash) and fades back to BG, so a Stop-hook
                // notification has an in-window visual even when the desktop
                // alert is suppressed (focused pane).
                let hdr_bg = match header_flash[hi] {
                    Some(k) => theme::lerp(theme::header_bg(), theme::success(), 0.7 * k),
                    // 원격 pane 은 헤더 바탕을 강조색으로 은은히 물들인다 — 칩 하나로는
                    // 여러 pane 이 깔린 화면에서 훑을 때 안 걸린다(어느 pane 이 저
                    // 기계 것인지는 바탕색이 먼저 말해야 한다).
                    None if pane_identities.get(&h.id).is_some_and(|p| p.machine.remote) => {
                        pane_identities[&h.id].machine.background(theme::header_bg())
                    }
                    None => theme::header_bg(),
                };
                g.rect(h.x, h.y, h.w, PANE_HEADER_HEIGHT, hdr_bg);
                // 「일하는 중」은 머리 띠가 아니라 칸 테두리가 숨쉰다(아래 footer 루프,
                // 2026-10-07 「프로세스바 걷어내고 숨쉬기 모션으로」). 머리에 남는 건 끝이
                // 있는 compact 진행뿐이다.
                if h.compacting {
                    // compact 중 — 쓸림 대신 왼쪽부터 채워지는 바. compact 는 끝이 있는
                    // 작업이라 이 모양이 상태를 옳게 읽히고, 화면에 뜨는 알림이 teammate
                    // 메시지에 가려져도 헤더는 남는다(사용자 2026-08-13: "가끔 sm으로
                    // 가려질때도 있어"). busy 보다 먼저 봐야 한다 — compact 중에도 스피너가
                    // 돌아 busy 가 함께 참이고, 순서가 뒤면 늘 쓸림바가 이긴다.
                    let bar_h = 3.0;
                    let by = h.y + PANE_HEADER_HEIGHT - bar_h;
                    if let Some(p) = h.compact_pct {
                        // 화면의 `▰▰▱ N%` 에서 읽은 진짜 진행률(2026-08-13 지시)을
                        // **칸이 차오르는 눈금 + 숫자**로(2026-08-15 지시 — 연속
                        // 띠는 얼마나 남았는지 눈금이 없어 안 읽혔다). 숫자는
                        // 오른쪽 버튼 무리를 피해 게이지 끝에 얹는다.
                        let cf = 10.5;
                        let label = format!("{p}%");
                        let lw = g.measure_chrome_text(&label, cf, true);
                        let btn_zone = (theme::ICON_SIZE + 2.0) * 4.0 + 8.0;
                        let track_w = (h.w - btn_zone - lw - 12.0).max(30.0);
                        let used =
                            draw_compact_cells(g, h.x, by, track_w, bar_h, theme::accent(), p);
                        g.draw_text(
                            h.x + used + 5.0,
                            h.y + (PANE_HEADER_HEIGHT - cf) / 2.0,
                            &label,
                            gpu::DrawOpts {
                                font_size: cf,
                                color: theme::accent(),
                                bold: true,
                                italic: false,
                            },
                        );
                    } else {
                        g.compact_bar(h.x, by, h.w, bar_h, theme::accent());
                    }
                }
                // 계정이 바뀌었는데 이 pane 은 옛 계정으로 돈다 — 헤더에 「⟳ 재시작」
                // 칩을 띄운다. 계정은 프로세스 env 라 뜰 때 박히고 도는 프로세스는 못
                // 바꾸므로, 되띄우는 것 말고는 새 계정으로 옮길 길이 없다.
                //
                // 오른쪽 버튼 무리보다 **먼저** 그린다 — 그쪽은 x 를 오른쪽 끝에서
                // 거꾸로 잡아 나가서, 나중에 그리면 칩 위에 겹친다.
                // 탭 띠 헤더(터미널 pane 에 탭이 둘 이상)는 오른쪽에 단추 넷 대신 ⋮ 하나만 —
                // 같은 항목이 ⋮ 메뉴에 다 있고, 탭 이름이 자리를 쓴다. 기기 칩도 여기선
                // 안 그린다(2026-09-14 지시 「탭 안에 있을 때 기기 표시 없애고 단추는 ⋮ 로」).
                let tab_strip = h.tabs.len() > 1
                    && !h.is_image && !h.is_web && !h.is_editor && !h.is_markdown;
                let n_btn: f32 = if tab_strip { 1.0 } else { 4.0 };
                let btn_zone = (theme::ICON_SIZE + 2.0) * n_btn + 8.0;
                let mut chip_right = h.x + h.w - btn_zone;
                // 칩이 실제로 먹은 폭 — 아래 탭 스트립이 오른쪽에 비워 둘 자리에
                // 얹는다. 탭은 칩보다 **나중에** 그려져서, 예약이 없으면 긴 탭
                // 이름이 칩 위를 덮는다(4분할 실측: 기계 배지 위에 탭 글자가 겹쳐
                // 둘 다 안 읽혔다).
                let chip_zone_right = chip_right;
                if let Some((from, to)) = self.pane_account_stale.get(&h.active_tab_pid) {
                    let label = format!("⟳ {from} → {to} 재시작");
                    let pad = 6.0;
                    let cw = g.measure_chrome_text(&label, chrome_font, true) + pad * 2.0;
                    let ch = PANE_HEADER_HEIGHT - 6.0;
                    // 오른쪽 버튼 무리를 피해 그 왼쪽에 붙인다. 자리가 모자라면
                    // 아예 안 그린다 — 겹쳐 그리면 둘 다 못 읽는다.
                    let cx = chip_right - cw;
                    if cx > h.x + 8.0 {
                        let cy = h.y + 3.0;
                        g.round_rect_fill(cx, cy, cw, ch, 4.0, theme::attention());
                        g.draw_text(
                            cx + pad,
                            h.y + (PANE_HEADER_HEIGHT - chrome_font) / 2.0,
                            &label,
                            gpu::DrawOpts {
                                font_size: chrome_font,
                                color: theme::bg(),
                                bold: true,
                                italic: false,
                            },
                        );
                        restart_chip_hits.push((h.active_tab_pid.clone(), (cx, cy, cw, ch)));
                        chip_right = cx - 6.0;
                    }
                }
                if let Some(identity) = pane_identities.get(&h.id).filter(|_| !tab_strip) {
                    let machine = &identity.machine;
                    let font = chrome_font - 1.0;
                    let icon = 12.0;
                    let pad = 6.0;
                    let room = (chip_right - h.x - 140.0 - pad * 2.0 - icon - 5.0)
                        .max(0.0).min(156.0);
                    let label = crate::info::fit_text(g, &machine.name, room, font, false);
                    if !label.is_empty() {
                        let cw = g.measure_chrome_text(&label, font, false) + pad * 2.0 + icon + 5.0;
                        let ch = PANE_HEADER_HEIGHT - 8.0;
                        let cx = chip_right - cw;
                        let cy = h.y + 4.0;
                        // 플랫 문법: 칩은 채우지 않고 기기색 **테두리+글자**로 말한다
                        // (2026-09-14 승인 목업). 바탕은 헤더 바탕 그대로다.
                        let bg = theme::bg();
                        let fg = machine.foreground(bg);
                        g.round_rect_stroke(cx, cy, cw, ch, 4.0, 1.0, fg);
                        g.queue_icon(machine.icon(), cx + pad, cy + (ch - icon) / 2.0, icon, fg);
                        g.draw_text(
                            cx + pad + icon + 5.0,
                            h.y + (PANE_HEADER_HEIGHT - font) / 2.0,
                            &label,
                            gpu::DrawOpts { font_size: font, color: fg, bold: false, italic: false },
                        );
                        chip_right = cx - 6.0;
                    }
                }
                // Codex의 모델명은 상태줄이 맡고, 여기서는 노력도와 협업 모드만
                // 작게 붙인다. 값이 없는 구버전 rollout에는 빈 장식을 만들지 않는다.
                if let Some(status) = h.codex_status.as_deref() {
                    let max_w = (chip_right - h.x - 16.0).min(160.0).max(0.0);
                    let label = crate::info::fit_text(g, status, max_w, chrome_font - 1.0, false);
                    if !label.is_empty() {
                        let pad = 6.0;
                        let cw =
                            g.measure_chrome_text(&label, chrome_font - 1.0, false) + pad * 2.0;
                        let ch = PANE_HEADER_HEIGHT - 8.0;
                        let cx = chip_right - cw;
                        if cx > h.x + 8.0 {
                            let cy = h.y + 4.0;
                            g.round_rect_fill(
                                cx,
                                cy,
                                cw,
                                ch,
                                4.0,
                                theme::with_alpha(theme::surface_active(), 0xB0),
                            );
                            g.draw_text(
                                cx + pad,
                                h.y + (PANE_HEADER_HEIGHT - (chrome_font - 1.0)) / 2.0,
                                &label,
                                gpu::DrawOpts {
                                    font_size: chrome_font - 1.0,
                                    color: theme::text_dim(),
                                    bold: false,
                                    italic: false,
                                },
                            );
                        }
                    }
                }
                // No bottom hairline: the band == body, and the active tab
                // flows straight into the cell grid (browser-tab feel).
                // Compact glyphs — a touch bigger than the label so icons
                // read, but no longer the bulky +10 of the old design.
                let icon_size = theme::ICON_SIZE;
                let act_fg: [u8; 4] = if h.is_active {
                    theme::text_dim()
                } else {
                    theme::with_alpha(theme::text_dim(), 0x6B)
                };
                // Right action button cluster. Terminal panes get
                // split-v / split-h (new-terminal and web were dropped —
                // the +button already opens a new shell, and the web
                // overlay added complexity for little payoff). Image panes
                // keep the 4-button zoom/rotate set.
                let abw = icon_size + 2.0;
                let agap = 2.0;
                // 터미널 묶음은 별도창(external-link)까지 넷.
                // Markdown panes show explicit view/edit, save and separate-window
                // controls instead of the terminal icon cluster.
                let seg_font = 11.0_f32;
                let seg_pad = 9.0_f32;
                let (md_rendered_w, md_raw_w) = if h.is_markdown {
                    (
                        g.measure_chrome_text("보기", seg_font, false),
                        g.measure_chrome_text("편집", seg_font, false),
                    )
                } else {
                    (0.0, 0.0)
                };
                let seg_w = if h.is_markdown {
                    md_rendered_w + md_raw_w + seg_pad * 4.0
                } else {
                    0.0
                };
                let md_compact = h.w < 520.0;
                let md_save_w = if h.is_editor {
                    g.measure_chrome_text("저장", seg_font, false) + seg_pad * 2.0
                } else {
                    0.0
                };
                let md_popout_w = if h.is_editor {
                    if md_compact {
                        icon_size + 6.0
                    } else {
                        g.measure_chrome_text("새 창", seg_font, false) + icon_size + seg_pad * 2.0
                    }
                } else {
                    0.0
                };
                let md_state_w = if h.is_editor && !md_compact {
                    g.measure_chrome_text(
                        if h.md_modified { "저장 안 됨" } else { "저장됨" },
                        seg_font,
                        false,
                    ) + 8.0
                } else {
                    0.0
                };
                let md_gap_count = if !h.is_editor {
                    0.0
                } else if h.is_markdown {
                    if md_compact { 2.0 } else { 3.0 }
                } else if md_compact {
                    1.0
                } else {
                    2.0
                };
                let md_tools_w = seg_w
                    + md_save_w
                    + md_popout_w
                    + md_state_w
                    + md_gap_count * 4.0;
                let btn_cluster = if h.is_editor {
                    md_tools_w + 12.0
                } else {
                    abw * n_btn + agap * (n_btn - 1.0) + 12.0
                };
                // 위에서 그린 칩(재시작·기계·codex)만큼을 함께 비운다.
                let btn_cluster = btn_cluster + (chip_zone_right - chip_right).max(0.0);
                // ── 웹 pane 주소 pill 자리 예약 ── 탭 pill 과 우측 버튼 사이.
                // btn_cluster 에 얹어 아래 tabs_area 계산이 그대로 따라온다.
                // 탭에 최소 140px 을 남기고 남는 만큼(상한 420px)만 가진다 —
                // 좁은 pane(주소폭 70px 미만)에선 아예 접는다(탭·버튼이 먼저다).
                let plus_w_early = icon_size;
                let addr_w = if h.is_web {
                    (h.w - 8.0 - btn_cluster - plus_w_early - 16.0 - 140.0).clamp(0.0, 420.0)
                } else {
                    0.0
                };
                let addr_vis = addr_w >= 70.0;
                let btn_cluster = btn_cluster + if addr_vis { addr_w + 6.0 } else { 0.0 };
                // ── In-pane tab bar ── 그리는 몫은 `draw_pane_tabs` 한 곳에.
                let strip = draw_pane_tabs(
                    g,
                    &TabStrip {
                        tabs: &h.tabs,
                        label: &h.label,
                        x: h.x,
                        y: h.y,
                        w: h.w,
                        active_tab: h.active_tab,
                        tab_first: h.tab_first,
                        tab_last_active: h.tab_last_active,
                        is_active: h.is_active,
                        color: h.color,
                        right_reserve: btn_cluster,
                        chrome_font,
                        icon_size,
                        act_fg,
                        cursor_px: self.cursor_px,
                        hover_tab: hover_info
                            .as_ref()
                            .filter(|(p, _)| *p == h.id)
                            .map(|(_, i)| *i),
                        drag_src: tab_drag_src
                            .as_ref()
                            .filter(|(p, _)| *p == h.id)
                            .map(|(_, i)| *i),
                        drag_target: tab_drag_info
                            .as_ref()
                            .filter(|(p, _)| *p == h.id)
                            .map(|(_, i)| *i),
                        frozen_slots: frozen_tabs
                            .as_ref()
                            .filter(|(p, _)| *p == h.id)
                            .map(|(_, s)| s.as_slice()),
                    },
                );
                pane_tab_windowing.push((h.id.clone(), strip.first, strip.n_vis, h.active_tab));
                tab_hits.extend(strip.tab_hits.iter().map(|(i, r)| (h.id.clone(), *i, *r)));
                tab_close_hits.extend(strip.close_hits.iter().map(|(i, r)| (h.id.clone(), *i, *r)));
                if let Some(r) = strip.plus_rect {
                    plus_hits.push((h.id.clone(), r));
                }
                if let Some(a) = strip.accent {
                    deferred_accents.push(a);
                }
                // 아래 오른쪽 버튼 무리가 그대로 쓰는 hover 판정 — 띠 안에서 쓰던
                // 것을 여기 남긴다(함수로 옮긴 쪽에도 자기 것이 따로 있다).
                let (cur_x, cur_y) = self.cursor_px;
                let inside = |rx: f32, ry: f32, rw: f32, rh: f32| {
                    cur_x >= rx && cur_x <= rx + rw && cur_y >= ry && cur_y <= ry + rh
                };
                // ── Right action buttons ── per-kind cluster: terminal panes
                // get new-terminal/web/split-v/split-h; image panes get
                // zoom-out / zoom-in / rotate / reset wired to the in-pane
                // image-view state mutated by forward_key as well.
                // Per cluster we carry either an ImageBtn (image pane) or an
                // ActionKind (terminal pane). Keeping both as Option in one
                // tuple keeps the paint loop unified.
                let action_set: Vec<(&str, Option<ImageBtn>, Option<ActionKind>)> = if h.is_image {
                    vec![
                        ("minus", Some(ImageBtn::ZoomOut), None),
                        ("plus", Some(ImageBtn::ZoomIn), None),
                        ("rotate-cw", Some(ImageBtn::Rotate), None),
                        ("maximize", Some(ImageBtn::Reset), None),
                    ]
                } else if h.is_web {
                    // 브라우저 컨트롤 — 뒤로/앞으로/새로고침/기본 브라우저로 열기.
                    // split·상태바 버튼은 웹 pane 에 안 맞아 통째로 갈아 끼운다.
                    // 로딩 중엔 리로드가 ×(정지)가 된다 — web_nav("reload") 가
                    // loading 을 보고 window.stop() 으로 간다(브라우저 관례).
                    let reload_icon = if h.web_loading { "x" } else { "rotate-cw" };
                    vec![
                        ("chevron-left", None, Some(ActionKind::WebBack)),
                        ("chevron-right", None, Some(ActionKind::WebForward)),
                        (reload_icon, None, Some(ActionKind::WebReload)),
                        ("external-link", None, Some(ActionKind::WebOpenExternal)),
                    ]
                } else if h.is_editor {
                    // Document panes use the controls drawn below.
                    vec![]
                } else if tab_strip {
                    vec![("ellipsis-vertical", None, Some(ActionKind::HandleMenu))]
                } else if self.lite {
                    // lite 는 헤더에 버튼을 두지 않는다 — 분할은 단축키·⋮·CLI 로.
                    vec![]
                } else {
                    // The status-bar toggle reads "filled" (panel-bottom) when the
                    // bar is shown and "dashed" when it's collapsed, so the icon
                    // itself signals the current state. Visibility = global
                    // default flipped by the shown/hidden exception sets (mirrors
                    // `statusbar_visible`, inlined here under the gpu borrow).
                    let fvis = self.statusbar.shown.contains(&h.id)
                        || (!self.statusbar.hidden.contains(&h.id) && self.set_footer_default);
                    let sb_icon = if fvis {
                        "panel-bottom"
                    } else {
                        "panel-bottom-dashed"
                    };
                    vec![
                        ("external-link", None, Some(ActionKind::Undock)),
                        (sb_icon, None, Some(ActionKind::ToggleStatusbar)),
                        ("columns-2", None, Some(ActionKind::SplitV)),
                        ("rows-2", None, Some(ActionKind::SplitH)),
                    ]
                };
                let mut bx = h.x + h.w - 8.0 - (abw * n_btn + agap * (n_btn - 1.0));
                for (ic, kind, action) in action_set {
                    let chip_size = icon_size + 6.0;
                    let chip_y = h.y + (PANE_HEADER_HEIGHT - chip_size) / 2.0;
                    let chip_x = bx + (abw - chip_size) / 2.0;
                    let hover = inside(chip_x, chip_y, chip_size, chip_size);
                    if hover {
                        hover_rect(g, chip_x, chip_y, chip_size, chip_size, theme::radius_sm());
                    }
                    let color = if hover { theme::text() } else { act_fg };
                    g.queue_icon(
                        ic,
                        chip_x + (chip_size - icon_size) / 2.0,
                        chip_y + (chip_size - icon_size) / 2.0,
                        icon_size,
                        color,
                    );
                    if let Some(k) = kind {
                        image_btn_hits.push((
                            h.id.clone(),
                            k,
                            (chip_x, chip_y, chip_size, chip_size),
                        ));
                    }
                    if let Some(a) = action {
                        pane_action_hits.push((
                            h.id.clone(),
                            a,
                            (chip_x, chip_y, chip_size, chip_size),
                        ));
                    }
                    bx += abw + agap;
                }
                // ── 웹 pane 주소 pill ── 버튼 클러스터 왼쪽. 평소엔 현재 주소를
                // 흐리게 보여 주고, 클릭(또는 Cmd+L)하면 그 자리에서 인라인 편집
                // — 빈 버퍼 동안은 현재 주소가 자리표시자다(App.web_addr 주석).
                if addr_vis {
                    let ah = icon_size + 6.0;
                    let ay = h.y + (PANE_HEADER_HEIGHT - ah) / 2.0;
                    let ax = h.x + h.w - 8.0 - (abw * n_btn + agap * (n_btn - 1.0)) - 6.0 - addr_w;
                    let afont = 11.0_f32;
                    let tx = ax + 8.0;
                    let ty = ay + (ah - afont) / 2.0;
                    let clip_r = ax + addr_w - 8.0;
                    // 찾기 칸이 주소 pill 자리를 빌린다(서로 배타) — 접두 라벨과
                    // 자리표시자만 다르고 캐럿·클립 처리는 같다.
                    let finding = self
                        .web_find
                        .as_ref()
                        .filter(|e| e.pane == h.id)
                        .map(|e| (e.text.clone(), e.cursor));
                    let editing = if finding.is_some() {
                        None
                    } else {
                        self.web_addr
                            .as_ref()
                            .filter(|e| e.pane == h.id)
                            .map(|e| (e.text.clone(), e.cursor))
                    };
                    if let Some((text, cursor)) = finding {
                        round_rect(g, ax, ay, addr_w, ah, theme::radius_sm(), theme::bg());
                        let px = g.draw_text(
                            tx,
                            ty,
                            "찾기",
                            gpu::DrawOpts {
                                font_size: afont,
                                color: theme::text_mute(),
                                bold: false,
                                italic: false,
                            },
                        ) + 6.0;
                        let (mut head, tail) = crate::lineedit::split(&text, cursor);
                        if self.in_preedit {
                            head.push_str(&self.preedit);
                        }
                        let caret_x = px + g.measure_chrome_text(&head, afont, false);
                        let shown = format!("{head}{tail}");
                        g.draw_text_clipped(
                            px,
                            ty,
                            &shown,
                            gpu::DrawOpts {
                                font_size: afont,
                                color: theme::text(),
                                bold: false,
                                italic: false,
                            },
                            px,
                            clip_r,
                        );
                        if commit_caret_on {
                            g.rect(
                                caret_x.min(clip_r),
                                ay + (ah - afont - 2.0) / 2.0,
                                1.5,
                                afont + 2.0,
                                theme::text(),
                            );
                        }
                    } else if let Some((text, cursor)) = editing {
                        // 편집 중: bg 로 가라앉혀 입력칸임을 보이고 캐럿을 세운다.
                        round_rect(g, ax, ay, addr_w, ah, theme::radius_sm(), theme::bg());
                        let (mut head, tail) = crate::lineedit::split(&text, cursor);
                        if self.in_preedit {
                            head.push_str(&self.preedit);
                        }
                        let caret_x = tx + g.measure_chrome_text(&head, afont, false);
                        let shown = format!("{head}{tail}");
                        if shown.is_empty() {
                            g.draw_text_clipped(
                                tx,
                                ty,
                                h.web_url.as_deref().unwrap_or(""),
                                gpu::DrawOpts {
                                    font_size: afont,
                                    color: theme::text_mute(),
                                    bold: false,
                                    italic: false,
                                },
                                tx,
                                clip_r,
                            );
                        } else {
                            g.draw_text_clipped(
                                tx,
                                ty,
                                &shown,
                                gpu::DrawOpts {
                                    font_size: afont,
                                    color: theme::text(),
                                    bold: false,
                                    italic: false,
                                },
                                tx,
                                clip_r,
                            );
                        }
                        if commit_caret_on {
                            g.rect(
                                caret_x.min(clip_r),
                                ay + (ah - afont - 2.0) / 2.0,
                                1.5,
                                afont + 2.0,
                                theme::text(),
                            );
                        }
                    } else {
                        let hover = inside(ax, ay, addr_w, ah);
                        g.hover_pointer |= hover;
                        round_rect(g, ax, ay, addr_w, ah, theme::radius_sm(), theme::surface());
                        if hover {
                            hover_rect(g, ax, ay, addr_w, ah, theme::radius_sm());
                        }
                        g.draw_text_clipped(
                            tx,
                            ty,
                            h.web_url.as_deref().unwrap_or(""),
                            gpu::DrawOpts {
                                font_size: afont,
                                color: if hover {
                                    theme::text()
                                } else {
                                    theme::text_dim()
                                },
                                bold: false,
                                italic: false,
                            },
                            tx,
                            clip_r,
                        );
                    }
                    pane_action_hits.push((
                        h.id.clone(),
                        ActionKind::WebAddress,
                        (ax, ay, addr_w, ah),
                    ));
                }
                // ── Markdown document controls ── View and Edit never imply a
                // save; Save is a separate action and the dirty state stays
                // visible until a write succeeds. Narrow panes retain every
                // action, shortening only the separate-window label to its icon.
                if h.is_editor {
                    let seg_h = icon_size + 6.0;
                    let seg_y = h.y + (PANE_HEADER_HEIGHT - seg_h) / 2.0;
                    let mut sx = h.x + h.w - 8.0 - md_tools_w;
                    if !md_compact {
                        let state = if h.md_modified { "저장 안 됨" } else { "저장됨" };
                        g.draw_text(
                            sx + 4.0,
                            seg_y + (seg_h - seg_font) / 2.0,
                            state,
                            gpu::DrawOpts {
                                font_size: seg_font,
                                color: if h.md_modified {
                                    theme::attention()
                                } else {
                                    theme::text_mute()
                                },
                                bold: false,
                                italic: false,
                            },
                        );
                        sx += md_state_w + 4.0;
                    }
                    let save_hover = inside(sx, seg_y, md_save_w, seg_h);
                    g.hover_pointer |= save_hover;
                    round_rect(
                        g,
                        sx,
                        seg_y,
                        md_save_w,
                        seg_h,
                        theme::radius_sm(),
                        if save_hover { theme::surface_hover() } else { theme::surface() },
                    );
                    g.draw_text(
                        sx + seg_pad,
                        seg_y + (seg_h - seg_font) / 2.0,
                        "저장",
                        gpu::DrawOpts {
                            font_size: seg_font,
                            color: if h.md_modified || save_hover {
                                theme::text()
                            } else {
                                theme::text_dim()
                            },
                            bold: h.md_modified,
                            italic: false,
                        },
                    );
                    pane_action_hits.push((h.id.clone(), ActionKind::MdSave, (sx, seg_y, md_save_w, seg_h)));
                    sx += md_save_w;
                    if h.is_markdown {
                        sx += 4.0;
                        round_rect(
                            g,
                            sx,
                            seg_y,
                            seg_w,
                            seg_h,
                            theme::radius_sm(),
                            theme::surface(),
                        );
                        let ty = seg_y + (seg_h - seg_font) / 2.0;
                        for (label, lw, raw) in
                            [("보기", md_rendered_w, false), ("편집", md_raw_w, true)]
                        {
                            let cell_w = lw + seg_pad * 2.0;
                            let active = h.md_raw_mode == raw;
                            let hover = inside(sx, seg_y, cell_w, seg_h);
                            g.hover_pointer |= hover;
                            if active {
                                round_rect(
                                    g,
                                    sx,
                                    seg_y,
                                    cell_w,
                                    seg_h,
                                    theme::radius_sm(),
                                    theme::surface_hover(),
                                );
                            } else if hover {
                                hover_rect(g, sx, seg_y, cell_w, seg_h, theme::radius_sm());
                            }
                            let color = if active {
                                theme::text()
                            } else {
                                theme::text_dim()
                            };
                            g.draw_text(
                                sx + seg_pad,
                                ty,
                                label,
                                gpu::DrawOpts {
                                    font_size: seg_font,
                                    color,
                                    bold: false,
                                    italic: false,
                                },
                            );
                            let act = if raw {
                                ActionKind::MdRaw
                            } else {
                                ActionKind::MdRender
                            };
                            pane_action_hits.push((h.id.clone(), act, (sx, seg_y, cell_w, seg_h)));
                            sx += cell_w;
                        }
                    }
                    sx += 4.0;
                    let pop_hover = inside(sx, seg_y, md_popout_w, seg_h);
                    g.hover_pointer |= pop_hover;
                    if pop_hover {
                        hover_rect(g, sx, seg_y, md_popout_w, seg_h, theme::radius_sm());
                    }
                    let pop_color = if pop_hover { theme::text() } else { theme::text_dim() };
                    g.queue_icon(
                        "external-link",
                        sx + seg_pad.min(6.0),
                        seg_y + (seg_h - icon_size) / 2.0,
                        icon_size,
                        pop_color,
                    );
                    if !md_compact {
                        g.draw_text(
                            sx + seg_pad.min(6.0) + icon_size + 5.0,
                            seg_y + (seg_h - seg_font) / 2.0,
                            "새 창",
                            gpu::DrawOpts {
                                font_size: seg_font,
                                color: pop_color,
                                bold: false,
                                italic: false,
                            },
                        );
                    }
                    pane_action_hits.push((
                        h.id.clone(),
                        ActionKind::MdPopout,
                        (sx, seg_y, md_popout_w, seg_h),
                    ));
                }
            }
            // Focus by contrast: unfocused panes fade their text only (via
            // PaneSlot.dim in draw_cells), not the whole box — no dark veil.
            // Ghostty-style: one hairline per interior split boundary, drawn
            // after the veil so the seam stays crisp on top. No per-pane box
            // border (that doubled into a thick seam between abutting panes
            // and read as caged tiles).
            for (sx, sy, sw, sh) in &pane_seams {
                g.rect(*sx, *sy, *sw, *sh, theme::border());
            }
            // Re-paint the active-tab accent strips so a horizontal pane
            // divider just above a pane doesn't wipe its accent color.
            for (ax, ay, aw, ac) in &deferred_accents {
                g.rect(*ax, *ay, *aw, ACTIVE_ACCENT_STROKE, *ac);
            }
            // ── ghostty식 pane 핸들(⋮) + active 보더 ───────────────────
            // 헤더 띠를 없앤 대신: ① active pane은 얇은 accent 보더로 강조
            // (비활성 dim과 함께 focus 단서) ② pane에 마우스를 올리면 우상단에
            // ⋮ 핸들이 떠서 클릭=메뉴(Phase 3)·드래그=이동(Phase 4) 진입점이 됨.
            // 설정 화면이 떠 있으면 pane 핸들·보더를 그리지 않는다 — 불투명 설정
            // backdrop 위로 ⋮ 가 비쳐 보이던 잔상(사용자). hit-rect 도 비워 설정 영역
            // 클릭이 유령 핸들에 안 걸리게 한다.
            // active_pane + is_split + 헤더 보유 pane 집합을 한 번에 스냅샷 —
            // 루프 안에서 self를 재borrow하면 g(=&mut self.gpu)와 충돌하므로 미리
            // 모은다. statusbar 루프(아래)도 active 보더 inset 계산에 active_pane/
            // is_split을 쓰므로 settings 분기 밖, 더 넓은 스코프에 둔다.
            let is_split = footer_slots.len() > 1;
            // 테두리를 실제로 그린 pane 과 그 두께. 하단바(footer)는 나중에 그려지므로
            // 이 값만큼 안쪽으로 물러나야 테두리를 안 덮는다. 두 곳이 각자 조건을
            // 계산하면 반드시 어긋난다 — 줌 pane 은 테두리가 있는데 하단바는 그걸
            // 모르고 덮어 아래쪽만 끊겨 보였다(사용자). 그린 쪽이 기록하고 덮는 쪽이 읽는다.
            let mut border_inset: HashMap<String, f32> = HashMap::new();
            // 줌 pane 은 claude 여부·split 여부와 무관하게 테두리를 두른다 — 줌의
            // 유일한 시각 단서라서(하단 dock 칩 하나로는 안 읽힌다). g(=&mut
            // self.gpu) 를 잡기 전에 스냅샷.
            let zoomed_now = self.zoomed_pane.clone();
            // 헤더를 실제로 그린 pane 집합 — compact 바가 헤더에 뜨므로 footer 쪽 바는
            // 이 pane 들을 건너뛴다. `ws.panes.has_header()` 가 아니라 방금 그린 `headers`
            // (pty_layout 기반)에서 뽑아야 ws.panes↔pty_layout 데싱크로 한 pane 에 헤더(위)·
            // footer(아래) 스윕바가 동시에 뜨는 "로딩바 두개" 버그가 안 난다(사용자).
            let headered: std::collections::HashSet<String> =
                headers.iter().map(|h| h.id.clone()).collect();
            {
                let (hmx, hmy) = self.cursor_px;
                let accent = theme::accent_color(theme::accent_name());
                let anim_phase = anim_phase_secs();
                let mut handle_rects: Vec<(String, (f32, f32, f32, f32))> = Vec::new();
                let mut zones: Vec<(String, (f32, f32, f32, f32))> = Vec::new();
                let mut menu_hits: Vec<(ActionKind, (f32, f32, f32, f32))> = Vec::new();
                const HANDLE: f32 = 22.0;
                const HMARGIN: f32 = 5.0;
                for (fid, fx, fy, fw, fbox_h) in &footer_slots {
                    // pane 테두리 — 포커스된(active) claude pane 만 자기 학생 고정색
                    // 테두리(지금 어느 pane 을 보고 있는지 한눈에). 비활성·순수 셸은
                    // 무테두리 — 여러 pane 이 동시에 테두리를 둘러 지저분하던 걸 정리(사용자).
                    let zoom_focus = zoomed_now.as_deref() == Some(fid.as_str());
                    let focused = active_pane.as_deref() == Some(fid.as_str());
                    // 학생색은 claude 가 도는 pane 에만 — 순수 셸에 남의 학생색이 둘러지면
                    // 「저 pane 에 누가 있다」로 잘못 읽힌다. 목록 보기는 앱 강조색을 따른다.
                    let edge_col = if agents_view_panes.contains(fid.as_str()) {
                        Some(theme::accent())
                    } else {
                        pane_chars
                            .get(fid.as_str())
                            .filter(|_| claude_panes.contains(fid.as_str()))
                            .and_then(|n| {
                                theme::character_accent_n(n, theme::character_ordinal(&pane_chars, fid))
                            })
                    };
                    // 멈춘 테두리는 「지금 보는 칸」(분할 초점·줌)만 말한다. 줌은 학생이 없는 순수
                    // 셸에서도 테두리가 있어야 한다 — 없으면 줌 자체가 안 보인다.
                    let mut still_t = 0.0_f32;
                    if zoom_focus || (is_split && focused && claude_panes.contains(fid.as_str())) {
                        let border_col = edge_col.or_else(|| zoom_focus.then_some(accent));
                        if let Some(col) = border_col {
                            // 줌은 조금 두껍게 — 여백 위에 홀로 뜬 카드의 윤곽선이다.
                            let t = if zoom_focus { 2.0_f32 } else { 1.5_f32 };
                            g.rect(*fx, *fy, *fw, t, col);
                            g.rect(*fx, fy + fbox_h - t, *fw, t, col);
                            g.rect(*fx, *fy, t, *fbox_h, col);
                            g.rect(fx + fw - t, *fy, t, *fbox_h, col);
                            border_inset.insert(fid.clone(), t);
                            still_t = t;
                        }
                    }
                    // 끝을 모르는 일은 테두리가 움직인다 — 일하는 중은 빛 조각이 한 바퀴씩 돌고, 뒤에서
                    // 도는 중은 점선이 흐른다. 숨쉬던 테두리는 가장 진할 때 초점 테두리와 같아 고른
                    // 칸이 어디인지 흐려졌다(2026-10-07 「숨쉬는 거 헷갈리는데 선택이랑. 한 바퀴 도는
                    // 거로 하자 아웃라인을」). 멈춘 테두리가 있으면 그 안쪽을 돈다 — 같은 색 실선 위에
                    // 겹치면 꼬리가 묻힌다. 셰이더가 `u.time` 으로 움직이므로 CPU 는 위상을 안 센다.
                    // 손을 기다리는 칸(아래 깜빡임)과는 상태가 배타적이라 같이 서지 않는다.
                    let activity = self.pane_activity.get(fid).and_then(|a| {
                        if a.state.is_busy() {
                            Some(theme::Activity::Working)
                        } else if a.bg_active {
                            Some(theme::Activity::Background)
                        } else {
                            None
                        }
                    });
                    if let Some(kind) = activity {
                        let look = theme::activity_edge(kind, focused || zoom_focus, false);
                        let rect = (fx + still_t, fy + still_t, fw - 2.0 * still_t, fbox_h - 2.0 * still_t);
                        let col = edge_col.unwrap_or(accent);
                        g.activity_outline(rect, 0.0, theme::beside_still_edge(col, still_t > 0.0), look);
                        border_inset.insert(fid.clone(), still_t + look.reach());
                    }
                    // compact 중이면 box 상단에 왼쪽부터 채워지는 바 — 끝이 있는 일이라 도는 빛이
                    // 아니라 차오르는 모양으로 말한다(헤더 pane 과 같은 형태 언어).
                    if !headered.contains(fid.as_str()) {
                        const BAR_H: f32 = 2.5;
                        let (st, pct) = self
                            .pane_activity
                            .get(fid)
                            .map_or((crate::agent_state::AgentState::Unknown, None), |a| (a.state.clone(), a.compact_pct));
                        if matches!(st, crate::agent_state::AgentState::Compacting) {
                            if let Some(p) = pct {
                                // 화면의 `▰▰▱ N%` 그대로 — 진짜 진행률(2026-08-13
                                // 지시)을 칸 게이지 + 숫자로(2026-08-15 지시, 헤더
                                // pane 과 같은 형태 언어). 숫자는 게이지 아래
                                // 오른쪽 끝 — compact 중에만 잠깐 얹힌다.
                                let cf = 10.0;
                                let label = format!("{p}%");
                                let lw = g.measure_chrome_text(&label, cf, true);
                                draw_compact_cells(g, *fx, *fy, *fw, BAR_H, accent, p);
                                g.draw_text(
                                    fx + fw - lw - 4.0,
                                    fy + BAR_H + 2.0,
                                    &label,
                                    gpu::DrawOpts {
                                        font_size: cf,
                                        color: accent,
                                        bold: true,
                                        italic: false,
                                    },
                                );
                            } else {
                                g.rect(*fx, *fy, *fw, BAR_H, theme::with_alpha(accent, 0x2e));
                                g.compact_bar(*fx, *fy, *fw, BAR_H, accent);
                            }
                        }
                    }
                    // 손을 기다리는 pane — 네 변이 핑크로 깜빡인다(사용자: "내가
                    // 엔터해야되거나 그런거는 핑크색으로 깜빡이게"). 일하는 중의 도는
                    // 테두리와 **뜻이 정반대**라 색과 움직임으로 갈랐다: 학생색으로 도는 빛은 "놔둬도
                    // 진행된다", 핑크 깜빡임은 "내가 손대야 풀린다". 상태가 배타적이라
                    // (working ≠ waiting) 둘이 한 pane 에 같이 뜨지 않는다.
                    //
                    // 포커스 테두리(학생색) 위에 덧그린다 — 지금 보고 있는 pane 이
                    // 물어보고 멈춘 경우, 급한 쪽이 이겨야 한다. border_inset 도
                    // 다시 적어 하단바가 이 테두리를 덮지 않게 한다.
                    // 메서드(`pane_needs_you`)를 쓰면 `&self` 를 통째로 빌려 렌더 루프의
                    // 가변 대여와 부딪힌다 — 필드만 집어 자유함수로 판정한다.
                    if self
                        .pane_activity
                        .get(fid)
                        .is_some_and(|a| a.state.needs_you())
                    {
                        let mut col = theme::attention();
                        // 사람이 이 방을 보러 왔으면 테두리는 그대로 두고 깜빡임만 멈춘다.
                        col[3] = if self.blink_quiet.contains(fid) {
                            0xd0
                        } else {
                            (90.0 + 165.0 * breathe(anim_phase, 1.1)) as u8
                        };
                        let t = 2.0_f32;
                        g.rect(*fx, *fy, *fw, t, col);
                        g.rect(*fx, fy + fbox_h - t, *fw, t, col);
                        g.rect(*fx, *fy, t, *fbox_h, col);
                        g.rect(fx + fw - t, *fy, t, *fbox_h, col);
                        let inset = border_inset.entry(fid.clone()).or_insert(t);
                        *inset = inset.max(t);
                    }
                    // 헤더 있는 pane(image/md/탭 2개+)은 헤더에 컨트롤이 다 있으니
                    // ··· 핸들은 생략한다 — 중복 진입점 제거. 단 메뉴 자체는 그린다:
                    // 헤더 우클릭이 이 메뉴를 열어 상단바를 다시 접는 유일한 입구다
                    // (2026-08-13 지적: 점3개로 만든 헤더를 되돌릴 길이 없었다).
                    let is_headered = headered.contains(fid.as_str());
                    let hx = fx + (fw - HANDLE) / 2.0;
                    let hy = fy + HMARGIN;
                    if !is_headered {
                        // ⋮ 핸들 — 상단 중앙. 평소엔 완전히 숨김. pane 상단 30% 띠에
                        // 커서가 들어오면 흐릿하게 등장하고, ⋮ 바로 위로 가면 진해진다
                        // (그때 손모양 커서 — handler 측). 클릭=메뉴·드래그=이동.
                        let on_handle =
                            hmx >= hx && hmx <= hx + HANDLE && hmy >= hy && hmy <= hy + HANDLE;
                        let zone_h = fbox_h * 0.30;
                        let in_zone =
                            hmx >= *fx && hmx <= fx + fw && hmy >= *fy && hmy <= fy + zone_h;
                        // 글자 위에서도 보이게 **칩 위에 점 셋**을 chrome 사각형으로 그린다.
                        // SVG 아이콘은 텍스트 글리프 밑으로 깔려(2026-09-14 실측: 학생 pane
                        // 처럼 첫 줄에 글자가 차면 점이 글자 뒤로 숨어 「⋮ 이 안 뜬다」가
                        // 됐다) 흐린 색으로는 아예 안 보였다. 칩은 hover 띠 안에서만.
                        if on_handle || in_zone {
                            round_rect(g, hx, hy, HANDLE, HANDLE, 6.0, theme::surface_active());
                            g.round_rect_stroke(hx, hy, HANDLE, HANDLE, 6.0, 1.0, theme::border());
                            let dot = 2.5_f32;
                            let cy = hy + (HANDLE - dot) / 2.0;
                            let col = if on_handle { theme::text() } else { theme::text_dim() };
                            for i in 0..3 {
                                let cx = hx + HANDLE / 2.0 - dot / 2.0 + (i as f32 - 1.0) * 5.0;
                                round_rect(g, cx, cy, dot, dot, dot / 2.0, col);
                            }
                        }
                        handle_rects.push((fid.clone(), (hx, hy, HANDLE, HANDLE)));
                        zones.push((fid.clone(), (*fx, *fy, *fw, zone_h)));
                    }
                    // ⋮ 메뉴 열림 → 이 pane ⋮ 아래 버튼3개(좌우분할·상하분할·닫기).
                    if self.handle_menu.as_deref() == Some(fid.as_str()) {
                        // 상태바(footer) 토글 아이콘은 현재 표시 상태를 그대로
                        // 드러낸다 — 보이면 panel-bottom, 접혀 있으면 dashed.
                        let fvis = self.statusbar.shown.contains(fid.as_str())
                            || (!self.statusbar.hidden.contains(fid.as_str())
                                && self.set_footer_default);
                        let sb_icon = if fvis {
                            "panel-bottom"
                        } else {
                            "panel-bottom-dashed"
                        };
                        // 상단바(헤더 띠)도 같은 방식 — 지금 보이는 상태를 아이콘이
                        // 그대로 드러낸다. hdr_vis 는 has_header() 와 같은 답이어야
                        // 하므로 pane 에 직접 물어본다(override 포함).
                        // 별도창은 터미널 탭에만 — 그림·문서·웹 탭은 창이 못 그린다.
                        let (hdr_vis, term_tab) = {
                            let ws = self.ws.lock().unwrap();
                            let p = ws.panes.get(fid.as_str());
                            (
                                p.is_some_and(|p| p.has_header()),
                                p.is_some_and(|p| {
                                    p.tabs.get(p.active_tab).is_some_and(|t| t.term().is_some())
                                }),
                            )
                        };
                        // 보기 전환 — 학생 pane 은 대화, 셸 pane 은 명령 묶음.
                        let view = handle_chat
                            .as_ref()
                            .filter(|(pid, _)| pid == fid)
                            .and_then(|(_, view)| *view);
                        let can_chat = view.is_some();
                        let view = view.unwrap_or((false, false));
                        let chat_icon = match view {
                            (_, true) => "terminal",
                            (true, false) => "list",
                            (false, false) => "message-circle",
                        };
                        let hdr_icon = if hdr_vis {
                            "panel-top"
                        } else {
                            "panel-top-dashed"
                        };
                        let items = [
                            (chat_icon, ActionKind::ChatView),
                            ("plus", ActionKind::NewTab),
                            // columns-2(세로선=좌우 2칸) → Horizontal(right),
                            // rows-2(가로선=상하 2칸) → Vertical(bottom). 아이콘이
                            // 곧 결과 배치다 — SplitDir 이름과는 반대 매핑.
                            ("columns-2", ActionKind::SplitH),
                            ("rows-2", ActionKind::SplitV),
                            (hdr_icon, ActionKind::ToggleHeader),
                            (sb_icon, ActionKind::ToggleStatusbar),
                            ("maximize", ActionKind::ToggleZoom),
                            ("rotate-cw", ActionKind::RefreshRenderer),
                            ("external-link", ActionKind::Undock),
                            ("x", ActionKind::Close),
                        ];
                        // lite 의 ⋮ 는 새 탭·쪼개기·최대화·닫기뿐 — 헤더·하단바 토글은
                        // 걷어낸 크롬이고, 별도창·렌더러 새로고침은 본판의 일이다.
                        let lite = self.lite;
                        let items: Vec<(&str, ActionKind)> = items
                            .into_iter()
                            .filter(|(_, a)| term_tab || *a != ActionKind::Undock)
                            .filter(|(_, a)| (can_chat && !lite) || *a != ActionKind::ChatView)
                            .filter(|(_, a)| {
                                !lite
                                    || !matches!(
                                        a,
                                        ActionKind::ToggleHeader
                                            | ActionKind::ToggleStatusbar
                                            | ActionKind::RefreshRenderer
                                            | ActionKind::Undock
                                    )
                            })
                            .collect();
                        let bw = 30.0_f32;
                        let bh = 28.0_f32;
                        let gap = 2.0_f32;
                        let pad = 4.0_f32;
                        let n = items.len() as f32;
                        let mw = pad * 2.0 + bw * n + gap * (n - 1.0);
                        let mh = bh + pad * 2.0;
                        let mut mx = hx + HANDLE / 2.0 - mw / 2.0;
                        // pane 가장자리 안으로 클램프(좌측/우측 끝 pane). 단 메뉴가
                        // pane 보다 넓으면(좁은 3분할 + 아이콘 8개) 좌우 경계가 서로를
                        // 뒤집어 메뉴를 창 밖으로 밀어낸다 — 그땐 창 기준으로 물러선다.
                        // 메뉴는 pane 위에 뜨는 오버레이라 옆 pane 을 덮는 건 무방하다.
                        let (lo, hi) = if mw + 4.0 <= *fw {
                            (*fx + 2.0, *fx + *fw - mw - 2.0)
                        } else {
                            (2.0, (win_px.0 / scale - mw - 2.0).max(2.0))
                        };
                        mx = mx.clamp(lo, hi);
                        // 앵커: ⋮ 핸들 아래 / 헤더 pane 은 헤더 띠 바로 아래(핸들이
                        // 없고, 띠 위에 겹치면 우클릭한 자리가 가려진다).
                        let my = if is_headered {
                            fy + PANE_HEADER_HEIGHT + 3.0
                        } else {
                            hy + HANDLE + 3.0
                        };
                        round_rect(g, mx, my, mw, mh, theme::radius_sm(), theme::border());
                        round_rect(
                            g,
                            mx + 1.0,
                            my + 1.0,
                            mw - 2.0,
                            mh - 2.0,
                            theme::radius_sm() - 1.0,
                            theme::surface_hover(),
                        );
                        let mut bx2 = mx + pad;
                        let by2 = my + pad;
                        let mut tip_at: Option<(&str, f32, f32)> = None;
                        for (icon, act) in items {
                            let on = hmx >= bx2 && hmx <= bx2 + bw && hmy >= by2 && hmy <= by2 + bh;
                            if on {
                                round_rect(
                                    g,
                                    bx2,
                                    by2,
                                    bw,
                                    bh,
                                    theme::radius_sm(),
                                    theme::surface_active(),
                                );
                            }
                            let bisz = 16.0_f32;
                            g.queue_icon(
                                icon,
                                bx2 + (bw - bisz) / 2.0,
                                by2 + (bh - bisz) / 2.0,
                                bisz,
                                if on { theme::text() } else { theme::text_dim() },
                            );
                            // 아이콘만으로는 뜻이 갈리는 칸이 있다 — 올리면 이름을 띄운다.
                            if on {
                                let tip = handle_menu_tip(act, view);
                                if !tip.is_empty() {
                                    tip_at = Some((tip, bx2, by2 + bh + 6.0));
                                }
                            }
                            menu_hits.push((act, (bx2, by2, bw, bh)));
                            bx2 += bw + gap;
                        }
                        if let Some((tip, tx, ty)) = tip_at.take() {
                            Self::draw_hover_tip(g, tip, tx, ty, win_px.0 / scale, win_px.1 / scale);
                        }
                    }
                }
                self.pane_handle_rects = handle_rects;
                self.pane_top_zones = zones;
                self.handle_menu_hits = menu_hits;
            }
            // Per-pane status bar at the foot of each pane box: cwd + branch
            // chips (click → cd / checkout dropdowns) on the left, ± diff on
            // the right. The gpu borrow rules out &self method calls in here,
            // so visibility / cwd / badge all read the fields directly.
            self.statusbar.path_rects.clear();
            self.statusbar.branch_rects.clear();
            self.statusbar.toggle_rects.clear();
            self.statusbar.diff_rects.clear();
            let (sb_mx, sb_my) = self.cursor_px;
            for (fid, fx, fy, fw, fbox_h) in &footer_slots {
                // `statusbar_visible` 과 같은 판정 — lite 는 pane 하단바도 없다.
                let fvis = !self.lite
                    && (self.statusbar.shown.contains(fid)
                        || (!self.statusbar.hidden.contains(fid) && self.set_footer_default));
                if !fvis || *fbox_h < pane_footer_h + 4.0 {
                    continue;
                }
                let bar_y = fy + fbox_h - pane_footer_h;
                // 테두리를 footer 배경이 덮지 않게 좌우·하단을 그 두께만큼 안쪽으로
                // 그린다 — 안 그러면 나중에 그려지는 footer bg 가 보더의 하단·좌우 끝을
                // 덮어 "선이 하단바를 제외하고 감싸는" 것처럼 보인다(사용자). 두께는
                // 실제로 그린 쪽이 남긴 값을 쓴다(줌은 2.0, 분할 active 는 1.5).
                let bt = border_inset.get(fid.as_str()).copied().unwrap_or(0.0);
                g.rect(
                    fx + bt,
                    bar_y,
                    fw - 2.0 * bt,
                    pane_footer_h - bt,
                    theme::bg(),
                );
                g.rect(fx + bt, bar_y, fw - 2.0 * bt, 1.0, theme::border());
                // Pill metrics shared by every chip. 12/13 은 앱을 통틀어 가장 작은
                // 글자·아이콘이었다 — 같은 화면의 사이드바(13~14)와 나란히 놓이니
                // 하단바만 축소된 것처럼 읽혔다(사용자). 본문 단과 같은 단으로 올린다.
                let pill_h = 22.0_f32;
                let pill_y = bar_y + (pane_footer_h - pill_h) / 2.0;
                let icon_sz = 14.0_f32;
                let pad_x = 9.0_f32;
                let icon_gap = 6.0_f32;
                let chip_gap = 7.0_f32;
                let font = 13.0_f32;
                let txt_y = pill_y + (pill_h - font) / 2.0;
                let footer_hover = sb_my >= bar_y
                    && sb_my <= bar_y + pane_footer_h
                    && sb_mx >= *fx
                    && sb_mx <= fx + fw;
                let mut cx = fx + 8.0;
                let cwd = self.pane_cwd_cache.get(fid).cloned();
                // Home-relative cwd (~/…), matching the screenshot's breadcrumb.
                let disp = cwd
                    .as_ref()
                    .map(|p| nfc_hangul(&crate::session::tilde_home(&p.to_string_lossy())))
                    .unwrap_or_else(|| "—".to_string());
                let badge = cwd
                    .as_ref()
                    .and_then(|p| self.window_git.lock().ok().and_then(|m| m.get(p).cloned()));
                // ── 폭 예산 ───────────────────────────────────────────────
                // 칩 셋은 지금까지 pane 폭을 아무도 안 재고 왼쪽부터 이어 그렸다.
                // 좁은 pane 에서는 그대로 옆 칼럼 위로 뻗는다(640px 실측: 282px
                // 자리에 398px 어치). 그리기 전에 예산을 잡고 모자라면 줄인다.
                //
                // 줄이는 차례는 **cwd 가 먼저**다. 그 경로는 파일트리 루트와 타이틀
                // 칩에도 있는 중복이고, 브랜치와 변경 수는 이 띠에만 있다. 경로를
                // 꼬리만 남겨도 모자라면 그때 변경 수를 파일 수 하나로 줄이고,
                // 그 다음에야 칩을 접는다.
                let pill_w = |tw: f32| pad_x + icon_sz + icon_gap + tw + pad_x;
                // hover 때 오른쪽 끝에 나오는 접기 손잡이 자리는 늘 비워 둔다 —
                // 그때만 빼면 손잡이가 뜨는 순간 칩이 흔들린다.
                let avail = (fw - 16.0 - 21.0).max(0.0);
                // ── 기계 칩 ── 이 pane 의 몸이 어느 기계에 있는지. 맨 앞에 두고
                // 좁아져도 **마지막까지 남긴다**: 헤더 띠는 pane 이 하나뿐인 창에서
                // 아예 없고 좁으면 배지를 생략하므로, 그런 화면에서 「이 pane 이 어느
                // 기계인가」에 답하는 자리가 여기밖에 없다(2026-09-02 감사 ⑤).
                // 나머지 칩이 말하는 것(경로·브랜치·변경 수)은 파일트리와 Git 탭에도
                // 있는 중복이지만, 기계는 이 띠가 접히면 화면에서 통째로 사라진다.
                let machine = pane_identities.get(fid)
                    .map(|identity| {
                        // 칩 하나만 남는 폭까지 좁아져도 이름 앞머리는 읽히게 자른다.
                        let room = (avail - pad_x * 2.0 - icon_sz - icon_gap).max(0.0);
                        (crate::info::fit_text(g, &identity.machine.name, room, font, false), &identity.machine)
                    })
                    .filter(|(l, _)| !l.is_empty());
                let machine_w = machine
                    .as_ref()
                    .map(|(l, _)| pill_w(g.measure_chrome_text(l, font, false)))
                    .unwrap_or(0.0);
                // 기계 몫을 뗀 나머지로 아래 칩들이 다툰다 — 여기서 먼저 빼 두면
                // 접는 차례(변경 수 → 브랜치 → 경로)가 그대로 기계 칩을 비껴간다.
                let avail = (avail
                    - if machine_w > 0.0 {
                        machine_w + chip_gap
                    } else {
                        0.0
                    })
                .max(0.0);
                let branch_w = badge
                    .as_ref()
                    .map(|b| pill_w(g.measure_chrome_text(&b.branch, font, false)))
                    .unwrap_or(0.0);
                let diff_parts = badge
                    .as_ref()
                    .filter(|b| b.files > 0 || b.insertions > 0 || b.deletions > 0)
                    .map(|b| {
                        let files_s = b.files.to_string();
                        let plus_s = format!("+{}", b.insertions);
                        let minus_s = format!("−{}", b.deletions);
                        let full = g.measure_chrome_text(&files_s, font, false)
                            + g.measure_chrome_text(" · ", font, false)
                            + g.measure_chrome_text(&plus_s, font, false)
                            + g.measure_chrome_text(" ", font, false)
                            + g.measure_chrome_text(&minus_s, font, false);
                        let short = g.measure_chrome_text(&files_s, font, false);
                        (files_s, plus_s, minus_s, pill_w(full), pill_w(short))
                    });
                // 아이콘과 여백만 남은 경로 칩은 아무 말도 못 한다 — 글자 한 자는
                // 들어갈 폭을 바닥으로 잡고, 거기 못 미치면 뒤 칩을 줄인다.
                let min_cwd = pill_w(g.measure_chrome_text("…w", font, false));
                let mut show_branch = branch_w > 0.0;
                let mut diff_mode = if diff_parts.is_some() { 2u8 } else { 0 };
                let tail_w = |show_branch: bool, diff_mode: u8| {
                    (if show_branch {
                        branch_w + chip_gap
                    } else {
                        0.0
                    }) + match (diff_mode, &diff_parts) {
                        (2, Some((.., full, _))) => full + chip_gap,
                        (1, Some((.., short))) => short + chip_gap,
                        _ => 0.0,
                    }
                };
                while avail - tail_w(show_branch, diff_mode) < min_cwd {
                    if diff_mode > 0 {
                        diff_mode -= 1;
                    } else if show_branch {
                        show_branch = false;
                    } else {
                        break;
                    }
                }
                let cwd_room = (avail - tail_w(show_branch, diff_mode)).max(0.0);
                let disp = crate::info::fit_text_tail(
                    g,
                    &disp,
                    (cwd_room - pad_x - icon_sz - icon_gap - pad_x).max(0.0),
                    font,
                    false,
                );
                if let Some((label, machine)) = &machine {
                    let pw = machine_w;
                    let bg = machine.background(theme::bg());
                    let fg = machine.foreground(bg);
                    round_rect(g, cx, pill_y, pw, pill_h, theme::radius_sm(), bg);
                    g.queue_icon(
                        machine.icon(), cx + pad_x, pill_y + (pill_h - icon_sz) / 2.0,
                        icon_sz, fg,
                    );
                    g.draw_text(
                        cx + pad_x + icon_sz + icon_gap, txt_y, label,
                        gpu::DrawOpts { font_size: font, color: fg, bold: false, italic: false },
                    );
                    cx += pw + chip_gap;
                }
                // cwd pill — folder icon + path.
                if !disp.is_empty() {
                    let tw = g.measure_chrome_text(&disp, font, false);
                    let pw = pill_w(tw);
                    let hov = sb_mx >= cx
                        && sb_mx <= cx + pw
                        && sb_my >= pill_y
                        && sb_my <= pill_y + pill_h;
                    round_rect(
                        g,
                        cx,
                        pill_y,
                        pw,
                        pill_h,
                        theme::radius_sm(),
                        theme::border(),
                    );
                    round_rect(
                        g,
                        cx + 1.0,
                        pill_y + 1.0,
                        pw - 2.0,
                        pill_h - 2.0,
                        theme::radius_sm() - 1.0,
                        if hov {
                            theme::surface_active()
                        } else {
                            theme::surface_hover()
                        },
                    );
                    g.queue_icon(
                        "folder",
                        cx + pad_x,
                        pill_y + (pill_h - icon_sz) / 2.0,
                        icon_sz,
                        theme::text_dim(),
                    );
                    g.draw_text(
                        cx + pad_x + icon_sz + icon_gap,
                        txt_y,
                        &disp,
                        gpu::DrawOpts {
                            font_size: font,
                            color: theme::text(),
                            bold: false,
                            italic: false,
                        },
                    );
                    self.statusbar
                        .path_rects
                        .push((fid.clone(), (cx, pill_y, pw, pill_h)));
                    cx += pw + chip_gap;
                }
                if let Some(badge) = badge {
                    // branch pill — git-branch icon + branch name.
                    if show_branch {
                        let pw = branch_w;
                        let hov = sb_mx >= cx
                            && sb_mx <= cx + pw
                            && sb_my >= pill_y
                            && sb_my <= pill_y + pill_h;
                        round_rect(
                            g,
                            cx,
                            pill_y,
                            pw,
                            pill_h,
                            theme::radius_sm(),
                            theme::border(),
                        );
                        round_rect(
                            g,
                            cx + 1.0,
                            pill_y + 1.0,
                            pw - 2.0,
                            pill_h - 2.0,
                            theme::radius_sm() - 1.0,
                            if hov {
                                theme::surface_active()
                            } else {
                                theme::surface_hover()
                            },
                        );
                        g.queue_icon(
                            "git-branch",
                            cx + pad_x,
                            pill_y + (pill_h - icon_sz) / 2.0,
                            icon_sz,
                            theme::text_dim(),
                        );
                        g.draw_text(
                            cx + pad_x + icon_sz + icon_gap,
                            txt_y,
                            &badge.branch,
                            gpu::DrawOpts {
                                font_size: font,
                                color: theme::text(),
                                bold: false,
                                italic: false,
                            },
                        );
                        self.statusbar
                            .branch_rects
                            .push((fid.clone(), (cx, pill_y, pw, pill_h)));
                        cx += pw + chip_gap;
                    }
                    // diff pill — file icon + "N · +ins −del" (green / red).
                    // 좁으면 파일 수만 남긴다 — 「몇 개 바뀌었나」가 「몇 줄인가」보다
                    // 먼저 알아야 할 쪽이고, 줄 수는 Git 탭 머리에 그대로 있다.
                    if let (Some((files_s, plus_s, minus_s, full_w, short_w)), true) =
                        (diff_parts.as_ref(), diff_mode > 0)
                    {
                        let pw = if diff_mode == 2 { *full_w } else { *short_w };
                        let hov = sb_mx >= cx
                            && sb_mx <= cx + pw
                            && sb_my >= pill_y
                            && sb_my <= pill_y + pill_h;
                        round_rect(
                            g,
                            cx,
                            pill_y,
                            pw,
                            pill_h,
                            theme::radius_sm(),
                            theme::border(),
                        );
                        round_rect(
                            g,
                            cx + 1.0,
                            pill_y + 1.0,
                            pw - 2.0,
                            pill_h - 2.0,
                            theme::radius_sm() - 1.0,
                            if hov {
                                theme::surface_active()
                            } else {
                                theme::surface_hover()
                            },
                        );
                        g.queue_icon(
                            "file-text",
                            cx + pad_x,
                            pill_y + (pill_h - icon_sz) / 2.0,
                            icon_sz,
                            theme::text_dim(),
                        );
                        let mut tx = cx + pad_x + icon_sz + icon_gap;
                        tx = g.draw_text(
                            tx,
                            txt_y,
                            files_s,
                            gpu::DrawOpts {
                                font_size: font,
                                color: theme::text(),
                                bold: false,
                                italic: false,
                            },
                        );
                        if diff_mode == 2 {
                            tx = g.draw_text(
                                tx,
                                txt_y,
                                " · ",
                                gpu::DrawOpts {
                                    font_size: font,
                                    color: theme::text_mute(),
                                    bold: false,
                                    italic: false,
                                },
                            );
                            tx = g.draw_text(
                                tx,
                                txt_y,
                                plus_s,
                                gpu::DrawOpts {
                                    font_size: font,
                                    color: theme::success(),
                                    bold: false,
                                    italic: false,
                                },
                            );
                            tx = g.draw_text(
                                tx,
                                txt_y,
                                " ",
                                gpu::DrawOpts {
                                    font_size: font,
                                    color: theme::text_mute(),
                                    bold: false,
                                    italic: false,
                                },
                            );
                            g.draw_text(
                                tx,
                                txt_y,
                                minus_s,
                                gpu::DrawOpts {
                                    font_size: font,
                                    color: theme::danger(),
                                    bold: false,
                                    italic: false,
                                },
                            );
                        }
                        let _ = tx;
                        self.statusbar
                            .diff_rects
                            .push((fid.clone(), (cx, pill_y, pw, pill_h)));
                        cx += pw + chip_gap;
                    }
                }
                // Codex의 native statusline에는 협업 mode가 없을 수 있다. 현재 pane의
                // rollout이 보고한 effort·mode만 앱 footer에 보태고, 경로/Git 칩이 이미
                // 폭을 썼으면 접는다 — 옆 pane 위로 넘기는 것보다 없는 편이 정직하다.
                if let Some(status) = codex_footer_status.get(fid) {
                    let right = fx + fw - 8.0 - 21.0;
                    let label = crate::info::fit_text(
                        g,
                        status,
                        (right - cx - pad_x * 2.0).min(150.0).max(0.0),
                        font,
                        false,
                    );
                    if !label.is_empty() {
                        let pw = pad_x * 2.0 + g.measure_chrome_text(&label, font, false);
                        if cx + pw <= right {
                            round_rect(
                                g,
                                cx,
                                pill_y,
                                pw,
                                pill_h,
                                theme::radius_sm(),
                                theme::border(),
                            );
                            round_rect(
                                g,
                                cx + 1.0,
                                pill_y + 1.0,
                                pw - 2.0,
                                pill_h - 2.0,
                                theme::radius_sm() - 1.0,
                                theme::surface_hover(),
                            );
                            g.draw_text(
                                cx + pad_x,
                                txt_y,
                                &label,
                                gpu::DrawOpts {
                                    font_size: font,
                                    color: theme::text_dim(),
                                    bold: false,
                                    italic: false,
                                },
                            );
                            cx += pw + chip_gap;
                        }
                    }
                }
                let _ = cx;
                // Collapse handle — surfaced only on footer hover so the resting
                // bar matches the screenshot (chips only). Right edge.
                if footer_hover {
                    let h_sz = 13.0;
                    let h_x = fx + fw - h_sz - 8.0;
                    let h_y = bar_y + (pane_footer_h - h_sz) / 2.0;
                    let h_hover = sb_mx >= h_x - 4.0
                        && sb_mx <= h_x + h_sz + 4.0
                        && sb_my >= bar_y
                        && sb_my <= bar_y + pane_footer_h;
                    if h_hover {
                        hover_rect(
                            g,
                            h_x - 4.0,
                            h_y - 2.0,
                            h_sz + 8.0,
                            h_sz + 4.0,
                            theme::radius_sm(),
                        );
                    }
                    g.queue_icon(
                        "chevrons-down-up",
                        h_x,
                        h_y,
                        h_sz,
                        if h_hover {
                            theme::text()
                        } else {
                            theme::text_mute()
                        },
                    );
                    self.statusbar
                        .toggle_rects
                        .push((fid.clone(), (h_x - 4.0, bar_y, h_sz + 12.0, pane_footer_h)));
                }
            }
            if let Some(snapshot) = settings_snapshot.as_ref() {
                settings_paint = Some(crate::native_settings::paint(g, snapshot));
            }
            kasa_gridview::overlay::paint(g, &overlay);
            // Status-bar dropdown (directory picker / branch switcher), drawn
            // last so it overlays the cell grid + every bar. Anchored to the
            // chip that opened it and expanded UPWARD — the bar lives at the
            // pane's bottom, so a downward menu would fall off the edge.
            self.statusbar.menu_dir_rects.clear();
            self.statusbar.menu_branch_rects.clear();
            if let Some((menu_pid, kind)) = self.statusbar.menu.clone() {
                let anchor = match kind {
                    StatusbarMenu::Path => self
                        .statusbar
                        .path_rects
                        .iter()
                        .find(|(p, _)| *p == menu_pid)
                        .map(|(_, r)| *r),
                    StatusbarMenu::Branch => self
                        .statusbar
                        .branch_rects
                        .iter()
                        .find(|(p, _)| *p == menu_pid)
                        .map(|(_, r)| *r),
                };
                if let Some((ax, ay, _aw, _ah)) = anchor {
                    // Item labels (and the value each row carries on click).
                    // Dir names normalized NFC so macOS-decomposed Hangul reads
                    // as composed syllables, not scattered jamo.
                    let is_path = matches!(kind, StatusbarMenu::Path);
                    let labels: Vec<String> = match kind {
                        StatusbarMenu::Path => self
                            .statusbar
                            .menu_dirs
                            .iter()
                            .enumerate()
                            .map(|(i, p)| {
                                if i == 0 {
                                    ".. (상위 폴더)".to_string()
                                } else {
                                    nfc_hangul(
                                        p.file_name().and_then(|s| s.to_str()).unwrap_or("?"),
                                    )
                                }
                            })
                            .collect(),
                        StatusbarMenu::Branch => self.statusbar.menu_branches.clone(),
                    };
                    // Live-search filter (path picker only). Inlined as field
                    // reads — the gpu borrow (`g`) rules out &self method calls.
                    let q = self.statusbar.menu_search.to_lowercase();
                    let fidx: Vec<usize> = if is_path {
                        self.statusbar
                            .menu_dirs
                            .iter()
                            .enumerate()
                            .filter(|(i, p)| {
                                q.is_empty()
                                    || *i == 0
                                    || p.file_name()
                                        .and_then(|s| s.to_str())
                                        .map(|s| nfc_hangul(s).to_lowercase().contains(&q))
                                        .unwrap_or(false)
                            })
                            .map(|(i, _)| i)
                            .collect()
                    } else {
                        (0..labels.len()).collect()
                    };
                    let item_h = if is_path { 28.0 } else { 24.0 };
                    // Search field band at the top of the path picker.
                    let search_h = if is_path { 34.0 } else { 0.0 };
                    let max_rows = 12usize;
                    let total = fidx.len();
                    let view_rows = total.min(max_rows);
                    let menu_w = if is_path { 300.0_f32 } else { 240.0_f32 };
                    let menu_h = search_h + item_h * view_rows.max(1) as f32 + 8.0;
                    let menu_x = ax.min((win_px.0 / scale) - menu_w - 8.0).max(4.0);
                    let menu_y = (ay - menu_h - 2.0).max(TITLE_HEIGHT + 2.0);
                    // Whole-row scroll: 이 메뉴는 클립을 안 세우므로 반쪽 행이
                    // 둥근 모서리 밖으로 삐져나간다. 휠 오프셋을 행 단위로 스냅해
                    // 정수 행씩 넘긴다. 시저를 세워도 되지만 둥근 모서리는 시저의
                    // 직사각형으로 못 흉내 낸다 — 모서리에서 각지게 잘린다.
                    let overflow = total.saturating_sub(view_rows);
                    let scroll = self
                        .statusbar
                        .menu_scroll
                        .clamp(0.0, overflow as f32 * item_h);
                    self.statusbar.menu_scroll = scroll;
                    let first = ((scroll / item_h).round() as usize).min(overflow);
                    self.statusbar.menu_rect = Some((menu_x, menu_y, menu_w, menu_h));
                    panel_rect_outlined(
                        g,
                        menu_x,
                        menu_y,
                        menu_w,
                        menu_h,
                        theme::radius_md(),
                        theme::surface(),
                    );
                    let rows_top = menu_y + 4.0 + search_h;
                    // Inset search field + live query (or dim placeholder). Typing
                    // anywhere while the picker is open feeds this (forward_key).
                    if is_path {
                        let fy = menu_y + 6.0;
                        let fh = search_h - 8.0;
                        round_rect(
                            g,
                            menu_x + 8.0,
                            fy,
                            menu_w - 16.0,
                            fh,
                            theme::radius_sm(),
                            theme::bg(),
                        );
                        g.queue_icon(
                            "folder-tree",
                            menu_x + 16.0,
                            fy + (fh - 14.0) / 2.0,
                            14.0,
                            theme::text_dim(),
                        );
                        let (mut head, tail) = crate::lineedit::split(
                            &self.statusbar.menu_search,
                            self.statusbar.menu_search_cursor,
                        );
                        if self.in_preedit {
                            head.push_str(&self.preedit);
                        }
                        let caret_w = g.measure_chrome_text(&head, 13.0, false);
                        let shown = format!("{head}{tail}");
                        let (txt, col) = if shown.is_empty() {
                            ("디렉터리 검색…".to_string(), theme::text_mute())
                        } else {
                            (shown, theme::text())
                        };
                        g.draw_text(
                            menu_x + 38.0,
                            fy + (fh - 13.0) / 2.0,
                            &txt,
                            gpu::DrawOpts {
                                font_size: 13.0,
                                color: col,
                                bold: false,
                                italic: false,
                            },
                        );
                        // 이 칸엔 캐럿이 없었다 — 끝에만 붙는 칸이라 커서가 어딘지
                        // 물을 일이 없었기 때문이다. 이제 가운데를 고칠 수 있으니
                        // 자리를 보여 줘야 한다.
                        if commit_caret_on {
                            g.rect(
                                menu_x + 38.0 + caret_w,
                                fy + (fh - 14.0) / 2.0,
                                1.5,
                                14.0,
                                theme::text(),
                            );
                        }
                    }
                    if total == 0 {
                        g.draw_text(
                            menu_x + 16.0,
                            rows_top + 4.0,
                            "(없음)",
                            gpu::DrawOpts {
                                font_size: 12.0,
                                color: theme::text_mute(),
                                bold: false,
                                italic: false,
                            },
                        );
                    }
                    let current_branch = matches!(kind, StatusbarMenu::Branch)
                        .then(|| {
                            self.pane_cwd_cache.get(&menu_pid).and_then(|p| {
                                self.window_git
                                    .lock()
                                    .ok()
                                    .and_then(|m| m.get(p).map(|b| b.branch.clone()))
                            })
                        })
                        .flatten();
                    let font = if is_path { 13.0 } else { 12.0 };
                    for vis in 0..view_rows {
                        let Some(&i) = fidx.get(first + vis) else {
                            break;
                        };
                        let Some(label) = labels.get(i) else { break };
                        let iy = rows_top + vis as f32 * item_h;
                        let row = (menu_x, iy, menu_w, item_h);
                        let hover = sb_mx >= row.0
                            && sb_mx <= row.0 + row.2
                            && sb_my >= row.1
                            && sb_my <= row.1 + row.3;
                        // Hovered row = bright accent fill (cursor's selected-item
                        // cue); its glyphs flip to dark for contrast.
                        if hover {
                            round_rect(
                                g,
                                row.0 + 2.0,
                                row.1,
                                row.2 - 4.0,
                                row.3,
                                theme::radius_sm(),
                                theme::accent(),
                            );
                        }
                        let is_current = current_branch.as_deref() == Some(label.as_str());
                        let mut text_x = menu_x + 12.0;
                        // Path picker: leading ↑ / folder / file icon per row.
                        if is_path {
                            let is_parent = i == 0;
                            let is_dir = is_parent
                                || self
                                    .statusbar
                                    .menu_dirs
                                    .get(i)
                                    .map(|p| p.is_dir())
                                    .unwrap_or(false);
                            let glyph = if is_parent {
                                "arrow-up"
                            } else if is_dir {
                                "folder"
                            } else {
                                "file"
                            };
                            let icon_c = if hover {
                                theme::bg()
                            } else {
                                theme::text_dim()
                            };
                            g.queue_icon(glyph, text_x, iy + (item_h - 15.0) / 2.0, 15.0, icon_c);
                            text_x += 15.0 + 9.0;
                        }
                        let color = if hover {
                            theme::bg()
                        } else if is_current {
                            theme::accent()
                        } else {
                            theme::text()
                        };
                        g.draw_text(
                            text_x,
                            iy + (item_h - font) / 2.0,
                            label,
                            gpu::DrawOpts {
                                font_size: font,
                                color,
                                bold: is_current,
                                italic: false,
                            },
                        );
                        match kind {
                            StatusbarMenu::Path => self
                                .statusbar
                                .menu_dir_rects
                                .push((self.statusbar.menu_dirs[i].clone(), row)),
                            StatusbarMenu::Branch => {
                                self.statusbar.menu_branch_rects.push((label.clone(), row))
                            }
                        }
                    }
                    // Scrollbar — thin thumb on the right edge so overflow is
                    // visible; only when the list exceeds the viewport.
                    if overflow > 0 {
                        let track_x = menu_x + menu_w - 4.0;
                        let track_y = rows_top;
                        let track_h = view_rows as f32 * item_h;
                        let thumb_h = (track_h * view_rows as f32 / total as f32).max(18.0);
                        let thumb_y =
                            track_y + (track_h - thumb_h) * (first as f32 / overflow as f32);
                        pill_rect(
                            g,
                            track_x,
                            thumb_y,
                            3.0,
                            thumb_h,
                            theme::with_alpha(theme::text(), 0x55),
                        );
                    }
                } else {
                    self.statusbar.menu_rect = None;
                }
            } else {
                self.statusbar.menu_rect = None;
            }
            // 오른쪽 위 알림. 승인 알림은 답할 때까지 서 있고, 나머지는 잠깐 섰다가 흐려진다.
            self.collab.toast_rect = None;
            self.collab.toast_approve_rect = None;
            self.collab.toast_deny_rect = None;
            if collab_toast_alpha > 0.0 {
                if let Some(msg) = collab_toast_msg.as_ref() {
                    // 센티널만 남고 글이 다른 알림으로 덮인 한 프레임은 칩 없이 그린다.
                    let actions = match update_chips {
                        Some((chips, _)) => Some(chips),
                        None if collab_toast_action_on
                            && self.collab.toast_action.as_deref() != Some(crate::update_notice::ACTION) =>
                        {
                            Some(("승인", "거부"))
                        }
                        None => None,
                    };
                    let hits = toast::paint_notice(
                        g,
                        win_px.0 / scale,
                        (self.cursor_px.0 / scale, self.cursor_px.1 / scale),
                        &toast::Notice {
                            message: msg,
                            alpha: collab_toast_alpha,
                            elapsed_ms: collab_toast_elapsed_ms,
                            actions,
                            decline_is_danger: update_chips.is_none(),
                            tone: update_chips.and_then(|(_, tone)| tone),
                        },
                    );
                    self.collab.toast_rect = Some(hits.card);
                    self.collab.toast_approve_rect = hits.approve;
                    self.collab.toast_deny_rect = hits.deny;
                }
            }
            if self.show_pane_numbers {
                for (id, rect) in &body_rects {
                    if let Some(identity) = pane_identities.get(id) {
                        pane_identity::draw_card(g, identity, *rect);
                    }
                }
            }
            // ── 하단 상태줄 ─────────────────────────────────────────────────
            // 창 맨 아래 한 줄. **계정 한도가 늘 보이는 자리**다 — 패널을 열어야
            // 보이면 「지금 얼마나 남았나」를 확인하려는 순간마다 손이 한 번 더 가고,
            // 그 손이 아까워 안 보다가 한도에 부딪힌다(사용자 2026-08-11 「orca랑
            // 똑같이 하단바 그 형식으로」). 형식은 Orca 하단바에서 가져왔다:
            // 게이지 + 퍼센트 + 언제 풀리는지, 폭이 좁아지면 정해진 순서로 무너진다.
            // lite 는 이 줄 자체가 없다(status_h 도 0).
            if !self.lite {
                status_bar::paint(
                    g,
                    status_bar::Chips {
                        bar: &mut self.statusbar,
                        account_rect: &mut self.status_account_rect,
                        version_rect: &mut self.status_version_rect,
                    },
                    &status_bar::Frame {
                        viewport: (win_px.0 / scale, win_px.1 / scale),
                        status_h,
                        cursor: self.cursor_px,
                        prefs: &self.set_statusbar,
                        accounts: AccountSettings {
                            claude: &self.set_claude_account,
                            claude_list: &self.set_claude_accounts,
                            codex: &self.set_codex_account,
                            codex_list: &self.set_codex_accounts,
                        },
                        claude_observations: &claude_observations,
                        account_flash: self.account_flash,
                        info_view: &self.info.view,
                    },
                );
            }
            // 통째 이동(header/handle·단일탭 tab 드래그)은 실제 레이아웃이 라이브로
            // reflow 되므로 오버레이가 없다 — 진짜 재배치가 곧 프리뷰다. 파란 drop-zone
            // 박스는 라이브가 아닌 tab 드래그(멀티탭 탭 추출)의 착지 지점 힌트로만 남긴다.
            if let Some((zx, zy, zw, zh)) = drop_zone_rect {
                g.rect(zx, zy, zw, zh, theme::with_alpha(theme::accent(), 90));
            }
            if !self.tabs_on_top && self.info.machine_menu.is_none() && tab_strip_w > 0.0
                && sb_cursor.0 >= sb_plus.0 && sb_cursor.0 <= sb_plus.0 + sb_plus.2
                && sb_cursor.1 >= sb_plus.1 && sb_cursor.1 <= sb_plus.1 + sb_plus.3
            {
                Self::draw_hover_tip(g, "기기 추가", sb_plus.0, sb_plus.1,
                    win_px.0 / scale, sb_win_h);
            }
            if self.info.machine_menu.is_some() {
                info::draw_machine_menu(g, sb_cursor, &mut self.info, 0.0, tab_strip_w.max(240.0), TITLE_HEIGHT, sb_win_h - status_h);
            }
            // Launch build banner, bottom-right, painted last so it sits
            // on top. Faint and short-lived — fades out after a few
            // seconds. Coords are logical px (gpu promotes to physical).
            // 계정 드롭다운 — 앵커는 Info 탭 머리의 계정 행(`account_chip_rect`,
            // info::draw_info_actions 가 채운다). 패널 본문 위로 떠야 해서 그 안에서
            // 같이 못 그리고, 모든 pane·오버레이가 끝난 여기서 마지막에 그린다.
            //
            // 계정 행이 곧 계정 스위처다(사용자 요청) — 거기 보이는 한도가 **활성
            // 계정의** 것이라, 이름을 같은 행에 적고 클릭을 전환에 쓰는 게 별도
            // 칩보다 정직하다.
            // 폴러에게 「목록이 펼쳐져 있다」를 알린다. 여닫는 손잡이가 여럿이라
            // (상태줄·Info 계정 행·설정) 각 자리에 심으면 하나를 빠뜨리는 날
            // 플래그가 켜진 채 남아 폴러가 영영 빠른 박자로 돈다. 그리는 자리에서
            // 한 번 맞추면 그런 경로가 없다 — 닫힌 프레임이 한 번만 그려져도
            // 내려간다.
            //
            // `swap` 하나로 **동기화와 열림 감지**를 겸한다. 방금 열렸으면 폴러의
            // 남은 잠을 걷어내(`usage_poke`) 그 자리에서 한 바퀴 돌게 한다 —
            // 열자마자 최신을 보는 것이 이 화면의 첫 인상이다.
            {
                use std::sync::atomic::Ordering;
                let was =
                    crate::handler::usage_menu_open().swap(self.account_menu, Ordering::Relaxed);
                if self.account_menu && !was {
                    crate::handler::usage_poke().store(true, Ordering::Relaxed);
                    // 판 확인도 여는 순간에 붙인다 — 상시 폴링을 안 하는 대신
                    // 사람이 궁금해서 연 그 자리에서 한 바퀴 돈다(30분 캐시).
                    crate::version::ensure_check();
                }
            }
            // 앵커는 **연 손잡이**를 따라간다. 손잡이가 둘이라(Info 탭 계정 행 ·
            // 상태줄) 하나로 고정하면 다른 쪽에서 열었을 때 메뉴가 화면 반대편에
            // 뜬다. 기록이 없으면 옛 동작대로 계정 행에 붙인다.
            account_popover::paint(
                g,
                account_popover::MenuState {
                    hits: &mut self.account_menu_hits,
                    rect: &mut self.account_menu_rect,
                    body_rect: &mut self.account_menu_body_rect,
                    scroll: &mut self.account_menu_scroll,
                    scroll_max: &mut self.account_menu_scroll_max,
                    submenu_rect: &mut self.account_menu_submenu_rect,
                    submenu_body_rect: &mut self.account_menu_submenu_body_rect,
                    corridor_rect: &mut self.account_menu_corridor_rect,
                    submenu_scroll: &mut self.account_menu_submenu_scroll,
                    submenu_scroll_max: &mut self.account_menu_submenu_scroll_max,
                    submenu_hit_start: &mut self.account_menu_submenu_hit_start,
                },
                &account_popover::Frame {
                    open: self.account_menu,
                    anchor: self.account_menu_anchor.or(self.account_chip_rect),
                    viewport: (win_px.0 / scale, win_px.1 / scale),
                    status_h,
                    cursor: self.cursor_px,
                    compact: self.set_usage_compact,
                    accounts: AccountSettings {
                        claude: &self.set_claude_account,
                        claude_list: &self.set_claude_accounts,
                        codex: &self.set_codex_account,
                        codex_list: &self.set_codex_accounts,
                    },
                    provider: self.account_menu_provider,
                    claude_observations: &claude_observations,
                    codex_rollout: codex_rollout.as_ref(),
                },
            );
            let v_alpha = version_alpha;
            if v_alpha > 0.0 {
                let label = Self::version_label();
                let v_font = 11.0_f32;
                let win_w = win_px.0 / scale;
                let win_h = win_px.1 / scale;
                let text_w = g.measure_chrome_text(&label, v_font, false);
                let margin = 8.0;
                let x = (win_w - text_w - margin).max(margin);
                // 상태줄 **위**로. 창 바닥에 붙이던 자리인데 그 자리는 이제 상태줄이
                // 쓰고, 버전이 그 위에 덧그려져 자원 수치와 글자가 포개졌다(부팅 후
                // 몇 초라 놓치기 쉽다 — 2026-08-15 캡처에서 잡았다).
                let y = win_h - status_h - v_font - margin;
                let a = (170.0 * v_alpha).round() as u8;
                g.draw_text(
                    x,
                    y,
                    &label,
                    gpu::DrawOpts {
                        font_size: v_font,
                        color: theme::with_alpha(theme::text_dim(), a),
                        bold: false,
                        italic: false,
                    },
                );
            }
            // 커밋 모달 — 전면 스크림을 깐 진짜 대화상자라, 창 안의 모든 것보다
            // 나중에 그려져야 한다. 사이드바 블록 안에서 그리던 동안엔 그 뒤에
            // 오는 pane 헤더·divider·활성 보더가 카드 위를 가로질렀다(사용자).
            // ── Commit modal (screenshot #5): dim + centered card.
            self.git.commit_modal_rects.clear();
            if self.git.commit_modal_open {
                // Full-window dim + centered card (not clipped to the git
                // column) so the modal reads as a real dialog and nothing
                // behind it bleeds through.
                let win_w = win_px.0 / scale;
                let win_h = win_px.1 / scale;
                g.rect(
                    0.0,
                    0.0,
                    win_w,
                    win_h,
                    theme::with_alpha([0, 0, 0, 255], 0xCC),
                );
                let bw = 560.0_f32.min(win_w - 60.0).max(0.0);
                let bx = (win_w - bw) / 2.0;
                let bh = (win_h - TITLE_HEIGHT - 60.0).min(660.0).max(0.0);
                let bxy = TITLE_HEIGHT + (win_h - TITLE_HEIGHT - bh) / 2.0;
                round_rect(
                    g,
                    bx - 1.0,
                    bxy - 1.0,
                    bw + 2.0,
                    bh + 2.0,
                    theme::radius_md(),
                    theme::with_alpha(theme::border(), 0xFF),
                );
                round_rect(g, bx, bxy, bw, bh, theme::radius_md(), theme::bg());
                let pad = 22.0_f32;
                let cx = bx + pad;
                let cw = bw - pad * 2.0;
                let mut my = bxy + pad;
                // Header: icon chip + X
                round_rect(
                    g,
                    cx,
                    my,
                    36.0,
                    36.0,
                    theme::radius_sm(),
                    theme::surface_active(),
                );
                g.queue_icon(
                    "git-commit-horizontal",
                    cx + 10.0,
                    my + 10.0,
                    16.0,
                    theme::text(),
                );
                let xx = bx + bw - pad - 16.0;
                let xhov = self.cursor_px.0 >= xx - 5.0
                    && self.cursor_px.0 <= xx + 21.0
                    && self.cursor_px.1 >= my
                    && self.cursor_px.1 <= my + 24.0;
                g.queue_icon(
                    "x",
                    xx,
                    my + 4.0,
                    16.0,
                    if xhov {
                        theme::text()
                    } else {
                        theme::text_mute()
                    },
                );
                self.git
                    .commit_modal_rects
                    .push((GitModalBtn::Close, (xx - 5.0, my, 26.0, 26.0)));
                my += 36.0 + 18.0;
                g.draw_text(
                    cx,
                    my,
                    "Commit your changes",
                    gpu::DrawOpts {
                        font_size: 19.0,
                        color: theme::text(),
                        bold: true,
                        italic: false,
                    },
                );
                my += 36.0;
                // Branch
                g.draw_text(
                    cx,
                    my,
                    "Branch",
                    gpu::DrawOpts {
                        font_size: 13.0,
                        color: theme::text_mute(),
                        bold: false,
                        italic: false,
                    },
                );
                my += 22.0;
                g.queue_icon("git-branch", cx, my, 15.0, theme::text_dim());
                let mbranch = if git_view.branch.is_empty() {
                    "—"
                } else {
                    git_view.branch.as_str()
                };
                g.draw_text(
                    cx + 22.0,
                    my + 1.0,
                    mbranch,
                    gpu::DrawOpts {
                        font_size: 14.0,
                        color: theme::text(),
                        bold: false,
                        italic: false,
                    },
                );
                my += 34.0;
                // Changes + Include unstaged toggle
                g.draw_text(
                    cx,
                    my,
                    "Changes",
                    gpu::DrawOpts {
                        font_size: 13.0,
                        color: theme::text_mute(),
                        bold: false,
                        italic: false,
                    },
                );
                let tw = 38.0_f32;
                let th = 20.0_f32;
                let tx = bx + bw - pad - tw;
                let tlbl = "Include unstaged";
                let tlw = g.measure_chrome_text(tlbl, 13.0, false);
                g.draw_text(
                    tx - 8.0 - tlw,
                    my,
                    tlbl,
                    gpu::DrawOpts {
                        font_size: 13.0,
                        color: theme::text_dim(),
                        bold: false,
                        italic: false,
                    },
                );
                let on = self.git.commit_modal_include_unstaged;
                pill_rect(
                    g,
                    tx,
                    my - 2.0,
                    tw,
                    th,
                    if on {
                        theme::accent()
                    } else {
                        theme::surface_active()
                    },
                );
                let knob = th - 6.0;
                let kx = if on { tx + tw - knob - 3.0 } else { tx + 3.0 };
                circle_rect(g, kx, my - 2.0 + 3.0, knob, [255, 255, 255, 255]);
                self.git.commit_modal_rects.push((
                    GitModalBtn::IncludeUnstaged,
                    (tx - 4.0, my - 5.0, tw + 8.0, th + 8.0),
                ));
                my += 28.0;
                // File list box
                let lh = (bh * 0.28).min(180.0).max(60.0);
                panel_rect_outlined(g, cx, my, cw, lh, theme::radius_sm(), theme::surface());
                let nf = git_view.staged.len() + git_view.unstaged.len();
                let mut fx = g.draw_text(
                    cx + 12.0,
                    my + 10.0,
                    &format!("{} files", nf),
                    gpu::DrawOpts {
                        font_size: 13.0,
                        color: theme::text(),
                        bold: true,
                        italic: false,
                    },
                );
                fx = g.draw_text(
                    fx + 10.0,
                    my + 10.0,
                    &format!("+{}", git_view.insertions),
                    gpu::DrawOpts {
                        font_size: 13.0,
                        color: theme::success(),
                        bold: false,
                        italic: false,
                    },
                );
                g.draw_text(
                    fx + 8.0,
                    my + 10.0,
                    &format!("-{}", git_view.deletions),
                    gpu::DrawOpts {
                        font_size: 13.0,
                        color: DIFF_RED,
                        bold: false,
                        italic: false,
                    },
                );
                let mut ly = my + 34.0;
                for (_m, path) in git_view.staged.iter().chain(git_view.unstaged.iter()) {
                    if ly > my + lh - 18.0 {
                        break;
                    }
                    let fname = path.rsplit('/').next().unwrap_or(path.as_str());
                    let dir = path.strip_suffix(fname).unwrap_or("").trim_end_matches('/');
                    let ex = g.draw_text(
                        cx + 12.0,
                        ly,
                        fname,
                        gpu::DrawOpts {
                            font_size: 13.0,
                            color: theme::text(),
                            bold: false,
                            italic: false,
                        },
                    );
                    if !dir.is_empty() {
                        g.draw_text(
                            ex + 7.0,
                            ly + 0.5,
                            dir,
                            gpu::DrawOpts {
                                font_size: 11.0,
                                color: theme::text_mute(),
                                bold: false,
                                italic: false,
                            },
                        );
                    }
                    if let Some((ins, del)) = git_view.numstat.get(path) {
                        let minus = format!("-{del}");
                        let plus = format!("+{ins}");
                        let wm = g.measure_chrome_text(&minus, 12.0, false);
                        let wp = g.measure_chrome_text(&plus, 12.0, false);
                        let mut rx = cx + cw - 12.0;
                        if *del > 0 {
                            rx -= wm;
                            g.draw_text(
                                rx,
                                ly,
                                &minus,
                                gpu::DrawOpts {
                                    font_size: 12.0,
                                    color: DIFF_RED,
                                    bold: false,
                                    italic: false,
                                },
                            );
                            rx -= 6.0;
                        }
                        if *ins > 0 {
                            rx -= wp;
                            g.draw_text(
                                rx,
                                ly,
                                &plus,
                                gpu::DrawOpts {
                                    font_size: 12.0,
                                    color: theme::success(),
                                    bold: false,
                                    italic: false,
                                },
                            );
                        }
                    }
                    ly += 22.0;
                }
                my += lh + 18.0;
                // Commit message box
                g.draw_text(
                    cx,
                    my,
                    "Commit message",
                    gpu::DrawOpts {
                        font_size: 13.0,
                        color: theme::text_mute(),
                        bold: false,
                        italic: false,
                    },
                );
                my += 22.0;
                let inh = 70.0_f32;
                if self.git.commit_focused {
                    round_rect(
                        g,
                        cx - 1.0,
                        my - 1.0,
                        cw + 2.0,
                        inh + 2.0,
                        theme::radius_sm(),
                        theme::accent(),
                    );
                }
                panel_rect(g, cx, my, cw, inh, theme::radius_sm(), theme::surface());
                let itx = cx + 10.0;
                let ity = my + 9.0;
                let preedit = if self.git.commit_focused {
                    self.preedit.as_str()
                } else {
                    ""
                };
                if self.git.commit_msg.is_empty() && preedit.is_empty() {
                    g.draw_text(
                        itx,
                        ity,
                        "변경 사항 설명…",
                        gpu::DrawOpts {
                            font_size: 13.0,
                            color: theme::text_mute(),
                            bold: false,
                            italic: false,
                        },
                    );
                }
                let cur = self
                    .git
                    .commit_cursor
                    .min(self.git.commit_msg.chars().count());
                let before: String = self.git.commit_msg.chars().take(cur).collect();
                let after: String = self.git.commit_msg.chars().skip(cur).collect();
                let mut px = g.draw_text(
                    itx,
                    ity,
                    &before,
                    gpu::DrawOpts {
                        font_size: 13.0,
                        color: theme::text(),
                        bold: false,
                        italic: false,
                    },
                );
                let caret_x = px;
                if !preedit.is_empty() {
                    px = g.draw_text(
                        px,
                        ity,
                        preedit,
                        gpu::DrawOpts {
                            font_size: 13.0,
                            color: theme::accent(),
                            bold: false,
                            italic: false,
                        },
                    );
                }
                if !after.is_empty() {
                    g.draw_text(
                        px,
                        ity,
                        &after,
                        gpu::DrawOpts {
                            font_size: 13.0,
                            color: theme::text(),
                            bold: false,
                            italic: false,
                        },
                    );
                }
                if self.git.commit_focused && preedit.is_empty() && commit_caret_on {
                    g.rect(caret_x, ity, 1.5, 14.0, theme::text());
                }
                self.git.commit_input_rect = Some((cx, my, cw, inh));
                my += inh + 14.0;
                // Commit / Commit and push buttons (full width)
                let bbh = 36.0_f32;
                for (icon, label, btn) in [
                    ("git-commit-horizontal", "Commit", GitModalBtn::Commit),
                    ("arrow-up", "Commit and push", GitModalBtn::CommitAndPush),
                ] {
                    let hov = self.cursor_px.0 >= cx
                        && self.cursor_px.0 <= cx + cw
                        && self.cursor_px.1 >= my
                        && self.cursor_px.1 <= my + bbh;
                    panel_rect(
                        g,
                        cx,
                        my,
                        cw,
                        bbh,
                        theme::radius_sm(),
                        if hov {
                            theme::surface_hover()
                        } else {
                            theme::surface_active()
                        },
                    );
                    g.queue_icon(
                        icon,
                        cx + 14.0,
                        my + (bbh - 15.0) / 2.0,
                        15.0,
                        theme::text(),
                    );
                    g.draw_text(
                        cx + 38.0,
                        my + (bbh - 13.0) / 2.0,
                        label,
                        gpu::DrawOpts {
                            font_size: 13.0,
                            color: theme::text(),
                            bold: false,
                            italic: false,
                        },
                    );
                    self.git.commit_modal_rects.push((btn, (cx, my, cw, bbh)));
                    my += bbh + 8.0;
                }
                // Cancel / Confirm (bottom-right)
                let confirm_w = 96.0_f32;
                let cancel_w = 80.0_f32;
                let cby = bxy + bh - pad - 34.0;
                let conf_x = bx + bw - pad - confirm_w;
                let canc_x = conf_x - 10.0 - cancel_w;
                let conf_hov = self.cursor_px.0 >= conf_x
                    && self.cursor_px.0 <= conf_x + confirm_w
                    && self.cursor_px.1 >= cby
                    && self.cursor_px.1 <= cby + 34.0;
                let canc_hov = self.cursor_px.0 >= canc_x
                    && self.cursor_px.0 <= canc_x + cancel_w
                    && self.cursor_px.1 >= cby
                    && self.cursor_px.1 <= cby + 34.0;
                let wcanc = g.measure_chrome_text("Cancel", 13.0, false);
                g.draw_text(
                    canc_x + (cancel_w - wcanc) / 2.0,
                    cby + 10.0,
                    "Cancel",
                    gpu::DrawOpts {
                        font_size: 13.0,
                        color: if canc_hov {
                            theme::text()
                        } else {
                            theme::text_dim()
                        },
                        bold: false,
                        italic: false,
                    },
                );
                self.git
                    .commit_modal_rects
                    .push((GitModalBtn::Cancel, (canc_x, cby, cancel_w, 34.0)));
                panel_rect(
                    g,
                    conf_x,
                    cby,
                    confirm_w,
                    34.0,
                    theme::radius_sm(),
                    if conf_hov {
                        theme::accent()
                    } else {
                        theme::surface_active()
                    },
                );
                let wconf = g.measure_chrome_text("Confirm", 13.0, true);
                g.draw_text(
                    conf_x + (confirm_w - wconf) / 2.0,
                    cby + 10.0,
                    "Confirm",
                    gpu::DrawOpts {
                        font_size: 13.0,
                        color: theme::text(),
                        bold: true,
                        italic: false,
                    },
                );
                self.git
                    .commit_modal_rects
                    .push((GitModalBtn::Confirm, (conf_x, cby, confirm_w, 34.0)));
            }
            // Confirm-close modal: a dim scrim + centered card with 취소/닫기,
            // queued last so it sits over every pane, overlay and toast.
            if let Some(dlg) = self.confirm_close.clone().filter(|_| !self.confirm_native) {
                let win_w = win_px.0 / scale;
                let win_h = win_px.1 / scale;
                g.rect(
                    0.0,
                    0.0,
                    win_w,
                    win_h,
                    theme::with_alpha([0, 0, 0, 255], 0xB0),
                );
                let dirty = matches!(dlg.why, crate::CloseWhy::Dirty(_));
                let mirror = matches!(dlg.why, crate::CloseWhy::Mirror { .. });
                let mirror_closing = matches!(dlg.why, crate::CloseWhy::Mirror { closing: true, .. });
                // pane 통째 닫기(⋮ ×)를 「탭」이라 부르면 무엇이 사라지는지가
                // 어긋난다 — 그건 그 pane 의 탭을 전부 걷는다. 와일드카드를 안 쓰는
                // 이유는 `ActionKind` 디스패치와 같다: variant 가 늘었을 때 문구가
                // 조용히 엉뚱한 쪽으로 접히지 않고 컴파일에서 걸리게.
                let what = match dlg.action {
                    crate::PendingClose::Window => "앱을",
                    crate::PendingClose::Session(_) => "이 세션을",
                    crate::PendingClose::RemoteRoom { .. } => "그 기기의 방을",
                    crate::PendingClose::Pane { .. } => "이 pane 을",
                    crate::PendingClose::Tab { .. } => "이 탭을",
                    crate::PendingClose::AuxEditor(_) => "이 문서 창을",
                };
                let (title, subtitle) = match &dlg.why {
                    crate::CloseWhy::Mirror { targets, closing, error } => {
                        let source = targets.iter().take(3)
                            .map(|t| format!("{} {}", if t.label.is_empty() { "원본 기기" } else { &t.label }, t.source))
                            .collect::<Vec<_>>().join(", ");
                        if *closing {
                            ("원본 기기의 창을 닫는 중".to_string(), source)
                        } else if error.is_some() {
                            ("원본을 닫지 못했어".to_string(), "거울은 그대로 있어. 다시 시도하거나 거울만 닫을 수 있어".to_string())
                        } else {
                            ("원본 기기의 창도 닫을까?".to_string(), source)
                        }
                    }
                    crate::CloseWhy::Busy(proc) => {
                        (format!("{proc} 실행 중이에요"), format!("{what} 닫을까요?"))
                    }
                    // 파일 이름을 보여줘야 뭘 잃는지 안다. 셋을 넘으면 카드가
                    // 감당 못 하니 나머지는 개수로 접는다.
                    crate::CloseWhy::Dirty(docs) => {
                        let head: Vec<&str> =
                            docs.iter().take(3).map(|(_, n)| n.as_str()).collect();
                        let rest = docs.len().saturating_sub(head.len());
                        let names = if rest > 0 {
                            format!("{} 외 {rest}개", head.join(", "))
                        } else {
                            head.join(", ")
                        };
                        (
                            "저장하지 않은 변경이 있어요".to_string(),
                            format!("{names} — {what} 닫을까요?"),
                        )
                    }
                    // 왜 하나가 아니라 방이 닫히는지부터 말한다 — Cmd+W 는 「하나
                    // 닫기」로 익힌 키라, 이유 없이 방 확인이 뜨면 오작동처럼 읽힌다.
                    crate::CloseWhy::LastPane => (
                        "이 방의 마지막 pane 이에요".to_string(),
                        format!("{what} 닫을까요?"),
                    ),
                };
                let title = &title;
                let subtitle = subtitle.as_str();
                // 부제에 파일 이름이 들어와 길이가 변하니 카드도 재서 정한다 —
                // 고정 폭이던 시절엔 이름 셋이 그대로 카드 밖으로 나갔다.
                let pad = 24.0_f32;
                let title_w = g.measure_chrome_text(title, 15.0, true);
                let sub_w = g.measure_chrome_text(subtitle, 13.0, false);
                let card_w = (title_w.max(sub_w) + pad * 2.0)
                    .clamp(if dirty || mirror { 420.0 } else { 360.0 }, (win_w - 48.0).max(420.0));
                let card_h = 168.0_f32;
                let cx0 = ((win_w - card_w) / 2.0).round();
                let cy0 = ((win_h - card_h) / 2.0).round();
                panel_rect_outlined(
                    g,
                    cx0,
                    cy0,
                    card_w,
                    card_h,
                    theme::radius_md(),
                    theme::surface_active(),
                );
                g.draw_text(
                    cx0 + pad,
                    cy0 + 30.0,
                    &title,
                    gpu::DrawOpts {
                        font_size: 15.0,
                        color: theme::text(),
                        bold: true,
                        italic: false,
                    },
                );
                g.draw_text(
                    cx0 + pad,
                    cy0 + 60.0,
                    subtitle,
                    gpu::DrawOpts {
                        font_size: 13.0,
                        color: theme::text_dim(),
                        bold: false,
                        italic: false,
                    },
                );
                let (mx, my) = self.cursor_px;
                let bf = 13.0_f32;
                let bpad = 18.0_f32;
                let btn_h = 34.0_f32;
                let btn_y = cy0 + card_h - 20.0 - btn_h;
                // 오른쪽부터 왼쪽으로 쌓는다 — 기본 동작이 오른쪽 끝에 오는 배치.
                let mut right = cx0 + card_w - pad;
                // 채움색이 곧 뜻이다: accent = 기본 동작, danger = 되돌릴 수
                // 없는 쪽, 무채색 = 그 외. 저장은 파괴적이지 않으니 빨강을 안 쓴다.
                let mut button = |g: &mut gpu::GpuRenderer,
                                  hits: &mut Vec<(crate::ConfirmBtn, (f32, f32, f32, f32))>,
                                  label: &str,
                                  btn: crate::ConfirmBtn,
                                  tone: Option<[u8; 4]>| {
                    let w = g.measure_chrome_text(label, bf, tone.is_some()) + bpad * 2.0;
                    let x = right - w;
                    right = x - 10.0;
                    let hot = mx >= x && mx <= x + w && my >= btn_y && my <= btn_y + btn_h;
                    g.hover_pointer |= hot;
                    let (fill, fg, bold) = match tone {
                        Some(c) => (
                            theme::with_alpha(c, if hot { 0xFF } else { 0xDD }),
                            [0xFF, 0xFF, 0xFF, 0xFF],
                            true,
                        ),
                        None => (
                            theme::raised_on(theme::surface_active(), hot),
                            theme::text(),
                            false,
                        ),
                    };
                    // 무채색 쪽은 테두리로 선다 — 주액션과 갈리는 축이 색 하나가
                    // 아니라 형태여야 흑백에서도 어느 쪽이 기본인지 읽힌다.
                    if tone.is_some() {
                        panel_rect(g, x, btn_y, w, btn_h, theme::radius_sm(), fill);
                    } else {
                        panel_rect_outlined(g, x, btn_y, w, btn_h, theme::radius_sm(), fill);
                    }
                    g.draw_text(
                        x + bpad,
                        btn_y + (btn_h - bf) / 2.0,
                        label,
                        gpu::DrawOpts {
                            font_size: bf,
                            color: fg,
                            bold,
                            italic: false,
                        },
                    );
                    hits.push((btn, (x, btn_y, w, btn_h)));
                };
                if mirror && !mirror_closing {
                    button(g, &mut confirm_btn_hits, "원본도 닫기", crate::ConfirmBtn::CloseSource, Some(theme::danger()));
                    button(g, &mut confirm_btn_hits, "거울만 닫기", crate::ConfirmBtn::Close, Some(theme::accent()));
                } else if dirty {
                    // 저장이 기본이라 오른쪽 끝 — 실수로 끝을 눌러도 안전한 쪽이
                    // 걸리게. 편집분을 버리는 "저장 안 함" 은 그 왼쪽에 빨강으로.
                    let acc = theme::accent();
                    button(
                        g,
                        &mut confirm_btn_hits,
                        "저장",
                        crate::ConfirmBtn::Save,
                        Some(acc),
                    );
                    let dg = theme::danger();
                    button(
                        g,
                        &mut confirm_btn_hits,
                        "저장 안 함",
                        crate::ConfirmBtn::Close,
                        Some(dg),
                    );
                } else if !mirror_closing {
                    let dg = theme::danger();
                    button(
                        g,
                        &mut confirm_btn_hits,
                        "닫기",
                        crate::ConfirmBtn::Close,
                        Some(dg),
                    );
                }
                if !mirror_closing { button(
                    g,
                    &mut confirm_btn_hits,
                    "취소",
                    crate::ConfirmBtn::Cancel,
                    None,
                ); }
            }
            // Chrome-style restore prompt: dim scrim + centered card offering to
            // reopen the last session's panes. Queued after the confirm modal so
            // it sits over everything at launch. [복원] rebuilds the workspace,
            // [새로 시작] keeps the fresh session.
            if let Some(state) = self.restore_prompt.clone() {
                let win_w = win_px.0 / scale;
                let win_h = win_px.1 / scale;
                g.rect(
                    0.0,
                    0.0,
                    win_w,
                    win_h,
                    theme::with_alpha([0, 0, 0, 255], 0xB0),
                );
                let n = crate::App::count_claude_panes(&state);
                let total = crate::App::count_panes(&state);
                // 돌아올 학생들. 「pane 20개」는 숫자일 뿐이지만 얼굴은 누가 오는지를
                // 말한다 — 이 카드가 크롬 대화상자처럼 보이던 이유가 그 자리가 비어
                // 있어서였다(2026-08-27 지적: 「디자인이 구려」).
                let faces: Vec<String> = {
                    fn walk(node: &serde_json::Value, out: &mut Vec<String>) {
                        if let Some(leaf) = node.get("leaf") {
                            if let Some(c) = leaf.get("character").and_then(|c| c.as_str()) {
                                if !c.is_empty() && !out.iter().any(|x| x == c) {
                                    out.push(c.to_string());
                                }
                            }
                        } else if let Some(split) = node.get("split") {
                            for k in ["a", "b"] {
                                if let Some(x) = split.get(k) {
                                    walk(x, out);
                                }
                            }
                        }
                    }
                    let mut out = Vec::new();
                    if let Some(sessions) = state.get("sessions").and_then(|s| s.as_array()) {
                        for sess in sessions {
                            if let Some(ws) = sess.get("windows").and_then(|w| w.as_array()) {
                                for w in ws {
                                    walk(w, &mut out);
                                }
                            }
                        }
                    }
                    out
                };
                const RESTORE_TITLE: &str = "이전 세션을 이어서 켤까요?";
                let subtitle = if n > 0 {
                    format!("{} · 창 {total}개 · 캐릭터 {n}명", crate::restore_progress::last_used_label(&state))
                } else {
                    format!("{} · 창 {total}개", crate::restore_progress::last_used_label(&state))
                };
                let pad = 24.0_f32;
                let bf = 13.0_f32;
                let bpad = 18.0_f32;
                let btn_h = 34.0_f32;
                let btn_gap = 8.0_f32;
                // 프사는 얼굴만 잘라 온 그림이라 **바닥이 평평하게 끊긴다**. 그대로
                // 놓으면 36px 에서 그 절단면이 그대로 보인다 — 작은 자리(22px 세션탭)
                // 에서는 안 보이던 것이다. 테두리 있는 칩 **안에** 넣으면 그 끊김이
                // 상자 안쪽 일이 되어 의도된 모양으로 읽힌다(2026-08-27 지적
                // 「테두리나 모양도 신경써줘」).
                let chip = 44.0_f32;
                let ring = 2.0_f32;
                let chip_inset = ring + 2.0;
                let face = chip - chip_inset * 2.0;
                let face_gap = 8.0_f32;
                // 카드 폭이 감당하는 얼굴 수를 먼저 정하고(최대 9), 나머지는 +N 로 접는다.
                let face_slots = (((win_w - 72.0).max(0.0) + face_gap) / (chip + face_gap)) as usize;
                let face_max = if win_h < 260.0 { 0 } else {
                    9usize.min(faces.len()).min(face_slots.saturating_sub(usize::from(faces.len() > face_slots)))
                };
                let overflow = faces.len().saturating_sub(face_max);
                let btn_w = g
                    .measure_chrome_text("새로 시작", bf, false)
                    .max(g.measure_chrome_text("복원", bf, true))
                    + bpad * 2.0;
                let title_w = g.measure_chrome_text(RESTORE_TITLE, 16.0, true);
                let sub_w = g.measure_chrome_text(&subtitle, 12.5, false);
                let hint = "esc 닫기";
                let hint_w = g.measure_chrome_text(hint, 11.5, false);
                let faces_w = if face_max > 0 {
                    face_max as f32 * chip
                        + (face_max as f32 - 1.0).max(0.0) * face_gap
                        + if overflow > 0 { chip + face_gap } else { 0.0 }
                } else {
                    0.0
                };
                // 닫기(×) 자리를 제목 오른쪽에 비워 둔다 — 제목이 그 밑으로 흐르면
                // 카드가 삐뚤어 보인다.
                // 닫기는 **눌러야 하는 것**이라 글리프 크기가 아니라 손가락 크기로
                // 잡는다 — 26px 짜리는 화면에서 먼지처럼 보였다(2026-08-27 지적).
                let close = 36.0_f32;
                let body_w = (title_w + close + 12.0)
                    .max(sub_w)
                    .max(faces_w)
                    .max(hint_w + 16.0 + btn_w * 2.0 + btn_gap);
                let card_w = (body_w + pad * 2.0).max(440.0).min((win_w - 24.0).max(1.0));
                let title_display = crate::info::fit_text(g, RESTORE_TITLE, (card_w - pad * 2.0 - close - 12.0).max(1.0), 16.0, true);
                let subtitle = crate::info::fit_text(g, &subtitle, (card_w - pad * 2.0).max(1.0), 12.5, false);
                let hint = if hint_w + 16.0 + btn_w * 2.0 + btn_gap <= card_w - pad * 2.0 { hint } else { "" };
                let btn_w = btn_w.min(((card_w - pad * 2.0 - btn_gap) / 2.0).max(1.0));
                let title_y = 26.0_f32;
                let sub_y = title_y + 26.0;
                let faces_y = sub_y + 24.0;
                let btn_dy = if face_max > 0 {
                    faces_y + chip + 22.0
                } else {
                    sub_y + 34.0
                };
                let card_h = btn_dy + btn_h + pad;
                let cx0 = ((win_w - card_w) / 2.0).round();
                let cy0 = ((win_h - card_h) / 2.0).round();
                // 라운드는 토큰의 배수 — 픽셀 실루엣(0)에서는 그대로 0 이라 각진
                // 카드가 유지된다. 테두리는 `panel_rect_outlined` 가 채움 기준으로
                // 잡는다(`theme::edge_on`).
                let card_r = theme::radius_md() * 1.5;
                panel_rect_outlined(g, cx0, cy0, card_w, card_h, card_r, theme::surface_active());
                let (mx, my) = self.cursor_px;
                g.draw_text(
                    cx0 + pad,
                    cy0 + title_y,
                    &title_display,
                    gpu::DrawOpts {
                        font_size: 16.0,
                        color: theme::text(),
                        bold: true,
                        italic: false,
                    },
                );
                g.draw_text(
                    cx0 + pad,
                    cy0 + sub_y,
                    &subtitle,
                    gpu::DrawOpts {
                        font_size: 12.5,
                        color: theme::text_dim(),
                        bold: false,
                        italic: false,
                    },
                );
                // 닫기(×) — 이 카드에 없던 것. 저장본은 그대로 두고 카드만 접는다.
                let close_x = cx0 + card_w - pad - close;
                let close_y = cy0 + title_y - 10.0;
                let close_hover = mx >= close_x
                    && mx <= close_x + close
                    && my >= close_y
                    && my <= close_y + close;
                g.hover_pointer |= close_hover;
                // 판은 **호버 전에도** 깔아 둔다 — 글리프만 떠 있으면 장식으로 읽혀,
                // 카드를 접는 길이 있는데도 없는 것과 같았다. 그리고 `×` 글자가 아니라
                // 아이콘이라야 굵기가 제 크기로 선다(폰트 글리프는 자릿수만 크고
                // 잉크가 얇다 — 2026-08-27 지적 「x도 작잖아」).
                circle_rect(
                    g,
                    close_x,
                    close_y,
                    close,
                    theme::raised_on(theme::surface_active(), close_hover),
                );
                let xs = 30.0_f32;
                g.queue_icon(
                    "x",
                    close_x + (close - xs) / 2.0,
                    close_y + (close - xs) / 2.0,
                    xs,
                    if close_hover {
                        theme::text()
                    } else {
                        theme::text_dim()
                    },
                );
                restore_btn_hits
                    .push((crate::RestoreBtn::Dismiss, (close_x, close_y, close, close)));
                // 돌아올 학생들의 얼굴. 그림이 없는 이름은 조용히 건너뛴다 — 빈 네모를
                // 그리면 「없는 학생」처럼 보인다.
                if face_max > 0 {
                    let fy = cy0 + faces_y;
                    let mut fx = cx0 + pad;
                    for name in faces.iter().take(face_max) {
                        // 칩을 먼저 깔고 그 위에 얼굴 — 순서가 바뀌면 칩이 얼굴을 덮는다.
                        // 칩 색은 카드보다 한 단 어둡게: 같은 톤이면 테두리만 떠서
                        // 상자가 아니라 선으로 보인다.
                        // 링은 **둥근 사각 두 장**으로 만든다 — 직선 네 개를 얹으면
                        // 모서리가 둥근 상자 밖으로 튀어 각진 테두리처럼 보인다.
                        let r_out = 12.0_f32;
                        let tint = theme::character_accent_any(name).unwrap_or_else(theme::border);
                        round_rect(g, fx, fy, chip, chip, r_out, theme::with_alpha(tint, 0xCC));
                        round_rect(
                            g,
                            fx + ring,
                            fy + ring,
                            chip - ring * 2.0,
                            chip - ring * 2.0,
                            r_out - ring,
                            theme::surface(),
                        );
                        if !crate::sprites::draw_student_face(
                            g,
                            name,
                            fx + chip_inset,
                            fy + chip_inset,
                            face,
                        ) {
                            // 그림이 없는 학생 — 빈 칩 대신 이름 첫 글자.
                            let ch: String = name.chars().take(1).collect();
                            let cw = g.measure_chrome_text(&ch, 15.0, true);
                            g.draw_text(
                                fx + (chip - cw) / 2.0,
                                fy + (chip - 15.0) / 2.0,
                                &ch,
                                gpu::DrawOpts {
                                    font_size: 15.0,
                                    color: theme::text_dim(),
                                    bold: true,
                                    italic: false,
                                },
                            );
                        }
                        fx += chip + face_gap;
                    }
                    if overflow > 0 {
                        let r_out = 12.0_f32;
                        round_rect(
                            g,
                            fx,
                            fy,
                            chip,
                            chip,
                            r_out,
                            theme::with_alpha(theme::border(), 0xCC),
                        );
                        round_rect(
                            g,
                            fx + ring,
                            fy + ring,
                            chip - ring * 2.0,
                            chip - ring * 2.0,
                            r_out - ring,
                            theme::surface(),
                        );
                        let more = format!("+{overflow}");
                        let mw = g.measure_chrome_text(&more, 13.0, true);
                        g.draw_text(
                            fx + (chip - mw) / 2.0,
                            fy + (chip - 13.0) / 2.0,
                            &more,
                            gpu::DrawOpts {
                                font_size: 13.0,
                                color: theme::text_dim(),
                                bold: true,
                                italic: false,
                            },
                        );
                    }
                }
                let btn_y = cy0 + btn_dy;
                // 키 안내는 버튼 반대편 바닥에 — 길이 있는데 안 보이면 없는 것과 같다.
                g.draw_text(
                    cx0 + pad,
                    btn_y + (btn_h - 11.5) / 2.0,
                    hint,
                    gpu::DrawOpts {
                        font_size: 11.5,
                        color: theme::text_dim(),
                        bold: false,
                        italic: false,
                    },
                );
                let hit = |x: f32| mx >= x && mx <= x + btn_w && my >= btn_y && my <= btn_y + btn_h;
                // 복원 (primary/accent), flush to the card's right edge.
                let restore_x = cx0 + card_w - pad - btn_w;
                let restore_hover = hit(restore_x);
                g.hover_pointer |= restore_hover;
                panel_rect(
                    g,
                    restore_x,
                    btn_y,
                    btn_w,
                    btn_h,
                    theme::radius_sm(),
                    theme::with_alpha(theme::accent(), if restore_hover { 0xFF } else { 0xDD }),
                );
                let rl_w = g.measure_chrome_text("복원", bf, true);
                g.draw_text(
                    restore_x + (btn_w - rl_w) / 2.0,
                    btn_y + (btn_h - bf) / 2.0,
                    "복원",
                    gpu::DrawOpts {
                        font_size: bf,
                        color: theme::fg(),
                        bold: true,
                        italic: false,
                    },
                );
                restore_btn_hits
                    .push((crate::RestoreBtn::Restore, (restore_x, btn_y, btn_w, btn_h)));
                // 새로 시작, to its left. 채움 대신 테두리로 갈린다 — 주액션과 갈리는 축이
                // 색 하나가 아니라 형태여야 흑백에서도 어느 쪽이 기본인지 읽힌다.
                let fresh_x = restore_x - btn_gap - btn_w;
                let fresh_hover = hit(fresh_x);
                g.hover_pointer |= fresh_hover;
                panel_rect_outlined(
                    g,
                    fresh_x,
                    btn_y,
                    btn_w,
                    btn_h,
                    theme::radius_sm(),
                    theme::raised_on(theme::surface_active(), fresh_hover),
                );
                let fl_w = g.measure_chrome_text("새로 시작", bf, false);
                g.draw_text(
                    fresh_x + (btn_w - fl_w) / 2.0,
                    btn_y + (btn_h - bf) / 2.0,
                    "새로 시작",
                    gpu::DrawOpts {
                        font_size: bf,
                        color: theme::text(),
                        bold: false,
                        italic: false,
                    },
                );
                restore_btn_hits.push((crate::RestoreBtn::Fresh, (fresh_x, btn_y, btn_w, btn_h)));
            }
            // Restoration stays visible without a scrim or keyboard trap.
            let mut restore_retry = None;
            let mut restore_toast = None;
            if restore_toast_visible {
                let win_w = win_px.0 / scale;
                let win_h = win_px.1 / scale;
                let progress = self.restore_progress.as_ref();
                let total = progress.map(|p| p.expected).unwrap_or_else(|| {
                    self.restore_applying.as_ref().map_or(0, |(state, _)| crate::App::count_panes(state))
                });
                let ready = progress.map_or(0, |p| p.ready);
                let failed = progress.is_some_and(|p| p.failure.is_some());
                let msg = "pane을 복원하는 중…";
                // 남은 것을 **하나씩** 세운다 — 전엔 첫 줄 하나뿐이라 무엇이 남았는지 몰랐다.
                let detail = progress.map(|p| p.pending_lines())
                    .unwrap_or_else(|| vec!["저장된 pane과 탭을 불러오는 중…".to_string()]);
                let layout = crate::restore_progress::toast_layout_for(win_w, win_h, restore_bottom_reserved, detail.len());
                let (x, y, card_w, card_h) = layout.card;
                let pad = 16.0_f32.min(card_w / 8.0);
                let width = (card_w - 2.0 * pad).max(1.0);
                let count = format!("{ready}/{total}");
                let count_w = g.measure_chrome_text(&count, 12.0, false);
                let title_width = (width - count_w - 12.0).max(1.0);
                let msg = crate::info::fit_text(g, msg, title_width, 14.0, true);
                let lines: Vec<String> = detail.iter()
                    .map(|line| crate::info::fit_text(g, line, width, 12.0, false)).collect();
                panel_rect_outlined(g, x, y, card_w, card_h, theme::radius_md() * 1.5, theme::surface_active());
                if card_h >= 52.0 && width >= count_w + 24.0 {
                    g.draw_text(x + pad, y + 14.0, &msg, gpu::DrawOpts {
                        font_size: 14.0, color: theme::text(), bold: true, italic: false,
                    });
                    g.draw_text(x + card_w - pad - count_w, y + 15.0, &count, gpu::DrawOpts {
                        font_size: 12.0, color: theme::with_alpha(theme::text(), 0xB0), bold: false, italic: false,
                    });
                }
                let show_retry = failed && card_h >= 120.0;
                let bar_y = if show_retry { layout.retry.1 - 10.0 } else { y + card_h - 18.0 };
                for (i, line) in lines.iter().enumerate() {
                    let line_y = y + 40.0 + i as f32 * 17.0;
                    if line_y + 14.0 > bar_y - 6.0 { break; }
                    g.draw_text(x + pad, line_y, line, gpu::DrawOpts {
                        font_size: 12.0, color: theme::with_alpha(theme::text(), 0xB0), bold: false, italic: false,
                    });
                }
                let fraction = if total == 0 { 0.0 } else { ready as f32 / total as f32 };
                if card_h >= 24.0 {
                    g.rect(x + pad, bar_y, width, 5.0, theme::surface());
                    g.rect(x + pad, bar_y, width * fraction.clamp(0.0, 1.0), 5.0, theme::text());
                }
                if show_retry {
                    let rect = layout.retry;
                    let (mx, my) = self.cursor_px;
                    let hover = mx >= rect.0 && mx <= rect.0 + rect.2 && my >= rect.1 && my <= rect.1 + rect.3;
                    g.hover_pointer |= hover;
                    panel_rect_outlined(g, rect.0, rect.1, rect.2, rect.3, theme::radius_md(), theme::raised_on(theme::surface(), hover));
                    let label = crate::info::fit_text(g, "다시 시도", (rect.2 - 16.0).max(1.0), 13.0, true);
                    let tw = g.measure_chrome_text(&label, 13.0, true);
                    g.draw_text(rect.0 + (rect.2 - tw) / 2.0, rect.1 + (rect.3 - 13.0) / 2.0, &label, gpu::DrawOpts {
                        font_size: 13.0, color: theme::text(), bold: true, italic: false,
                    });
                }
                restore_retry = show_retry.then_some(layout.retry);
                restore_toast = Some(layout.card);
            }
            self.restore_retry_rect = restore_retry;
            self.restore_toast_rect = restore_toast;
            // 계정 전환 확인 — 인라인 웹에서 누른 것은 웹이 그리므로 메인 몫만 본다.
            if let Some(p) = self.account_switch_confirm.as_ref() {
                if p.surface == crate::session::ConfirmSurface::Main {
                    account_confirm_hits = paint_account_switch_confirm(
                        g,
                        (win_px.0 / scale, win_px.1 / scale),
                        self.cursor_px,
                        p,
                    );
                }
            }
            // 학생 교체 확인 — 계정 카드와 같은 층. 둘이 동시에 뜰 일은 없다(둘 다
            // 모든 클릭을 삼키는 모달이라 하나가 떠 있으면 다른 진입점이 안 열린다).
            if let Some(p) = self.character_swap_confirm.as_ref() {
                swap_confirm_hits = paint_character_swap_confirm(
                    g,
                    (win_px.0 / scale, win_px.1 / scale),
                    self.cursor_px,
                    p,
                );
            }
            // File-tree drag ghost — a small pill trailing the cursor with the
            // dragged item's name, drawn last so it floats over everything.
            if let Some(drag) = self.file_tree.drag.as_ref() {
                if drag.active {
                    let name = drag
                        .path
                        .file_name()
                        .map(|n| nfc_hangul(&n.to_string_lossy()))
                        .unwrap_or_default();
                    let is_dir = self
                        .file_tree
                        .nodes
                        .iter()
                        .find(|n| n.path == drag.path)
                        .map(|n| n.is_dir)
                        .unwrap_or(false);
                    let (cx, cy) = self.cursor_px;
                    let gf = 12.0_f32;
                    let tw = g.measure_chrome_text(&name, gf, false);
                    let pill_w = 18.0 + tw + 16.0;
                    let pill_h = 22.0_f32;
                    let gx = cx + 12.0;
                    let gy = cy + 10.0;
                    round_rect(
                        g,
                        gx,
                        gy,
                        pill_w,
                        pill_h,
                        theme::radius_sm(),
                        theme::accent(),
                    );
                    round_rect(
                        g,
                        gx + 1.0,
                        gy + 1.0,
                        pill_w - 2.0,
                        pill_h - 2.0,
                        theme::radius_sm() - 1.0,
                        theme::with_alpha(theme::surface_active(), 0xF5),
                    );
                    g.queue_icon(
                        if is_dir { "folder" } else { "file" },
                        gx + 6.0,
                        gy + (pill_h - 14.0) / 2.0,
                        14.0,
                        theme::text(),
                    );
                    g.draw_text(
                        gx + 24.0,
                        gy + (pill_h - gf) / 2.0,
                        &name,
                        gpu::DrawOpts {
                            font_size: gf,
                            color: theme::text(),
                            bold: false,
                            italic: false,
                        },
                    );
                }
            }
            // Pane header drag ghost — 잡은 pane 이 커서를 따라오는 pill(파일트리
            // drag ghost 와 동일 방식). 사용자: pane 을 잡았을 때 "잡혔다"는 피드백이
            // 없어 마우스가 안 따라오는 느낌. 라벨은 display_pane_char(캐릭터 표시명,
            // 없으면 pane id). update_live_drag(라이브 재배치)와 별개의 최상단 층이라
            // 미리보기 무손상 — 커서가 사이드바로 나가 자리 프리뷰가 원위치로 돌아가도
            // 이 pill 은 계속 커서를 따라와 무엇을 어디로 옮기는지 보여준다.
            if let Some(label) = drag_label.as_deref() {
                let (cx, cy) = self.cursor_px;
                let gf = 12.0_f32;
                let tw = g.measure_chrome_text(label, gf, true);
                let pill_w = 14.0 + tw + 14.0;
                let pill_h = 22.0_f32;
                let gx = cx + 12.0;
                let gy = cy + 10.0;
                round_rect(
                    g,
                    gx,
                    gy,
                    pill_w,
                    pill_h,
                    theme::radius_sm(),
                    theme::accent(),
                );
                round_rect(
                    g,
                    gx + 1.0,
                    gy + 1.0,
                    pill_w - 2.0,
                    pill_h - 2.0,
                    theme::radius_sm() - 1.0,
                    theme::with_alpha(theme::surface_active(), 0xF5),
                );
                g.draw_text(
                    gx + 14.0,
                    gy + (pill_h - gf) / 2.0,
                    label,
                    gpu::DrawOpts {
                        font_size: gf,
                        color: theme::accent(),
                        bold: true,
                        italic: false,
                    },
                );
            }
            // 테마 전환 — 옛 배경색이 픽셀 블록으로 부서지며 걷힌다. 맨 마지막에
            // 그려 화면 전체(터미널·크롬·모달)를 한 장으로 덮는다.
            if let Some((at, old_bg)) = self.theme_fx {
                let t = at.elapsed().as_secs_f32() / THEME_FX_SECS;
                if t >= 1.0 {
                    self.theme_fx = None;
                } else {
                    paint_theme_dissolve(g, t, old_bg, win_px.0 / scale, win_px.1 / scale);
                }
            }
            // 방 펼침이 도는 동안은 다음 장을 스스로 부른다 — 사이드바는 입력이
            // 없으면 다시 안 그려지므로, 손을 뗀 자리에서 목록이 멈춰 버린다.
            if let Some((_, _, at)) = self.expand_anim {
                if at.elapsed().as_secs_f32() >= EXPAND_ANIM_SECS {
                    self.expand_anim = None;
                } else {
                    self.chrome_dirty = true;
                }
            }
            weather_spots = std::mem::take(&mut g.weather_spots);
            if weather_frame.is_none() {
                g.weather = None;
            }
            g.weather_frame = weather_frame;
            if let Err(e) = g.render(&slot_views, scale, time_secs, true) {
                eprintln!("[gpu] render error: {e:?}");
            }
        }
        self.weather_take_spots(weather_spots);
        if let Some(output) = settings_paint {
            self.finish_native_settings_paint(output);
        }
        if let Some(p) = self.account_switch_confirm.as_mut() {
            if !account_confirm_hits.is_empty() {
                p.rects = account_confirm_hits;
            }
        }
        if let Some(p) = self.character_swap_confirm.as_mut() {
            if !swap_confirm_hits.is_empty() {
                p.rects = swap_confirm_hits;
            }
        }
        self.confirm_btn_rects = confirm_btn_hits;
        self.restore_btn_rects = restore_btn_hits;
        self.pane_tab_rects = tab_hits;
        self.pane_tab_close_rects = tab_close_hits;
        self.pane_restart_chip_rects = restart_chip_hits;
        self.pane_plus_rects = plus_hits;
        // Tab-windowing write-back: clamped first + fit count for the wheel
        // handler, and this frame's active tab for the next reveal check.
        // No dirty flip — this must not schedule another frame.
        if let Ok(mut ws) = self.ws.lock() {
            for (id, first, vis, act) in &pane_tab_windowing {
                if let Some(p) = ws.panes.get_mut(id) {
                    p.tab_first = *first;
                    p.tab_vis = *vis;
                    p.tab_last_active = *act;
                }
            }
        }
        self.image_btn_rects = image_btn_hits;
        self.pane_action_hits = pane_action_hits;
        // body_rects collected per pane in case future overlays need them.
        let _ = body_rects;
        // Damage flags get cleared here (parity with sugarloaf path
        // below) so successive frames short-circuit on idle.
        if let Ok(mut ws) = self.ws.lock() {
            for pane in ws.panes.values_mut() {
                pane.dirty = false;
            }
        }
        self.chrome_dirty = false;
        // A bake that found no room during this frame left blank cells behind.
        // The repack happens at the top of the next frame — but an idle app
        // paints no next frame, so the blanks would just sit there. Ask for it.
        if self
            .gpu
            .as_ref()
            .is_some_and(|g| g.atlas_needs_another_frame())
        {
            self.chrome_dirty = true;
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
        // Keep the frame loop alive while a git op spins, so the spinner
        // animates until GitOpDone clears it.
        if self.git.op.is_some() {
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
    }

    /// Find/replace bar, floated over the top-right of a raw editor's body box
    /// (`x`/`y`/`w`). Returns its clickable rects in logical px.
    ///
    /// 호버 툴팁 — 마우스 아래에 뜨는 작은 상자. rust-analyzer 는 타입에 문서
    /// 전체를 붙여 주기도 해서 가로·세로 둘 다 자른다: 화면 절반을 덮는 툴팁은
    /// 정보가 아니라 방해다.
    /// `[Image #N]` 글자 옆에 그 그림의 썸네일을 띄운다.
    ///
    /// 텍스처 키는 픽셀 버퍼의 주소라, 툴팁이 다른 그림으로 바뀌면 키도 바뀐다 —
    /// 앞 키를 놓지 않으면 호버할 때마다 GPU 메모리가 는다. 그래서 툴팁이 없는
    /// 프레임(`tip` = `None`)에도 불러 정리할 기회를 준다.
    fn paint_image_tip(
        g: &mut gpu::GpuRenderer,
        tip: Option<((Arc<Vec<u8>>, u32, u32), (f32, f32, f32, f32))>,
        win_w: f32,
        win_h: f32,
    ) {
        thread_local! {
            static LAST_KEY: std::cell::RefCell<Option<String>> =
                const { std::cell::RefCell::new(None) };
        }
        let Some(((rgba, iw, ih), (ax, ay, _aw, ah))) = tip else {
            LAST_KEY.with(|l| {
                if let Some(old) = l.borrow_mut().take() {
                    g.drop_image(&old);
                }
            });
            return;
        };
        if iw == 0 || ih == 0 {
            return;
        }
        let key = format!("imgtip:{:x}:{}", Arc::as_ptr(&rgba) as usize, rgba.len());
        LAST_KEY.with(|l| {
            let mut l = l.borrow_mut();
            if l.as_deref() != Some(key.as_str()) {
                if let Some(old) = l.take() {
                    g.drop_image(&old);
                }
                g.upload_image(&key, &rgba, iw, ih);
                *l = Some(key.clone());
            }
        });
        // 액자. 확대는 하지 않는다 — 작은 그림을 늘리면 흐려지기만 한다.
        const MAX_W: f32 = 320.0;
        const PAD: f32 = 5.0;
        let s = (MAX_W / iw as f32)
            .min((win_h * 0.5).max(120.0) / ih as f32)
            .min(1.0);
        let (dw, dh) = (iw as f32 * s, ih as f32 * s);
        let (w, h) = (dw + PAD * 2.0, dh + PAD * 2.0);
        // 글자 바로 아래가 기본 — 그 위는 방금 읽은 프롬프트라 덮으면 안 된다.
        // 아래가 모자라면 위로 뒤집고, 오른쪽으로 넘치면 왼쪽으로 민다.
        let x = ax.min(win_w - w - 4.0).max(4.0);
        let y = if ay + ah + 6.0 + h < win_h {
            ay + ah + 6.0
        } else {
            (ay - 6.0 - h).max(4.0)
        };
        let r = theme::radius_sm();
        // 뒷판을 한 겹 넓게 깔아 터미널 글자 위에서 액자가 떠 보이게 한다 —
        // 이 렌더러엔 그림자가 없다.
        g.round_rect_fill(
            x - 2.0,
            y - 1.0,
            w + 4.0,
            h + 5.0,
            r + 2.0,
            theme::with_alpha(theme::bg(), 0x66),
        );
        g.round_rect_fill(x, y, w, h, r, theme::surface());
        let edge = theme::border();
        g.rect(x, y, w, 1.0, edge);
        g.rect(x, y + h - 1.0, w, 1.0, edge);
        g.rect(x, y, 1.0, h, edge);
        g.rect(x + w - 1.0, y, 1.0, h, edge);
        // icon 패스라 방금 깐 액자 위에 온다.
        g.queue_image_above(&key, x + PAD, y + PAD, dw, dh);
    }

    fn draw_hover_tip(
        g: &mut gpu::GpuRenderer,
        text: &str,
        mx: f32,
        my: f32,
        win_w: f32,
        win_h: f32,
    ) {
        const MAX_LINES: usize = 10;
        const PAD: f32 = 7.0;
        let (_, lh0) = g.raw_editor_metrics();
        let size = lh0 / 1.25 * 0.92;
        let lh = size * 1.35;
        let max_w = (win_w - PAD * 2.0 - 8.0).min(600.0);
        let max_lines = (((win_h - PAD * 2.0 - 8.0) / lh).floor().max(0.0) as usize).min(MAX_LINES);
        if max_lines == 0 || max_w < g.measure_pen_run("…", size, false, false) {
            return;
        }
        let mut lines = Vec::new();
        let mut line = String::new();
        let mut truncated = false;
        for ch in text.trim().chars() {
            let next = format!("{line}{ch}");
            if ch == '\n' || (!line.is_empty() && g.measure_pen_run(&next, size, false, false) > max_w) {
                lines.push(std::mem::take(&mut line));
                if lines.len() == max_lines {
                    truncated = true;
                    break;
                }
            }
            if ch != '\n' { line.push(ch); }
        }
        if !line.is_empty() { lines.push(line); }
        if truncated {
            if let Some(last) = lines.last_mut() {
                while !last.is_empty() && g.measure_pen_run(&format!("{last}…"), size, false, false) > max_w {
                    last.pop();
                }
                last.push('…');
            }
        }
        if lines.is_empty() {
            return;
        }
        let tw = lines
            .iter()
            .map(|l| g.measure_pen_run(l, size, false, false))
            .fold(0.0f32, f32::max);
        let w = tw.min(max_w) + PAD * 2.0;
        let h = lines.len() as f32 * lh + PAD * 2.0;
        // 마우스 아래가 기본. 아래가 모자라면 위로 뒤집고, 오른쪽으로 넘치면
        // 왼쪽으로 민다 — 창 밖으로 나간 툴팁은 그리나 마나다.
        let x = (mx + 12.0).min(win_w - w - 4.0).max(4.0);
        let y = if my + 18.0 + h < win_h {
            my + 18.0
        } else {
            (my - 12.0 - h).max(4.0)
        };
        g.rect(x, y, w, h, theme::surface());
        let edge = theme::border();
        g.rect(x, y, w, 1.0, edge);
        g.rect(x, y + h - 1.0, w, 1.0, edge);
        g.rect(x, y, 1.0, h, edge);
        g.rect(x + w - 1.0, y, 1.0, h, edge);
        g.push_clip(x + PAD, y + PAD, w - PAD * 2.0, h - PAD * 2.0);
        for (i, l) in lines.iter().enumerate() {
            g.draw_text(
                x + PAD,
                y + PAD + i as f32 * lh,
                l,
                gpu::DrawOpts {
                    font_size: size,
                    color: theme::text(),
                    bold: false,
                    italic: false,
                },
            );
        }
        g.pop_clip();
    }

    /// It overlays rather than pushing the text down, so opening it never
    /// reflows what you were reading — the same reason VS Code floats its own.
    pub(crate) fn draw_find_bar(
        g: &mut gpu::GpuRenderer,
        f: &FindState,
        x: f32,
        y: f32,
        w: f32,
        preedit: &str,
        caret_on: bool,
        cursor: (f32, f32),
    ) -> Vec<(FindBtn, (f32, f32, f32, f32))> {
        Self::draw_find_bar_with_options(g, f, x, y, w, preedit, caret_on, cursor, true, false)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_find_bar_with_options(
        g: &mut gpu::GpuRenderer,
        f: &FindState,
        x: f32,
        y: f32,
        w: f32,
        preedit: &str,
        caret_on: bool,
        cursor: (f32, f32),
        allow_replace: bool,
        polished_group: bool,
    ) -> Vec<(FindBtn, (f32, f32, f32, f32))> {
        const PAD: f32 = 8.0;
        const ROW: f32 = 26.0;
        const BTN: f32 = 24.0;
        const COUNT_W: f32 = 58.0;
        const TOGGLE_W: f32 = 16.0;
        const FS: f32 = 12.0;
        let mut hits = Vec::new();

        let toggle_slot = if allow_replace { TOGGLE_W + 6.0 } else { 0.0 };
        let nav_gap = if polished_group { 4.0 } else { 0.0 };
        let nav_w = BTN * 3.0 + nav_gap * 2.0;
        let fixed_w = PAD * 2.0 + toggle_slot + COUNT_W + 6.0 + nav_w;
        let field_w = ((w - 20.0).max(1.0) - fixed_w).clamp(72.0, 190.0);
        let bar_w = fixed_w + field_w;
        let rows = if allow_replace && f.replacing { 2.0 } else { 1.0 };
        let bar_h = PAD + ROW * rows + (rows - 1.0) * 4.0 + PAD;
        let x0 = (x + w - bar_w - 10.0).max(x + 4.0);
        let y0 = y + 6.0;
        // 얇은 테두리는 바깥에 한 겹 더 그려서 낸다 — 편집기 본문 위에 뜨는
        // 물건이라 경계가 없으면 글자에 파묻힌다.
        round_rect(
            g,
            x0 - 1.0,
            y0 - 1.0,
            bar_w + 2.0,
            bar_h + 2.0,
            theme::radius_md() + 1.0,
            theme::border(),
        );
        round_rect(
            g,
            x0,
            y0,
            bar_w,
            bar_h,
            theme::radius_md(),
            theme::surface(),
        );

        let text_baseline = |row_y: f32| row_y + (ROW - FS) * 0.5 - 1.0;
        let row1 = y0 + PAD;

        let hot = |r: (f32, f32, f32, f32)| {
            cursor.0 >= r.0 && cursor.0 <= r.0 + r.2 && cursor.1 >= r.1 && cursor.1 <= r.1 + r.3
        };

        // 바꾸기 행 펼침/접기.
        if allow_replace {
            let tg = (x0 + PAD, row1 + (ROW - TOGGLE_W) * 0.5, TOGGLE_W, TOGGLE_W);
            let tg_hit = (tg.0 - 2.0, row1, TOGGLE_W + 4.0, ROW);
            let tg_hov = hot(tg_hit);
            if tg_hov {
                hover_rect(
                    g,
                    tg_hit.0,
                    tg_hit.1,
                    tg_hit.2,
                    tg_hit.3,
                    theme::radius_sm(),
                );
            }
            g.queue_icon(
                if f.replacing {
                    "chevron-down"
                } else {
                    "chevron-right"
                },
                tg.0,
                tg.1,
                TOGGLE_W,
                if tg_hov {
                    theme::text()
                } else {
                    theme::text_dim()
                },
            );
            hits.push((FindBtn::ToggleReplace, tg_hit));
        }

        // 입력칸 하나를 그린다. 넘치는 글자는 왼쪽으로 밀어 끝(캐럿 쪽)을
        // 보여 준다 — 앞머리만 남으면 지금 뭘 치고 있는지 안 보인다.
        let field_x = x0 + PAD + toggle_slot;
        let field = |g: &mut gpu::GpuRenderer,
                     row_y: f32,
                     width: f32,
                     text: &str,
                     placeholder: &str,
                     focused: bool,
                     pe: &str| {
            round_rect(
                g,
                field_x,
                row_y,
                width,
                ROW,
                theme::radius_sm(),
                theme::bg(),
            );
            if focused {
                round_rect(
                    g,
                    field_x,
                    row_y,
                    width,
                    ROW,
                    theme::radius_sm(),
                    theme::with_alpha(theme::accent(), 0x22),
                );
            }
            let inner_l = field_x + 7.0;
            let inner_r = field_x + width - 7.0;
            let by = text_baseline(row_y);
            if text.is_empty() && pe.is_empty() {
                g.draw_text(
                    inner_l,
                    by,
                    placeholder,
                    gpu::DrawOpts {
                        font_size: FS,
                        color: theme::text_mute(),
                        bold: false,
                        italic: false,
                    },
                );
                if focused && caret_on {
                    g.rect(inner_l, row_y + 5.0, 1.5, ROW - 10.0, theme::accent());
                }
                return;
            }
            let tw = g.measure_chrome_text(text, FS, false)
                + if pe.is_empty() {
                    0.0
                } else {
                    g.measure_chrome_text(pe, FS, false)
                };
            let shift = (tw - (inner_r - inner_l)).max(0.0);
            let mut pen = g.draw_text_clipped(
                inner_l - shift,
                by,
                text,
                gpu::DrawOpts {
                    font_size: FS,
                    color: theme::text(),
                    bold: false,
                    italic: false,
                },
                inner_l,
                inner_r,
            );
            if !pe.is_empty() {
                let pw = g.measure_chrome_text(pe, FS, false);
                pen = g.draw_text_clipped(
                    pen,
                    by,
                    pe,
                    gpu::DrawOpts {
                        font_size: FS,
                        color: theme::text(),
                        bold: false,
                        italic: false,
                    },
                    inner_l,
                    inner_r,
                );
                g.rect(pen - pw, row_y + ROW - 6.0, pw, 1.5, theme::accent());
            }
            if focused && caret_on && pen <= inner_r {
                g.rect(pen + 1.0, row_y + 5.0, 1.5, ROW - 10.0, theme::accent());
            }
        };

        field(
            g,
            row1,
            field_w,
            &f.query,
            "찾기",
            !f.focus_replace,
            preedit,
        );

        // n/m — 검색어가 있는데 0건이면 빨갛게. 빈 검색어는 아무 말도 안 한다.
        let count = if f.query.is_empty() {
            String::new()
        } else if f.hits.is_empty() {
            "결과 없음".to_string()
        } else {
            format!("{}/{}", f.idx + 1, f.hits.len())
        };
        if !count.is_empty() {
            let col = if f.hits.is_empty() {
                theme::danger()
            } else {
                theme::text_dim()
            };
            let cw = g.measure_chrome_text(&count, FS, false);
            let count_x = if polished_group {
                field_x + field_w + (COUNT_W - cw) * 0.5
            } else {
                field_x + field_w + COUNT_W - 8.0 - cw
            };
            g.draw_text(
                count_x,
                text_baseline(row1),
                &count,
                gpu::DrawOpts {
                    font_size: FS,
                    color: col,
                    bold: false,
                    italic: false,
                },
            );
        }

        let btn_x = field_x + field_w + COUNT_W + 6.0;
        for (i, (icon, btn)) in [
            ("chevron-up", FindBtn::Prev),
            ("chevron-down", FindBtn::Next),
            ("x", FindBtn::Close),
        ]
        .into_iter()
        .enumerate()
        {
            let bx = btn_x + (BTN + nav_gap) * i as f32;
            let dim = f.hits.is_empty() && btn != FindBtn::Close;
            let hov = hot((bx, row1, BTN, ROW));
            if hov {
                hover_rect(g, bx, row1, BTN, ROW, theme::radius_sm());
            }
            g.queue_icon(
                icon,
                bx + (BTN - 14.0) * 0.5,
                row1 + (ROW - 14.0) * 0.5,
                14.0,
                match (hov, dim) {
                    (true, _) => theme::text(),
                    (_, true) => theme::text_mute(),
                    _ => theme::text_dim(),
                },
            );
            hits.push((btn, (bx, row1, BTN, ROW)));
        }

        if allow_replace && f.replacing {
            let row2 = row1 + ROW + 4.0;
            field(g, row2, field_w, &f.replace, "바꾸기", f.focus_replace, "");
            let mut lx = field_x + field_w + 6.0;
            for (label, btn) in [
                ("바꾸기", FindBtn::ReplaceOne),
                ("전부", FindBtn::ReplaceAll),
            ] {
                let lw = g.measure_chrome_text(label, FS, false) + 14.0;
                let hov = hot((lx, row2, lw, ROW));
                if hov {
                    g.hover_pointer = true;
                }
                round_rect(
                    g,
                    lx,
                    row2 + 2.0,
                    lw,
                    ROW - 4.0,
                    theme::radius_sm(),
                    if hov {
                        theme::surface_active()
                    } else {
                        theme::surface_hover()
                    },
                );
                g.draw_text(
                    lx + 7.0,
                    text_baseline(row2),
                    label,
                    gpu::DrawOpts {
                        font_size: FS,
                        color: if hov {
                            theme::text()
                        } else {
                            theme::text_dim()
                        },
                        bold: false,
                        italic: false,
                    },
                );
                hits.push((btn, (lx, row2, lw, ROW)));
                lx += lw + 5.0;
            }
        }
        hits
    }

    /// PTY 출력·redraw 요청처럼 몰려오는 자리의 그리기. 직전 그림이 한 주기 안이면 이번 것을
    /// 주기 끝으로 미뤄 한 장으로 합친다(`frame_pace`). 사람이 한 동작 뒤의 그림·캡처·하네스는
    /// `render_frame` 을 그대로 불러 미루지 않는다.
    pub(crate) fn render_frame_paced(&mut self) {
        if let (Some(w), Some(g)) = (self.window.as_ref(), self.gpu.as_ref()) {
            let size = w.inner_size();
            // 크기가 바뀐 그림은 미루지 않는다 — 창을 끄는 동안엔 그 자리에서 그려야 늘어난
            // 옛 그림이 안 비친다. 캡처가 걸린 그림도 그 순간의 화면이어야 한다.
            if g.surface_size() == (size.width, size.height)
                && g.capture_next.is_none()
                && crate::frame_pace::hold(w, &self.proxy)
            {
                return;
            }
        }
        self.render_frame();
    }

    pub(crate) fn render_frame(&mut self) {
        // 유휴인데 프레임이 나가는지를 **숫자로** 본다(`KASATERM_PUMP_DEBUG=1`).
        // 펌프 사유는 그 옆 계측이 찍지만, 사유가 참인 것과 실제로 몇 장이
        // 나가는지는 다른 얘기다 — 최적화 전후를 견주려면 장수가 필요하다.
        if std::env::var_os("KASATERM_PUMP_DEBUG").is_some() {
            thread_local! {
                static FPS: std::cell::RefCell<(u32, Option<std::time::Instant>)> =
                    const { std::cell::RefCell::new((0, None)) };
            }
            FPS.with(|c| {
                let mut c = c.borrow_mut();
                c.0 += 1;
                let at = c.1.get_or_insert_with(std::time::Instant::now);
                if at.elapsed() >= std::time::Duration::from_secs(1) {
                    eprintln!("[fps] {}", c.0);
                    *c = (0, Some(std::time::Instant::now()));
                }
            });
        }
        self.probe_pane_labels();
        // commit_overlay's job ends the moment the echo lands and moves
        // the cursor. Retire it permanently then — otherwise erasing
        // back to the commit position re-satisfies `cursor == stored`
        // and the stale "안" reappears.
        if let Some((before, owner)) = self
            .commit_overlay
            .as_ref()
            .map(|(_, b, owner)| (*b, owner.clone()))
        {
            let active_surface = self.target_surface();
            let cur = self.ws.lock().ok().and_then(|ws| {
                ws.active_pane.clone().and_then(|id| {
                    ws.panes
                        .get(&id)
                        .and_then(|p| p.term())
                        .map(|t| (t.cursor_row, t.cursor_col))
                })
            });
            if active_surface.as_deref() != Some(owner.as_str()) || cur != Some(before) {
                self.commit_overlay = None;
            }
        }
        let t0 = Instant::now();
        let trace = std::env::var_os("KASATERM_PROFILE").is_some();
        let now = Instant::now();
        let blink_on = self.cursor_blink_on(now);
        // Damage gate: skip the GPU pass when nothing changed since
        // the last frame. winit keeps showing the previous swapchain
        // image, so the user sees the same picture without us
        // emitting 10k+ sugarloaf calls. PTY updates flag the per-
        // pane dirty bit; chrome events flag `self.chrome_dirty`;
        // cursor blink phase toggles count separately.
        // 대화 보기는 기록이 새로 오면 그려야 한다 — 격자가 그대로여도 게이트를 연다.
        self.pump_chat_views();
        self.pump_shell_views();
        let blink_changed = blink_on != self.last_blink_on;
        // 보이는 pane 으로 한정한다 — 안 보이는 방의 pane 이 dirty 여도 그릴 그림이
        // 없는데, 전에는 그 하나가 프레임을 통째로 불렀다. 방마다 claude 를 띄우면
        // 다른 방의 스트리밍이 지금 보는 방의 프레임을 계속 태운다(2026-08-13).
        // `visible_pane_ids` 가 ws 락을 잡으므로 아래 락보다 **먼저** 부른다.
        let visible_panes = self.visible_pane_ids();
        let pty_dirty = self
            .ws
            .lock()
            .unwrap()
            .panes
            .iter()
            .any(|(id, p)| p.dirty && visible_panes.contains(id));
        // The launch banner fade is its own animation source: while it's
        // still visible the picture changes every frame, so force the GPU
        // pass even when panes are clean (about_to_wait re-arms WaitUntil
        // to keep waking us through the fade).
        let version_animating = self.version_alpha() > 0.0;
        // Same for the toast: its slide and fade change the picture every frame.
        let toast_animating = self.collab_toast_animating();
        // A busy pane's header bar sweeps every frame, so it's an animation
        // source too — keep painting while any pane is working.
        //
        // 단 **보이는** pane 만 센다. 그 바는 pane 헤더에 그려지므로 다른 방의 pane 은
        // 아무리 바빠도 화면에 없다. 좁히지 않으면 claude 를 여러 방에 띄운 것만으로
        // 상시 애니메이션 모드가 되어, 유휴여도 30fps 로 9~11ms 프레임을 계속 갈았다.
        // (사이드바에 뜨는 다른 방의 상태 표시는 정적이고, 깜빡이는 것들은
        // `window_alert`·`needs_you` 가 따로 펌프를 건다 — 여기서 좁혀도 안 멈춘다.)
        let bar_animating = self.pane_activity.iter().any(|(id, a)| {
            (a.state.is_busy() || (a.state.needs_you() && !self.blink_quiet.contains(id)))
                && visible_panes.contains(id)
        });
        // Split "needs a full chrome+grid rebuild" from "only the working-bar
        // sweep advances". A bar-only frame redraws cached chrome with a fresh
        // GPU time uniform — no clear_chrome, no per-pane grid clone, no draw-
        // list rebuild — so a busy pane no longer pins the CPU at 30fps.
        // A running git op spins a button spinner every frame.
        let git_op_animating = self.git.op.is_some();
        // 학생 도트 배너(Clawd 자리)가 보이는 동안은 idle 애니가 그림을
        // 바꾼다 — 전용 타이머(handler.rs)가 깨운 redraw 를 여기서
        // 통과시켜야 프레임이 넘어간다.
        let banner_animating = STUDENT_SPRITE_ANIMATING.load(std::sync::atomic::Ordering::Relaxed);
        // 사이드바의 걷는 학생 — 70ms 타이머(handler.rs)가 깨운 redraw 는
        // `chrome_dirty` 를 세우지 않으므로 여기서 직접 통과시켜야 걸음이 넘어간다.
        let walk_animating = STUDENT_WALK_ANIMATING.load(std::sync::atomic::Ordering::Relaxed);
        // ultracode 혜성은 셀 그리드(`composed`) 위에 얹혀 66ms 마다 위상이 바뀐다.
        // 그런데 이 게이트에 그 사유가 없어서, claude 가 idle 이면 통과하는 게 커서
        // blink(530ms) 뿐이었다 — 혜성이 프레임당 2.8셀이 아니라 **22셀씩** 튀어
        // 「흐르는 빛」이 아니라 순간이동으로 보였고, `KASATERM_NOBLINK=1` 이면 아예
        // 멈췄다. 66ms 타이머(handler.rs)가 깨운 redraw 는 `chrome_dirty` 를 세우지
        // 않으므로 여기서 직접 통과시켜야 한다.
        //
        // ⚠️`ULTRA_COMET_ANIMATING` 원자값을 쓰면 안 된다 — 그건 「어느 방에든
        // ultracode pane 이 하나라도 있으면 true」라, 위 `pty_dirty`·`bar_animating`
        // 을 보이는 pane 으로 좁힌 것을 통째로 되돌린다(다른 방의 ultracode 하나가
        // 지금 보는 방을 상시 15fps 로 태운다). 이미 잡아 둔 `visible_panes` 로
        // 좁힌다 — `pane_ultracode` 의 키는 `pane_claude_sid` 의 키(pane/pty id)라
        // `visible_pane_ids()` 와 같은 네임스페이스다(보조 탭도 접혀 들어온다).
        let comet_animating = self
            .pane_ultracode
            .iter()
            .any(|id| visible_panes.contains(id));
        let rebuild = pty_dirty
            || self.chrome_dirty
            || blink_changed
            // 계정 전환 반짝임 — 펌프가 깨워도 여기 사유가 없으면 GPU 패스를 건너뛰어
            // 커서 blink(530ms)에나 얹혀 그려진다(혜성이 순간이동으로 보이던 그 함정).
            || self.account_flash_factor().is_some()
            || version_animating
            || toast_animating
            || git_op_animating
            || banner_animating
            || walk_animating
            // 혜성은 그리드에 얹히므로 `bar_animating` 처럼 bar-only 경로로 두면 안 된다
            // — 전체 프레임을 다시 그려야 위상이 반영된다.
            || comet_animating;
        if !rebuild && !bar_animating {
            self.weather_only_frame();
            return;
        }
        self.last_blink_on = blink_on;
        if self.window.is_none() {
            return;
        }
        let scale = self.effective_scale();
        // Self-heal: if the GPU renderer's internal scale drifted from the
        // window's effective scale, every logical→physical mapping is off by
        // that ratio and the whole frame (chrome included) compresses into a
        // corner. This happens whenever a DPI change reaches the renderer
        // without a matching set_scale (a ScaleFactorChanged we didn't fully
        // apply, sleep/wake, clamshell). Re-sync once before drawing so a bad
        // frame fixes itself on the very next paint instead of staying broken.
        let drifted = self
            .gpu
            .as_ref()
            .map_or(false, |g| (g.scale() - scale).abs() > 0.001);
        if drifted {
            self.apply_effective_scale();
        }
        // 같은 이유로 **크기**도 자가치유한다. 지금까지 scale 만 되잡았는데,
        // 어긋날 수 있는 건 둘이고 크기 쪽은 한 번 틀어지면 되돌릴 장치가
        // 아예 없었다(실측: 스왑체인만 절반으로 만들어 두면 6초 뒤에도 그대로).
        //
        // 순서가 중요하다 — 뷰부터 창에 되맞춘다. 뷰가 작아지면 레이어도
        // `inner_size()` 도 스왑체인도 같이 작아져 앱 내부에선 완벽히 일관돼
        // 보이고, 어긋난 건 창과 뷰 사이뿐이라 크기 대조로는 안 잡힌다.
        // 정상 상태에선 둘 다 msg_send 몇 번·정수 비교 두 번이라 사실상 공짜다.
        // 어긋날 수 있는 자리는 둘이 아니라 셋이었다. 뷰도 스왑체인도 창과
        // 맞는데 **레이어의 backing scale 만** 옛 화면에 머무는 상태가 있고,
        // 그때 두 불변식은 나란히 "이상 없음" 이라 답한다 — 39번 수정이
        // 모니터 이동을 못 잡은 이유가 이 침묵이었다.
        if let Some(w) = self.window.clone() {
            let refit = gpu::ensure_view_fills_window(&w);
            let rescaled = gpu::ensure_layer_scale_matches(&w);
            let want = w.inner_size();
            let stale = self
                .gpu
                .as_ref()
                .map_or(false, |g| g.surface_size() != (want.width, want.height));
            // cs 를 고쳤으면 drawable 을 다시 잡아야 짝이 맞는다(`resize` 는
            // 같은 크기로 불러도 `surface.configure` 를 다시 태운다).
            if refit || rescaled || stale {
                if let Some(g) = self.gpu.as_mut() {
                    g.resize(want.width, want.height);
                }
                self.apply_effective_scale();
            }
        }
        // gpu path takes over the whole frame — no chrome yet, just
        // the cell grid through the cell-renderer pipeline.
        if self.gpu.is_some() {
            let time_secs = self.version_anim_start.elapsed().as_secs_f32();
            // (echo-stale 격리) bar-only 경로 임시 제거 — busy여도 항상 전체
            // render_frame_gpu로 cells를 다시 그려 echo가 stale되지 않게.
            let _ = rebuild;
            self.render_frame_gpu(scale, time_secs);
            crate::frame_pace::drew(t0);
            // 리드백은 render_frame_gpu 안에서 device.poll(Wait) 로 끝나므로 여기선
            // 파일이 이미 디스크에 있다. 회신을 여기 두는 이유가 그것 — 무장 시점에
            // 답하면 받는 쪽이 아직 없는 파일을 Read 한다.
            self.settle_pane_captures();
            if trace {
                eprintln!(
                    "[render-gpu] {}us since_input={}ms",
                    t0.elapsed().as_micros(),
                    now.saturating_duration_since(self.last_input_at)
                        .as_millis()
                );
            }
            return;
        }
    }
}

/// compact 진행 게이지 — 칸이 차오르는 눈금(2026-08-15 지시: 연속 띠는 얼마나
/// 남았는지 눈금이 없어 안 읽혔다). 찬 칸은 accent 원색, 빈 칸은 흐린 트랙.
/// 헤더 하단과 머리 없는 pane 상단이 같은 형태 언어를 쓰도록 한 손으로 그린다.
/// 반환: 게이지가 실제로 차지한 폭(숫자를 그 오른쪽에 얹을 때 쓴다).
fn draw_compact_cells(
    g: &mut gpu::GpuRenderer,
    x: f32,
    y: f32,
    w: f32,
    bar_h: f32,
    accent: [u8; 4],
    pct: u8,
) -> f32 {
    let seg_w = 7.0_f32;
    let gap = 2.0_f32;
    let n = (((w + gap) / (seg_w + gap)).floor() as usize).max(1);
    let filled = ((pct.min(100) as f32 / 100.0) * n as f32).round() as usize;
    for i in 0..n {
        let sx = x + i as f32 * (seg_w + gap);
        let col = if i < filled {
            accent
        } else {
            theme::with_alpha(accent, 0x2e)
        };
        g.rect(sx, y, seg_w, bar_h, col);
    }
    n as f32 * (seg_w + gap) - gap
}

/// 하단바에 적을 계정 이름. 라벨을 안 지은 슬롯은 이름이 이메일로 폴백되는데
/// 통째로 적으면 한 줄의 절반을 주소가 먹는다 — 그래서 `@` 앞만 남긴다.
///
/// **겹치면 안 줄인다.** 슬롯 둘이 같은 아이디에 다른 도메인이면
/// (`sampleuser@maila.example.test` · `sampleuser@mailb.example.test`) 화면에서 통째로 같은 글자가 되어,
/// 지금 어느 계정인지 이 자리로는 알 수가 없다(토키 실측 2026-08-15). 그때만
/// 도메인 앞머리를 붙여 가른다(`sampleuser·mailb`) — 짧은 채로 갈리는 것이 요점이라
/// 도메인 전체는 안 쓴다.
///
/// `others` 는 자기 자신을 포함해도 된다(같은 문자열은 겹침으로 안 센다).
pub(crate) fn statusbar_account_short(name: &str, others: &[String]) -> String {
    fn local(s: &str) -> &str {
        s.split_once('@').map(|(a, _)| a).unwrap_or(s)
    }
    let dup = others.iter().any(|o| o != name && local(o) == local(name));
    match name.split_once('@') {
        Some((a, d)) if dup => format!("{a}·{}", d.split('.').next().unwrap_or(d)),
        Some((a, _)) => a.to_string(),
        None => name.to_string(),
    }
}

/// Codex rollout이 마지막으로 보고한 한도 창. 임의의 구독제 이름을 붙이지 않고,
/// 로그가 준 시간 창만 짧게 보인다.
/// 코덱스 한도를 화면이 쓰는 모양으로. 값이 없으면 빈 목록이고, 그때 호출부는
/// **아무것도 안 그린다** — 코덱스를 안 쓰는 창에서 빈 칸이 자리만 먹는 것이
/// 이 줄이 가장 피해야 할 일이다.
fn codex_windows_for(id: &str) -> Option<Vec<(String, f32)>> {
    crate::codexlimits::snapshot_for(id).map(|limits| {
        limits
            .windows
            .iter()
            .map(|(minutes, pct, _)| (codex_rate_window_label(Some(*minutes)), *pct))
            .chain(limits.named_windows.iter().map(|window| {
                (
                    format!(
                        "{} {}",
                        codex_rate_window_label(Some(window.minutes)),
                        window.name
                    ),
                    window.pct,
                )
            }))
            .collect()
    })
}

fn selected_status_usage_windows(
    badge: Option<&crate::UsageBadge>,
    prefs: &crate::statusbar_config::Prefs,
    provider: &str,
    switching: bool,
) -> Vec<(String, Option<f32>)> {
    let Some(badge) = badge else { return Vec::new() };
    let source: Vec<(String, f32)> = if badge.windows.is_empty() {
        vec![(badge.label.clone(), badge.pct)]
    } else {
        badge
            .windows
            .iter()
            .map(|window| (window.label.clone(), window.pct))
            .collect()
    };
    source
        .into_iter()
        .filter(|(label, _)| {
            let field = if label.starts_with("5h") {
                "session"
            } else if label == "7d" {
                "weekly"
            } else {
                "model"
            };
            prefs.has_usage_field(provider, field)
        })
        .map(|(label, pct)| (label, (!switching).then_some(pct)))
        .collect()
}

fn selected_codex_windows(
    windows: &[(String, f32)],
    prefs: &crate::statusbar_config::Prefs,
) -> Vec<(String, f32)> {
    windows
        .iter()
        .filter(|(label, _)| {
            let field = if label == "5h" {
                "session"
            } else if label == "7d" {
                "weekly"
            } else {
                "model"
            };
            prefs.has_usage_field("codex", field)
        })
        .cloned()
        .collect()
}

fn missing_codex_windows(
    windows: &[(String, f32)],
    prefs: &crate::statusbar_config::Prefs,
) -> Vec<&'static str> {
    [("session", "5h"), ("weekly", "7d"), ("model", "모델별")]
        .into_iter()
        .filter(|(field, label)| {
            let present = if *field == "model" {
                windows
                    .iter()
                    .any(|(value, _)| value != "5h" && value != "7d")
            } else {
                windows.iter().any(|(value, _)| value == label)
            };
            prefs.has_usage_field("codex", field) && !present
        })
        .map(|(_, label)| label)
        .collect()
}

fn codex_statusbar_visible(win_w: f32, logged_in: bool, configured: bool) -> bool {
    win_w >= 720.0 && (logged_in || configured)
}

/// 하단바 도구 칩 하나를 그린 뒤 다음 칩이 설 오른쪽 끝. 칸(`allocated`)은 칩이 옆
/// 칩을 침범하지 않게 막는 **상한**일 뿐이다 — 칸 끝으로 건너뛰면 짧은 칩마다 남은
/// 칸이 빈 틈이 되어 줄이 띄엄띄엄해진다(2026-09-25, 1200 창에서 틈 71~97px).
/// 값이 없어 아무것도 안 그린 칩은 자리를 먹지 않는다.
fn statusbar_next_right(slot_right: f32, drawn_left: f32, allocated: f32) -> f32 {
    drawn_left.max(slot_right - allocated)
}

/// 하단바 칸의 상태. 글자색이 아니라 값 뒤의 점 하나가 말한다 — 칸마다 글자색·강조색·
/// 점을 섞어 쓰니 같은 「연결됨」이 칸마다 다르게 보였다(2026-09-29 「통일이 안 된 느낌」).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChipState {
    Ok,
    Warn,
    Bad,
    Todo,
    Off,
}

impl ChipState {
    fn color(self) -> [u8; 4] {
        match self {
            Self::Ok => theme::success(),
            Self::Warn => theme::attention(),
            Self::Bad => theme::danger(),
            Self::Todo => theme::accent(),
            Self::Off => theme::text_mute(),
        }
    }
}

const STATUS_DOT: f32 = 4.0;
const STATUS_DOT_GAP: f32 = 4.0;

/// 값 오른쪽 `x` 에서 틈을 두고 점을 찍는다. 먹은 폭(틈 포함)을 돌려준다.
fn status_dot(g: &mut gpu::GpuRenderer, x: f32, sy: f32, status_h: f32, state: ChipState) -> f32 {
    circle_rect(g, x + STATUS_DOT_GAP, sy + (status_h - STATUS_DOT) / 2.0, STATUS_DOT, state.color());
    STATUS_DOT_GAP + STATUS_DOT
}

/// 칸의 아이콘·글자색. 누른 칸(팝오버가 열린 칸)만 밝다 — 상태는 점이 따로 말한다.
fn chip_ink(prefs: &crate::statusbar_config::Prefs, id: &str, open: bool) -> [u8; 4] {
    prefs.color(id, if open { theme::text() } else { theme::text_dim() })
}

/// 직통 칸의 점 — 중계로 돌거나 느린(150ms↑) 기기가 하나라도 있으면 주의.
fn link_state(worst_tone: u8) -> ChipState {
    if worst_tone == 0 { ChipState::Warn } else { ChipState::Ok }
}

/// 하단바 도구 칩이 차지하는 칸 수. 도구가 아닌 것(계정)은 0.
fn statusbar_tool_weight(id: &str) -> f32 {
    match id {
        "tunnel" => 2.0,
        "resources" | "link" | "version" | "ports" | "schedules" | "pet" | "clipboard" => 1.0,
        _ => 0.0,
    }
}

/// 아직 안 그린 칩을 위해 남겨 둘 폭 — 직전 프레임에 실제로 쓴 폭, 처음 보는 칩은
/// 제 몫(`share`) 통째. 제 몫을 넘겨 잡지는 않는다.
fn statusbar_tool_reserve(last_used: Option<f32>, share: f32) -> f32 {
    last_used.map_or(share, |w| w.min(share))
}

/// 이번 칩이 쓸 수 있는 폭. 아직 안 그린 칩들 몫(`later_reserve`)만 남기고 나머지는
/// 이번 칩 차지다 — 숫자 하나짜리 칩에 칸을 통째로 남겨 두면 가운데가 빈 채로 기기
/// 이름·판 번호가 「…」로 잘린다. 제 몫(`share`)보다 적게 받는 칩은 없다.
fn statusbar_slot_cap(room: f32, share: f32, later_reserve: f32) -> f32 {
    (room - later_reserve).max(share)
}

fn codex_account_name(id: &str, accounts: &[crate::socket::CodexAccount]) -> String {
    match accounts.iter().position(|account| account.id == id) {
        Some(index) => crate::settings::codex_account_display(
            id,
            &accounts[index].label,
            &format!("계정 {}", index + 2),
        ),
        None => crate::settings::codex_account_display("", "", "기본 계정"),
    }
}

fn claude_account_label(id: &str, accounts: &[crate::socket::ClaudeAccount]) -> String {
    match accounts.iter().position(|account| account.id == id) {
        Some(index) => {
            let label = accounts[index].label.trim();
            if label.is_empty() {
                format!("계정 {}", index + 1)
            } else {
                label.to_string()
            }
        }
        None => "계정 선택 필요".to_string(),
    }
}

/// 하단의 `account` 항목은 별명만 말한다. `codex_account_display`는 별명과 이메일을
/// 합치므로 이메일 항목을 꺼도 주소가 새는 데다, 둘을 켜면 같은 주소가 두 번 선다.
fn codex_account_label(id: &str, accounts: &[crate::socket::CodexAccount]) -> String {
    match accounts.iter().position(|account| account.id == id) {
        Some(index) => {
            let label = accounts[index].label.trim();
            if label.is_empty() {
                format!("계정 {}", index + 2)
            } else {
                label.to_string()
            }
        }
        None => "기본 계정".to_string(),
    }
}

fn codex_rate_window_label(minutes: Option<u32>) -> String {
    match minutes {
        Some(0) | None => String::new(),
        Some(m) if m % (60 * 24) == 0 => format!("{}d", m / (60 * 24)),
        Some(m) if m % 60 == 0 => format!("{}h", m / 60),
        Some(m) => format!("{m}m"),
    }
}

/// 계정 메뉴의 최근 실행 설명. model/effort/mode는 Codex가 rollout에 직접 싣는
/// 값만 보이고, 빠진 필드는 추측하지 않는다.
fn codex_run_summary(s: &crate::transcript::CodexRolloutSnapshot) -> String {
    let mut parts = Vec::new();
    if !s.model.is_empty() {
        parts.push(s.model.clone());
    }
    if !s.effort.is_empty() {
        parts.push(format!("effort {}", s.effort));
    }
    if !s.collaboration_mode.is_empty() {
        parts.push(format!("mode {}", s.collaboration_mode));
    }
    if let Some(plan) = s.plan_type.as_deref().filter(|p| !p.is_empty()) {
        parts.push(plan.to_string());
    }
    parts.join(" · ")
}

/// Pane header는 모델명보다 지금 고른 추론 강도와 모드를 먼저 읽히게 한다.
fn codex_header_summary(s: &crate::transcript::CodexRolloutSnapshot) -> Option<String> {
    let mut parts = Vec::new();
    if !s.effort.is_empty() {
        parts.push(s.effort.clone());
    }
    if !s.collaboration_mode.is_empty() {
        parts.push(s.collaboration_mode.clone());
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_statusbar_does_not_depend_on_usage_success() {
        assert!(codex_statusbar_visible(720.0, true, false));
        assert!(codex_statusbar_visible(720.0, false, true));
        assert!(!codex_statusbar_visible(719.0, true, true));
        assert!(!codex_statusbar_visible(1200.0, false, false));
    }

    #[test]
    fn statusbar_tools_advance_by_drawn_width_not_slot() {
        // 88 칸에 30 짜리 칩: 다음 칩은 칩 바로 옆에 선다.
        assert_eq!(statusbar_next_right(1000.0, 970.0, 88.0), 970.0);
        // 아무것도 안 그린 칩은 자리를 안 먹는다.
        assert_eq!(statusbar_next_right(1000.0, 1000.0, 88.0), 1000.0);
        // 칸보다 넓게 그렸어도 칸 밖으로는 안 민다.
        assert_eq!(statusbar_next_right(1000.0, 880.0, 88.0), 912.0);
    }

    #[test]
    fn statusbar_tools_reserve_only_what_later_chips_used() {
        // 처음 보는 칩은 제 몫 통째 — 그때는 칸이 고르게 나뉠 때와 같다.
        assert_eq!(statusbar_tool_reserve(None, 100.0), 100.0);
        assert_eq!(statusbar_slot_cap(900.0, 100.0, 800.0), 100.0);
        // 숫자 칩이 30 만 썼으면 30 만 남겨 두고, 나머지는 판 번호가 쓴다.
        assert_eq!(statusbar_tool_reserve(Some(30.0), 100.0), 30.0);
        assert_eq!(statusbar_slot_cap(900.0, 100.0, 4.0 * 30.0), 780.0);
        // 제 몫보다 많이 썼어도(두 칸짜리 등) 예약은 제 몫까지만.
        assert_eq!(statusbar_tool_reserve(Some(250.0), 200.0), 200.0);
        // 자리가 모자라도 제 몫 밑으로는 안 내려간다.
        assert_eq!(statusbar_slot_cap(150.0, 100.0, 100.0), 100.0);
        assert_eq!(statusbar_tool_weight("tunnel"), 2.0);
        assert_eq!(statusbar_tool_weight("claude"), 0.0);
    }

    #[test]
    fn terminal_preedit_never_follows_focus_to_another_surface() {
        assert_eq!(
            terminal_preedit_for_active("한", Some("%tab-a"), Some("%tab-a")),
            "한"
        );
        assert_eq!(
            terminal_preedit_for_active("한", Some("%tab-a"), Some("%tab-b")),
            ""
        );
        assert_eq!(terminal_preedit_for_active("한", None, Some("%tab-b")), "");
    }

    #[test]
    fn statusbar_account_name_keeps_the_domain_only_when_slots_collide() {
        let alone = vec![
            "sampleuser@mailb.example.test".to_string(),
            "workuser@work.example.test".to_string(),
        ];
        assert_eq!(
            statusbar_account_short("sampleuser@mailb.example.test", &alone),
            "sampleuser"
        );

        let clash = vec![
            "sampleuser@maila.example.test".to_string(),
            "sampleuser@mailb.example.test".to_string(),
        ];
        assert_eq!(
            statusbar_account_short("sampleuser@mailb.example.test", &clash),
            "sampleuser·mailb"
        );
        assert_eq!(
            statusbar_account_short("sampleuser@maila.example.test", &clash),
            "sampleuser·maila"
        );

        // 사람이 지은 라벨엔 `@` 가 없다 — 손대지 않는다.
        assert_eq!(
            statusbar_account_short("사이오닉팀플랜", &clash),
            "사이오닉팀플랜"
        );
        // 라벨끼리 같아 보이는 경우도 붙일 도메인이 없으니 그대로 둔다.
        let same = vec!["기본".to_string(), "기본".to_string()];
        assert_eq!(statusbar_account_short("기본", &same), "기본");
    }

    #[test]
    fn codex_rollout_labels_keep_only_reported_values() {
        let s = crate::transcript::CodexRolloutSnapshot {
            model: "gpt-5.5".to_string(),
            effort: "high".to_string(),
            collaboration_mode: "default".to_string(),
            rate_used_pct: Some(62.5),
            rate_window_minutes: Some(300),
            rate_resets_at: None,
            plan_type: Some("plus".to_string()),
            ..Default::default()
        };
        assert_eq!(
            codex_run_summary(&s),
            "gpt-5.5 · effort high · mode default · plus"
        );
        assert_eq!(codex_header_summary(&s).as_deref(), Some("high · default"));

        let empty = crate::transcript::CodexRolloutSnapshot::default();
        assert_eq!(codex_run_summary(&empty), "");
        assert_eq!(codex_header_summary(&empty), None);
    }
}

/// 사용량 임계 색. 60/80 이 경계고 그 아래는 초록이 아니라 **중립**이다 — 초록은
/// 「좋다」는 신호라 늘 켜져 있으면 아무 말도 안 하는 색이 된다.
pub(crate) fn usage_pct_color(pct: f32) -> [u8; 4] {
    if pct >= 80.0 {
        theme::danger()
    } else if pct >= 60.0 {
        theme::syn_number()
    } else {
        theme::text_dim()
    }
}

/// 여유 구간의 막대는 강조색이다 — 설정의 토글·탭 색선과 같은 파랑이라 「지금
/// 어디까지 찼나」가 한 색으로 읽히고, 한도가 다가오면 노랑·빨강으로 갈아탄다
/// (플랫 정리 2026-09-14, 안 B).
pub(crate) fn usage_bar_color(pct: f32) -> [u8; 4] {
    if pct >= 80.0 {
        theme::danger()
    } else if pct >= 60.0 {
        theme::syn_number()
    } else {
        theme::accent()
    }
}

/// `[창 이름][막대][퍼센트]` 를 창마다 하나씩 왼쪽부터. 5시간이 앞이다 — 지금 당장
/// 막히는 건 그쪽이고 주간은 「이번 주가 어떻게 흘러가나」라 참고에 가깝다.
///
/// `right` 를 넘칠 것 같으면 **그 창을 아예 안 그린다.** 잘린 막대는 값을 잘못
/// 읽히게 하므로 없느니만 못하다. 그린 만큼의 오른쪽 끝을 돌려준다.
///
/// 막대는 트랙을 함께 그린다 — 채움만 있으면 15% 짜리가 어디까지 갈 수 있는
/// 것인지 알 수가 없어 그냥 얼룩이 된다.
/// 계정이 바뀐 자리를 잠깐 반짝인다 — 칩을 한 번 물들이고 둘레로 별이 퍼진다.
///
/// `k` 는 `1.0 → 0.0` 진행도. 입자 배치에 **난수를 쓰지 않는다**: 프레임마다 같은
/// 답이 나와야 별이 제자리에서 사그라든다(난수면 매 프레임 다른 곳에 튀어 지직거린다).
fn paint_account_flash(g: &mut gpu::GpuRenderer, r: (f32, f32, f32, f32), k: f32) {
    let k = k.clamp(0.0, 1.0);
    let (x, y, w, h) = r;
    // 칩 자체를 물들인다 — 별만 있으면 「무엇이」 바뀌었는지 안 짚어진다.
    round_rect(
        g,
        x,
        y,
        w,
        h,
        theme::radius_sm(),
        theme::with_alpha(theme::accent(), (k * 70.0) as u8),
    );
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    // 퍼짐은 빠르게 시작해 느려진다(1-k 를 제곱근으로) — 튀어나오는 인상이 난다.
    let spread = (1.0 - k).sqrt();
    // 세로로는 거의 안 퍼진다 — 이 칩이 앉는 하단바는 24px 라, 위아래로 퍼뜨리면
    // 별이 바 밖으로 잘려 반쪽만 보인다. 납작한 타원으로 옆으로만 번지게 한다.
    let rx = w / 2.0 + 6.0 + spread * 14.0;
    let ry = h / 2.0 + spread * 3.0;
    for i in 0..6 {
        let ang = (i as f32 / 6.0) * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
        // 별마다 조금씩 늦게 뜨고 늦게 진다 — 여섯이 한 몸으로 깜빡이면 별이
        // 아니라 테두리 한 겹으로 보인다.
        let lag = (i as f32) * 0.06;
        let a = ((k - lag) / (1.0 - lag).max(0.001)).clamp(0.0, 1.0);
        if a <= 0.01 {
            continue;
        }
        let size = 6.0 + a * 2.0;
        g.queue_icon(
            "sparkles",
            cx + ang.cos() * rx - size / 2.0,
            cy + ang.sin() * ry - size / 2.0,
            size,
            theme::with_alpha(theme::accent(), (a * 235.0) as u8),
        );
    }
}

/// 계정 전환 확인 카드 — 스크림 + 가운데 카드 + 버튼 둘. 히트박스를 돌려준다.
///
/// **자유함수인 이유**: 메인 창 안의 여러 계정 진입점이 같은 카드를 그린다.
/// 확인 카드 한 장 — 스크림 + 제목 + 본문 + 오른쪽 정렬 버튼들.
///
/// 계정 전환과 학생 교체가 같은 층에 같은 모양으로 뜬다. 그리는 몸통이 둘이면
/// 한쪽만 고쳐져 카드 두 장이 조용히 달라진다.
///
/// 버튼은 **오른쪽부터** 주어진 순서로 놓인다 — 첫 항목이 가장 오른쪽, 즉 기본
/// 행동이다. `tone` 이 있으면 채운 버튼, 없으면 테두리 버튼: 흑백에서도 어느 쪽이
/// 기본인지 형태로 읽혀야 한다.
pub(crate) fn paint_confirm_card<B: Copy>(
    g: &mut gpu::GpuRenderer,
    win: (f32, f32),
    cursor: (f32, f32),
    title: &str,
    lines: &[String],
    buttons: &[(&str, B, Option<[u8; 4]>)],
) -> Vec<(B, (f32, f32, f32, f32))> {
    let (win_w, win_h) = win;
    g.rect(
        0.0,
        0.0,
        win_w,
        win_h,
        theme::with_alpha([0, 0, 0, 255], 0xB0),
    );

    let pad = 24.0_f32;
    let line_h = 20.0_f32;
    let title_w = g.measure_chrome_text(title, 15.0, true);
    let body_w = lines
        .iter()
        .map(|l| g.measure_chrome_text(l, 13.0, false))
        .fold(0.0_f32, f32::max);
    let card_w = (title_w.max(body_w) + pad * 2.0).clamp(380.0, (win_w - 48.0).max(380.0));
    let card_h = 104.0 + lines.len() as f32 * line_h;
    let cx0 = ((win_w - card_w) / 2.0).round();
    let cy0 = ((win_h - card_h) / 2.0).round();
    panel_rect_outlined(
        g,
        cx0,
        cy0,
        card_w,
        card_h,
        theme::radius_md(),
        theme::surface_active(),
    );
    g.draw_text(
        cx0 + pad,
        cy0 + 28.0,
        title,
        gpu::DrawOpts {
            font_size: 15.0,
            color: theme::text(),
            bold: true,
            italic: false,
        },
    );
    for (i, l) in lines.iter().enumerate() {
        g.draw_text(
            cx0 + pad,
            cy0 + 56.0 + i as f32 * line_h,
            l,
            gpu::DrawOpts {
                font_size: 13.0,
                color: theme::text_dim(),
                bold: false,
                italic: false,
            },
        );
    }

    let (mx, my) = cursor;
    let bf = 13.0_f32;
    let bpad = 18.0_f32;
    let btn_h = 34.0_f32;
    let btn_y = cy0 + card_h - 20.0 - btn_h;
    let mut right = cx0 + card_w - pad;
    let mut hits: Vec<(B, (f32, f32, f32, f32))> = Vec::new();
    for (label, btn, tone) in buttons {
        let w = g.measure_chrome_text(label, bf, tone.is_some()) + bpad * 2.0;
        let x = right - w;
        right = x - 10.0;
        let hot = mx >= x && mx <= x + w && my >= btn_y && my <= btn_y + btn_h;
        g.hover_pointer |= hot;
        let (fill, fg, bold) = match tone {
            Some(c) => (
                theme::with_alpha(*c, if hot { 0xFF } else { 0xDD }),
                [0xFF, 0xFF, 0xFF, 0xFF],
                true,
            ),
            None => (
                theme::raised_on(theme::surface_active(), hot),
                theme::text(),
                false,
            ),
        };
        if tone.is_some() {
            panel_rect(g, x, btn_y, w, btn_h, theme::radius_sm(), fill);
        } else {
            panel_rect_outlined(g, x, btn_y, w, btn_h, theme::radius_sm(), fill);
        }
        g.draw_text(
            x + bpad,
            btn_y + (btn_h - bf) / 2.0,
            label,
            gpu::DrawOpts {
                font_size: bf,
                color: fg,
                bold,
                italic: false,
            },
        );
        hits.push((*btn, (x, btn_y, w, btn_h)));
    }
    hits
}

pub(crate) fn paint_account_switch_confirm(
    g: &mut gpu::GpuRenderer,
    win: (f32, f32),
    cursor: (f32, f32),
    p: &crate::session::PendingAccountSwitch,
) -> Vec<(crate::session::AccountSwitchBtn, (f32, f32, f32, f32))> {
    use crate::session::AccountSwitchBtn;
    let (title, lines) = crate::session::account_switch_confirm_text(&p.to_label, &p.impact);
    // 채움색이 곧 뜻이다 — 이어붙일 대화가 없는 pane 이 섞여 있으면 그 전환은
    // 되돌릴 수 없으므로 빨강. 아니면 대화가 이어지니 기본 accent.
    let tone = if p.impact.fresh > 0 {
        theme::danger()
    } else {
        theme::accent()
    };
    paint_confirm_card(
        g,
        win,
        cursor,
        &title,
        &lines,
        &[
            ("전환", AccountSwitchBtn::Switch, Some(tone)),
            ("취소", AccountSwitchBtn::Cancel, None),
        ],
    )
}

/// 학생 교체 확인 — 계정 전환과 같은 층·같은 모양이다.
pub(crate) fn paint_character_swap_confirm(
    g: &mut gpu::GpuRenderer,
    win: (f32, f32),
    cursor: (f32, f32),
    p: &crate::session::PendingCharacterSwap,
) -> Vec<(crate::session::CharacterSwapBtn, (f32, f32, f32, f32))> {
    use crate::session::CharacterSwapBtn;
    let (title, lines) = crate::session::character_swap_confirm_text(&p.to, p.resumable);
    // 이어붙일 대화가 없으면 다시 띄우기가 지금 내용을 버린다 — 계정 카드의
    // `fresh` 와 같은 규칙으로 빨강.
    let tone = if p.resumable {
        theme::accent()
    } else {
        theme::danger()
    };
    paint_confirm_card(
        g,
        win,
        cursor,
        &title,
        &lines,
        &[
            ("다시 띄우기", CharacterSwapBtn::Relaunch, Some(tone)),
            ("취소", CharacterSwapBtn::Cancel, None),
        ],
    )
}

/// 창 목록을 [이름][막대][퍼센트] 로 늘어놓는다. claude 의 배지와 codex 의
/// rollout 은 자료 모양이 다르지만 **화면에서는 같은 것**이라, 그리는 자리를
/// 하나로 둔다 — 코덱스만 글자로 나오던 동안 한도가 찬 것이 눈에 안 띄었다.
pub(crate) fn draw_window_gauges(
    g: &mut gpu::GpuRenderer,
    x: f32,
    y: f32,
    right: f32,
    font: f32,
    wins: &[(String, f32)],
    stale: bool,
) -> f32 {
    const GW: f32 = 40.0;
    const GH: f32 = 4.0;
    let gy = y + (font - GH) / 2.0;
    let mut bx = x;
    for (label, pct) in wins {
        let (label, pct) = (label.as_str(), *pct);
        let need = g.measure_chrome_text(label, font, false)
            + 6.0
            + GW
            + 6.0
            + g.measure_chrome_text("100%", font, false);
        if bx + need > right {
            break;
        }
        g.draw_text(
            bx,
            y,
            label,
            gpu::DrawOpts {
                font_size: font,
                color: theme::text_mute(),
                bold: false,
                italic: false,
            },
        );
        let gx = bx + g.measure_chrome_text(label, font, false) + 6.0;
        round_rect(g, gx, gy, GW, GH, GH / 2.0, theme::with_alpha(theme::text_dim(), 0x59));
        let w = (GW * (pct / 100.0).clamp(0.0, 1.0)).max(GH);
        round_rect(g, gx, gy, w, GH, GH / 2.0, usage_bar_color(pct));
        // stale 은 `~` 로만 말한다 — 색까지 흐리면 「급하지 않다」로 읽힌다.
        let pt = if stale {
            format!("~{pct:.0}%")
        } else {
            format!("{pct:.0}%")
        };
        g.draw_text(
            gx + GW + 6.0,
            y,
            &pt,
            gpu::DrawOpts {
                font_size: font,
                color: usage_pct_color(pct),
                bold: false,
                italic: false,
            },
        );
        bx = gx + GW + 6.0 + g.measure_chrome_text(&pt, font, false) + 12.0;
    }
    bx
}

/// 사용량 팝오버의 창 한 줄: `[창][막대][퍼센트][풀리는 때]`. 칸 너비가 고정이라
/// 5h 줄과 7d 줄의 막대가 같은 자리에 서고, 두 줄을 위아래로 견줄 수 있다.
/// 모델별 창(`sub`)은 글자를 낮추고 흐리게 — 계정 전체의 창이 아니라 그 안의 한 칸이다.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_usage_win_row(
    g: &mut gpu::GpuRenderer,
    x: f32,
    y: f32,
    right: f32,
    h: f32,
    f: f32,
    lab: &str,
    pct: f32,
    rs: &str,
    sub: bool,
    stale: bool,
) {
    const LAB_W: f32 = 22.0;
    const GG_W: f32 = 64.0;
    const GG_H: f32 = 4.0;
    const PCT_W: f32 = 34.0;
    const GAP: f32 = 8.0;
    let rf = if sub { f - 2.5 } else { f - 2.0 };
    let ty = y + (h - rf) / 2.0 - 1.0;
    g.draw_text(
        x,
        ty,
        lab,
        gpu::DrawOpts { font_size: rf, color: theme::text_mute(), bold: false, italic: false },
    );
    let gx = x + LAB_W + GAP;
    let gy = y + (h - GG_H) / 2.0;
    round_rect(g, gx, gy, GG_W, GG_H, GG_H / 2.0, theme::with_alpha(theme::text_dim(), 0x59));
    let w = (GG_W * (pct / 100.0).clamp(0.0, 1.0)).max(GG_H);
    round_rect(g, gx, gy, w, GG_H, GG_H / 2.0, usage_bar_color(pct));
    // stale 은 `~` 로만 말한다 — 색까지 흐리면 「급하지 않다」로 읽힌다.
    let pt = if stale { format!("~{pct:.0}%") } else { format!("{pct:.0}%") };
    let px = gx + GG_W + GAP;
    let pw = g.measure_chrome_text(&pt, rf, false);
    g.draw_text(
        px + PCT_W - pw,
        ty,
        &pt,
        gpu::DrawOpts {
            font_size: rf,
            color: if sub { theme::text_mute() } else { usage_pct_color(pct) },
            bold: false,
            italic: false,
        },
    );
    if !rs.is_empty() {
        let rx = px + PCT_W + GAP;
        let sf = f - 2.5;
        let t = crate::info::fit_text(g, rs, (right - rx).max(20.0), sf, false);
        g.draw_text(
            rx,
            y + (h - sf) / 2.0 - 1.0,
            &t,
            gpu::DrawOpts { font_size: sf, color: theme::text_mute(), bold: false, italic: false },
        );
    }
}

/// 창 줄 자리에 놓는 한 줄 안내(「기록 없음」·「5h 미제공」). 줄 높이를 창 줄과 같게
/// 맞춰 블록 높이 계산이 한 가지로 남는다.
pub(crate) fn draw_usage_note(
    g: &mut gpu::GpuRenderer,
    x: f32,
    y: f32,
    h: f32,
    f: f32,
    t: &str,
    col: [u8; 4],
) {
    g.draw_text(
        x,
        y + (h - f) / 2.0 - 1.0,
        t,
        gpu::DrawOpts { font_size: f, color: col, bold: false, italic: false },
    );
}

#[cfg(test)]
mod minimap_box_tests {
    use super::*;

    /// 도는 칸에는 걷기와 띠가 **함께** 선다(사용자 2026-08-24). 그러면 둘이 자리를
    /// 다투는데, 화면으로는 잡기 어렵다 — 겹치는 건 세로로 갈린 좁은 칸뿐이고
    /// 스프라이트 하단이 발이라 「좀 지저분하다」로만 보인다. 산술로 못 박는다.
    ///
    /// 방 카드 높이는 `36 + 13*pane수` 를 46..150 으로 조인 값이고(session.rs),
    /// 칸은 그것을 split 트리로 나눈 조각이다 — 아래 치수가 그 범위다.
    #[test]
    fn walking_student_never_touches_the_bar() {
        for (mw, mh) in [
            (200.0, 31.0), // 세로 2분할 — 자리를 안 비우면 여기서 2px 겹쳤다
            (200.0, 19.0), // 띠가 서는 가장 좁은 칸
            (100.0, 62.0), // 가로 2분할
            (60.0, 25.0),
            (200.0, 150.0), // 가장 큰 카드
        ] {
            assert!(minimap_has_bar(mw, mh), "{mw}x{mh}: 이 칸엔 띠가 서야 한다");
            let (_, fy, face) = minimap_face_box(0.0, 0.0, mw, mh);
            // `draw_student_walk` 는 얼굴 상자를 위로 2px 올리고 4px 키워 그린다.
            let walk_bottom = fy - MINI_WALK_PAD / 2.0 + face + MINI_WALK_PAD;
            let bar_top = mh - MINI_BAR_H - MINI_BAR_PAD;
            assert!(
                walk_bottom <= bar_top,
                "{mw}x{mh}: 걷기 하단 {walk_bottom} 이 띠 {bar_top} 를 파고든다"
            );
            assert!(fy >= 0.0, "{mw}x{mh}: 얼굴이 칸 위로 넘쳤다");
        }
    }

    /// 띠가 안 서는 칸은 자리를 안 뺀다 — 뺐다면 그 칸만 얼굴이 이유 없이 작아진다.
    #[test]
    fn tiny_cells_give_the_whole_box_to_the_face() {
        for (mw, mh) in [(8.0, 40.0), (200.0, 15.0), (5.0, 5.0)] {
            assert!(!minimap_has_bar(mw, mh));
            let (_, fy, face) = minimap_face_box(0.0, 0.0, mw, mh);
            assert_eq!(fy, (mh - face) / 2.0, "{mw}x{mh}: 안 그릴 띠 자리를 뺐다");
        }
    }

    /// 얼굴은 칸 가운데다 — 세로만 띠 쪽으로 치우친다.
    #[test]
    fn face_is_centered_horizontally() {
        let (fx, _, face) = minimap_face_box(10.0, 0.0, 100.0, 62.0);
        assert_eq!(fx, 10.0 + (100.0 - face) / 2.0);
    }
}

#[cfg(test)]
mod elapsed_tests {
    use super::{elapsed_label, elapsed_style};

    #[test]
    fn 짧게_도는_일은_숫자를_안_단다() {
        assert_eq!(elapsed_label(0), None);
        assert_eq!(elapsed_label(59), None);
        assert_eq!(elapsed_label(60).as_deref(), Some("1분"));
    }

    #[test]
    fn 단위가_바뀌어도_숫자가_거꾸로_가지_않는다() {
        // 한 시간에서 갈면 「99분 → 1시간」이라 값이 줄어 보인다. 두 시간에서
        // 갈아야 「119분 → 2시간」으로 이어진다.
        assert_eq!(elapsed_label(7199).as_deref(), Some("119분"));
        assert_eq!(elapsed_label(7200).as_deref(), Some("2시간"));
        assert_eq!(elapsed_label(3600).as_deref(), Some("60분"));
    }

    #[test]
    fn 시간대는_내림이다() {
        // 올림하면 2시간 1분이 「3시간」이 되어 실제보다 오래 도는 것처럼 읽힌다.
        assert_eq!(elapsed_label(7260).as_deref(), Some("2시간"));
        assert_eq!(elapsed_label(10799).as_deref(), Some("2시간"));
        assert_eq!(elapsed_label(10800).as_deref(), Some("3시간"));
    }

    #[test]
    fn 오래_걸릴수록_눈에_띈다() {
        // 세 단이 각자 다른 값이어야 한다 — 같은 색이 겹치면 단을 나눈 뜻이 없다.
        let (c1, b1) = elapsed_style(599);
        let (c2, b2) = elapsed_style(600);
        let (c3, b3) = elapsed_style(1800);
        assert_ne!(c1, c2);
        assert_ne!(c2, c3);
        assert!(!b1 && !b2 && b3);
    }
}

/// pane 안 탭 띠 한 벌.
pub(crate) struct TabStrip<'a> {
    pub tabs: &'a [String],
    /// `tabs` 가 비었을 때 쓸 단일 탭 제목.
    pub label: &'a str,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub active_tab: usize,
    pub tab_first: usize,
    pub tab_last_active: usize,
    /// 이 pane 이 활성인가 — 활성 탭 accent 선의 색만 가른다.
    pub is_active: bool,
    pub color: Option<[u8; 4]>,
    /// 오른쪽 버튼 무리가 이미 가져간 폭 — 탭이 그 밑으로 깔리지 않게.
    pub right_reserve: f32,
    pub chrome_font: f32,
    pub icon_size: f32,
    pub act_fg: [u8; 4],
    pub cursor_px: (f32, f32),
    /// 이 pane 위에서 hover 중인 탭 index(다른 pane 것이면 None).
    pub hover_tab: Option<usize>,
    pub drag_src: Option<usize>,
    pub drag_target: Option<usize>,
    /// 닫는 동안 얼려 둔 알약 자리 `[(x, w)]`. 남은 탭이 이 슬롯을 앞에서부터
    /// 채운다 — 그래야 × 가 방금 누른 자리에 온다. `None` = 평소(라벨 실측 폭).
    pub frozen_slots: Option<&'a [(f32, f32)]>,
}

/// 그린 결과 — 눌린 자리를 가릴 rect 들과, 띠가 실제로 보인 창(`first`·`n_vis`).
#[derive(Default)]
pub(crate) struct TabStripOut {
    pub first: usize,
    pub n_vis: usize,
    pub tab_hits: Vec<(usize, (f32, f32, f32, f32))>,
    pub close_hits: Vec<(usize, (f32, f32, f32, f32))>,
    pub plus_rect: Option<(f32, f32, f32, f32)>,
    /// 활성 탭 accent 선 — 호출부가 pane 경계 위에 한 번 더 얹는다.
    pub accent: Option<(f32, f32, f32, [u8; 4])>,
}

pub(crate) fn draw_pane_tabs(g: &mut gpu::GpuRenderer, c: &TabStrip) -> TabStripOut {
    let mut out = TabStripOut::default();
    let text_y = c.y + (PANE_HEADER_HEIGHT - c.chrome_font) / 2.0;
    let icon_y = c.y + (PANE_HEADER_HEIGHT - c.icon_size) / 2.0;
    // ── In-pane tab bar ── empty tabs = single tab from `label`.
    let tab_list: Vec<&str> = if c.tabs.is_empty() {
        vec![c.label]
    } else {
        c.tabs.iter().map(|s| s.as_str()).collect()
    };
    // SVG icons are square at c.icon_size; reserve that exact width
    // (not a glyph measurement) so the × never crowds the tab edge.
    let close_w = c.icon_size;
    let plus_w = c.icon_size;
    // Each tab's title gets an equal share of the leftover width.
    let tabs_area = (c.w - 8.0 - c.right_reserve - plus_w - 16.0).max(0.0);
    let gap = 6.0_f32;
    // Overflow windowing: whole tabs only — 이 띠는 클립을 안 세우므로
    // 반쪽 알약이 나온다. When they can't all fit at the 56px minimum,
    // show a contiguous run from `tab_first` and reserve 12px at
    // each end for the overflow chevrons; the wheel over the strip
    // steps the run.
    let n_tabs = tab_list.len();
    let fits = |area: f32| (((area + gap) / (56.0 + gap)) as usize).max(1);
    let overflowing = n_tabs > fits(tabs_area);
    let (strip_pad, area_eff) = if overflowing {
        (12.0_f32, (tabs_area - 24.0).max(56.0))
    } else {
        (0.0, tabs_area)
    };
    let n_vis = n_tabs.min(fits(area_eff));
    let mut first = c.tab_first.min(n_tabs - n_vis);
    // A tab switch since the last frame (click, close, shortcut —
    // whichever of the many sites) reveals the newly active tab;
    // plain wheel scrolling is left where the user put it.
    if c.active_tab != c.tab_last_active {
        if c.active_tab < first {
            first = c.active_tab;
        } else if c.active_tab >= first + n_vis {
            first = c.active_tab + 1 - n_vis;
        }
    }
    out.first = first;
    out.n_vis = n_vis;
    let per_tab = if n_vis == 1 {
        area_eff
    } else {
        ((area_eff - gap * n_vis.saturating_sub(1) as f32) / n_vis as f32).clamp(56.0, 320.0)
    };
    // 닫는 동안엔 알약을 새로 재지 않고 얼려 둔 자리를 앞에서부터 채운다. 탭이 하나
    // 빠지면 `n_vis` 가 줄어 `per_tab` 이 커지고 남은 탭이 넓어지는데, 그러면 × 가
    // 방금 누른 자리에서 달아난다. 슬롯이 모자라면(닫기 전보다 탭이 늘었다) 그
    // 지점부터는 평소 계산으로 돌아간다.
    let slot = |i: usize| -> Option<(f32, f32)> { c.frozen_slots.and_then(|s| s.get(i).copied()) };
    // Left edge of each visible tab's pill, for the drag insertion bar.
    let mut tab_edges: Vec<f32> = Vec::with_capacity(n_vis);
    // Geometry for the post-loop structural border pass.
    let mut tabs_left: Option<f32> = None;
    let mut tabs_right_edge: f32 = 0.0;
    let mut inter_boundaries: Vec<f32> = Vec::new();
    let mut active_tab_box: Option<(f32, f32)> = None;
    let mut tx = c.x + 8.0 + strip_pad;
    for (i, tab) in tab_list.iter().enumerate().skip(first).take(n_vis) {
        // 화면상 몇 번째 칸인가 — 얼려 둔 슬롯은 화면 순서로 담겨 있다.
        let vi = i - first;
        if let Some((sx, _)) = slot(vi) {
            tx = sx + 6.0;
        }
        let tab_x0 = tx;
        // This pane's active tab — gets the pill + focus strip + ×.
        let active = tab_list.len() == 1 || i == c.active_tab;
        let is_hover = c.hover_tab == Some(i);
        // × on the active tab always; on inactive only while
        // hovered. The width is reserved either way so hover
        // doesn't shift the surrounding layout.
        let show_x = active || is_hover;
        let reserve_x = true;
        let bright = active || is_hover;
        // Tab being lifted in a cross-pane / reorder drag is
        // drawn faint — reads as "in transit" against the
        // insertion bar at the drop position.
        let being_dragged = c.drag_src == Some(i);
        let alpha_mul = if being_dragged { 0x55 } else { 0xFF };
        let combine = |a: u8| ((a as u16 * alpha_mul as u16) / 0xFF) as u8;
        // Per-pane accent (set via `surface.set_color`) recolors the
        // tab-name text only; None = default chrome text. Brightness
        // (active/hover) still rides on the alpha.
        let label_fg = c.color.unwrap_or_else(theme::text);
        let t_fg = if bright {
            theme::with_alpha(label_fg, combine(0xFF))
        } else {
            theme::with_alpha(label_fg, combine(0x82))
        };
        let t_icon = if bright {
            theme::with_alpha(theme::text_dim(), combine(0xFF))
        } else {
            theme::with_alpha(theme::text_dim(), combine(0x82))
        };
        // Truncate this tab's title to its share of the bar.
        // × space is reserved on every tab — see `reserve_x`.
        // No per-tab terminal glyph: the +button already signals
        // "new shell"; doubling that icon on every tab was noise.
        let x_reserve = if reserve_x { close_w + 8.0 } else { 0.0 };
        let budget = match slot(vi) {
            Some((_, sw)) => (sw - 12.0 - x_reserve).max(0.0),
            None => (per_tab - x_reserve - 14.0).max(0.0),
        };
        // 자를 땐 **뒤**를 자른다. 여기 오는 라벨은 전부 앞이 정체다 —
        // 학생 이름(`미도리 · 작업명`), 폴더명 한 조각, 프로세스명. 앞을
        // 자르면 그 정체가 먼저 사라져 탭들이 다 `…작업명` 이 된다.
        // 앞자르기가 옳은 건 경로 전체를 실을 때인데, 셸 탭이 받는 값은
        // `smart_pane_label` → `cwd_basename`, 즉 경로가 아니라 마지막
        // 폴더명 하나다(경로를 줄이는 `shorten_cwd` 는 이 자리를 안 탄다).
        let mut label = tab.to_string();
        let mut lw = g.measure_chrome_text(&label, c.chrome_font, active);
        if lw > budget {
            while label.chars().count() > 1 {
                label.pop();
                lw = g.measure_chrome_text(&format!("{label}…"), c.chrome_font, active);
                if lw <= budget {
                    break;
                }
            }
            label.push('…');
        }
        // Pill geometry: label + reserved × slot (terminal icon
        // removed — +button covers "new shell" duty).
        let content_w = lw + x_reserve;
        // First tab sits flush with the pane's left edge so the
        // active tab's accent strip joins the pane divider with
        // no visible gap — only while nothing is windowed off
        // (the overflow chevron owns that sliver otherwise).
        let (box_x, box_right) = match slot(vi) {
            Some((sx, sw)) => (sx, sx + sw),
            None => (
                if i == 0 && !overflowing {
                    c.x
                } else {
                    tab_x0 - 6.0
                },
                tab_x0 + content_w + 6.0,
            ),
        };
        let tw = (box_right - box_x).max(0.0);
        tab_edges.push(box_x);
        if tabs_left.is_none() {
            tabs_left = Some(box_x);
        } else {
            inter_boundaries.push(box_x);
        }
        tabs_right_edge = box_x + tw;
        if active {
            active_tab_box = Some((box_x, tw));
        }
        // Active tab keeps the band BG (= terminal body) — no
        // fill — so the tab reads as continuous with the content
        // below it. The accent top + broken bottom are what
        // differentiate it. Structural lines drawn post-loop.
        let stroke = 1.0_f32;
        let _ = stroke;
        let cx = g.draw_text(
            tx,
            text_y,
            &label,
            gpu::DrawOpts {
                font_size: c.chrome_font,
                color: t_fg,
                bold: active,
                italic: false,
            },
        );
        // Pop-out icon (external-link): file tabs only, shown on the
        // active or hovered tab. Sits left of the ×; clicking it
        // moves the tab's editor into its own OS window.
        // 평소엔 라벨 끝 바로 뒤에 붙인다. 얼려 둔 자리에서는 **알약 오른쪽 끝**
        // 기준으로 놓는다 — 라벨 길이가 탭마다 달라서, 라벨 끝에 붙이면 폭을 얼려
        // 놓고도 × 가 제각각이 되어 자리를 얼린 뜻이 없어진다. 두 식은 평소 배치에서
        // 같은 값을 낸다(`box_right = tab_x0 + lw + x_reserve + 6`).
        let action_x = match slot(vi) {
            Some(_) => box_right - x_reserve + 2.0,
            None => cx + 8.0,
        };
        if show_x {
            let close_x = action_x;
            // Hover chip behind the × — same lift the +button gets,
            // so the close target reads as clickable on hover.
            let chip = c.icon_size + 6.0;
            let chip_x = close_x + (c.icon_size - chip) / 2.0;
            let chip_y = c.y + (PANE_HEADER_HEIGHT - chip) / 2.0;
            let (mx, my) = c.cursor_px;
            let x_hover =
                mx >= chip_x && mx <= chip_x + chip && my >= chip_y && my <= chip_y + chip;
            if x_hover {
                hover_rect(g, chip_x, chip_y, chip, chip, theme::radius_sm());
            }
            let xcol = if x_hover { theme::text() } else { t_icon };
            g.queue_icon("x", close_x, icon_y, c.icon_size, xcol);
            // × close hit (widen a little for an easy target).
            out.close_hits.push((
                i,
                (close_x - 2.0, c.y, c.icon_size + 4.0, PANE_HEADER_HEIGHT),
            ));
        }
        // Whole-pill click/drag hit. Inactive tabs have no × inside,
        // so the entire pill switches; the active tab's × is checked
        // first by the handler.
        out.tab_hits.push((i, (box_x, c.y, tw, PANE_HEADER_HEIGHT)));
        tx = box_right + gap;
    }
    // Structural borders. Browser-tab pattern:
    //   - Top BORDER across the strip, with the active tab's
    //     segment painted in the focus color (same thickness).
    //   - Bottom BORDER across the strip but BROKEN under the
    //     active tab so the active opens straight into the body.
    //   - Vertical BORDER at each inter-tab boundary (single line
    //     shared between neighbours).
    // No outer left/right of the strip — the pane dividers fill
    // those roles, so leftmost-active never gets two stacked lines.
    if let Some(left) = tabs_left {
        let stroke = 1.0_f32;
        let band_w = (tabs_right_edge - left).max(0.0);
        g.rect(left, c.y, band_w, stroke, theme::border());
        // Bottom BORDER across the WHOLE pane header (tabs + plus
        // button + action cluster), broken only under the active
        // tab so it flows into the body.
        let by = c.y + PANE_HEADER_HEIGHT - stroke;
        let h_right = c.x + c.w;
        if let Some((ax, aw)) = active_tab_box {
            let lw = (ax - c.x).max(0.0);
            g.rect(c.x, by, lw, stroke, theme::border());
            let rx = ax + aw;
            let rw = (h_right - rx).max(0.0);
            g.rect(rx, by, rw, stroke, theme::border());
        } else {
            g.rect(c.x, by, c.w, stroke, theme::border());
        }
        for b in &inter_boundaries {
            g.rect(*b, c.y, stroke, PANE_HEADER_HEIGHT, theme::border());
        }
        // Right edge of the strip — gives the last tab (often the
        // active one when only the trailing tab is selected) a
        // visible right boundary. Left edge is left to the pane
        // divider so it never doubles up.
        g.rect(
            tabs_right_edge - stroke,
            c.y,
            stroke,
            PANE_HEADER_HEIGHT,
            theme::border(),
        );
        if let Some((ax, aw)) = active_tab_box {
            let accent_col = if c.is_active {
                theme::accent()
            } else {
                theme::text()
            };
            // accent 선은 BORDER stroke(1px)보다 살짝 굵게 — 활성 pane 강조.
            g.rect(ax, c.y, aw, ACTIVE_ACCENT_STROKE, accent_col);
            out.accent = Some((ax, c.y, aw, accent_col));
        }
    }
    // Drag insertion bar: 6px accent line spanning the strip.
    // 옛 2px는 Retina+at-speed drag에서 사실상 안 보였음.
    if let Some(target) = c.drag_target {
        // tab_edges holds visible tabs only — offset by `first`.
        let bar_x = tab_edges
            .get(target.saturating_sub(first))
            .copied()
            .unwrap_or(tx - gap);
        g.rect(
            bar_x - 3.0,
            c.y + 1.0,
            6.0,
            PANE_HEADER_HEIGHT - 2.0,
            theme::accent(),
        );
    }
    let (cur_x, cur_y) = c.cursor_px;
    let inside = |rx: f32, ry: f32, rw: f32, rh: f32| {
        cur_x >= rx && cur_x <= rx + rw && cur_y >= ry && cur_y <= ry + rh
    };
    // [+] new-tab button right after the tabs. Hover chip is a
    // tight rounded square centered on the glyph so the glow
    // hugs the icon instead of stretching across a tall band.
    // Hidden while a tab drag is active so the +button doesn't
    // sit on top of the insertion bar / accept a stray drop.
    let dragging_tab = c.drag_src.is_some();
    let plus_iw = g.measure_chrome_text("\u{ea60}", c.icon_size, false);
    let chip_size = (c.icon_size + 6.0).max(plus_iw + 6.0);
    let chip_x = tx + (plus_iw - chip_size) / 2.0;
    let chip_y = c.y + (PANE_HEADER_HEIGHT - chip_size) / 2.0;
    let plus_rect = (chip_x, chip_y, chip_size, chip_size);
    let plus_hover = !dragging_tab && inside(plus_rect.0, plus_rect.1, plus_rect.2, plus_rect.3);
    if plus_hover {
        hover_rect(
            g,
            plus_rect.0,
            plus_rect.1,
            plus_rect.2,
            plus_rect.3,
            theme::radius_sm(),
        );
    }
    let plus_color = if plus_hover { theme::text() } else { c.act_fg };
    if !dragging_tab {
        g.queue_icon("plus", tx, icon_y, c.icon_size, plus_color);
        out.plus_rect = Some(plus_rect);
    }
    // Overflow chevrons in the reserved end slots — more tabs
    // exist past this edge; the wheel over the strip scrolls.
    if overflowing {
        let cis = 12.0_f32;
        let ccy = c.y + (PANE_HEADER_HEIGHT - cis) / 2.0;
        if first > 0 {
            g.queue_icon("chevron-left", c.x + 4.0, ccy, cis, theme::text_mute());
        }
        if first + n_vis < n_tabs {
            g.queue_icon(
                "chevron-right",
                tx + plus_iw + 8.0,
                ccy,
                cis,
                theme::text_mute(),
            );
        }
    }
    out
}
