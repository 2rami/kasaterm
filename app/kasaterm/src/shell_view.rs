//! 거울 셸 칸을 「명령 + 결과」 묶음으로 그린다 — `docs/mirror-render.md` 의 셸 갈래.
//! 원본이 모은 OSC 133 구간을 `/term/blocks`(긴 폴링)로 받아 카드로 쌓고, 결과는 이 칸
//! 폭으로 다시 접는다. 원본 PTY 크기는 안 건드린다. 키는 터미널과 같이 원본 PTY 로
//! 가고(탭 완성·기록·Ctrl-C 가 그대로 산다), 맨 아래 입력 줄은 원본 격자의 커서 줄을
//! 이 폭으로 접어 보인다.
//!
//! 셸 통합 표지가 없거나(bash·ssh 너머) 전체 화면 프로그램이 도는 동안은 카드를 접고
//! 원본 격자를 칸에 맞춰 줄여 보기만 한다(`layout.rs pane_display_scale`).
//!
//! 이 기기 셸 칸도 ⋮ 「명령으로 보기」로 같은 카드가 된다(워프처럼). 데이터는 같은 창구를
//! 제 HTTP 로 읽는다. 학생(claude·codex)이 뜨면 터미널로 돌아가고, 나가면 다시 카드다.

use super::*;

mod paint;

pub(crate) use paint::{Hit, Slot};

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

type Rect = (f32, f32, f32, f32);

/// 긴 폴링 한 번의 기다림. 원격 GET 의 10초 제한 안에 답이 오게 한다.
const WAIT_MS: u64 = 8_000;
const RETRY: std::time::Duration = std::time::Duration::from_secs(3);
const MAX_BYTES: usize = 8 << 20;

#[derive(Default)]
pub(crate) struct ShellViews {
    panes: HashMap<String, ShellPane>,
    /// 이번 프레임에 카드로 그린 pane 의 본문 자리. 휠·클릭은 이것만 본다.
    drawn: HashMap<String, Rect>,
    hits: Vec<(String, Hit, Rect)>,
    /// `/term/blocks` 를 모르는 옛 원본(404) — 그 거울은 옛 격자 보기로 남는다.
    old_sources: HashSet<String>,
    /// 명령으로 보기를 켠 이 기기 셸 칸(바깥 pane id).
    local: HashSet<String>,
}

pub(crate) struct ShellPane {
    /// 거울 링크가 걸린 탭 pid. 탭이 바뀌면 받기를 새로 연다.
    tab: String,
    feed: Arc<Mutex<Feed>>,
    stop: Arc<AtomicBool>,
    /// 맨 아래에서 위로 올라간 거리(px). 0 이면 새 출력이 오면 따라 내려간다.
    scroll: f32,
    scroll_max: f32,
    /// 펼친 블록 id.
    open: HashSet<u64>,
    /// 펼침이 바뀔 때마다 오른다 — 접어 둔 배치를 다시 하게.
    open_gen: u64,
    layout: paint::Layout,
    painted: u64,
}

impl Drop for ShellPane {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub(crate) struct Feed {
    since: Option<u64>,
    pub(crate) loaded: bool,
    pub(crate) integration: bool,
    pub(crate) alt: bool,
    pub(crate) old_source: bool,
    pub(crate) gone: bool,
    pub(crate) error: Option<String>,
    pub(crate) blocks: Vec<Block>,
    /// 바뀔 때마다 오른다 — 화면이 이것만 보고 다시 그린다.
    pub(crate) version: u64,
    /// 통째로 다시 받는 중인 블록(펼치기).
    fetching: HashSet<u64>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Block {
    pub(crate) id: u64,
    pub(crate) cmd: String,
    pub(crate) exit: Option<i32>,
    pub(crate) ms: Option<u64>,
    pub(crate) start_ms: u64,
    pub(crate) running: bool,
    pub(crate) tui: bool,
    pub(crate) dropped: usize,
    /// 가운데가 빠졌을 수 있다(`gap_at` 자리에 `gap` 줄).
    pub(crate) lines: Vec<Vec<Span>>,
    pub(crate) gap_at: Option<usize>,
    pub(crate) gap: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Span {
    pub(crate) text: String,
    pub(crate) fg: kasa_bridge::screen::Color,
    pub(crate) bg: kasa_bridge::screen::Color,
    pub(crate) flags: u8,
}

fn color_of(v: Option<&serde_json::Value>) -> kasa_bridge::screen::Color {
    use kasa_bridge::screen::Color;
    match v.and_then(serde_json::Value::as_u64) {
        None => Color::Default,
        Some(n) if n < 256 => Color::Idx(n as u8),
        Some(n) => Color::Rgb((n >> 16) as u8, (n >> 8) as u8, n as u8),
    }
}

fn parse_line(v: &serde_json::Value) -> Vec<Span> {
    v.as_array()
        .map(|spans| {
            spans
                .iter()
                .map(|s| Span {
                    text: s.get("t").and_then(|t| t.as_str()).unwrap_or_default().to_string(),
                    fg: color_of(s.get("f")),
                    bg: color_of(s.get("b")),
                    flags: s.get("s").and_then(|f| f.as_u64()).unwrap_or(0) as u8,
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn parse_block(v: &serde_json::Value) -> Option<Block> {
    let num = |k: &str| v.get(k).and_then(serde_json::Value::as_u64);
    Some(Block {
        id: num("id")?,
        cmd: v.get("cmd").and_then(|c| c.as_str()).unwrap_or_default().to_string(),
        exit: v.get("exit").and_then(|e| e.as_i64()).map(|e| e as i32),
        ms: num("ms"),
        start_ms: num("start_ms").unwrap_or(0),
        running: v.get("running").and_then(|r| r.as_bool()).unwrap_or(false),
        tui: v.get("tui").and_then(|r| r.as_bool()).unwrap_or(false),
        dropped: num("dropped").unwrap_or(0) as usize,
        lines: v
            .get("lines")
            .and_then(|l| l.as_array())
            .map(|l| l.iter().map(parse_line).collect())
            .unwrap_or_default(),
        gap_at: num("gap_at").map(|n| n as usize),
        gap: num("gap").unwrap_or(0) as usize,
    })
}

impl Feed {
    /// 다 끝난 블록 중 가장 큰 id — 그 아래 끝난 블록은 다시 안 받는다.
    fn have(&self) -> u64 {
        self.blocks.iter().filter(|b| !b.running).map(|b| b.id).max().unwrap_or(0)
    }

    /// 원본 답 하나를 합친다. 원본 셸이 새로 떠 번호가 처음부터 다시 매겨졌으면 버리고 다시 받는다.
    pub(crate) fn merge(&mut self, answer: &serde_json::Value) {
        self.since = answer.get("since").and_then(|v| v.as_u64());
        self.integration = answer.get("integration").and_then(|v| v.as_bool()).unwrap_or(false);
        self.alt = answer.get("alt").and_then(|v| v.as_bool()).unwrap_or(false);
        let oldest = answer.get("oldest").and_then(|v| v.as_u64()).unwrap_or(0);
        let incoming: Vec<Block> = answer
            .get("blocks")
            .and_then(|b| b.as_array())
            .map(|b| b.iter().filter_map(parse_block).collect())
            .unwrap_or_default();
        let newest = answer.get("newest").and_then(|v| v.as_u64()).unwrap_or(0);
        if newest < self.have() {
            self.blocks.clear();
            self.since = None;
        }
        self.blocks.retain(|b| b.id >= oldest);
        for block in incoming {
            match self.blocks.iter_mut().find(|b| b.id == block.id) {
                Some(slot) => *slot = block,
                None => self.blocks.push(block),
            }
        }
        self.blocks.sort_by_key(|b| b.id);
        self.loaded = true;
        self.error = None;
        self.version += 1;
    }
}

fn source_of(tab: &str) -> Option<(String, String)> {
    kasa_mcp::remote::remote_info(tab).map(|info| (info.base, info.remote_id)).or_else(|| {
        let port = std::env::var("KASASPACE_MCP_PORT").ok()?;
        Some((format!("http://127.0.0.1:{port}"), tab.to_string()))
    })
}

fn spawn_feed(tab: String, feed: Arc<Mutex<Feed>>, stop: Arc<AtomicBool>, proxy: EventLoopProxy<UserEvent>) {
    std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            let Some((base, remote)) = source_of(&tab) else { return };
            let (since, have) = match feed.lock() {
                Ok(f) => (f.since, f.have()),
                Err(_) => return,
            };
            let query = format!(
                "/term/blocks?pane={}&have={have}&wait={WAIT_MS}{}",
                kasa_mcp::remote::urlencode(&remote),
                since.map(|s| format!("&since={s}")).unwrap_or_default(),
            );
            let answer = kasa_mcp::remote::remote_get_json_bounded(&base, &query, MAX_BYTES);
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let Ok(mut f) = feed.lock() else { return };
            let pause = match answer {
                Ok(v) if v.get("ok").and_then(|o| o.as_bool()) == Some(true) => {
                    let before = f.since;
                    f.merge(&v);
                    f.gone = false;
                    // 도는 빌드처럼 계속 바뀌면 답이 바로바로 온다 — 숨 돌릴 틈을 둔다.
                    (before != f.since).then_some(std::time::Duration::from_millis(250))
                }
                Ok(v) => {
                    f.gone = v.get("gone").and_then(|g| g.as_bool()) == Some(true);
                    f.error = v.get("error").and_then(|e| e.as_str()).map(str::to_string);
                    f.version += 1;
                    Some(RETRY)
                }
                Err(e) => {
                    if e.to_string().contains("404") {
                        f.old_source = true;
                        f.version += 1;
                        drop(f);
                        let _ = proxy.send_event(UserEvent::Redraw);
                        return;
                    }
                    f.error = Some(format!("원본에 못 닿았어요 · {e}"));
                    f.version += 1;
                    Some(RETRY)
                }
            };
            drop(f);
            let _ = proxy.send_event(UserEvent::Redraw);
            if let Some(pause) = pause {
                std::thread::sleep(pause);
            }
        }
    });
}

/// 가운데가 빠진 긴 결과를 통째로 받아 갈아 끼운다(펼치기).
fn fetch_whole(tab: &str, id: u64, feed: Arc<Mutex<Feed>>, proxy: EventLoopProxy<UserEvent>) {
    let Some((base, remote)) = source_of(tab) else { return };
    std::thread::spawn(move || {
        let query = format!("/term/blocks?pane={}&block={id}", kasa_mcp::remote::urlencode(&remote));
        let whole = kasa_mcp::remote::remote_get_json_bounded(&base, &query, MAX_BYTES)
            .ok()
            .and_then(|v| v.get("blocks")?.as_array()?.first().and_then(parse_block));
        let Ok(mut f) = feed.lock() else { return };
        f.fetching.remove(&id);
        if let Some(whole) = whole {
            if let Some(slot) = f.blocks.iter_mut().find(|b| b.id == id) {
                *slot = whole;
            }
        }
        f.version += 1;
        drop(f);
        let _ = proxy.send_event(UserEvent::Redraw);
    });
}

impl ShellViews {
    pub(crate) fn unsupported(&self, tab: &str) -> bool {
        self.old_sources.contains(tab)
    }
}

impl App {
    /// 거울 셸 칸이 지금 카드로 그려질 수 있나 — 원본이 셸 통합 표지를 냈고 전체 화면
    /// 프로그램이 아닐 때. 아니면 격자를 줄여 보인다.
    pub(crate) fn shell_view_ready(&self, id: &str, tab: &str, alt_now: bool) -> bool {
        let shell = self.mirror_kind(tab) == crate::mirror_render::MirrorKind::Shell
            || (self.shell_view.local.contains(id) && !kasa_mcp::remote::is_view_pane(tab));
        if !shell || alt_now {
            return false;
        }
        self.shell_view.panes.get(id).is_some_and(|p| {
            p.tab == tab && p.feed.try_lock().is_ok_and(|f| !f.loaded || (f.integration && !f.alt))
        })
    }

    pub(crate) fn shell_view_showing(&self, id: &str) -> bool {
        self.shell_view.drawn.contains_key(id)
    }

    /// 명령으로 볼 수 있는 이 기기 셸 칸 — 터미널 탭이고 학생이 안 돈다. 거울 셸은 이미
    /// 카드로 그려지니 고를 것이 없다. lite 는 HTTP 창구가 없어 못 읽는다.
    pub(crate) fn pane_can_blocks(&self, ws: &Workspace, id: &str) -> bool {
        let Some(pane) = ws.panes.get(id) else { return false };
        if self.lite || !pane.tabs.get(pane.active_tab).is_some_and(|t| t.term().is_some()) {
            return false;
        }
        let tab = ws.active_tab_pid(id);
        !kasa_mcp::remote::is_view_pane(&tab) && !self.pane_can_chat(ws, id)
    }

    pub(crate) fn shell_view_on(&self, id: &str) -> bool {
        self.shell_view.local.contains(id)
    }

    pub(crate) fn toggle_shell_view(&mut self, id: &str) {
        if !self.shell_view.local.remove(id) {
            self.shell_view.local.insert(id.to_string());
            self.selection = None;
            self.drag_anchor = None;
            self.focus_pane(id);
        }
        self.chrome_dirty = true;
    }

    /// ⋮ 의 보기 전환이 이 칸에서 무엇인가 — `Some((셸 칸인가, 켜졌나))`. 학생 칸은 대화,
    /// 셸 칸은 명령 묶음이다.
    pub(crate) fn pane_view_toggle(&self, ws: &Workspace, id: &str) -> Option<(bool, bool)> {
        if self.pane_can_chat(ws, id) {
            Some((false, self.chat_view_on(id)))
        } else if self.pane_can_blocks(ws, id) {
            Some((true, self.shell_view_on(id)))
        } else {
            None
        }
    }

    pub(crate) fn toggle_pane_view(&mut self, id: &str) {
        let shell = {
            let ws = self.ws.lock().unwrap();
            self.pane_view_toggle(&ws, id).map(|(shell, _)| shell)
        };
        match shell {
            Some(true) => self.toggle_shell_view(id),
            Some(false) => self.toggle_chat_view(id),
            None => {}
        }
    }

    /// 셸 거울마다 받기를 열고, 닫히거나 셸이 아니게 된 칸은 거둔다. 받은 것이 바뀌었으면
    /// 이번 프레임을 다시 그리게 한다.
    pub(crate) fn pump_shell_views(&mut self) {
        let (mirrors, locals): (Vec<(String, String)>, Vec<(String, String)>) = {
            let ws = self.ws.lock().unwrap();
            self.shell_view.local.retain(|id| ws.panes.contains_key(id));
            let locals = self
                .shell_view
                .local
                .iter()
                .filter(|id| self.pane_can_blocks(&ws, id))
                .map(|id| (id.clone(), ws.active_tab_pid(id)))
                .collect();
            let mirrors = ws
                .panes
                .keys()
                .map(|id| (id.clone(), ws.active_tab_pid(id)))
                .filter(|(_, tab)| kasa_mcp::remote::is_view_pane(tab))
                .collect();
            (mirrors, locals)
        };
        let wanted: Vec<(String, String)> = mirrors
            .into_iter()
            .filter(|(_, tab)| self.mirror_kind(tab) == crate::mirror_render::MirrorKind::Shell)
            .chain(locals)
            .collect();
        self.shell_view
            .panes
            .retain(|id, pane| wanted.iter().any(|(w, tab)| w == id && *tab == pane.tab));
        self.shell_view.drawn.retain(|id, _| wanted.iter().any(|(w, _)| w == id));
        for (id, tab) in wanted {
            if !self.shell_view.panes.contains_key(&id) {
                let pane = ShellPane {
                    tab: tab.clone(),
                    feed: Arc::new(Mutex::new(Feed::default())),
                    stop: Arc::new(AtomicBool::new(false)),
                    scroll: 0.0,
                    scroll_max: 0.0,
                    open: HashSet::new(),
                    open_gen: 0,
                    layout: paint::Layout::default(),
                    painted: 0,
                };
                spawn_feed(tab.clone(), Arc::clone(&pane.feed), Arc::clone(&pane.stop), self.proxy.clone());
                self.shell_view.panes.insert(id.clone(), pane);
            }
            let pane = &self.shell_view.panes[&id];
            let (version, old) = pane
                .feed
                .try_lock()
                .map(|f| (f.version, f.old_source))
                .unwrap_or((pane.painted, false));
            if old && self.shell_view.old_sources.insert(tab) {
                self.chrome_dirty = true;
            }
            if version != pane.painted {
                self.chrome_dirty = true;
            }
        }
    }

    /// 원본 격자의 커서 줄(감긴 줄은 이어 붙여)을 입력 줄로 — 글자·색과 커서 칸.
    pub(crate) fn shell_view_prompt(&self, ws: &Workspace, id: &str) -> Option<(Vec<GridCell>, usize)> {
        let term = ws.panes.get(id)?.term()?;
        let row = term.cursor_row as usize;
        let mut start = row;
        while start > 0 && term.cells.get(start - 1).and_then(|r| r.last()).is_some_and(|c| c.wrapped) {
            start -= 1;
        }
        let cols = term.cols as usize;
        let mut cells: Vec<GridCell> = Vec::new();
        for r in start..=row {
            cells.extend(term.cells.get(r)?.iter().cloned());
        }
        let cursor = (row - start) * cols + term.cursor_col as usize;
        while cells.len() > cursor && cells.last().is_some_and(|c| (c.ch == ' ' || c.ch == '\0') && c.bg == kasa_bridge::screen::Color::Default) {
            cells.pop();
        }
        Some((cells, cursor))
    }

    /// 렌더 루프가 모은 셸 칸을 그린다. 이 pane 의 터미널 셀은 안 그렸다.
    pub(crate) fn paint_shell_views(g: &mut gpu::GpuRenderer, views: &mut ShellViews, slots: &[Slot], cursor: (f32, f32)) {
        views.drawn.clear();
        views.hits.clear();
        for slot in slots {
            let Some(pane) = views.panes.get_mut(&slot.pane) else { continue };
            views.drawn.insert(slot.pane.clone(), slot.rect);
            let feed = Arc::clone(&pane.feed);
            let Ok(f) = feed.try_lock() else {
                pane.painted = 0;
                continue;
            };
            pane.painted = f.version;
            let hits = paint::paint(g, cursor, slot, pane, &f);
            views.hits.extend(hits.into_iter().map(|(h, r)| (slot.pane.clone(), h, r)));
        }
    }

    fn shell_view_pane_at(&self, x: f32, y: f32) -> Option<String> {
        self.shell_view
            .drawn
            .iter()
            .find(|(_, r)| x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3)
            .map(|(id, _)| id.clone())
    }

    /// 셸 카드 칸 안의 누름. 칸 안이면 전부 삼킨다 — 밑의 격자로 선택·마우스 보고가 새면 안 된다.
    pub(crate) fn shell_view_press(&mut self, x: f32, y: f32) -> bool {
        let Some(id) = self.shell_view_pane_at(x, y) else { return false };
        self.focus_pane(&id);
        self.selection = None;
        self.drag_anchor = None;
        self.mouse_forward_pane = None;
        let hit = self
            .shell_view
            .hits
            .iter()
            .find(|(p, _, r)| *p == id && x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3)
            .map(|(_, h, _)| h.clone());
        let proxy = self.proxy.clone();
        match hit {
            Some(Hit::Fold(block)) => {
                if let Some(pane) = self.shell_view.panes.get_mut(&id) {
                    pane.open_gen += 1;
                    if !pane.open.remove(&block) {
                        pane.open.insert(block);
                        let whole_needed = pane
                            .feed
                            .lock()
                            .ok()
                            .and_then(|mut f| {
                                let gap = f.blocks.iter().any(|b| b.id == block && b.gap > 0 && !b.running);
                                (gap && f.fetching.insert(block)).then_some(())
                            })
                            .is_some();
                        if whole_needed {
                            fetch_whole(&pane.tab, block, Arc::clone(&pane.feed), proxy);
                        }
                    }
                }
            }
            Some(Hit::Copy(block)) => {
                let text = self.shell_view.panes.get(&id).and_then(|pane| {
                    let f = pane.feed.lock().ok()?;
                    let b = f.blocks.iter().find(|b| b.id == block)?;
                    let body: Vec<String> = b.lines.iter().map(|l| l.iter().map(|s| s.text.as_str()).collect()).collect();
                    Some(format!("$ {}\n{}", b.cmd, body.join("\n")))
                });
                if let Some(text) = text {
                    self.copy_to_clipboard(text, "명령과 결과를 복사했어요");
                }
            }
            Some(Hit::Bottom) => {
                if let Some(pane) = self.shell_view.panes.get_mut(&id) {
                    pane.scroll = 0.0;
                }
            }
            None => {}
        }
        self.chrome_dirty = true;
        true
    }

    /// 휠 — 커서 밑이 셸 카드 칸이면 카드를 굴린다.
    pub(crate) fn shell_view_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let (cx, cy) = self.cursor_px;
        let Some(id) = self.shell_view_pane_at(cx, cy) else { return false };
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * 54.0,
            MouseScrollDelta::PixelDelta(p) => p.y as f32,
        };
        if let Some(pane) = self.shell_view.panes.get_mut(&id) {
            let next = (pane.scroll + dy).clamp(0.0, pane.scroll_max);
            if next != pane.scroll {
                pane.scroll = next;
                self.chrome_dirty = true;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(since: u64, oldest: u64, blocks: serde_json::Value) -> serde_json::Value {
        let newest = blocks.as_array().and_then(|b| b.iter().filter_map(|b| b["id"].as_u64()).max()).unwrap_or(oldest);
        serde_json::json!({"ok": true, "since": since, "integration": true, "alt": false, "oldest": oldest, "newest": newest, "blocks": blocks})
    }

    #[test]
    fn merge_keeps_finished_blocks_and_replaces_the_running_one() {
        let mut f = Feed::default();
        f.merge(&answer(4, 1, serde_json::json!([
            {"id": 1, "cmd": "ls", "running": false, "exit": 0, "ms": 3, "count": 1, "lines": [[{"t": "a", "f": 4}]]},
            {"id": 2, "cmd": "make", "running": true, "count": 1, "lines": [[{"t": "cc"}]]},
        ])));
        assert_eq!(f.have(), 1);
        assert_eq!(f.blocks[0].lines[0][0].fg, kasa_bridge::screen::Color::Idx(4));
        f.merge(&answer(6, 1, serde_json::json!([
            {"id": 2, "cmd": "make", "running": false, "exit": 2, "ms": 900, "count": 2, "lines": [[{"t": "cc"}], [{"t": "error", "f": 1}]]},
        ])));
        assert_eq!(f.blocks.len(), 2);
        assert_eq!(f.blocks[1].exit, Some(2));
        assert_eq!(f.have(), 2);
        // 바뀜 없이 기다림이 끝난 빈 답은 가진 것을 지우지 않는다.
        f.merge(&serde_json::json!({"ok": true, "since": 6, "integration": true, "oldest": 1, "newest": 2, "blocks": []}));
        assert_eq!(f.blocks.len(), 2);
    }

    #[test]
    fn merge_drops_evicted_and_restarts_after_a_new_shell() {
        let mut f = Feed::default();
        f.merge(&answer(9, 3, serde_json::json!([
            {"id": 3, "cmd": "a", "running": false, "count": 0, "lines": []},
            {"id": 4, "cmd": "b", "running": false, "count": 0, "lines": []},
        ])));
        f.merge(&answer(11, 4, serde_json::json!([{"id": 5, "cmd": "c", "running": false, "count": 0, "lines": []}])));
        assert_eq!(f.blocks.iter().map(|b| b.id).collect::<Vec<_>>(), vec![4, 5]);
        f.merge(&answer(2, 1, serde_json::json!([{"id": 1, "cmd": "fresh", "running": true, "count": 0, "lines": []}])));
        assert_eq!(f.blocks.iter().map(|b| b.cmd.as_str()).collect::<Vec<_>>(), vec!["fresh"]);
    }

    #[test]
    fn truecolor_numbers_unpack() {
        let line = parse_line(&serde_json::json!([{"t": "x", "f": 0x1000000 | 0x0a0b0c, "b": 236, "s": 1}]));
        assert_eq!(line[0].fg, kasa_bridge::screen::Color::Rgb(10, 11, 12));
        assert_eq!(line[0].bg, kasa_bridge::screen::Color::Idx(236));
        assert_eq!(line[0].flags, 1);
    }
}
