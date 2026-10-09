//! 학생 pane 을 대화로 보기 — 폰의 「터미널 | 대화」 전환(`mobile/lib/screens/terminal.dart`)의
//! PC판. 격자 대신 그 pane 의 기록(jsonl)을 말풍선으로 펴고, 아래 입력칸에서 한 번에 보낸다.
//! PTY 는 그대로 살아 있다 — 보기만 바꾸므로 ⋮ 에서 언제든 터미널로 돌아간다.
//!
//! 데이터는 화면이 아니라 기록에서 온다. 화면 글로 말풍선을 만들면 하네스 UI 가 바뀔
//! 때마다 깨지고, 스크롤로 밀려난 말은 되살릴 수 없다. 화면에서 읽는 것은 지금 떠 있는
//! 선택지(승인·질문)뿐이다 — 그건 기록에 답이 오기 전엔 화면에만 있다.

use super::*;

mod live;
mod paint;
pub(crate) mod parse;

pub(crate) use paint::{Hit, Slot};

use std::collections::HashSet;
use std::sync::Mutex;

type Rect = (f32, f32, f32, f32);

/// 기록을 읽는 간격. 폰과 같다 — 더 잦으면 긴 턴에서 다시 펴는 값만 늘어난다.
const POLL_EVERY: std::time::Duration = std::time::Duration::from_millis(1000);
/// 화면에서 선택지를 다시 읽는 간격.
const MENU_EVERY: std::time::Duration = std::time::Duration::from_millis(250);
/// 고른 직후엔 화면이 아직 옛 메뉴를 그리고 있다 — 두 번 누르지 않게 잠깐 접는다.
const MENU_HOLD: std::time::Duration = std::time::Duration::from_millis(1200);

/// 격리 검증용 자동 조작 한 걸음(`KASATERM_AUTOCHAT`).
enum AutoStep {
    Toggle,
    /// 셸 칸의 명령으로 보기 전환(`shell_view.rs`).
    Blocks,
    Draft(String),
    Send,
    Pick(usize),
    /// 화면 선택지 없이 선 승인 카드의 허락·거절.
    Decide(bool),
    OpenAll,
    Scroll(f32),
    /// 그 칸만 확대(⌘⇧+ 와 같은 길) — 배율을 이만큼 더한다.
    Zoom(f32),
    /// ⋮ 메뉴의 대화 보기 칸 위에 커서를 둔다(툴팁 확인).
    HoverMenu,
}

#[derive(Default)]
pub(crate) struct ChatViews {
    /// 대화로 보는 pane(바깥 pane id). 없으면 터미널이다. pane 마다 따로 기억한다.
    panes: HashMap<String, ChatPane>,
    /// 사람이 터미널로 돌려 둔 거울 — 대화형 기본(`mirror_render`)이 다시 열지 않는다.
    pub(crate) declined: HashSet<String>,
    pub(crate) mirror_checked: Option<Instant>,
    /// 이번 프레임에 실제로 대화로 그린 pane 의 본문 자리. 입력·휠·클릭은 이것만 본다 —
    /// 대화 모드여도 학생이 나가 셸만 남았으면 터미널을 그리고, 그때 키는 셸로 가야 한다.
    drawn: HashMap<String, Rect>,
    hits: Vec<(String, Hit, Rect)>,
    /// `KASATERM_AUTOCHAT="6000:toggle;9000:draft=글;9500:send"` — 시각(앱 시작 기준 ms)과 걸음.
    /// 키 입력을 흉내 낼 길이 없어 입력칸·선택지·펼치기를 직접 건드린다.
    auto: Option<Vec<(Instant, AutoStep)>>,
}

pub(crate) struct ChatPane {
    feed: Arc<Mutex<Feed>>,
    draft: String,
    cursor: usize,
    /// 맨 아래에서 위로 올라간 거리(px). 0 이면 새 말이 오면 따라 내려간다.
    scroll: f32,
    scroll_max: f32,
    open: HashSet<usize>,
    layout: paint::Layout,
    menu: Option<parse::PromptMenu>,
    menu_read: Option<Instant>,
    menu_hold: Option<Instant>,
    polled: Option<Instant>,
    /// 마지막으로 그린 기록 판 — 다르면 다음 프레임을 다시 그린다.
    painted: u64,
    /// 그 칸 mod 의 지금(일 상태·도는 도구·승인 요청).
    live: Arc<Mutex<live::ModLive>>,
    live_read: Option<Instant>,
    /// 마지막으로 그린 mod 판.
    live_painted: Option<live::ModLive>,
    /// 거울이면 원본 (주소, 칸). 기록과 mod 사실을 거기서 받는다.
    remote: Option<(String, String)>,
    watching: Option<(String, String)>,
    /// mod 결정을 못 보낸 요청 — 그 요청은 화면 키로 고른다.
    refused: Arc<Mutex<HashSet<String>>>,
    /// 거울 입력이 원본에서 거절된 까닭. 다음 보내기가 지운다.
    pub(crate) send_error: Arc<Mutex<Option<String>>>,
}

impl ChatPane {
    fn new() -> Self {
        Self {
            feed: Arc::new(Mutex::new(Feed::default())),
            draft: String::new(),
            cursor: 0,
            scroll: 0.0,
            scroll_max: 0.0,
            open: HashSet::new(),
            layout: paint::Layout::default(),
            menu: None,
            menu_read: None,
            menu_hold: None,
            polled: None,
            painted: 0,
            live: Arc::new(Mutex::new(live::ModLive::default())),
            live_read: None,
            live_painted: None,
            remote: None,
            watching: None,
            refused: Arc::default(),
            send_error: Arc::default(),
        }
    }

    /// 그릴·고를 때 보는 mod 의 지금. 결정을 못 보낸 요청은 뺀다.
    pub(crate) fn mod_live(&self) -> live::ModLive {
        let mut l = self.live.lock().map(|l| l.clone()).unwrap_or_default();
        if let Ok(refused) = self.refused.lock() {
            l.permissions.retain(|p| !refused.contains(&p.id));
        }
        l
    }
}

#[derive(Default)]
pub(crate) struct Feed {
    path: Option<std::path::PathBuf>,
    offset: u64,
    pub(crate) conv: parse::Conversation,
    /// 바뀔 때마다 오른다 — 화면이 이것만 보고 다시 편다.
    pub(crate) version: u64,
    /// 대화를 처음부터 다시 받은 횟수. 오르면 펼친 칸 기억을 버린다(번호가 새로 매겨진다).
    pub(crate) generation: u64,
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

    fn read(&self, offset: u64) -> Result<Option<kasa_socket::backend::TranscriptChunk>, String> {
        match self {
            Source::File(p) => crate::socket::read_incremental(p, offset).map(Some).map_err(|e| e.to_string()),
            Source::Remote(base, rid) => live::remote_chunk(base, rid, offset)
                .map(|c| c.map(|(raw, offset, reset)| kasa_socket::backend::TranscriptChunk { raw, offset, reset }))
                .map_err(|e| e.to_string()),
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
        let (offset, same) = match feed.lock() {
            Ok(f) => (f.offset, f.path.as_ref() == Some(&path)),
            Err(_) => return,
        };
        let chunk = source.read(if same { offset } else { 0 });
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
                    f.generation += 1;
                    changed = true;
                }
                if !chunk.raw.is_empty() {
                    changed = true;
                    if f.conv.apply(&chunk.raw) {
                        f.offset = chunk.offset;
                    } else {
                        // 세션이 바뀌었다 — 다음 바퀴가 새 파일의 꼬리부터 다시 받는다.
                        f.conv = parse::Conversation::default();
                        f.offset = 0;
                        f.generation += 1;
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

/// 입력칸 글을 PTY 로 보낼 바이트. 여러 줄이면 붙여넣기로 감싸야 줄바꿈이 제출로 안 읽힌다.
/// 맨 끝의 CR 은 소켓 전달(`SocketBytes`)이 따로 늦춰 보내 Ink 가 Enter 로 읽는다.
fn submit_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let body = if text.contains('\n') && bracketed {
        format!("\x1b[200~{text}\x1b[201~")
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r")
    };
    format!("{body}\r").into_bytes()
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

    /// 이번 프레임에 대화로 그려진 pane 인가 — 입력을 가로챌지의 판정.
    pub(crate) fn chat_view_showing(&self, id: &str) -> bool {
        self.chat_view.drawn.contains_key(id)
    }

    pub(crate) fn toggle_chat_view(&mut self, id: &str) {
        if self.chat_view.panes.remove(id).is_some() {
            self.chat_view.drawn.remove(id);
            self.chat_view.declined.insert(id.to_string());
            if matches!(&self.ime_focus, Some(crate::ImeFocus::Chat(p)) if p == id) {
                self.handoff_ime_to_active_surface();
            }
        } else {
            self.chat_view.declined.remove(id);
            self.chat_view.panes.insert(id.to_string(), ChatPane::new());
            self.selection = None;
            self.drag_anchor = None;
            self.mouse_forward_pane = None;
            self.focus_pane(id);
            self.ime_retarget(crate::ImeFocus::Chat(id.to_string()));
        }
        self.chrome_dirty = true;
    }

    /// 초점·IME 는 그대로 두고 대화로 연다 — 거울의 대화형 기본(`mirror_render`).
    pub(crate) fn open_chat_view_quietly(&mut self, id: &str) {
        if !self.chat_view.panes.contains_key(id) {
            self.chat_view.panes.insert(id.to_string(), ChatPane::new());
            self.chrome_dirty = true;
        }
    }

    /// 닫힌 pane 의 대화 상태를 버리고, 대화 pane 마다 기록과 화면 선택지를 읽는다.
    /// 그릴 것이 바뀌었으면 이번 프레임을 다시 그리게 한다.
    pub(crate) fn pump_chat_views(&mut self) {
        self.open_mirror_chats();
        if self.chat_view.panes.is_empty() {
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
                self.chat_view.drawn.remove(&id);
                continue;
            }
            let showing = self.chat_view.drawn.contains_key(&id);
            let remote = [tab.as_str(), id.as_str()]
                .into_iter()
                .find(|p| kasa_mcp::remote::is_view_pane(p))
                .and_then(kasa_mcp::remote::remote_meta);
            let path = self
                .collab
                .hub
                .bound
                .lock()
                .ok()
                .and_then(|b| b.get(&tab).or_else(|| b.get(&id)).cloned());
            // 격리 검증 — 로그인 안 된 리그에서도 도구·코드·표가 섞인 대화를 그려 본다.
            #[cfg(debug_assertions)]
            let path = std::env::var_os("KASATERM_CHAT_FIXTURE").map(std::path::PathBuf::from).or(path);
            let source = match &remote {
                Some((base, rid)) => Some(Source::Remote(base.clone(), rid.clone())),
                None => path.map(Source::File),
            };
            let menu_lines = {
                let pane = &self.chat_view.panes[&id];
                showing
                    && pane.menu_read.is_none_or(|t| now.duration_since(t) >= MENU_EVERY)
            }
            .then(|| self.pty_for_pane(&id).map(|p| p.visible_text(60)))
            .flatten();
            let proxy = self.proxy.clone();
            let Some(pane) = self.chat_view.panes.get_mut(&id) else { continue };
            if pane.polled.is_none_or(|t| now.duration_since(t) >= POLL_EVERY) {
                pane.polled = Some(now);
                poll_feed(Arc::clone(&pane.feed), source, proxy.clone());
            }
            pane.remote = remote.clone();
            match &remote {
                Some(r) if pane.watching.as_ref() != Some(r) => {
                    pane.watching = Some(r.clone());
                    live::watch_remote(Arc::downgrade(&pane.live), r.0.clone(), r.1.clone(), proxy);
                }
                Some(_) => {}
                None if pane.live_read.is_none_or(|t| now.duration_since(t) >= MENU_EVERY) => {
                    pane.live_read = Some(now);
                    let next = live::ModLive::from_json(&kasa_mcp::claude_mod::mirror_view(&tab));
                    if let Ok(mut l) = pane.live.lock() {
                        *l = next;
                    }
                }
                None => {}
            }
            if pane.live.try_lock().is_ok_and(|l| pane.live_painted.as_ref() != Some(&*l)) {
                self.chrome_dirty = true;
            }
            if let Some(text) = menu_lines {
                pane.menu_read = Some(now);
                let held = pane.menu_hold.is_some_and(|t| now < t);
                let lines: Vec<String> = text.lines().map(str::to_string).collect();
                let menu = if held { None } else { parse::parse_prompt_menu(&lines) };
                if menu != pane.menu {
                    pane.menu = menu;
                    self.chrome_dirty = true;
                }
            }
            let version = pane.feed.try_lock().map(|f| f.version).unwrap_or(pane.painted);
            if version != pane.painted {
                self.chrome_dirty = true;
            }
        }
    }

    /// 렌더 루프가 모은 대화 칸을 그린다. 터미널 셀은 이 pane 에서 안 그렸다.
    pub(crate) fn paint_chat_views(g: &mut gpu::GpuRenderer, views: &mut ChatViews, slots: &[Slot], cursor: (f32, f32)) {
        views.drawn.clear();
        views.hits.clear();
        for slot in slots {
            let Some(pane) = views.panes.get_mut(&slot.pane) else { continue };
            views.drawn.insert(slot.pane.clone(), slot.rect);
            let feed = Arc::clone(&pane.feed);
            let z = slot.zoom;
            g.begin_zoom(z);
            let hits = match feed.try_lock() {
                Ok(f) => {
                    pane.painted = f.version;
                    paint::paint(g, cursor, slot, pane, Some(&f))
                }
                Err(_) => paint::paint(g, cursor, slot, pane, None),
            };
            g.end_zoom(z);
            views.hits.extend(hits.into_iter().map(|(h, r)| (slot.pane.clone(), h, (r.0 * z, r.1 * z, r.2 * z, r.3 * z))));
        }
    }

    pub(crate) fn chat_view_pane_at(&self, x: f32, y: f32) -> Option<String> {
        self.chat_view
            .drawn
            .iter()
            .find(|(_, r)| x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3)
            .map(|(id, _)| id.clone())
    }

    /// 대화 칸 안의 누름. 칸 안이면 전부 삼킨다 — 밑의 격자로 선택·마우스 보고가 새면 안 된다.
    pub(crate) fn chat_view_press(&mut self, x: f32, y: f32) -> bool {
        let Some(id) = self.chat_view_pane_at(x, y) else { return false };
        self.focus_pane(&id);
        self.selection = None;
        self.drag_anchor = None;
        self.mouse_forward_pane = None;
        self.ime_retarget(crate::ImeFocus::Chat(id.clone()));
        let hit = self
            .chat_view
            .hits
            .iter()
            .find(|(p, _, r)| *p == id && x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3)
            .map(|(_, h, _)| h.clone());
        match hit {
            Some(Hit::Fold(key)) | Some(Hit::Tool(key)) => {
                if let Some(pane) = self.chat_view.panes.get_mut(&id) {
                    if !pane.open.remove(&key) {
                        pane.open.insert(key);
                    }
                    pane.layout.invalidate();
                }
            }
            Some(Hit::Copy(item)) => {
                let text = self.chat_view.panes.get(&id).and_then(|pane| {
                    let f = pane.feed.lock().ok()?;
                    match f.conv.items.get(item)? {
                        parse::Item::Bubble { text, .. } => Some(text.clone()),
                        _ => None,
                    }
                });
                if let Some(text) = text {
                    self.copy_to_clipboard(text, "복사했어요");
                }
            }
            Some(Hit::Send) => self.chat_view_submit(&id),
            Some(Hit::Stop) => self.chat_send_raw(&id, b"\x1b"),
            Some(Hit::Terminal) => self.toggle_chat_view(&id),
            Some(Hit::Bottom) => {
                if let Some(pane) = self.chat_view.panes.get_mut(&id) {
                    pane.scroll = 0.0;
                }
            }
            Some(Hit::Pick(i)) => self.chat_view_pick(&id, i),
            Some(Hit::Decide(allow)) => {
                let live = self.chat_view.panes.get(&id).map(ChatPane::mod_live).unwrap_or_default();
                if let Some(p) = live.permissions.first() {
                    self.chat_view_decide(&id, &p.id, &live.session, allow);
                }
            }
            Some(Hit::Dismiss) => {
                self.chat_send_raw(&id, b"\x1b");
                if let Some(pane) = self.chat_view.panes.get_mut(&id) {
                    pane.menu = None;
                    pane.menu_hold = Some(Instant::now() + MENU_HOLD);
                }
            }
            Some(Hit::Composer) => {
                if let Some(pane) = self.chat_view.panes.get_mut(&id) {
                    pane.cursor = pane.draft.chars().count();
                }
            }
            None => {}
        }
        self.chrome_dirty = true;
        true
    }

    /// 화면 선택지의 한 칸을 고른다 — 지금 커서 자리에서 그만큼 ↑↓ 옮기고 Enter. 번호
    /// 단축키는 AskUserQuestion 의 다중 선택에서 뜻이 달라 쓰지 않는다.
    fn chat_view_pick(&mut self, id: &str, i: usize) {
        let Some(menu) = self.chat_view.panes.get(id).and_then(|p| p.menu.clone()) else { return };
        if i >= menu.options.len() {
            return;
        }
        // 승인 창의 「Yes」·「No」는 요청 id 로 답한다 — 화면 키는 그사이 다른 창이 뜨면 그것을 고른다.
        let live = self.chat_view.panes.get(id).map(ChatPane::mod_live).unwrap_or_default();
        if let Some((allow, p)) = live::decision_for(&menu, &live, i) {
            self.chat_view_decide(id, &p.id, &live.session, allow);
            return;
        }
        let delta = i as i64 - menu.cursor() as i64;
        let arrow: &[u8] = if delta > 0 { b"\x1b[B" } else { b"\x1b[A" };
        let mut bytes = arrow.repeat(delta.unsigned_abs() as usize);
        bytes.push(b'\r');
        self.chat_send_raw(id, &bytes);
        if let Some(pane) = self.chat_view.panes.get_mut(id) {
            pane.menu = None;
            pane.menu_hold = Some(Instant::now() + MENU_HOLD);
            pane.scroll = 0.0;
        }
    }

    /// 승인 요청에 mod 결정으로 답한다(원격 승인 계약과 같은 요청 id). 거절이면 입력칸 글이 까닭이다.
    /// 결정은 뒤에서 보낸다 — 거울이면 원본까지 네트워크를 탄다. 못 보내면 그 요청은 화면 키로 고르게 남긴다.
    fn chat_view_decide(&mut self, id: &str, request: &str, session: &str, allow: bool) {
        let tab = self.ws.lock().unwrap().active_tab_pid(id);
        let proxy = self.proxy.clone();
        let Some(pane) = self.chat_view.panes.get_mut(id) else { return };
        let message = if allow {
            String::new()
        } else {
            pane.cursor = 0;
            std::mem::take(&mut pane.draft).trim().to_string()
        };
        pane.menu = None;
        pane.menu_hold = Some(Instant::now() + MENU_HOLD);
        pane.scroll = 0.0;
        let remote = pane.remote.clone();
        let surface = remote.as_ref().map_or(tab, |r| r.1.clone());
        let refused = Arc::clone(&pane.refused);
        let (request, session) = (request.to_string(), session.to_string());
        std::thread::spawn(move || {
            if let Err(e) = live::decide(remote.as_ref(), &surface, &session, &request, allow, &message) {
                eprintln!("[chat-view] 승인 결정을 못 보냈어요 {request}: {e}");
                if let Ok(mut r) = refused.lock() {
                    r.insert(request);
                }
                let _ = proxy.send_event(UserEvent::Redraw);
            }
        });
        self.chrome_dirty = true;
    }

    /// 키 그대로(Esc·화살표) — 붙여넣기로 감싸지 않는다. 바깥 pane id 를 지금 탭으로 바꿔 보낸다.
    fn chat_send_raw(&self, id: &str, bytes: &[u8]) {
        let tab = self.ws.lock().unwrap().active_tab_pid(id);
        self.send_bytes_to_surface(Some(&tab), bytes);
    }

    fn chat_view_submit(&mut self, id: &str) {
        if let Some(flushed) = self.hangul.flush() {
            self.chat_view_insert(id, &flushed);
        }
        self.preedit.clear();
        self.in_preedit = false;
        let Some(pane) = self.chat_view.panes.get_mut(id) else { return };
        let text = pane.draft.trim().to_string();
        if text.is_empty() {
            return;
        }
        pane.draft.clear();
        pane.cursor = 0;
        pane.scroll = 0.0;
        // 보낸 직후 한 번 더 읽는다 — 내 말이 기다림 없이 말풍선으로 선다.
        pane.polled = None;
        // 거울은 원본이 넣는다 — 안전한 tell 로 입력칸이 빌 때 붙여넣고, 일하는 칸이면 진행 중인 턴 안으로 든다
        // (`/term/chat-send`, docs/mirror-render.md).
        if let Some((base, rid)) = pane.remote.clone() {
            let failed = Arc::clone(&pane.send_error);
            if let Ok(mut f) = failed.lock() {
                *f = None;
            }
            let proxy = self.proxy.clone();
            std::thread::spawn(move || {
                let body = serde_json::json!({ "surface": rid, "text": text });
                let error = match kasa_mcp::remote::remote_post_json(&base, "/term/chat-send", &body) {
                    Ok(v) if v["ok"] == false => Some(v["error"].as_str().unwrap_or("거절").to_string()),
                    Ok(_) => None,
                    Err(e) => Some(e.to_string()),
                };
                if let Some(e) = error {
                    eprintln!("[chat-view] 거울 입력을 못 보냈어요: {e}");
                    if let Ok(mut f) = failed.lock() {
                        *f = Some(e);
                    }
                    let _ = proxy.send_event(UserEvent::Redraw);
                }
            });
            return;
        }
        let bracketed = {
            let ws = self.ws.lock().unwrap();
            ws.panes
                .get(id)
                .and_then(|p| p.tabs.get(p.active_tab))
                .and_then(|t| t.term())
                .is_some_and(|t| t.bracketed_paste)
        };
        let _ = self.proxy.send_event(UserEvent::SocketBytes(Some(id.to_string()), submit_bytes(&text, bracketed)));
    }

    pub(crate) fn chat_view_insert(&mut self, id: &str, text: &str) {
        if let Some(pane) = self.chat_view.panes.get_mut(id) {
            crate::lineedit::insert(&mut pane.draft, &mut pane.cursor, text);
            self.chrome_dirty = true;
        }
    }

    /// 대화 칸이 붙잡은 pane 의 키. 처리했으면 true — 입력칸에 쓰던 글이 셸로 새면 안 된다.
    pub(crate) fn chat_view_key(&mut self, event: &KeyEvent) -> bool {
        use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
        let Some(id) = self.target_pane().filter(|id| self.chat_view_showing(id)) else { return false };
        if event.state != ElementState::Pressed || crate::input::is_modifier_key(event) {
            return true;
        }
        self.last_input_at = Instant::now();
        // 앱 단축키(새 탭·분할·방 이동)는 대화 보기에서도 산다 — 붙여넣기만 입력칸이 받는다.
        if self.modifiers.super_key() || self.modifiers.control_key() {
            if self.host_mod() && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyV)) {
                return self.chat_view_paste();
            }
            return false;
        }
        if matches!(event.logical_key, Key::Named(NamedKey::Enter)) {
            if self.modifiers.shift_key() {
                if let Some(flushed) = self.hangul.flush() {
                    self.chat_view_insert(&id, &flushed);
                }
                self.preedit.clear();
                self.in_preedit = false;
                self.chat_view_insert(&id, "\n");
            } else {
                self.chat_view_submit(&id);
            }
            return true;
        }
        self.ime_retarget(crate::ImeFocus::Chat(id.clone()));
        #[cfg(target_os = "macos")]
        if let Some(t) = &event.text {
            if let Some(c) = t.chars().next().filter(|_| t.chars().count() == 1) {
                if (0x3130..=0x318F).contains(&(c as u32)) {
                    if let Some(done) = self.hangul.feed(c) {
                        self.chat_view_insert(&id, &done);
                    }
                    self.preedit = self.hangul.preedit().unwrap_or_default();
                    self.in_preedit = !self.preedit.is_empty();
                    self.chrome_dirty = true;
                    return true;
                }
            }
        }
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) && self.hangul.backspace() {
            self.preedit = self.hangul.preedit().unwrap_or_default();
            self.in_preedit = !self.preedit.is_empty();
            self.chrome_dirty = true;
            return true;
        }
        if let Some(flushed) = self.hangul.flush() {
            self.chat_view_insert(&id, &flushed);
        }
        self.preedit.clear();
        self.in_preedit = false;
        let empty = self.chat_view.panes.get(&id).is_none_or(|p| p.draft.is_empty());
        // 빈 칸의 Esc 는 터미널에서처럼 학생을 멈춘다. 쓰던 글이 있으면 글을 지키고 아무 일도 안 한다.
        if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
            if empty {
                self.chat_send_raw(&id, b"\x1b");
            }
            return true;
        }
        if let Some(pane) = self.chat_view.panes.get_mut(&id) {
            if matches!(event.logical_key, Key::Named(NamedKey::PageUp | NamedKey::PageDown)) {
                let step = self.chat_view.drawn.get(&id).map_or(300.0, |r| r.3 * 0.8);
                let up = matches!(event.logical_key, Key::Named(NamedKey::PageUp));
                pane.scroll = (pane.scroll + if up { step } else { -step }).clamp(0.0, pane.scroll_max);
            } else {
                crate::lineedit::key(&mut pane.draft, &mut pane.cursor, &event.logical_key);
            }
        }
        self.chrome_dirty = true;
        true
    }

    /// 플랫폼 IME(윈도·리눅스)의 조합·확정. 대화 입력칸이 주인일 때만 받는다.
    pub(crate) fn chat_view_ime(&mut self, ime: &winit::event::Ime) -> bool {
        use winit::event::Ime;
        let Some(id) = self.target_pane().filter(|id| self.chat_view_showing(id)) else { return false };
        match ime {
            Ime::Preedit(text, _) => {
                self.ime_focus = Some(crate::ImeFocus::Chat(id));
                self.in_preedit = !text.is_empty();
                self.preedit = text.clone();
            }
            Ime::Commit(text) => {
                self.in_preedit = false;
                self.preedit.clear();
                self.chat_view_insert(&id, text);
            }
            Ime::Enabled | Ime::Disabled => return false,
        }
        self.chrome_dirty = true;
        true
    }

    /// 붙여넣기 — 대화 칸이면 입력칸으로. 그림은 터미널 보기에서 붙인다.
    pub(crate) fn chat_view_paste(&mut self) -> bool {
        let Some(id) = self.target_pane().filter(|id| self.chat_view_showing(id)) else { return false };
        if let Ok(text) = arboard::Clipboard::new().and_then(|mut cb| cb.get_text()) {
            if let Some(flushed) = self.hangul.flush() {
                self.chat_view_insert(&id, &flushed);
            }
            self.preedit.clear();
            self.in_preedit = false;
            self.chat_view_insert(&id, &text.replace("\r\n", "\n"));
        }
        true
    }

    /// 휠 — 커서 밑이 대화 칸이면 대화를 굴린다.
    pub(crate) fn chat_view_wheel(&mut self, delta: MouseScrollDelta) -> bool {
        let (cx, cy) = self.cursor_px;
        let Some(id) = self.chat_view_pane_at(cx, cy) else { return false };
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * 54.0,
            MouseScrollDelta::PixelDelta(p) => p.y as f32,
        };
        if let Some(pane) = self.chat_view.panes.get_mut(&id) {
            let next = (pane.scroll + dy).clamp(0.0, pane.scroll_max);
            if next != pane.scroll {
                pane.scroll = next;
                self.chrome_dirty = true;
            }
        }
        true
    }

    /// 렌더 루프용 — 대화 입력칸이 조합기 주인이면 그 조합 글.
    pub(crate) fn chat_view_preedit(&self, id: &str) -> String {
        match &self.ime_focus {
            Some(crate::ImeFocus::Chat(p)) if p == id => self.preedit.clone(),
            _ => String::new(),
        }
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
                        "toggle" => AutoStep::Toggle,
                        "blocks" => AutoStep::Blocks,
                        "draft" => AutoStep::Draft(arg.replace("\\n", "\n")),
                        "send" => AutoStep::Send,
                        "pick" => AutoStep::Pick(arg.trim().parse().ok()?),
                        "decide" => AutoStep::Decide(arg.trim() == "allow"),
                        "open" => AutoStep::OpenAll,
                        "scroll" => AutoStep::Scroll(arg.trim().parse().ok()?),
                        "zoom" => AutoStep::Zoom(arg.trim().parse().ok()?),
                        "hovermenu" => AutoStep::HoverMenu,
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
                AutoStep::Toggle => self.toggle_chat_view(&id),
                AutoStep::Blocks => self.toggle_shell_view(&id),
                AutoStep::Draft(text) => {
                    if let Some(pane) = self.chat_view.panes.get_mut(&id) {
                        pane.draft = text;
                        pane.cursor = pane.draft.chars().count();
                    }
                }
                AutoStep::Send => self.chat_view_submit(&id),
                AutoStep::Pick(i) => self.chat_view_pick(&id, i),
                AutoStep::Decide(allow) => {
                    let live = self.chat_view.panes.get(&id).map(ChatPane::mod_live).unwrap_or_default();
                    if let Some(p) = live.permissions.first() {
                        self.chat_view_decide(&id, &p.id, &live.session, allow);
                    }
                }
                AutoStep::OpenAll => {
                    if let Some(pane) = self.chat_view.panes.get_mut(&id) {
                        let n = pane.feed.lock().map(|f| f.conv.items.len()).unwrap_or(0);
                        pane.open.extend(0..n);
                        pane.layout.invalidate();
                    }
                }
                AutoStep::Scroll(px) => {
                    if let Some(pane) = self.chat_view.panes.get_mut(&id) {
                        pane.scroll = px;
                    }
                }
                AutoStep::Zoom(delta) => self.change_pane_font(delta),
                AutoStep::HoverMenu => {
                    if let Some((_, r)) = self.handle_menu_hits.iter().find(|(a, _)| *a == ActionKind::ChatView) {
                        self.cursor_px = (r.0 + r.2 / 2.0, r.1 + r.3 / 2.0);
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

    #[test]
    fn a_photo_line_longer_than_the_tail_window_is_kept_whole() {
        let dir = std::env::temp_dir().join(format!("kasaterm-chat-photo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let photo = format!("{{\"photo\":\"{}\"}}\n", "A".repeat(700 * 1024));
        std::fs::write(&path, format!("{{\"old\":1}}\n{photo}{{\"after\":2}}\n")).unwrap();
        let first = crate::socket::read_incremental(&path, 0).unwrap();
        assert!(first.reset);
        assert!(first.raw.starts_with("{\"photo\":\""), "{:?}", &first.raw[..40]);
        assert!(first.raw.ends_with("{\"after\":2}\n"));
        assert!(!first.raw.contains("old"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn multiline_drafts_are_pasted_so_newlines_do_not_submit() {
        assert_eq!(submit_bytes("안녕", true), "안녕\r".as_bytes());
        assert_eq!(submit_bytes("a\nb", true), "\x1b[200~a\nb\x1b[201~\r".as_bytes());
        assert_eq!(submit_bytes("a\nb", false), "a\rb\r".as_bytes());
    }
}
