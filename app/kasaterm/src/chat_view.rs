//! 학생 칸을 대화로 보기(PC) — 터미널 칸과 같은 격자·같은 글꼴로, 대화는 그 칸의 기록(jsonl·rollout)을
//! claude classic 화면 꼴(`classic.rs`)로 칸 폭에 다시 늘어놓고, 입력칸·상태줄·스피너·선택지 창은 진짜
//! 화면 바닥을 그대로 옮겨 붙인다. 그래서 치는 것·한글 조합·`/` 명령·승인 답은 터미널과 똑같이 학생
//! TUI 로 가고, 이 보기가 바꾸는 것은 위쪽 대화 줄뿐이다.
//!
//! 기록에서 그리는 까닭: 풀스크린 claude 는 지난 대화를 앱 안에만 두고 `--resume` 뒤엔 압축 경계 앞을
//! 아예 안 보이며, 거울은 원본 칸 크기를 못 바꾼다. 기록은 처음부터 다 있고 어느 폭으로든 다시 접힌다.
//! 바닥 영역을 못 찾는 화면(`/config` 같은 판)이나 화면 대부분을 차지하는 창은 그동안 진짜 터미널을 보인다.

use super::*;

mod bottom;
pub(crate) mod classic;
mod codex;
pub(crate) mod history;
pub(crate) mod parse;
mod select;

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::sync::Mutex;

type Rect = (f32, f32, f32, f32);

/// 기록을 읽는 간격. 폰과 같다 — 더 잦으면 긴 턴에서 다시 펴는 값만 늘어난다.
const POLL_EVERY: std::time::Duration = std::time::Duration::from_millis(1000);
/// 이보다 크게 화면을 덮는 창(편집 승인의 긴 diff 같은)은 진짜 터미널로 본다.
const REGION_MAX: f32 = 0.6;
/// 빈 입력칸에서 Esc 를 이 안에 두 번 치면 claude 는 되감기(Rewind) 창을 연다 — 둘째 것은 삼킨다.
const ESC_TWICE: std::time::Duration = std::time::Duration::from_millis(800);

/// 격리 검증용 자동 조작 한 걸음(`KASATERM_AUTOCHAT`).
enum AutoStep {
    /// 그 칸(없으면 초점 칸)의 대화 보기를 켜고 끈다.
    Toggle(Option<String>),
    /// 셸 칸의 명령으로 보기 전환(`shell_view.rs`).
    Blocks,
    OpenAll,
    /// 맨 아래에서 이만큼 줄 위로.
    Scroll(usize),
    /// 그 칸만 확대(⌘⇧+ 와 같은 길) — 배율을 이만큼 더한다.
    Zoom(f32),
    /// ⋮ 메뉴의 대화 보기 칸 위에 커서를 둔다(툴팁 확인).
    HoverMenu,
    /// Info 「내 프롬프트」 n 번째로 옮긴다.
    Jump(usize),
}

#[derive(Default)]
pub(crate) struct ChatViews {
    /// 대화로 보는 pane(바깥 pane id). 없으면 터미널이다. pane 마다 따로 기억한다.
    panes: HashMap<String, ChatPane>,
    /// 이번 프레임에 실제로 대화로 그린 pane. 휠·누름·선택 복사는 이것만 본다 — 판이 떠 진짜 터미널을
    /// 보인 프레임이면 비어 있어 손이 그대로 터미널로 간다.
    drawn: RefCell<HashMap<String, Drawn>>,
    /// `KASATERM_AUTOCHAT="6000:toggle;9000:scroll=40"` — 시각(앱 시작 기준 ms)과 걸음.
    auto: Option<Vec<(Instant, AutoStep)>>,
}

/// 한 프레임에 그린 대화 칸의 자리.
pub(crate) struct Drawn {
    /// 본문(격자) 자리 — 논리 px.
    rect: Rect,
    cell: (f32, f32),
    /// 위에서부터 이만큼 줄이 대화, 그 아래가 진짜 화면에서 옮긴 바닥이다.
    hist_rows: usize,
    /// 보이는 대화 줄마다 누르면 펼치는 묶음 열쇠.
    toggles: Vec<Option<usize>>,
    /// 그린 격자 — 바닥 줄 글을 고를 때·링크를 찾을 때 읽는다.
    rows: Vec<Vec<GridCell>>,
    /// 원본 커서가 이 격자에서 선 칸 — 커서 블록·한글 조합 글자가 여기 선다.
    cursor: Option<(usize, usize)>,
    /// 원본 커서 자리(보이든 말든) — 입력칸을 누르면 여기서부터 화살표를 센다.
    caret: Option<(usize, usize)>,
    /// 이 화면이 보인 대화 줄 범위 — 문서 선택을 화면 칸으로 옮긴다.
    view: select::View,
}

/// 렌더가 터미널 합성기에 넘길 이번 프레임의 화면.
pub(crate) struct ChatFrame {
    pub(crate) term: TerminalPane,
    /// (보이는 줄, 얼굴) — 학생 얼굴을 얹을 자리.
    pub(crate) faces: Vec<(usize, classic::Face)>,
    pub(crate) face_slug: Option<&'static str>,
    hist_rows: usize,
    toggles: Vec<Option<usize>>,
    caret: Option<(usize, usize)>,
    view: select::View,
}

pub(crate) struct ChatPane {
    feed: Arc<Mutex<Feed>>,
    open: HashSet<usize>,
    /// 맨 아래에서 위로 올라간 줄 수. 0 이면 새 줄이 오면 따라 내려간다.
    back: Cell<usize>,
    /// 지난 프레임의 총 줄 수 — 올라가 있는 동안 기록이 늘면 보던 자리를 지킨다.
    total: Cell<usize>,
    cache: RefCell<classic::Cache>,
    /// 다음 프레임에 이 칸이 맨 위에 서게 옮긴다(Info 「내 프롬프트」).
    jump: Cell<Option<usize>>,
    polled: Option<Instant>,
    painted: Cell<u64>,
    live_tab: Option<String>,
    /// 거울이면 원본 (주소, 칸). 기록을 거기서 받는다.
    remote: Option<(String, String)>,
    esc_at: Option<Instant>,
    /// 진짜 화면에서 본 코덱스 사람 말 칸 바탕 — 그 줄이 화면 밖으로 밀려도 같은 색을 쓴다.
    prompt_bg: RefCell<Option<kasa_bridge::screen::Color>>,
    /// 진짜 화면에서 본 배너 줄 — 첫 말 전 새 세션엔 기록이 없고, 말이 쌓이면 화면 밖으로 밀린다.
    banner: RefCell<Option<Vec<Vec<GridCell>>>>,
    /// 트랙패드 휠의 한 줄이 안 된 나머지 px.
    wheel_px: f32,
    /// 고른 글 — 문서 좌표라 굴려도 남는다(`select.rs`).
    sel: Option<select::ChatSel>,
    /// 입력칸 누름이 커서를 옮기는 중 — ↑/↓ 뒤 그 줄에 서면 ←/→ 로 마무리한다.
    caret_goal: Option<select::CaretGoal>,
    /// 본문 밖으로 끌 때 마지막으로 굴린 때.
    drag_tick: Option<Instant>,
}

impl ChatPane {
    fn new() -> Self {
        Self {
            feed: Arc::new(Mutex::new(Feed::default())),
            open: HashSet::new(),
            back: Cell::new(0),
            total: Cell::new(0),
            cache: RefCell::new(classic::Cache::default()),
            jump: Cell::new(None),
            polled: None,
            painted: Cell::new(0),
            live_tab: None,
            remote: None,
            esc_at: None,
            prompt_bg: RefCell::new(None),
            banner: RefCell::new(None),
            wheel_px: 0.0,
            sel: None,
            caret_goal: None,
            drag_tick: None,
        }
    }

    fn use_source(&mut self, tab: &str, remote: Option<&(String, String)>) {
        if self.live_tab.as_deref() == Some(tab) && self.remote.as_ref() == remote {
            return;
        }
        // 진행 중인 이전 읽기가 끝나도 새 탭의 통에는 쓸 수 없어야 한다.
        self.feed = Arc::default();
        self.cache = RefCell::default();
        self.banner = RefCell::default();
        self.live_tab = Some(tab.to_string());
        self.remote = remote.cloned();
        self.polled = None;
        self.painted.set(0);
        self.open.clear();
        self.back.set(0);
        self.total.set(0);
        self.jump.set(None);
        self.sel = None;
        self.caret_goal = None;
    }

    fn scroll_by(&mut self, lines: isize) {
        let next = (self.back.get() as isize + lines).max(0) as usize;
        self.back.set(next);
    }
}

#[derive(Default)]
pub(crate) struct Feed {
    path: Option<std::path::PathBuf>,
    offset: u64,
    /// `offset` 이 가리키는 파일(`TranscriptChunk::source`). 거울의 원본 칸이 다른 파일로 옮겨 가면 다르다.
    source: String,
    pub(crate) conv: parse::Conversation,
    /// 바뀔 때마다 오른다 — 화면이 이것만 보고 다시 편다.
    pub(crate) version: u64,
    pub(crate) loaded: bool,
    /// 이 pane 에 묶인 기록이 아직 없다.
    pub(crate) missing: bool,
    pub(crate) error: Option<String>,
    busy: bool,
}

/// 기록을 어디서 읽나 — 이 기기 파일, 또는 거울이면 원본 기기의 `/transcript-raw`.
enum Source {
    File(std::path::PathBuf),
    Remote(String, String),
}

impl Source {
    fn key(&self) -> std::path::PathBuf {
        match self {
            Source::File(p) => p.clone(),
            Source::Remote(base, rid) => std::path::PathBuf::from(format!("remote:{base}|{rid}")),
        }
    }

    fn read(&self, offset: u64, held: &str) -> Result<Option<kasa_socket::backend::TranscriptChunk>, String> {
        match self {
            Source::File(p) => history::read(p, offset).map(Some).map_err(|e| e.to_string()),
            Source::Remote(base, rid) => history::remote(base, rid, offset, held).map_err(|e| e.to_string()),
        }
    }
}

fn poll_feed(feed: Arc<Mutex<Feed>>, source: Option<Source>, proxy: EventLoopProxy<UserEvent>) {
    {
        let Ok(mut f) = feed.lock() else { return };
        if f.busy {
            return;
        }
        if source.is_none() {
            let changed = !f.loaded || !f.missing;
            f.loaded = true;
            f.missing = true;
            if changed {
                f.version += 1;
            }
            return;
        }
        f.busy = true;
    }
    std::thread::spawn(move || {
        let Some(source) = source else { return };
        let path = source.key();
        let (offset, same, held) = match feed.lock() {
            Ok(f) => (f.offset, f.path.as_ref() == Some(&path), f.source.clone()),
            Err(_) => return,
        };
        let chunk = if same { source.read(offset, &held) } else { source.read(0, "") };
        let Ok(mut f) = feed.lock() else { return };
        f.busy = false;
        let mut changed = !f.loaded || f.missing;
        match chunk {
            Ok(None) => {
                // 원본에 묶인 기록이 아직 없다(막 띄운 학생).
                let was = f.missing;
                f.loaded = true;
                f.missing = true;
                f.error = None;
                if !was {
                    f.version += 1;
                    drop(f);
                    let _ = proxy.send_event(UserEvent::Redraw);
                }
                return;
            }
            Ok(Some(chunk)) => {
                if !same || chunk.reset {
                    f.conv = parse::Conversation::default();
                    f.path = Some(path);
                    changed = true;
                }
                f.source = chunk.source.clone();
                if !chunk.raw.is_empty() {
                    changed = true;
                    if f.conv.apply(&chunk.raw) {
                        f.offset = chunk.offset;
                    } else {
                        // 세션이 바뀌었다 — 다음 바퀴가 새 파일을 처음부터 다시 받는다.
                        f.conv = parse::Conversation::default();
                        f.offset = 0;
                    }
                } else {
                    f.offset = chunk.offset;
                }
                f.error = None;
            }
            Err(e) => {
                if !f.loaded {
                    f.error = Some(format!("기록을 못 읽었어요 · {e}"));
                    changed = true;
                }
            }
        }
        f.loaded = true;
        f.missing = false;
        if changed {
            f.version += 1;
            drop(f);
            let _ = proxy.send_event(UserEvent::Redraw);
        }
    });
}

// ── 진짜 화면의 바닥 ─────────────────────────────────────────────────────────

fn row_text(row: &[GridCell]) -> String {
    row.iter().filter(|c| c.ch != '\0').map(|c| c.ch).collect()
}

fn full_rule(row: &[GridCell]) -> bool {
    let t = row_text(row);
    let t = t.trim();
    t.chars().count() >= 8 && t.chars().all(|c| matches!(c, '─' | '━'))
}

/// 진짜 화면에서 바닥에 붙여 옮길 줄의 시작 — 스피너·입력칸·상태줄, 입력칸 대신 선 선택지 창(승인·
/// 질문). 둘 다 못 찾으면 None: 대화 보기가 못 그리는 판이니 진짜 터미널을 보인다.
pub(crate) fn bottom_region(rows: &[Vec<GridCell>]) -> Option<usize> {
    if rows.is_empty() {
        return None;
    }
    let top = match crate::screenread::prompt_box(rows) {
        Some(crate::screenread::PromptBox::Bordered { top, .. }) => top,
        Some(crate::screenread::PromptBox::Filled { rows }) => rows.start,
        None => {
            let lines: Vec<String> = rows.iter().map(|r| row_text(r)).collect();
            parse::parse_prompt_menu(&lines)?;
            // 지금 고른 선택지 위로 가장 가까운 칸막이 가로줄이 창 머리다. 창이 화면보다 커서 머리가 밀려
            // 올라갔으면(좁은 원본의 승인 창) 화면 맨 위부터가 창이다.
            let current = lines.iter().rposition(|l| {
                let t = l.trim_start().trim_start_matches(['│', '|']).trim_start();
                t.starts_with(['❯', '›']) && t[t.chars().next().map_or(0, char::len_utf8)..].trim_start().starts_with(|c: char| c.is_ascii_digit())
            })?;
            (current.saturating_sub(40)..current).rev().find(|&r| full_rule(&rows[r])).unwrap_or(0)
        }
    };
    // 일하는 동안의 스피너 줄과 풀스크린의 「◉ effort」 줄은 입력칸 바로 위에 선다.
    let mut top = top;
    if let Some((spin, _)) = crate::screenread::find_claude_spinner(rows) {
        if spin < top && top - spin <= 4 {
            top = spin;
        }
    }
    if top > 0 && row_text(&rows[top - 1]).trim_end().ends_with("/effort") {
        top -= 1;
    }
    // 코덱스의 「• Working (5s • esc to interrupt)」 줄.
    if let Some(r) = (top.saturating_sub(4)..top).find(|&r| row_text(&rows[r]).to_ascii_lowercase().contains("esc to interrupt")) {
        top = r;
    }
    Some(top)
}

/// 진짜 화면에 보이는 하네스 배너 — claude 로고 세 줄, 코덱스 `>_ OpenAI Codex` 와 폴더 두 줄.
fn screen_banner(rows: &[Vec<GridCell>]) -> Option<Vec<Vec<GridCell>>> {
    let at = rows.iter().position(|r| {
        let t = row_text(r);
        t.contains("▐▛███▛█") || t.contains(">_ OpenAI Codex")
    })?;
    let n = if row_text(&rows[at]).contains("▐▛███▛█") { 3 } else { 2 };
    (at + n <= rows.len()).then(|| rows[at..at + n].to_vec())
}

/// 진짜 화면에 보이는 코덱스 사람 말 칸의 바탕 — 입력칸 밖에서 `›` 로 시작하고 바탕이 칸 끝까지 같은 줄.
fn codex_prompt_bg(rows: &[Vec<GridCell>], input: std::ops::Range<usize>) -> Option<kasa_bridge::screen::Color> {
    rows.iter().enumerate().filter(|(r, _)| !input.contains(r)).find_map(|(_, row)| {
        let first = row.iter().find(|c| !matches!(c.ch, ' ' | '\0'))?;
        (first.ch == '›').then(|| crate::screenread::band_bg(row)).flatten()
    })
}

/// 아직 그 줄을 못 봤을 때 — 코덱스처럼 바탕에 흰색을 16% 섞는다(어두운 바탕 실측 38,44,52 → 72,77,84).
fn codex_prompt_guess() -> kasa_bridge::screen::Color {
    let bg = theme::pane_bg();
    let light = (bg[0] as u32 + bg[1] as u32 + bg[2] as u32) > 384;
    let (top, a) = if light { (0.0, 0.06) } else { (255.0, 0.157) };
    let mix = |c: u8| (c as f32 + (top - c as f32) * a).round() as u8;
    kasa_bridge::screen::Color::Rgb(mix(bg[0]), mix(bg[1]), mix(bg[2]))
}

impl App {
    /// 대화로 볼 수 있는 pane — 지금 학생(claude·codex)이 돌고 있는 터미널 탭. 셸은 숨긴다
    /// (폰의 `_canChat` 과 같은 규칙). 거울은 원본 기기의 판정을 따른다.
    pub(crate) fn pane_can_chat(&self, ws: &Workspace, id: &str) -> bool {
        let Some(pane) = ws.panes.get(id) else { return false };
        if !pane.tabs.get(pane.active_tab).is_some_and(|t| t.term().is_some()) {
            return false;
        }
        let tab = ws.active_tab_pid(id);
        kasa_mcp::remote::cached_agent_running(&tab).unwrap_or_else(|| {
            self.pty
                .get(tab.as_str())
                .or_else(|| self.pty.get(id))
                .and_then(|p| p.active_agent())
                .is_some()
        })
    }

    pub(crate) fn chat_view_on(&self, id: &str) -> bool {
        self.chat_view.panes.contains_key(id)
    }

    /// 이번 프레임에 대화로 그려진 pane 인가 — 휠·선택을 가로챌지의 판정.
    pub(crate) fn chat_view_showing(&self, id: &str) -> bool {
        self.chat_view.drawn.borrow().contains_key(id)
    }

    pub(crate) fn toggle_chat_view(&mut self, id: &str) {
        if self.chat_view.panes.remove(id).is_some() {
            self.chat_view.drawn.borrow_mut().remove(id);
        } else {
            self.chat_view.panes.insert(id.to_string(), ChatPane::new());
            self.focus_pane(id);
        }
        self.selection = None;
        self.drag_anchor = None;
        self.chrome_dirty = true;
    }

    /// 닫힌 pane 의 대화 상태를 버리고, 대화 pane 마다 기록을 읽는다. 바뀌었으면 다시 그린다.
    pub(crate) fn pump_chat_views(&mut self) {
        if self.chat_view.panes.is_empty() {
            self.chat_view.drawn.borrow_mut().clear();
            return;
        }
        let ids: Vec<String> = self.chat_view.panes.keys().cloned().collect();
        let now = Instant::now();
        for id in ids {
            let (alive, tab) = {
                let ws = self.ws.lock().unwrap();
                (ws.panes.contains_key(&id), ws.active_tab_pid(&id))
            };
            if !alive {
                self.chat_view.panes.remove(&id);
                self.chat_view.drawn.borrow_mut().remove(&id);
                continue;
            }
            let remote = kasa_mcp::remote::remote_meta(&tab);
            let path = self.collab.hub.bound.lock().ok().and_then(|b| b.get(&tab).cloned());
            // 격리 검증 — `%1:<jsonl>` 이면 그 칸(가짜 claude)에서만, 기록이 안 묶였을 때 긴 대화 사본을 읽기만 하며 편다.
            let path = path.or_else(|| {
                let spec = std::env::var("KASATERM_CHAT_FIXTURE").ok()?;
                match spec.split_once(':').filter(|(pane, _)| pane.starts_with('%')) {
                    Some((pane, file)) => (pane == id).then(|| std::path::PathBuf::from(file)),
                    None => Some(std::path::PathBuf::from(spec)),
                }
            });
            let source = match &remote {
                Some((base, rid)) => Some(Source::Remote(base.clone(), rid.clone())),
                None => path.map(Source::File),
            };
            let proxy = self.proxy.clone();
            let Some(pane) = self.chat_view.panes.get_mut(&id) else { continue };
            pane.use_source(&tab, remote.as_ref());
            if pane.polled.is_none_or(|t| now.duration_since(t) >= POLL_EVERY) {
                pane.polled = Some(now);
                poll_feed(Arc::clone(&pane.feed), source, proxy);
            }
            let version = pane.feed.try_lock().map(|f| f.version).unwrap_or(pane.painted.get());
            if version != pane.painted.get() {
                self.chrome_dirty = true;
            }
            // 고를 때 폭이 바뀌었으면 줄을 다시 펴 줄 번호가 달라졌다.
            let cols = self.chat_view.drawn.borrow().get(&id).map(|d| d.view.cols);
            if let Some(pane) = self.chat_view.panes.get_mut(&id) {
                if pane.sel.as_ref().zip(cols).is_some_and(|(s, cols)| s.cols != cols) {
                    pane.sel = None;
                }
            }
            if self.chat_view.panes.get(&id).is_some_and(|p| p.caret_goal.is_some()) {
                self.pump_chat_caret(&id);
            }
        }
        self.pump_chat_drag_scroll();
    }

    /// 대화로 보는 칸의 이번 프레임 화면 — 위는 기록에서 편 대화, 아래는 진짜 화면의 바닥. 판이 떠
    /// 바닥을 못 찾으면 None(진짜 터미널을 그린다).
    pub(crate) fn chat_frame(&self, ws: &Workspace, pane: &PaneState, id: &str, cols: usize, rows: usize) -> Option<ChatFrame> {
        let chat = self.chat_view.panes.get(id)?;
        if !self.pane_can_chat(ws, id) || cols < 8 || rows < 4 {
            return None;
        }
        let src = pane.term()?;
        let top = bottom_region(&src.cells)?;
        // 거울은 원본 칸 폭이 이 칸과 다르다 — 바닥도 이 폭으로 다시 짓는다.
        let src_cursor = (src.cursor_row as usize).checked_sub(top).map(|r| (r, src.cursor_col as usize));
        let bottom = bottom::fit(&src.cells[top..], src.cols as usize, src_cursor, cols);
        let region = bottom.rows.len();
        if region == 0 || region as f32 > rows as f32 * REGION_MAX {
            return None;
        }
        let hist_rows = rows.saturating_sub(region).max(1);
        let tab = ws.active_tab_pid(id);
        let name = ws.pane_character.get(&tab).cloned();
        let face_slug = name.as_deref().and_then(crate::screenread::student_face_slug);
        if let Some(rows) = screen_banner(&src.cells[..top]) {
            *chat.banner.borrow_mut() = Some(rows);
        }
        let look = classic::Look {
            face: face_slug.is_some(),
            student: name.as_deref().and_then(theme::character_accent_any),
            banner: chat.banner.borrow().clone(),
        };
        let mut cache = chat.cache.borrow_mut();
        if let Ok(f) = chat.feed.try_lock() {
            cache.build(&f.conv, f.version, cols, &chat.open, &look);
            chat.painted.set(f.version);
        }
        let total = cache.lines.len();
        let max_back = total.saturating_sub(hist_rows);
        let mut back = chat.back.get();
        if back > 0 && total > chat.total.get() {
            back += total - chat.total.get();
        }
        if let Some(line) = chat.jump.take().and_then(|item| cache.line_of(item)) {
            back = total.saturating_sub(line + hist_rows);
        }
        let back = back.min(max_back);
        chat.back.set(back);
        chat.total.set(total);
        let first = total.saturating_sub(hist_rows + back);
        let shown = &cache.lines[first..(first + hist_rows).min(total)];
        let mut cells: Vec<Vec<GridCell>> = Vec::with_capacity(rows);
        let mut toggles = Vec::with_capacity(hist_rows);
        let mut faces = Vec::new();
        for (i, l) in shown.iter().enumerate() {
            let mut row = l.cells.clone();
            row.resize(cols, GridCell::blank());
            cells.push(row);
            toggles.push(l.toggle);
            if let Some(face) = l.face.filter(|f| i + f.rows as usize <= hist_rows) {
                faces.push((i, face));
            }
        }
        while cells.len() < hist_rows {
            cells.push(vec![GridCell::blank(); cols]);
            toggles.push(None);
        }
        // 코덱스 사람 말 칸 바탕은 코덱스가 터미널 바탕에서 섞어 낸 값이다 — 진짜 화면에서 읽어 둔 것을 쓴다.
        if let Some(crate::screenread::PromptBox::Filled { rows: input }) = crate::screenread::prompt_box(&src.cells) {
            if let Some(seen) = codex_prompt_bg(&src.cells, input) {
                *chat.prompt_bg.borrow_mut() = Some(seen);
            }
            let fill = chat.prompt_bg.borrow().clone().unwrap_or_else(codex_prompt_guess);
            let mark = codex::PROMPT_FILL.color();
            for c in cells.iter_mut().flatten().filter(|c| c.bg == mark) {
                c.bg = fill.clone();
            }
        }
        cells.extend(bottom.rows);
        cells.truncate(rows);
        let cursor = bottom.cursor.filter(|(r, _)| hist_rows + r < rows).map(|(r, c)| ((hist_rows + r) as u16, c as u16));
        let view = select::View { first, hist: hist_rows, total, bottom: rows.saturating_sub(hist_rows), cols };
        let term = TerminalPane {
            rows: rows as u16,
            cols: cols as u16,
            cells,
            cursor_row: cursor.map_or(0, |c| c.0),
            cursor_col: cursor.map_or(0, |c| c.1),
            cursor_visible: src.cursor_visible && cursor.is_some(),
            alt_screen: src.alt_screen,
            app_cursor: src.app_cursor,
            bracketed_paste: src.bracketed_paste,
            laid_out: true,
            ..Default::default()
        };
        let caret = cursor.map(|(r, c)| (r as usize, c as usize));
        Some(ChatFrame { term, faces, face_slug, hist_rows, toggles, caret, view })
    }

    /// 렌더가 그린 대화 칸을 적는다 — 다음 입력이 이 자리를 본다.
    pub(crate) fn note_chat_frame(&self, id: &str, frame: &ChatFrame, rect: Rect, cell: (f32, f32)) {
        self.chat_view.drawn.borrow_mut().insert(
            id.to_string(),
            Drawn {
                rect,
                cell,
                hist_rows: frame.hist_rows,
                toggles: frame.toggles.clone(),
                rows: frame.term.cells.clone(),
                cursor: frame.term.cursor_visible.then_some((frame.term.cursor_row as usize, frame.term.cursor_col as usize)),
                caret: frame.caret,
                view: frame.view,
            },
        );
    }

    /// 이번 프레임 그리기 전에 지난 자리를 비운다 — 판이 떠 터미널로 간 칸이 남지 않게.
    pub(crate) fn clear_chat_frames(&self) {
        self.chat_view.drawn.borrow_mut().clear();
    }

    /// 대화 칸이 그린 격자 — 끌어 선택한 글을 복사할 때 터미널 격자 대신 읽는다.
    pub(crate) fn chat_view_rows(&self, id: &str) -> Option<Vec<Vec<GridCell>>> {
        self.chat_view.drawn.borrow().get(id).map(|d| d.rows.clone())
    }

    /// 대화 칸에 그린 (커서 칸, 격자 폭) — 원본 격자와 크기·자리가 달라 커서를 따로 옮겨 그린다.
    pub(crate) fn chat_view_cursor(&self, id: &str) -> Option<(Option<(usize, usize)>, usize)> {
        self.chat_view.drawn.borrow().get(id).map(|d| (d.cursor, d.rows.first().map_or(0, Vec::len)))
    }

    /// 대화 칸에 그린 격자에서 그 픽셀의 (칸, 열, 행)과 보이는 격자 — 링크는 원본 글자판이 아니라 이것에서 찾는다.
    pub(crate) fn chat_view_cell(&self, x: f32, y: f32) -> Option<(String, u16, u16, Vec<Vec<GridCell>>)> {
        let drawn = self.chat_view.drawn.borrow();
        drawn.iter().find_map(|(id, d)| {
            let (rx, ry, rw, rh) = d.rect;
            if x < rx || x >= rx + rw || y < ry || y >= ry + rh || d.cell.0 <= 0.0 || d.cell.1 <= 0.0 {
                return None;
            }
            Some((id.clone(), ((x - rx) / d.cell.0) as u16, ((y - ry) / d.cell.1) as u16, d.rows.clone()))
        })
    }

    fn chat_view_at(&self, x: f32, y: f32) -> Option<(String, Option<usize>)> {
        let drawn = self.chat_view.drawn.borrow();
        drawn.iter().find_map(|(id, d)| {
            let (rx, ry, rw, rh) = d.rect;
            if x < rx || x >= rx + rw || y < ry || y >= ry + rh || d.cell.1 <= 0.0 {
                return None;
            }
            let row = ((y - ry) / d.cell.1) as usize;
            Some((id.clone(), (row < d.hist_rows).then_some(row)))
        })
    }

    /// 그 픽셀이 대화로 그린 칸 안이면 (칸, 행, 열).
    fn chat_view_hit(&self, x: f32, y: f32) -> Option<(String, usize, usize)> {
        let drawn = self.chat_view.drawn.borrow();
        drawn.iter().find_map(|(id, d)| {
            let (rx, ry, rw, rh) = d.rect;
            if x < rx || x >= rx + rw || y < ry || y >= ry + rh || d.cell.0 <= 0.0 || d.cell.1 <= 0.0 {
                return None;
            }
            let row = (((y - ry) / d.cell.1) as usize).min(d.rows.len().saturating_sub(1));
            let col = (((x - rx) / d.cell.0) as usize).min(d.view.cols.saturating_sub(1));
            Some((id.clone(), row, col))
        })
    }

    /// 리그 — 대화로 그린 격자에서 `text` 가 처음 나오는 칸 가운데 px(넓은 글자 뒤 칸은 건너 센다).
    pub(crate) fn chat_view_text_px(&self, text: &str) -> Option<(f32, f32)> {
        let want: Vec<char> = text.chars().collect();
        let drawn = self.chat_view.drawn.borrow();
        drawn.values().find_map(|d| {
            d.rows.iter().enumerate().find_map(|(r, row)| {
                let line: Vec<(usize, char)> = row.iter().enumerate().filter(|(_, c)| c.ch != '\0').map(|(i, c)| (i, c.ch)).collect();
                let at = line.windows(want.len()).position(|w| w.iter().map(|(_, ch)| *ch).eq(want.iter().copied()))?;
                let col = line[at].0;
                Some((d.rect.0 + (col as f32 + 0.5) * d.cell.0, d.rect.1 + (r as f32 + 0.5) * d.cell.1))
            })
        })
    }

    /// 리그 — `from` 이 든 대화 칸 본문 위(`top`)·아래 밖 px. 끌어 굴리기를 재현한다.
    #[cfg(target_os = "macos")]
    pub(crate) fn chat_view_edge_px(&self, from: (f32, f32), top: bool) -> Option<(f32, f32)> {
        let drawn = self.chat_view.drawn.borrow();
        let d = drawn.values().find(|d| from.0 >= d.rect.0 && from.0 < d.rect.0 + d.rect.2 && from.1 >= d.rect.1 && from.1 < d.rect.1 + d.rect.3)?;
        let y = if top { d.rect.1 - d.cell.1 * 3.0 } else { d.rect.1 + d.view.hist as f32 * d.cell.1 + d.cell.1 * 3.0 };
        Some((from.0, y))
    }

    /// 휠 — 커서 밑이 대화 칸이면 대화 줄을 굴린다(학생 앱으로 넘기지 않는다).
    pub(crate) fn chat_view_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let (cx, cy) = self.cursor_px;
        let Some((id, _)) = self.chat_view_at(cx, cy) else { return false };
        let cell_h = self.chat_view.drawn.borrow().get(&id).map_or(self.cell.h, |d| d.cell.1).max(1.0);
        let Some(pane) = self.chat_view.panes.get_mut(&id) else { return true };
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => (y * 3.0).round() as isize,
            MouseScrollDelta::PixelDelta(p) => {
                pane.wheel_px += p.y as f32;
                let n = (pane.wheel_px / cell_h).trunc();
                pane.wheel_px -= n * cell_h;
                n as isize
            }
        };
        if lines != 0 {
            pane.scroll_by(lines);
            self.chrome_dirty = true;
        }
        true
    }

    /// 대화 칸이 그려진 pane 의 키. 거의 다 학생 TUI 로 그대로 보낸다(false) — 이 보기가 먹는 것은
    /// 대화 줄 넘기기(PgUp·PgDn·⌥↑↓), 고른 글 늘이기(⇧ 화살표, 고른 글이 있을 때만)와 빈 입력칸의 두 번째 Esc 뿐이다.
    pub(crate) fn chat_view_key(&mut self, event: &KeyEvent) -> bool {
        use winit::keyboard::{Key, NamedKey};
        let Some(id) = self.target_pane().filter(|id| self.chat_view_showing(id)) else { return false };
        if event.state != ElementState::Pressed || crate::input::is_modifier_key(event) {
            return false;
        }
        let (shift, alt) = (self.modifiers.shift_key(), self.modifiers.alt_key());
        let plain = !self.modifiers.super_key() && !self.modifiers.control_key();
        if let (true, Key::Named(arrow @ (NamedKey::ArrowUp | NamedKey::ArrowDown | NamedKey::ArrowLeft | NamedKey::ArrowRight))) =
            (plain, &event.logical_key)
        {
            let step = match arrow {
                NamedKey::ArrowUp | NamedKey::ArrowDown => select::Step::Line(*arrow == NamedKey::ArrowDown),
                _ if alt => select::Step::Word(*arrow == NamedKey::ArrowRight),
                _ => select::Step::Char(*arrow == NamedKey::ArrowRight),
            };
            if shift && self.chat_view_extend(&id, step) {
                return true;
            }
            if alt && !shift && matches!(arrow, NamedKey::ArrowUp | NamedKey::ArrowDown) {
                return self.chat_view_jump_turn(&id, *arrow == NamedKey::ArrowDown);
            }
        }
        let page = self.chat_view.drawn.borrow().get(&id).map_or(10, |d| d.hist_rows.saturating_sub(2).max(1)) as isize;
        let quiet = self.chat_input_empty(&id) && !self.pane_activity.get(&id).is_some_and(|a| a.state.is_busy());
        let Some(pane) = self.chat_view.panes.get_mut(&id) else { return false };
        match &event.logical_key {
            Key::Named(NamedKey::PageUp) => pane.scroll_by(page),
            Key::Named(NamedKey::PageDown) => pane.scroll_by(-page),
            Key::Named(NamedKey::Escape) => {
                let now = Instant::now();
                let twice = pane.esc_at.is_some_and(|t| now.duration_since(t) < ESC_TWICE);
                pane.esc_at = Some(now);
                if !(twice && quiet) {
                    return false;
                }
                pane.esc_at = None;
            }
            _ => {
                // 치기 시작하면 터미널처럼 맨 아래로 돌아온다.
                pane.back.set(0);
                return false;
            }
        }
        self.selection = None;
        self.chrome_dirty = true;
        true
    }

    /// 진짜 화면의 입력칸이 비었나 — 흐린 글(제안)만 있으면 빈 것이다.
    fn chat_input_empty(&self, id: &str) -> bool {
        let Ok(ws) = self.ws.lock() else { return false };
        let Some(t) = ws.panes.get(id).and_then(|p| p.term()) else { return false };
        let Some(pb) = crate::screenread::prompt_box(&t.cells) else { return false };
        pb.rows().all(|r| {
            t.cells[r]
                .iter()
                .skip_while(|c| matches!(c.ch, ' ' | '\0'))
                .skip(1)
                .all(|c| matches!(c.ch, ' ' | '\0' | '\u{a0}') || c.dim)
        })
    }

    /// Info 「내 프롬프트」의 그 질문을 이 보기 맨 위에 세운다. `text` 는 목록 글, `nth` 는 같은 글 중 몇 번째.
    pub(crate) fn chat_view_jump(&mut self, id: &str, text: &str, nth: usize) -> bool {
        let Some(pane) = self.chat_view.panes.get(id) else { return false };
        let item = pane.feed.lock().ok().and_then(|f| {
            let own = crate::info_prompts::own_prompt_items(&f.conv.items);
            own.into_iter().filter(|(_, p)| p.text == text).nth(nth).map(|(i, _)| i)
        });
        let Some(item) = item else { return false };
        pane.jump.set(Some(item));
        self.selection = None;
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        true
    }
}

impl App {
    pub(crate) fn run_pending_autochat(&mut self) {
        if self.chat_view.auto.is_none() {
            let start = Instant::now();
            let plan = std::env::var("KASATERM_AUTOCHAT").unwrap_or_default();
            let steps = plan
                .split(';')
                .filter_map(|part| {
                    let (ms, step) = part.trim().split_once(':')?;
                    let at = start + std::time::Duration::from_millis(ms.trim().parse().ok()?);
                    let (name, arg) = step.split_once('=').unwrap_or((step, ""));
                    let step = match name.trim() {
                        "toggle" => AutoStep::Toggle(Some(arg.trim()).filter(|a| !a.is_empty()).map(str::to_string)),
                        "blocks" => AutoStep::Blocks,
                        "open" => AutoStep::OpenAll,
                        "scroll" => AutoStep::Scroll(arg.trim().parse().ok()?),
                        "zoom" => AutoStep::Zoom(arg.trim().parse().ok()?),
                        "hovermenu" => AutoStep::HoverMenu,
                        "jump" => AutoStep::Jump(arg.trim().parse().ok()?),
                        _ => return None,
                    };
                    Some((at, step))
                })
                .collect();
            self.chat_view.auto = Some(steps);
        }
        let now = Instant::now();
        let due: Vec<AutoStep> = match self.chat_view.auto.as_mut() {
            Some(steps) if steps.iter().any(|(at, _)| *at <= now) => {
                let (due, rest): (Vec<_>, Vec<_>) = std::mem::take(steps).into_iter().partition(|(at, _)| *at <= now);
                *steps = rest;
                due.into_iter().map(|(_, s)| s).collect()
            }
            _ => return,
        };
        // 거울 리그에서는 대화로 연 칸이 초점 칸이 아닐 수 있다 — 대화 칸을 먼저 고른다.
        let chat = self.chat_view.panes.keys().min().cloned();
        let Some(id) = self.target_pane().filter(|id| self.chat_view_on(id)).or(chat).or_else(|| self.target_pane()) else { return };
        for step in due {
            match step {
                AutoStep::Toggle(pane) => self.toggle_chat_view(pane.as_deref().unwrap_or(&id)),
                AutoStep::Blocks => self.toggle_shell_view(&id),
                AutoStep::OpenAll => {
                    if let Some(pane) = self.chat_view.panes.get_mut(&id) {
                        let n = pane.feed.lock().map(|f| f.conv.items.len()).unwrap_or(0);
                        pane.open.extend(0..n);
                    }
                }
                AutoStep::Scroll(lines) => {
                    if let Some(pane) = self.chat_view.panes.get(&id) {
                        pane.back.set(lines);
                    }
                }
                AutoStep::Zoom(delta) => self.change_pane_font(delta),
                AutoStep::HoverMenu => {
                    if let Some((_, r)) = self.handle_menu_hits.iter().find(|(a, _)| *a == ActionKind::ChatView) {
                        self.cursor_px = (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
                    }
                }
                AutoStep::Jump(n) => {
                    let target = self.chat_view.panes.get(&id).and_then(|p| {
                        let f = p.feed.lock().ok()?;
                        crate::info_prompts::own_prompt_items(&f.conv.items).get(n).map(|(_, p)| p.text.clone())
                    });
                    if let Some(text) = target {
                        self.chat_view_jump(&id, &text, 0);
                    }
                }
            }
            eprintln!("[autochat] step on {id}");
        }
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str) -> Vec<GridCell> {
        let mut out = Vec::new();
        for ch in text.chars() {
            let mut c = GridCell::blank();
            c.ch = ch;
            out.push(c);
            if unicode_width::UnicodeWidthChar::width(ch) == Some(2) {
                let mut spacer = GridCell::blank();
                spacer.ch = '\0';
                out.push(spacer);
            }
        }
        out
    }

    fn screen(lines: &[&str], width: usize) -> Vec<Vec<GridCell>> {
        lines
            .iter()
            .map(|l| {
                let mut r = row(l);
                r.resize(width, GridCell::blank());
                r
            })
            .collect()
    }

    #[test]
    fn the_bottom_starts_at_the_spinner_above_the_input_box() {
        let rule = "─".repeat(40);
        let s = screen(&["⏺ 답", "", "✻ Cogitating… (3s · esc to interrupt)", "", &rule, "❯ ", &rule, "  상태줄"], 40);
        assert_eq!(bottom_region(&s), Some(2));
        let idle = screen(&["⏺ 답", "", "", &rule, "❯ ", &rule, "  상태줄"], 40);
        assert_eq!(bottom_region(&idle), Some(3));
    }

    #[test]
    fn a_question_window_in_place_of_the_input_box_is_the_bottom() {
        let rule = "─".repeat(40);
        let s = screen(&["⏺ 답", "", &rule, " ☐ 색", "좋아하는 색?", "❯ 1. 빨강", "  2. 파랑", "Enter to select · Esc to cancel"], 40);
        assert_eq!(bottom_region(&s), Some(2));
    }

    #[test]
    fn a_window_taller_than_the_screen_starts_at_the_top() {
        let s = screen(&[" │ mkdir -p", "╌".repeat(40).as_str(), " Do you want to proceed?", " ❯ 1. Yes", "   2. No", "", " Esc to cancel"], 40);
        assert_eq!(bottom_region(&s), Some(0));
    }

    #[test]
    fn a_panel_without_input_or_menu_falls_back_to_the_terminal() {
        let s = screen(&["   Settings  Status   Config", "   ❯ Auto-compact      true", "   Type to filter · Esc to clear"], 40);
        assert_eq!(bottom_region(&s), None);
    }

    #[test]
    fn rules_are_redrawn_across_the_wider_pane_keeping_the_session_label() {
        let label = format!("{} 작업 이름 ─", "─".repeat(20));
        let right = format!("{}◉ xhigh", " ".repeat(25));
        let src = screen(&[&label, &right], 32);
        let fitted = bottom::fit(&src, 32, None, 50);
        let text = row_text(&fitted.rows[0]);
        assert_eq!(fitted.rows[0].len(), 50);
        assert!(text.trim_end().ends_with(" 작업 이름 ─"), "{text}");
        assert!(text.starts_with(&"─".repeat(38)), "{text}");
        assert_eq!(fitted.rows[1].iter().position(|c| c.ch == '◉'), Some(43));
    }

    #[test]
    fn a_fresh_session_shows_the_banner_seen_on_screen() {
        let rule = "─".repeat(40);
        let rows = [" ▐▛███▛█   Claude Code v2.1.299", "▝▜██████▀  Opus 5.5 with xhigh effort", " ▝▝   ▝▝   /w"];
        let s = screen(&[rows[0], rows[1], rows[2], "", "", &rule, "❯ ", &rule], 40);
        let top = bottom_region(&s).unwrap();
        let banner = screen_banner(&s[..top]).unwrap();
        let mut cache = classic::Cache::default();
        let look = classic::Look { face: false, student: None, banner: Some(banner) };
        cache.build(&parse::Conversation::default(), 1, 40, &HashSet::new(), &look);
        let got: Vec<String> = cache.lines.iter().map(|l| row_text(&l.cells).trim_end().to_string()).collect();
        assert_eq!(got, [rows[0], rows[1], rows[2], ""]);
        let codex = screen(&["", "  >_ OpenAI Codex (v0.162.1)", "     /w", ""], 40);
        assert_eq!(screen_banner(&codex).map(|b| b.len()), Some(2));
    }

    #[test]
    fn a_short_transcript_keeps_its_first_line_and_appends_whole_lines() {
        let dir = std::env::temp_dir().join(format!("kasaterm-chat-read-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        std::fs::write(&path, "{\"a\":1}\n{\"b\":2}\n").unwrap();
        let first = crate::socket::read_incremental(&path, 0).unwrap();
        assert!(first.raw.starts_with("{\"a\":1}"), "{:?}", first.raw);
        std::fs::write(&path, "{\"a\":1}\n{\"b\":2}\n{\"c\":3}\n{\"d\"").unwrap();
        let next = crate::socket::read_incremental(&path, first.offset).unwrap();
        assert_eq!(next.raw, "{\"c\":3}\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
