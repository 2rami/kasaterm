//! Info 「결과물」 — 지금 보는 칸의 대화가 낸 그림·음성·영상(`imgopen`·`mediaopen` 표시 블록). 누르면 본문 카드와 같은
//! 미리보기로 연다. 목록은 기기 캐시가 아니라 그 칸의 대화 기록에서 뽑는다(`kasa_socket::inline_outputs`) — 거울은
//! 원본 기기의 기록과 그 기기 주소를 함께 쓴다. 캐시는 만든 날부터 30일이라 그 뒤의 것은 「정리됐어요」.

use crate::{gpu, theme, UserEvent};
use kasa_socket::inline_outputs::{Kind, Output};
use std::cell::RefCell;
use std::collections::HashSet;
use std::io::{BufRead, Seek};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Rect = (f32, f32, f32, f32);

const POLL_EVERY: Duration = Duration::from_millis(2000);
/// 묶음에 펼치는 최대 줄 — 새 것부터.
const SHOWN_MAX: usize = 30;

enum Source {
    File(PathBuf),
    Remote(String, String),
}

impl Source {
    fn key(&self) -> String {
        match self {
            Source::File(p) => p.to_string_lossy().into_owned(),
            Source::Remote(base, rid) => format!("remote:{base}|{rid}"),
        }
    }
}

#[derive(Default)]
struct Feed {
    offset: u64,
    /// 거울 기록이 어느 파일의 것인지(`TranscriptChunk::source`) — 바뀌면 처음부터.
    held: String,
    seen: HashSet<String>,
    items: Vec<Output>,
    loaded: bool,
    busy: bool,
    version: u64,
}

/// 줄 단위로 끝까지 읽어 표식을 찾는다. 끝의 덜 쓴 줄은 다음에 — 표식이 반쪽으로 잘리지 않게.
fn read_file(path: &PathBuf, offset: u64, feed: &mut Feed) -> std::io::Result<()> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let mut at = offset;
    if len < offset {
        *feed = Feed { version: feed.version, ..Feed::default() };
        at = 0;
    }
    file.seek(std::io::SeekFrom::Start(at))?;
    let mut reader = std::io::BufReader::new(file);
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line)?;
        if n == 0 || line.last() != Some(&b'\n') {
            break;
        }
        at += n as u64;
        if line.windows(3).any(|w| w == b"[[k") {
            kasa_socket::inline_outputs::scan(&String::from_utf8_lossy(&line), &mut feed.seen, &mut feed.items);
        }
    }
    feed.offset = at;
    Ok(())
}

fn poll(feed: Arc<Mutex<Feed>>, source: Source, proxy: winit::event_loop::EventLoopProxy<UserEvent>) {
    {
        let Ok(mut f) = feed.lock() else { return };
        if f.busy {
            return;
        }
        f.busy = true;
    }
    std::thread::spawn(move || {
        let Ok(mut f) = feed.lock() else { return };
        let before = f.items.len();
        match &source {
            Source::File(path) => {
                let offset = f.offset;
                let _ = read_file(path, offset, &mut f);
            }
            Source::Remote(base, rid) => {
                let (offset, held) = (f.offset, f.held.clone());
                // 원본 기기 요청이 길어도 다른 줄은 기다리지 않는다 — 통을 잠근 채 받는다(이 칸만의 통이다).
                if let Ok(Some(chunk)) = crate::chat_view::history::remote(base, rid, offset, &held) {
                    if chunk.reset || chunk.source != held {
                        *f = Feed { version: f.version, busy: true, ..Feed::default() };
                    }
                    let Feed { seen, items, .. } = &mut *f;
                    kasa_socket::inline_outputs::scan(&chunk.raw, seen, items);
                    f.offset = chunk.offset;
                    f.held = chunk.source;
                }
            }
        }
        f.busy = false;
        let changed = !f.loaded || f.items.len() != before;
        f.loaded = true;
        if changed {
            f.version += 1;
            drop(f);
            let _ = proxy.send_event(UserEvent::Redraw);
        }
    });
}

#[derive(Default)]
struct State {
    /// (칸, 탭 PTY).
    seen: Option<(String, String)>,
    key: Option<String>,
    feed: Option<Arc<Mutex<Feed>>>,
    polled: Option<Instant>,
    version: u64,
    /// 새 것이 위.
    items: Vec<Output>,
    /// 사람이 펼쳐 둔 묶음 — 칸을 옮겨도 그대로 둔다.
    open: bool,
    head: Option<Rect>,
    hits: Vec<(usize, Rect)>,
}

thread_local! {
    static OUTPUTS: RefCell<State> = RefCell::new(State::default());
}

fn focused(ws: &crate::Workspace) -> Option<(String, String)> {
    let pane = ws.active_pane.clone()?;
    let p = ws.panes.get(&pane)?;
    let tab = p.tabs.get(p.active_tab)?;
    tab.term()?;
    Some((pane, tab.pid.clone()?))
}

/// 격리 검증 — 기록이 안 묶인 칸이면 대화 보기와 같은 `KASATERM_CHAT_FIXTURE=[%N:]<jsonl>` 사본을 읽는다.
fn fixture(pane: &str) -> Option<PathBuf> {
    if !crate::verification_run() {
        return None;
    }
    let spec = std::env::var("KASATERM_CHAT_FIXTURE").ok()?;
    match spec.split_once(':').filter(|(p, _)| p.starts_with('%')) {
        Some((p, file)) => (p == pane).then(|| PathBuf::from(file)),
        None => Some(PathBuf::from(spec)),
    }
}

/// 줄 이름 — 이 기기 카드면 등록 때 이름, 아니면 종류와 차례.
fn label(item: &Output, nth: usize) -> String {
    match item.kind {
        Kind::Image => format!("그림 {nth}"),
        kind => kasa_socket::inline_media::metadata(&item.id)
            .map(|m| m.name)
            .unwrap_or_else(|_| format!("{} {nth}", if kind == Kind::Audio { "음성" } else { "영상" })),
    }
}

impl crate::App {
    pub(crate) fn pump_info_outputs(&mut self) {
        let open = self.info.runtime_page() && self.git.col_visible;
        let seen = if open { self.ws.lock().ok().and_then(|ws| focused(&ws)) } else { None };
        let source = seen.as_ref().and_then(|(pane, pid)| match kasa_mcp::remote::remote_meta(pid) {
            Some((base, rid)) => Some(Source::Remote(base, rid)),
            None => self.collab.hub.bound.lock().ok()?.get(pid).cloned().or_else(|| fixture(pane)).map(Source::File),
        });
        let key = source.as_ref().map(Source::key);
        let now = Instant::now();
        let (feed, due) = OUTPUTS.with(|o| {
            let mut o = o.borrow_mut();
            if o.seen != seen || o.key != key {
                let had = !o.items.is_empty();
                *o = State { seen: seen.clone(), key: key.clone(), open: o.open, ..State::default() };
                if key.is_some() {
                    o.feed = Some(Arc::new(Mutex::new(Feed::default())));
                }
                if had {
                    self.chrome_dirty = true;
                }
            }
            let due = o.polled.is_none_or(|t| now.duration_since(t) >= POLL_EVERY);
            if due {
                o.polled = Some(now);
            }
            (o.feed.clone(), due)
        });
        let Some(feed) = feed else { return };
        if due {
            if let Some(source) = source {
                poll(Arc::clone(&feed), source, self.proxy.clone());
            }
        }
        let Ok(f) = feed.try_lock() else { return };
        if OUTPUTS.with(|o| o.borrow().version) == f.version {
            return;
        }
        let items: Vec<Output> = f.items.iter().rev().cloned().collect();
        let version = f.version;
        drop(f);
        OUTPUTS.with(|o| {
            let mut o = o.borrow_mut();
            o.version = version;
            o.items = items;
        });
        self.chrome_dirty = true;
    }

    /// 묶음 머리면 펼치고 접는다. 줄이면 그 결과물을 연다.
    pub(crate) fn info_output_press(&mut self, x: f32, y: f32) -> bool {
        let inside = |r: &Rect| x >= r.0 && x <= r.0 + r.2 && y >= r.1 && y <= r.1 + r.3;
        if OUTPUTS.with(|o| o.borrow().head.as_ref().is_some_and(inside)) {
            OUTPUTS.with(|o| {
                let mut o = o.borrow_mut();
                o.open = !o.open;
            });
            self.chrome_dirty = true;
            return true;
        }
        let hit = OUTPUTS.with(|o| {
            let o = o.borrow();
            let i = o.hits.iter().find(|(_, r)| inside(r)).map(|(i, _)| *i)?;
            Some((o.items.get(i)?.clone(), o.seen.clone()?.1))
        });
        let Some((item, pid)) = hit else { return false };
        self.open_output(&item, &pid);
        true
    }

    fn open_output(&mut self, item: &Output, pid: &str) {
        let Some(owner) = self.window.as_ref().map(|w| w.id()) else { return };
        let base = kasa_mcp::remote::remote_meta(pid).map(|(base, _)| base);
        match item.kind {
            Kind::Image => match (crate::inline_result::local_png(&item.id), base) {
                (Some(path), _) => self.image_preview.open_path(owner, path.to_string_lossy().into_owned(), &self.proxy),
                (None, Some(base)) => {
                    crate::inline_result::fetch_from(&base, &item.id, self.proxy.clone());
                    self.set_toast("원본 기기에서 그림을 받는 중이에요 — 잠시 뒤 다시 눌러 주세요".into());
                }
                (None, None) => self.set_toast("이 그림은 정리됐어요 — 결과물은 만든 날부터 30일 둬요".into()),
            },
            Kind::Audio | Kind::Video => {
                let reference = crate::inline_media::Reference { id: item.id.clone(), source: base };
                self.media_preview.open_reference(owner, reference, &self.proxy);
            }
        }
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}

/// 「내 프롬프트」 아래 묶음의 높이. 결과물이 없는 칸은 묶음을 안 세운다.
pub(crate) fn height() -> f32 {
    OUTPUTS.with(|o| {
        let o = o.borrow();
        if o.items.is_empty() {
            return 0.0;
        }
        let rows = if o.open { o.items.len().min(SHOWN_MAX) } else { 0 };
        crate::info::SEC_GAP + crate::info::DEV_H + rows as f32 * crate::info::ROW_H
    })
}

/// `docs/design.md` 「Info 탭」 — 묶음 머리 30(누르면 펼침) 아래 결과물 한 줄 22. 새 것이 위.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw(
    g: &mut gpu::GpuRenderer,
    cursor: (f32, f32),
    tips: &mut Vec<(String, Rect)>,
    x: f32,
    w: f32,
    mut y: f32,
    top: f32,
    bottom: f32,
) {
    use crate::info::{fit_text, DEV_H as SECTION_H, ROW_H, SEC_GAP};
    OUTPUTS.with(|o| {
        let mut o = o.borrow_mut();
        o.hits.clear();
        o.head = None;
        if o.items.is_empty() {
            return;
        }
        let x0 = x + 14.0;
        let right = x + w - 12.0;
        y += SEC_GAP;
        if y + SECTION_H > top && y < bottom {
            // 「모든 방」 묶음과 같은 꼴 — 머리 줄 전체가 누름 자리, 오른쪽에 「펼치기·접기」.
            let head = (x, y, w, SECTION_H);
            if crate::info::hit(cursor, &head) {
                g.rect(x, y, w, SECTION_H, theme::surface());
                g.hover_pointer = true;
            }
            let affordance = if o.open { "접기" } else { "펼치기" };
            let aw = g.measure_chrome_text(affordance, 10.0, false);
            crate::info::draw_room_head(g, &format!("결과물 · {}개", o.items.len()), x0 - 12.0, right - aw - 8.0, y + 3.0);
            g.draw_text(right - aw, y + 9.0, affordance, gpu::DrawOpts { font_size: 10.0, color: theme::text_dim(), bold: false, italic: false });
            if let Some(r) = g.clip_hit(head) {
                tips.push((if o.open { "누르면 접어요" } else { "이 칸의 대화가 낸 그림·음성·영상 — 누르면 펼쳐요" }.to_string(), r));
                o.head = Some(r);
            }
        }
        y += SECTION_H;
        if !o.open {
            return;
        }
        let total = o.items.len();
        let mut hits = Vec::new();
        for (i, item) in o.items.iter().take(SHOWN_MAX).enumerate() {
            if y + ROW_H > top && y < bottom {
                let rect = (x, y, w, ROW_H);
                if crate::info::hit(cursor, &rect) {
                    g.rect(x, y, w, ROW_H, theme::surface());
                    g.hover_pointer = true;
                }
                let icon = if item.kind == Kind::Image { "image" } else { "play" };
                g.queue_icon(icon, x0, y + (ROW_H - 12.0) / 2.0, 12.0, theme::text_dim());
                let kind = match item.kind {
                    Kind::Image => "그림",
                    Kind::Audio => "음성",
                    Kind::Video => "영상",
                };
                let kind_w = g.measure_chrome_text(kind, 10.0, false);
                g.draw_text(right - kind_w, y + 6.0, kind, gpu::DrawOpts { font_size: 10.0, color: theme::text_mute(), bold: false, italic: false });
                let tx = x0 + 18.0;
                let text = fit_text(g, &label(item, total - i), (right - tx - kind_w - 8.0).max(0.0), 11.0, false);
                g.draw_text(tx, y + 5.0, &text, gpu::DrawOpts { font_size: 11.0, color: theme::text(), bold: false, italic: false });
                if let Some(r) = g.clip_hit(rect) {
                    tips.push(("누르면 미리보기로 열어요".to_string(), r));
                    hits.push((i, r));
                }
            }
            y += ROW_H;
        }
        o.hits = hits;
    });
}
