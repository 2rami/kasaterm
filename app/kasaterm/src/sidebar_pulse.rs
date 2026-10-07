//! 사이드바 현황 — 맨 위 「목록 | 배치도」 아이콘 전환과 목록 한 줄(세션 이름 / 진행 띠 · 경과 시간).
//!
//! 옛 현황 줄(「사람 차례 N · 작업 N · 끝 N」)은 걷었다 — 차례 수를 따로 세우지 않고 줄이 말한다
//! (2026-09-29 합의, `docs/boards.md` 「다음 구조」). 줄에서 학생 이름·마지막 보고도 뺐다 — 얼굴이
//! 누구인지 말하고, 글은 세션 이름과 배치도 칸과 같은 띠·시간만 남는다(2026-10-01). 보드 스냅샷은
//! 펫 현황판(`overlay.json`)이 떠 있을 때만 백그라운드로 읽는다. 치수는 `docs/design.md`
//! 「사이드바 현황 목록」.

use super::*;
use kasa_socket::backend::Backend;

/// 맨 위 전환 줄(옛 현황 줄 자리) — 높이 30, 안쪽 26.
pub(crate) const PULSE_H: f32 = 30.0;
/// 두 줄 목록 행. 얼굴 20 · 첫 줄 세션 이름 12 · 둘째 줄 진행 띠와 경과 시간 10.5(줄높이 14).
pub(crate) const LIST_ROW_H: f32 = 40.0;
/// 방 이름 아래 첫 줄까지의 틈. 행 사이는 0.
pub(crate) const LIST_TOP_GAP: f32 = 4.0;
const FACE: f32 = 20.0;
/// 둘째 줄 높이 — 띠를 이 줄의 세로 가운데에 앉힌다.
const LINE_H: f32 = 14.0;
/// 요약이라 몇 초 늦어도 뜻이 안 바뀌고, 사이드바가 열려 있는 내내 돈다.
const POLL: std::time::Duration = std::time::Duration::from_secs(3);

type Rect = (f32, f32, f32, f32);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PulseCounts {
    /// 승인·질문으로 사람을 부르는 칸과 실패 보고 — 보드의 「확인 필요」.
    pub(crate) yours: usize,
    pub(crate) working: usize,
    /// 성공 완료 보고가 서 있는 칸. 다음 턴을 시작하면 보드에서 빠진다.
    pub(crate) done: usize,
}

/// 사람을 기다리는 학생 한 줄 — 보드의 「확인 필요」 칸 하나.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct WaitingStudent {
    pub(crate) name: String,
    pub(crate) character: Option<String>,
    /// 「승인 기다림 · 일감」 꼴.
    pub(crate) line: String,
    pub(crate) machine_label: String,
    pub(crate) surface_id: String,
    pub(crate) local: bool,
    /// 다른 기기 학생을 비추는 이 기기의 거울 pane — 있으면 그리로 간다.
    pub(crate) mirror: Option<String>,
    pub(crate) since_ms: u64,
}

impl WaitingStudent {
    /// 누르면 갈 이 기기의 pane. 다른 기기 학생인데 거울이 없으면 갈 곳이 없다.
    fn jump_pane(&self) -> Option<&str> {
        if self.local { Some(&self.surface_id) } else { self.mirror.as_deref() }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PulseDigest {
    pub(crate) counts: PulseCounts,
    /// 오래 기다린 순.
    pub(crate) waiting: Vec<WaitingStudent>,
}

type Mailbox = Arc<Mutex<Option<Result<(), String>>>>;

#[derive(Default)]
pub(crate) struct Pulse {
    mailbox: Mailbox,
    inflight: bool,
    last_poll: Option<Instant>,
    failed: bool,
    /// 방 우클릭 메뉴 「목록·배치도 줄 숨기기」. settings.json `sidebar_pulse`.
    pub(crate) hidden: bool,
    /// 렌더가 적어 두는 전환 단추 `(목록, 배치도)`. 안 그린 프레임은 `None` 이라 유령 클릭이 없다.
    pub(crate) switch: Option<(Rect, Rect)>,
}

impl Pulse {
    pub(crate) fn from_settings() -> Self {
        Self {
            hidden: socket::read_settings().get("sidebar_pulse").and_then(|v| v.as_bool()) == Some(false),
            ..Default::default()
        }
    }
}

/// 펫 현황판이 읽는 한 장. 얼굴은 펫이 앱 번들 그림을 못 보므로 `faces/<slug>.png` 로 한 번 내보낸다.
fn overlay_json(digest: &PulseDigest, face: impl Fn(&WaitingStudent) -> Option<String>) -> serde_json::Value {
    let waiting: Vec<serde_json::Value> = digest.waiting.iter().map(|student| serde_json::json!({
        "name": student.name,
        "line": student.line,
        "machine": if student.local { String::new() } else { student.machine_label.clone() },
        "pane": student.jump_pane().unwrap_or_default(),
        "face": face(student).unwrap_or_default(),
    })).collect();
    serde_json::json!({
        "counts": {"yours": digest.counts.yours, "working": digest.counts.working, "done": digest.counts.done},
        "waiting": waiting,
    })
}

/// 바뀐 때, 그리고 20초마다 쓴다 — 펫은 mtime 이 1분 넘게 멈추면 카사텀이 꺼진 것으로 보고 판을
/// 내린다(옛 숫자를 떠 있게 두면 「사람 차례 0」을 믿고 자리를 비운다). 반쯤 쓴 파일을 읽지 않게
/// 옆에 쓰고 옮긴다.
fn write_pet_overlay(dir: &std::path::Path, digest: &PulseDigest) {
    const KEEPALIVE: std::time::Duration = std::time::Duration::from_secs(20);
    static LAST: Mutex<(String, Option<Instant>)> = Mutex::new((String::new(), None));
    let faces = dir.join("faces");
    let face = |student: &WaitingStudent| {
        let slug = theme::character_slug_any(student.character.as_deref()?)?;
        let path = faces.join(format!("{slug}.png"));
        if !path.is_file() {
            let (rgba, w, h) = sprites::student_profile_rgba(slug)?;
            std::fs::create_dir_all(&faces).ok()?;
            image::save_buffer(&path, &rgba, w, h, image::ColorType::Rgba8).ok()?;
        }
        Some(path.to_string_lossy().into_owned())
    };
    let body = overlay_json(digest, face).to_string();
    let mut last = LAST.lock().unwrap();
    if last.0 == body && last.1.is_some_and(|at| at.elapsed() < KEEPALIVE) {
        return;
    }
    let tmp = dir.join("overlay.json.tmp");
    if std::fs::write(&tmp, &body).is_ok() && std::fs::rename(&tmp, dir.join("overlay.json")).is_ok() {
        *last = (body, Some(Instant::now()));
    }
}

/// 방 우클릭 메뉴의 한 줄 — 이 기기 방과 다른 기기 방 메뉴가 같은 말을 쓴다.
pub(crate) fn menu_label(hidden: bool) -> &'static str {
    if hidden { "목록·배치도 줄 보이기" } else { "목록·배치도 줄 숨기기" }
}

/// 전환 아이콘 둘의 자리 `(목록, 배치도)`. 왼쪽 끝이 방 카드와 같은 선이라 그림이 기기 머리줄
/// 아이콘 밑에 선다. 둘이 안 들어가는 폭이면 `None`.
fn switch_rects(width: f32) -> Option<(Rect, Rect)> {
    const GAP: f32 = 6.0;
    let side = crate::native_controls::CONTROL_HEIGHT;
    if SIDEBAR_TAB_INSET + 2.0 * side + GAP > width {
        return None;
    }
    let y = TITLE_HEIGHT + (PULSE_H - side) / 2.0;
    Some(((SIDEBAR_TAB_INSET, y, side, side), (SIDEBAR_TAB_INSET + side + GAP, y, side, side)))
}

/// 사이드바 맨 위, 기기 머리줄 위의 「목록 | 배치도」 아이콘. 고른 것은 전체 기본(`sidebar_body`)이고,
/// 방마다 우클릭으로 고른 것이 그보다 이긴다. 글자가 없으니 마우스가 올라간 쪽의 이름을 툴팁으로
/// 돌려준다 — 칼럼들을 다 그린 뒤에 얹어야 파일트리에 안 가린다.
pub(crate) fn draw_view_switch(
    g: &mut gpu::GpuRenderer,
    pulse: &mut Pulse,
    cursor: (f32, f32),
    width: f32,
    list: bool,
) -> Option<(f32, f32, String)> {
    pulse.switch = None;
    let (list_rect, map_rect) = switch_rects(width)?;
    let style = |active| crate::native_controls::Style { active, ..Default::default() };
    let left = crate::native_controls::icon_button(g, list_rect, cursor, "list", style(list));
    let right = crate::native_controls::icon_button(g, map_rect, cursor, "layout-grid", style(!list));
    pulse.switch = Some((left, right));
    let inside = |r: Rect| cursor.0 >= r.0 && cursor.0 < r.0 + r.2 && cursor.1 >= r.1 && cursor.1 < r.1 + r.3;
    [(left, "목록"), (right, "배치도")]
        .into_iter()
        .find(|(r, _)| inside(*r))
        .map(|(_, name)| (cursor.0, cursor.1, name.to_string()))
}

/// 방 안 순서 — 내 차례 → 하는 중 → 쉬는 중.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RowTurn {
    Yours,
    Working,
    #[default]
    Resting,
}

/// 목록 한 줄의 글. 로컬 줄은 `App::row_student`, 다른 기기 줄은 `sidebar_navigation` 이 채운다.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RowStudent {
    /// 얼굴 열쇠(학생 이름). 셸·웹 pane 은 비어 있다. 글로는 안 적는다 — 얼굴이 말한다.
    pub(crate) who: String,
    /// 세션(창) 이름. 학생 없는 pane 은 그 pane 이름(zsh·웹…).
    pub(crate) title: String,
    pub(crate) turn: RowTurn,
}

/// 줄의 차례. 사람 손이 필요한 기다림과 안 본 보고가 「내 차례」다.
pub(crate) fn row_turn(waiting: bool, unread: bool, busy: bool) -> RowTurn {
    if waiting || unread {
        RowTurn::Yours
    } else if busy {
        RowTurn::Working
    } else {
        RowTurn::Resting
    }
}

/// 세션 이름 — 도는 표시를 뗀 창 이름, 비면 그 칸의 종류.
pub(crate) fn row_title(label: &str, kind: &str) -> String {
    let title = plain_title(label);
    if title.is_empty() { kind.to_string() } else { title.to_string() }
}

/// 창 이름 앞의 도는 표시(`◐ `·`✳ `)를 뗀다 — OSC 제목에는 붙어 오고 고정 제목에는 없다.
pub(crate) fn plain_title(title: &str) -> &str {
    title.trim_start_matches(|c: char| !c.is_alphanumeric() && !matches!(c, '[' | '(' | '「' | '"' | '\'')).trim()
}

/// 줄 하나를 그리는 데 필요한 것 — 이 기기 방과 다른 기기 방이 같은 그림을 쓴다.
pub(crate) struct RowPaint<'a> {
    pub(crate) student: &'a RowStudent,
    /// 얼굴이 없을 때 서는 아이콘.
    pub(crate) icon: &'static str,
    pub(crate) cur: bool,
    pub(crate) hover: bool,
    /// 도는 중 — 학생이 걷고 띠가 쓸린다.
    pub(crate) busy: bool,
    /// 아래 셋은 배치도 칸 바닥 띠와 같은 값이다(`render::progress_bar`·`elapsed_mark`).
    pub(crate) compact_pct: Option<u8>,
    pub(crate) bg_active: bool,
    pub(crate) busy_secs: Option<u64>,
    /// 저쪽에서 닫힌 줄 — 전부 흐리게.
    pub(crate) muted: bool,
}

pub(crate) fn paint_row(g: &mut gpu::GpuRenderer, rect: Rect, p: &RowPaint) {
    let (rx, ry, rw, rh) = rect;
    let s = p.student;
    if p.cur {
        round_rect(g, rx, ry, rw, rh, theme::radius_sm(), theme::surface_active());
    } else if p.hover {
        round_rect(g, rx, ry, rw, rh, theme::radius_sm(), theme::surface_hover());
    }
    let band = if s.turn == RowTurn::Yours && !p.muted {
        Some(theme::attention())
    } else {
        None
    };
    if let Some(color) = band {
        g.rect(rx, ry + 3.0, 2.0, rh - 6.0, color);
    }
    let phase = crate::sprites::anim_phase_secs();
    let (fx, fy) = (rx + 8.0, ry + 5.0);
    // 걷는 그림은 전신 + 여백이라 얼굴 칸 그대로면 작아 보인다 — 줄 높이 안에서 키운다.
    let walked = p.busy && crate::sprites::draw_student_walk(g, &s.who, fx - 5.0, fy - 4.0, FACE + 10.0, phase);
    if !walked && !crate::sprites::draw_student_face_anim(g, &s.who, fx, fy, FACE, phase) {
        g.queue_icon(p.icon, fx + 4.0, fy + 4.0, 12.0, theme::text_dim());
    }
    let tx = fx + FACE + 8.0;
    let right = rx + rw - 8.0;
    let title = crate::info::fit_text(g, &s.title, (right - tx).max(0.0), 12.0, false);
    let ink = if p.muted { theme::text_mute() } else { theme::text() };
    g.draw_text(tx, ry + 5.0, &title, gpu::DrawOpts { font_size: 12.0, color: ink, bold: false, italic: false });
    // 둘째 줄은 배치도 칸 바닥과 같다 — compact 눈금과 경과 시간. 시간은 오른쪽 끝을 내주고 눈금이 그만큼
    // 짧아진다. 「도는 중」은 띠가 아니라 줄 윤곽이 움직인다(배치도 칸과 같은 결).
    let line_y = ry + 22.0;
    let mut bar_w = right - tx;
    if let Some((txt, color, bold)) = crate::render::elapsed_mark(p.busy_secs, p.busy || p.bg_active) {
        let lw = g.measure_chrome_text(&txt, 10.5, bold);
        g.draw_text(right - lw, line_y, &txt, gpu::DrawOpts { font_size: 10.5, color, bold, italic: false });
        bar_w -= lw + 6.0;
    }
    if bar_w > 0.0 {
        let bar_y = line_y + (LINE_H - crate::render::MINI_BAR_H) / 2.0;
        crate::render::progress_bar(g, tx, bar_y, bar_w, p.compact_pct);
    }
    if band.is_none() {
        crate::render::activity_mark(g, rect, theme::radius_sm(), p.busy, p.bg_active, p.cur, 0.0);
    }
}

pub(crate) fn fixture_on() -> bool {
    cfg!(debug_assertions) && crate::verification_run() && std::env::var_os("KASATERM_SIDEBAR_FIXTURE").is_some()
}

/// 격리 검증 앱의 가짜 학생 — 진짜 claude 없이 줄 모양·순서·강조와 배치도 띠를 잰다. pane id 의
/// 숫자로 고른다. 넷째 값은 도는 중인 줄의 경과 초(배치도 칸과 목록 줄이 같은 시간을 말하나).
const FIXTURE_ROWS: [(&str, &str, RowTurn, Option<u64>); 6] = [
    ("케이", "계정 설정·하단바·우측패널", RowTurn::Working, Some(12 * 60)),
    ("유우카", "미러링 방식 변경", RowTurn::Yours, None),
    ("아로나", "MC 되돌리기 dev 반영", RowTurn::Resting, None),
    ("코유키", "카사넷 iroh P2P 직통", RowTurn::Yours, None),
    ("모모이", "작업현황 판 웹 목업", RowTurn::Working, Some(47 * 60)),
    ("호시노", "슬랙 요청 채널 접수대 — 스레드 원글까지 붙이는 긴 제목", RowTurn::Resting, None),
];

fn fixture_entry(pane: &str) -> Option<(&'static str, &'static str, RowTurn, Option<u64>)> {
    if !fixture_on() {
        return None;
    }
    let n: usize = pane.trim_start_matches(|c: char| !c.is_ascii_digit()).parse().ok()?;
    Some(FIXTURE_ROWS[n % FIXTURE_ROWS.len()])
}

fn fixture_row(pane: &str) -> Option<RowStudent> {
    let (name, title, turn, _) = fixture_entry(pane)?;
    Some(RowStudent { who: name.into(), title: title.into(), turn })
}

/// 가짜 학생이 도는 중이면 그 경과 초 — 렌더가 배치도 칸과 목록 줄에 같이 싣는다.
pub(crate) fn fixture_busy_secs(pane: &str) -> Option<u64> {
    fixture_entry(pane)?.3
}

impl App {
    /// 전환 줄이 먹는 높이. 세로 사이드바가 없거나 숨겼으면 0 — 기기 머리줄이 제자리로 올라간다.
    pub(crate) fn sidebar_pulse_h(&self) -> f32 {
        if self.lite || self.tabs_on_top || !self.sidebar_visible || self.pulse.hidden {
            0.0
        } else {
            PULSE_H
        }
    }

    /// 매 틱 — 펫 현황판이 떠 있으면 때마다 보드 요약 읽기를 백그라운드에 건다. 보드 스냅샷은
    /// 원격 기기까지 합친 것이라 GUI 스레드에서 기다리지 않는다.
    pub(crate) fn sidebar_pulse_tick(&mut self) {
        let arrived = self.pulse.mailbox.lock().unwrap().take();
        if let Some(result) = arrived {
            self.pulse.inflight = false;
            match result {
                Ok(()) => self.pulse.failed = false,
                Err(error) => {
                    if !self.pulse.failed {
                        eprintln!("[pulse] 보드 요약 실패: {error}");
                    }
                    self.pulse.failed = true;
                }
            }
        }
        if self.lite || self.pulse.inflight || self.pulse.last_poll.is_some_and(|at| at.elapsed() < POLL) {
            return;
        }
        // 사이드바를 접어도 읽는다 — 현황판은 카사텀이 가려진 동안 보는 자리다.
        if crate::chrome::pet_pid().is_none() {
            return;
        }
        let Some(backend) = self.socket_backend.clone() else {
            return;
        };
        self.pulse.inflight = true;
        self.pulse.last_poll = Some(Instant::now());
        let mailbox = self.pulse.mailbox.clone();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = backend
                .collab_snapshot(&serde_json::json!({"scope": "all"}))
                .map_err(|e| e.to_string())
                .and_then(crate::board_digest::pulse_digest);
            if let (Ok(digest), Some(dir)) = (result.as_ref(), crate::chrome::pet_model_dir()) {
                write_pet_overlay(&dir, digest);
            }
            *mailbox.lock().unwrap() = Some(result.map(|_| ()));
            let _ = proxy.send_event(UserEvent::Redraw);
        });
    }

    /// 「목록 | 배치도」를 눌렀나 — 누르면 전체 기본을 바꾸고 참을 돌려준다.
    pub(crate) fn sidebar_switch_click(&mut self, cursor: (f32, f32)) -> bool {
        let Some((list, map)) = self.pulse.switch.filter(|_| self.sidebar_pulse_h() > 0.0) else {
            return false;
        };
        let inside = |r: Rect| cursor.0 >= r.0 && cursor.0 < r.0 + r.2 && cursor.1 >= r.1 && cursor.1 < r.1 + r.3;
        let want = if inside(list) {
            true
        } else if inside(map) {
            false
        } else {
            return false;
        };
        if self.sidebar_list_body != want {
            self.sidebar_list_body = want;
            socket::write_setting("sidebar_body", serde_json::json!(if want { "list" } else { "map" }));
            self.mark_room_label_dirty();
        }
        self.chrome_dirty = true;
        true
    }

    /// 방 메뉴 「목록·배치도 줄 숨기기·보이기」. 방 목록의 윗변이 움직이므로 다음 프레임에 새로 잰다.
    pub(crate) fn toggle_sidebar_pulse(&mut self) {
        self.pulse.hidden = !self.pulse.hidden;
        self.pulse.switch = None;
        socket::write_setting("sidebar_pulse", serde_json::json!(!self.pulse.hidden));
        self.chrome_dirty = true;
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    /// 이 기기 pane 한 줄의 글 — 목록 순서와 그리기가 같은 값을 본다.
    pub(crate) fn row_student(&self, id: &str) -> RowStudent {
        if let Some(row) = fixture_row(id) {
            return row;
        }
        let (who, kind) = {
            let ws = self.ws.lock().unwrap();
            let kind = match ws.panes.get(id).map(|p| &p.content) {
                Some(PaneContent::Web(_)) => "웹",
                Some(PaneContent::Image(_)) => "이미지",
                Some(PaneContent::Markdown(_)) => "문서",
                Some(PaneContent::Settings) => "설정",
                _ => "터미널",
            };
            (self.display_pane_char(&ws, id).unwrap_or_default(), kind)
        };
        let label = self.pane_row_label(id);
        let turn = row_turn(self.pane_needs_you(id), self.unread_panes.contains(id), self.pane_is_busy(id));
        // 학생 없는 pane 은 그 pane 이름(zsh·웹…)을 그대로 — 경로 앞 `~/` 를 도는 표시로 떼면 안 된다.
        let title = if who.is_empty() {
            if label.trim().is_empty() { kind.to_string() } else { label }
        } else {
            row_title(&label, kind)
        };
        RowStudent { who, title, turn }
    }

    /// 목록 보기에 설 이 방 pane — 거울 줄을 빼고(원본 기기 줄과 같은 학생) 내 차례 → 하는 중 →
    /// 쉬는 중으로. 같은 차례 안은 배치 순서를 지킨다.
    pub(crate) fn sidebar_list_panes(&self, room: usize) -> Vec<String> {
        let mut panes: Vec<(RowTurn, String)> = self
            .window_leaves(room)
            .into_iter()
            .filter(|id| kasa_mcp::remote::remote_info(id).is_none())
            .map(|id| (self.row_student(&id).turn, id))
            .collect();
        panes.sort_by_key(|(turn, _)| *turn);
        panes.into_iter().map(|(_, id)| id).collect()
    }
}

impl App {
    /// 격리 앱에서 입구를 **진짜 클릭**(winit MouseInput 을 window_event 로)으로 눌러 본다 —
    /// 상태를 손으로 세우면 handler 의 클릭 순서에 가려지는 버그를 못 잡는다.
    /// 켜기: `KASATERM_PULSE_PROBE=1` + 검증 실행(`KASATERM_WINDOW_SIZE`). 결과는 `[pulse-probe]` 줄.
    /// 배치도 → 목록 전환(설정 저장까지) → 방 메뉴로 전환 줄 숨기기·보이기.
    pub(crate) fn run_pending_pulse_probe(&mut self, event_loop: &ActiveEventLoop) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::OnceLock;
        use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};
        if !cfg!(debug_assertions)
            || !crate::verification_run()
            || std::env::var("KASATERM_PULSE_PROBE").as_deref() != Ok("1")
        {
            return;
        }
        static START: OnceLock<Instant> = OnceLock::new();
        static STEP: AtomicUsize = AtomicUsize::new(0);
        let step = STEP.load(Ordering::Relaxed);
        if step > 9 || START.get_or_init(Instant::now).elapsed().as_millis() < 4000 + step as u128 * 1500 {
            return;
        }
        let Some(wid) = self.window.as_ref().map(|w| w.id()) else { return };
        let room = || {
            self.window_tab_rects.iter().find(|(i, _)| self.internal_room_kind_at(*i).is_none()).map(|(_, r)| *r)
        };
        let toggle_item = || {
            self.sidebar_menu_rects.iter().find(|(a, _)| *a == SidebarMenuAction::TogglePulse).map(|(_, r)| *r)
        };
        let mut button = MouseButton::Left;
        let target = match step {
            0 => {
                eprintln!("[pulse-probe] switch={:?} list={} line_h={}", self.pulse.switch, self.sidebar_list_body, self.sidebar_pulse_h());
                self.pulse.switch.map(|(_, map)| map)
            }
            1 => {
                eprintln!("[pulse-probe] map_chosen={} saved={:?}", !self.sidebar_list_body, socket::read_settings().get("sidebar_body"));
                self.pulse.switch.map(|(list, _)| list)
            }
            2 => {
                eprintln!("[pulse-probe] list_chosen={}", self.sidebar_list_body);
                button = MouseButton::Right;
                room()
            }
            3 => {
                eprintln!("[pulse-probe] room_menu_offers_toggle={}", toggle_item().is_some());
                toggle_item()
            }
            4 => {
                eprintln!(
                    "[pulse-probe] hidden={} head_top={} saved={:?}",
                    self.pulse.hidden,
                    self.sidebar_head_top(),
                    socket::read_settings().get("sidebar_pulse")
                );
                button = MouseButton::Right;
                room()
            }
            5 => toggle_item(),
            6 => {
                eprintln!("[pulse-probe] shown_again={} head_top={}", !self.pulse.hidden, self.sidebar_head_top());
                STEP.store(10, Ordering::Relaxed);
                return;
            }
            _ => {
                STEP.store(10, Ordering::Relaxed);
                return;
            }
        };
        STEP.store(step + 1, Ordering::Relaxed);
        let Some(r) = target else {
            eprintln!("[pulse-probe] FAILED missing_target step={step}");
            STEP.store(10, Ordering::Relaxed);
            return;
        };
        self.cursor_px = (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
        for state in [ElementState::Pressed, ElementState::Released] {
            self.window_event(event_loop, wid, WindowEvent::MouseInput { device_id: DeviceId::dummy(), state, button });
        }
        self.chrome_dirty = true;
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 현황판 한 장: 이 기기 학생은 제 pane 으로, 다른 기기 학생은 거울이 있을 때만 갈 곳이 있다.
    #[test]
    fn overlay_carries_counts_and_where_each_waiting_student_lives() {
        let student = |name: &str, local: bool, mirror: Option<&str>| WaitingStudent {
            name: name.into(), character: None, line: "승인 기다림 · 일감".into(), machine_label: "나쵸네코".into(),
            surface_id: "%3".into(), local, mirror: mirror.map(str::to_string), since_ms: 0,
        };
        let digest = PulseDigest {
            counts: PulseCounts { yours: 3, working: 2, done: 1 },
            waiting: vec![student("미도리", true, None), student("아리스", false, Some("%9")), student("유즈", false, None)],
        };
        let json = overlay_json(&digest, |_| None);
        assert_eq!(json["counts"]["yours"], 3);
        let rows = json["waiting"].as_array().unwrap();
        assert_eq!((rows[0]["pane"].as_str(), rows[0]["machine"].as_str()), (Some("%3"), Some("")));
        assert_eq!((rows[1]["pane"].as_str(), rows[1]["machine"].as_str()), (Some("%9"), Some("나쵸네코")), "거울로 간다");
        assert_eq!(rows[2]["pane"].as_str(), Some(""), "갈 곳이 없으면 비운다 — 남의 기기 pane id 로 이 기기를 찾지 않는다");
    }

    /// 방 안 순서 — 사람 손이 필요한 기다림과 안 본 보고가 내 차례다.
    #[test]
    fn row_turn_orders_yours_then_working_then_resting() {
        assert_eq!(row_turn(true, false, false), RowTurn::Yours);
        assert_eq!(row_turn(true, true, true), RowTurn::Yours, "기다림이 도는 중보다 먼저");
        assert_eq!(row_turn(false, true, true), RowTurn::Yours, "안 본 보고");
        assert_eq!(row_turn(false, false, true), RowTurn::Working);
        assert_eq!(row_turn(false, false, false), RowTurn::Resting);
        let mut turns = [RowTurn::Resting, RowTurn::Yours, RowTurn::Working, RowTurn::Yours];
        turns.sort();
        assert_eq!(turns, [RowTurn::Yours, RowTurn::Yours, RowTurn::Working, RowTurn::Resting]);
    }

    /// 줄 글은 세션 이름뿐 — 학생 이름은 얼굴이 말한다. 이름이 비면 종류로 남겨 빈 줄을 만들지 않는다.
    #[test]
    fn row_title_is_the_session_name_without_the_student() {
        assert_eq!(row_title("◑ 목록뷰 진행 막대·이름 정리", "터미널"), "목록뷰 진행 막대·이름 정리");
        assert_eq!(row_title("claude", "터미널"), "claude");
        assert_eq!(row_title("⠂ ", "터미널"), "터미널");
    }

    /// 전환은 아이콘 둘 — 공통 아이콘 단추 크기로 전환 줄 안에 서고, 왼쪽 끝이 방 카드 선이다.
    #[test]
    fn switch_icons_sit_in_the_switch_row_on_the_card_line() {
        let (list, map) = switch_rects(240.0).unwrap();
        assert_eq!((list.0, list.2, list.3), (SIDEBAR_TAB_INSET, 26.0, 26.0));
        assert_eq!(map.0, list.0 + list.2 + 6.0, "사이 6");
        assert!(list.1 >= TITLE_HEIGHT && list.1 + list.3 <= TITLE_HEIGHT + PULSE_H, "전환 줄 안");
        assert!(crate::gpu::GpuRenderer::icon_svg("list").is_some() && crate::gpu::GpuRenderer::icon_svg("layout-grid").is_some());
        assert!(switch_rects(60.0).is_none(), "둘이 안 들어가면 안 그린다 — 반쪽 단추를 누르게 두지 않는다");
    }

    #[test]
    fn plain_title_drops_the_spinner_but_keeps_brackets() {
        assert_eq!(plain_title("◑ 작업현황 판 웹 목업"), "작업현황 판 웹 목업");
        assert_eq!(plain_title("✳ Claude Code"), "Claude Code");
        assert_eq!(plain_title("「계정」 정리"), "「계정」 정리");
        assert_eq!(plain_title("⠂ "), "");
    }
}
