//! claude 칸의 스크롤바·프롬프트 눈금과 프롬프트 이동. 렌더러가 둘이라 정본도 둘이다.
//!
//! - **classic**: 대화가 이 터미널의 스크롤백에 쌓인다. 줄 번호를 아니 스크롤백(`view_state`)과
//!   프롬프트 줄(`prompt_anchors`)로 그리고, 이동도 `scroll_to_abs` 한 번이다.
//! - **풀스크린(대체 화면)**: claude 가 스크롤을 쥐고 있어 터미널 스크롤백도 줄 번호도 없다. 그래서
//!   칸마다 claude 안에 실은 mod(`collab-hooks/claude-mods/prompt-nav`)가 눈이 되고 손이 된다.
//!
//! 막대·눈금 그림과 누름 판정, 단축키(Option·Ctrl+↑↓)는 둘이 같다. 갈리는 것은 이동뿐이다
//! (`NavSource`). 아래는 풀스크린 쪽 이야기다.
//!
//! - **눈**: mod 가 대화 줄의 차례·높이·화면 맨 윗줄을 재서 `<dir>/<pane>.json` 에 쓴다
//!   (`total`·`top`·`current`·`prompts`). 여기서는 그것을 읽어 칸 오른쪽 여백에 그린다.
//! - **손**: 대화 줄 스크롤은 claude 안에서 **사람이 누른 입력**의 응답일 때만 허락되고, 그
//!   누름 처리에서 파일을 읽으며 기다리면 그 자격을 잃는다. 그래서 두 번 누른다 — 요청 파일을
//!   쓰고 장전 화음(`ctrl+x b`)을 보내면 mod 가 읽어 쥐고 `armed` 로 알린다. 그것을 보면 앞
//!   단추 단축키(`ctrl+↑`)를 보내고, mod 는 쥔 요청으로 곧장 스크롤한다.
//!
//! 엔진은 아직 안 잰 줄을 짐작 높이로 건너 착지가 몇 줄 어긋날 수 있다. 프롬프트로 간
//! 요청은 도착한 위치를 보고 한 번 더 보내 바로 세운다(두 번째는 잰 구간이라 정확하다).
//!
//! 상태는 GUI 스레드의 지역 통에 둔다 — 그리기(불변 `&self`)에서도 읽어야 하고 App 을 안
//! 늘리려는 것이다(턴 헤더 `TURN_HITS` 와 같은 방식).

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

/// 장전 화음 — 엔진 동작 `app:cycleDiffBase`. diff 창 밖에선 엔진이 안 쥐어 mod 단추가 받는다.
const ARM: &[u8] = b"\x18b";
/// 앞 단추 — `app:diffFileListUp`(ctrl+↑). 사람이 직접 누르면 앞 프롬프트로 간다.
const FIRE: &[u8] = b"\x1b[1;5A";
/// 맨 아래로 — 엔진 자신의 `scroll:bottom`(ctrl+end). 바닥에 붙어 따라가기까지 되살린다.
const BOTTOM: &[u8] = b"\x1b[1;5F";

const ARM_TIMEOUT: Duration = Duration::from_millis(700);
const DONE_TIMEOUT: Duration = Duration::from_millis(900);
/// mod 는 보고를 200ms 묶어 쓴다. 그 뒤라야 착지한 자리를 믿을 수 있다.
const SETTLE: Duration = Duration::from_millis(320);
const READ_EVERY: Duration = Duration::from_millis(90);

/// `docs/design.md` 「풀스크린 claude 스크롤바·프롬프트 눈금」.
pub(crate) const BAR_W: f32 = 3.5;
pub(crate) const BAR_W_ACTIVE: f32 = 5.0;
pub(crate) const THUMB_MIN_H: f32 = 24.0;
pub(crate) const TICK_W: f32 = 6.0;
pub(crate) const TICK_H: f32 = 2.0;
pub(crate) const TICK_HIT: f32 = 4.0;

static DIR: OnceLock<PathBuf> = OnceLock::new();

/// shim 이 `KASATERM_PROMPT_NAV_DIR` 로 내리는 자리와 같은 곳.
pub(crate) fn set_dir(dir: PathBuf) {
    let _ = DIR.set(dir);
}

fn state_path(pid: &str) -> Option<PathBuf> {
    DIR.get().map(|d| d.join(format!("{pid}.json")))
}

fn request_path(pid: &str) -> Option<PathBuf> {
    DIR.get().map(|d| d.join(format!("{pid}.req.json")))
}

/// mod 가 쓰는 한 장.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
pub(crate) struct NavState {
    #[serde(default)]
    pub session: String,
    #[serde(default)]
    pub armed: u64,
    #[serde(default)]
    pub done: u64,
    #[serde(default)]
    pub fullscreen: bool,
    #[serde(default)]
    pub total: i64,
    #[serde(default)]
    pub top: Option<i64>,
    #[serde(default)]
    pub current: i64,
    #[serde(default)]
    pub prompts: Vec<(i64, String)>,
    /// classic 칸에서 mod 가 맡긴 이동(`/prompt-nav`). 그 렌더러에선 엔진이 대화 줄을 못 옮긴다.
    #[serde(default)]
    pub ask: Option<NavAsk>,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
pub(crate) struct NavAsk {
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub at: f64,
    #[serde(default)]
    pub op: String,
    #[serde(default)]
    pub index: Option<usize>,
}

impl NavAsk {
    fn op(&self) -> Option<NavOp> {
        Some(match self.op.as_str() {
            "prev" => NavOp::Prev,
            "next" => NavOp::Next,
            "first" => NavOp::Prompt(0),
            "last" => NavOp::Last,
            "bottom" => NavOp::Bottom,
            "prompt" => NavOp::Prompt(self.index?),
            _ => return None,
        })
    }
}

/// 막대의 정본 — classic 은 이 터미널의 스크롤백 줄, 풀스크린은 칸 안 mod 가 잰 행.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NavSource {
    Mod,
    Scrollback,
}

/// classic 칸의 스크롤백을 mod 상태와 같은 꼴로 옮긴다 — 같은 막대·같은 판정을 쓰려고.
/// 행은 세션 시작부터 센 절대 줄이다(`PromptAnchor::abs_line`).
pub(crate) fn scrollback_state(hist: usize, offset: usize, screen: usize, anchors: &[kasa_pty::PromptAnchor]) -> NavState {
    let top = hist.saturating_sub(offset) as i64;
    let prompts: Vec<(i64, String)> = anchors.iter().map(|a| (a.abs_line, a.text.clone())).collect();
    let current = prompts.iter().rposition(|(row, _)| *row <= top).map_or(-1, |i| i as i64);
    NavState {
        session: "scrollback".into(),
        total: (hist + screen) as i64,
        top: Some(top),
        current,
        prompts,
        ..Default::default()
    }
}

/// 앞뒤 프롬프트 — 지금 턴의 머리가 화면 맨 위보다 위에 있으면 그 머리로, 맨 위에 있으면 하나
/// 앞으로. mod 의 `target`(nav.ts)과 같은 규칙이다. 다음이 없으면 `None`(대화 끝으로 간다).
pub(crate) fn step(state: &NavState, down: bool) -> Option<usize> {
    let n = state.prompts.len();
    let Some(top) = state.top else {
        return if down { None } else { n.checked_sub(1) };
    };
    let cur = usize::try_from(state.current).ok().filter(|i| *i < n);
    if down {
        return cur.map_or(Some(0), |i| Some(i + 1)).filter(|i| *i < n);
    }
    match cur {
        Some(i) if state.prompts[i].0 < top => Some(i),
        Some(i) => i.checked_sub(1),
        None => None,
    }
}

impl NavState {
    fn usable(&self) -> bool {
        self.fullscreen && !self.session.is_empty() && self.total > 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum NavOp {
    Prev,
    Next,
    Prompt(usize),
    Last,
    Row(i64),
    Bottom,
}

impl NavOp {
    fn json(self, seq: u64, at: u128) -> String {
        let op = match self {
            NavOp::Prev => r#""op":"prev""#.to_string(),
            NavOp::Next => r#""op":"next""#.to_string(),
            NavOp::Prompt(i) => format!(r#""op":"prompt","index":{i}"#),
            NavOp::Row(r) => format!(r#""op":"row","row":{r}"#),
            NavOp::Last => r#""op":"last""#.to_string(),
            NavOp::Bottom => r#""op":"bottom""#.to_string(),
        };
        format!(r#"{{"seq":{seq},"at":{at},{op}}}"#)
    }
}

struct Cached {
    read_at: Instant,
    mtime: Option<SystemTime>,
    state: Option<NavState>,
}

struct Pending {
    pid: String,
    seq: u64,
    op: NavOp,
    sent: Instant,
    fired: Option<Instant>,
    retried: bool,
}

/// 지난 프레임에 그린 막대 하나 — 누름·끌기가 이 자리로 판정한다.
#[derive(Clone, Debug)]
pub(crate) struct NavHit {
    pub pane: String,
    pub pid: String,
    pub source: NavSource,
    pub track: (f32, f32, f32, f32),
    pub thumb: (f32, f32),
    pub ticks: Vec<(f32, usize)>,
    pub total: i64,
    pub visible: i64,
}

struct Drag {
    pane: String,
    pid: String,
    source: NavSource,
    grab: f32,
}

struct Nav {
    cache: HashMap<String, Cached>,
    /// pane(백엔드 pid)마다 한 번에 하나. 끄는 동안 쌓인 것은 마지막 것만 `queued` 에 둔다.
    pending: HashMap<String, Pending>,
    queued: HashMap<String, NavOp>,
    seq: u64,
    hits: Vec<NavHit>,
    hover: Option<String>,
    drag: Option<Drag>,
    /// classic 칸마다 이미 한 mod 맡김(`ask`)의 번호.
    asked: HashMap<String, u64>,
}

thread_local! {
    static NAV: RefCell<Nav> = RefCell::new(Nav {
        cache: HashMap::new(),
        pending: HashMap::new(),
        queued: HashMap::new(),
        // mod 는 이미 한 번호 이하를 버린다. 앱을 다시 켜도 거꾸로 가지 않게 시각에서 시작한다.
        seq: now_ms() as u64,
        hits: Vec::new(),
        hover: None,
        drag: None,
        asked: HashMap::new(),
    });
}

fn now_ms() -> u128 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

fn read_state(pid: &str, force: bool) -> Option<NavState> {
    let path = state_path(pid)?;
    NAV.with(|n| {
        let mut n = n.borrow_mut();
        if let Some(c) = n.cache.get(pid) {
            if !force && c.read_at.elapsed() < READ_EVERY {
                return c.state.clone();
            }
        }
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let same = n.cache.get(pid).is_some_and(|c| c.mtime == mtime && mtime.is_some());
        let state = if same {
            n.cache.get(pid).and_then(|c| c.state.clone())
        } else {
            // 쓰는 도중이면 반쪽 JSON 이다 — 그때는 지난 값을 들고 다음에 다시 읽는다.
            match std::fs::read_to_string(&path).ok().map(|t| serde_json::from_str::<NavState>(&t)) {
                Some(Ok(s)) => Some(s),
                Some(Err(_)) => n.cache.get(pid).and_then(|c| c.state.clone()),
                None => None,
            }
        };
        let mtime = if state.is_some() || mtime.is_none() { mtime } else { None };
        n.cache.insert(pid.to_string(), Cached { read_at: Instant::now(), mtime, state: state.clone() });
        state
    })
}

/// 이 칸(백엔드 pid)에 살아 있는 mod 가 있나 — 있으면 그 상태.
pub(crate) fn live_state(pid: &str) -> Option<NavState> {
    read_state(pid, false).filter(NavState::usable)
}

pub(crate) fn set_hits(hits: Vec<NavHit>) {
    NAV.with(|n| n.borrow_mut().hits = hits);
}

pub(crate) fn hover_pane() -> Option<String> {
    NAV.with(|n| {
        let n = n.borrow();
        n.drag.as_ref().map(|d| d.pane.clone()).or_else(|| n.hover.clone())
    })
}

/// 대화 영역 높이(px) 안에서 막대·눈금이 놓일 자리. 줄 수는 전부 mod 의 행 단위다.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BarGeometry {
    pub thumb: (f32, f32),
    pub ticks: Vec<(f32, usize)>,
}

pub(crate) fn geometry(state: &NavState, track_y: f32, track_h: f32, visible: i64) -> Option<BarGeometry> {
    let total = state.total.max(1) as f32;
    let top = state.top? as f32;
    if state.total <= visible || track_h <= THUMB_MIN_H {
        return None;
    }
    let scale = track_h / total;
    let mut h = (visible as f32 * scale).max(THUMB_MIN_H).min(track_h);
    let mut y = track_y + top * scale;
    // 최소 높이로 키운 만큼은 위아래로 나눠 덮고, 홈 밖으로 나가지 않게 민다.
    let grown = h - visible as f32 * scale;
    if grown > 0.0 {
        y -= grown / 2.0;
    }
    y = y.clamp(track_y, track_y + track_h - h);
    h = h.min(track_h);
    let ticks = state
        .prompts
        .iter()
        .enumerate()
        .map(|(i, (row, _))| (track_y + (*row as f32 * scale).clamp(0.0, track_h - TICK_H), i))
        .collect();
    Some(BarGeometry { thumb: (y, h), ticks })
}

/// 막대 위 y 를 mod 의 행으로 — 끌 때는 잡은 자리만큼 빼서 손잡이 윗변이 그 행에 선다.
pub(crate) fn row_at(hit: &NavHit, y: f32) -> i64 {
    let (_, ty, _, th) = hit.track;
    let frac = ((y - ty) / th.max(1.0)).clamp(0.0, 1.0);
    (frac * hit.total as f32).round() as i64
}

pub(crate) fn tick_at(hit: &NavHit, y: f32) -> Option<usize> {
    hit.ticks
        .iter()
        .filter(|(ty, _)| (y - (ty + TICK_H / 2.0)).abs() <= TICK_HIT)
        .min_by(|a, b| (y - a.0).abs().total_cmp(&(y - b.0).abs()))
        .map(|(_, i)| *i)
}

fn hit_at(x: f32, y: f32) -> Option<NavHit> {
    NAV.with(|n| {
        n.borrow()
            .hits
            .iter()
            .find(|h| {
                let (tx, ty, tw, th) = h.track;
                x >= tx && x <= tx + tw && y >= ty && y <= ty + th
            })
            .cloned()
    })
}

impl crate::App {
    /// classic 칸의 막대 한 벌 — 그 칸이 스크롤백에 쌓는 claude 가 아니면 `None`.
    pub(crate) fn scrollback_nav(&mut self, pane: &str) -> Option<NavState> {
        let (pid, alt_screen) = {
            let ws = self.ws.lock().ok()?;
            let alt = ws.panes.get(pane).and_then(|p| p.term()).is_some_and(|t| t.alt_screen);
            (ws.active_tab_pid(pane), alt)
        };
        if alt_screen || kasa_mcp::remote::is_view_pane(&pid) {
            return None;
        }
        let sess = self.pty_for_pane(pane)?.clone();
        if !matches!(sess.active_agent(), Some(kasa_pty::AgentKind::Claude)) {
            return None;
        }
        let (offset, hist) = sess.view_state();
        let screen = sess.size().1 as usize;
        let anchors = self.turn.claude_anchors(&pid, &sess);
        Some(scrollback_state(hist, offset, screen, &anchors))
    }

    /// classic 칸 — 스크롤백 줄로 곧장 옮긴다. 갈 곳이 없으면 `false`.
    fn scrollback_go(&mut self, pane: &str, op: NavOp) -> bool {
        let Some(state) = self.scrollback_nav(pane) else { return false };
        let Some(sess) = self.pty_for_pane(pane).cloned() else { return false };
        let at = |i: Option<usize>| i.and_then(|i| state.prompts.get(i)).map(|(row, _)| *row);
        let abs = match op {
            NavOp::Row(row) => Some(row),
            NavOp::Prompt(i) => at(Some(i)),
            NavOp::Last => at(state.prompts.len().checked_sub(1)),
            NavOp::Prev => at(step(&state, false)),
            NavOp::Next => at(step(&state, true)),
            NavOp::Bottom => None,
        };
        match abs {
            Some(abs) => {
                sess.scroll_to_abs(abs);
            }
            None if matches!(op, NavOp::Next | NavOp::Bottom) => {
                if !self.follow_live_tail_at(pane) {
                    return false;
                }
            }
            None => return false,
        }
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        true
    }

    fn nav_go(&mut self, pane: &str, source: NavSource, op: NavOp) {
        match source {
            NavSource::Mod => {
                self.prompt_nav_request(pane, op);
            }
            NavSource::Scrollback => {
                self.scrollback_go(pane, op);
            }
        }
    }

    /// Option·Ctrl+↑↓ — classic claude 칸이면 여기서 스크롤백의 앞뒤 프롬프트로 옮기고 `true`.
    /// 풀스크린 칸은 `false` 라 키가 그대로 claude 로 가고, 칸 안 mod 의 보이지 않는 단추가 받는다.
    /// 프롬프트가 하나라도 있으면 갈 곳이 없어도 키를 먹는다 — 흘려 보내면 claude 입력칸이 받는다.
    pub(crate) fn prompt_nav_key(&mut self, down: bool) -> bool {
        let Some(pane) = self.ws.lock().ok().and_then(|ws| ws.active_pane.clone()) else { return false };
        let Some(state) = self.scrollback_nav(&pane) else { return false };
        if state.prompts.is_empty() {
            return false;
        }
        self.scrollback_go(&pane, if down { NavOp::Next } else { NavOp::Prev });
        true
    }

    fn nav_send(&self, pid: &str, bytes: &[u8]) {
        self.send_bytes_to_surface(Some(pid), bytes);
    }

    /// 요청을 보낸다. 같은 칸에 아직 도는 것이 있으면 마지막 하나만 줄 세운다(끌기).
    pub(crate) fn prompt_nav_request(&self, pane: &str, op: NavOp) -> bool {
        let pid = self.ws.lock().map(|ws| ws.active_tab_pid(pane)).unwrap_or_else(|_| pane.to_string());
        if live_state(&pid).is_none() {
            return false;
        }
        let busy = NAV.with(|n| n.borrow().pending.contains_key(&pid));
        if busy {
            NAV.with(|n| n.borrow_mut().queued.insert(pid, op));
            return true;
        }
        self.prompt_nav_start(&pid, op, false)
    }

    fn prompt_nav_start(&self, pid: &str, op: NavOp, retried: bool) -> bool {
        let Some(path) = request_path(pid) else { return false };
        let seq = NAV.with(|n| {
            let mut n = n.borrow_mut();
            n.seq += 1;
            n.seq
        });
        if std::fs::write(&path, op.json(seq, now_ms())).is_err() {
            return false;
        }
        NAV.with(|n| {
            n.borrow_mut().pending.insert(
                pid.to_string(),
                Pending { pid: pid.to_string(), seq, op, sent: Instant::now(), fired: None, retried },
            )
        });
        self.nav_send(pid, ARM);
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        true
    }

    /// 맨 아래로는 엔진 자신의 키 하나로 끝난다 — 바닥에 붙어 따라가는 것까지 되살아난다.
    pub(crate) fn prompt_nav_bottom(&self, pane: &str) -> bool {
        let pid = self.ws.lock().map(|ws| ws.active_tab_pid(pane)).unwrap_or_else(|_| pane.to_string());
        if live_state(&pid).is_none() {
            return false;
        }
        self.nav_send(&pid, BOTTOM);
        true
    }

    pub(crate) fn prompt_nav_current(&self, pane: &str) -> Option<usize> {
        let pid = self.ws.lock().ok()?.active_tab_pid(pane);
        let state = live_state(&pid)?;
        usize::try_from(state.current).ok()
    }

    /// about_to_wait 매 틱 — 장전을 확인하면 쏘고, 도착을 보고 한 번 바로 세우고, 막대가
    /// 그려진 칸의 상태가 바뀌었으면 다시 그린다(사람이 claude 안에서 휠로 굴린 경우).
    pub(crate) fn pump_prompt_nav(&mut self) {
        let pending: Vec<(String, u64, NavOp, Instant, Option<Instant>, bool)> = NAV.with(|n| {
            n.borrow().pending.values().map(|p| (p.pid.clone(), p.seq, p.op, p.sent, p.fired, p.retried)).collect()
        });
        let mut busy = false;
        for (pid, seq, op, sent, fired, retried) in pending {
            let state = read_state(&pid, true);
            let finish = |pid: &str| {
                NAV.with(|n| {
                    let mut n = n.borrow_mut();
                    n.pending.remove(pid);
                    n.queued.remove(pid)
                })
            };
            match fired {
                None if state.as_ref().is_some_and(|s| s.armed == seq) => {
                    self.nav_send(&pid, FIRE);
                    NAV.with(|n| {
                        if let Some(p) = n.borrow_mut().pending.get_mut(&pid) {
                            p.fired = Some(Instant::now());
                        }
                    });
                    busy = true;
                }
                None if sent.elapsed() > ARM_TIMEOUT => {
                    // 장전이 안 됐다 — 대화 상자가 떠 있거나 mod 가 죽었다. 쏘지 않는다.
                    if let Some(next) = finish(&pid) {
                        self.prompt_nav_start(&pid, next, false);
                    }
                }
                None => busy = true,
                Some(at) => {
                    let done = state.as_ref().is_some_and(|s| s.done >= seq);
                    // 프롬프트로 간 첫 요청만 착지를 보고 바로 세운다 — 그 판정은 보고가 모인 뒤라야 맞다.
                    let check = matches!(op, NavOp::Prompt(_)) && !retried;
                    if !done && at.elapsed() < DONE_TIMEOUT || done && check && at.elapsed() < SETTLE {
                        busy = true;
                        continue;
                    }
                    let queued = finish(&pid);
                    let want = match (op, &state) {
                        (NavOp::Prompt(i), Some(s)) => s.prompts.get(i).map(|(row, _)| *row),
                        _ => None,
                    };
                    let landed = state.as_ref().and_then(|s| s.top);
                    if let Some(next) = queued {
                        self.prompt_nav_start(&pid, next, false);
                    } else if done && check && want.is_some() && landed != want {
                        self.prompt_nav_start(&pid, op, true);
                    }
                }
            }
        }
        // classic 칸의 mod 가 맡긴 이동(`/prompt-nav`)을 스크롤백에서 한다. 앱을 다시 켜 처음 보는
        // 낡은 맡김은 시각으로 거른다.
        let classic: Vec<(String, String)> = NAV.with(|n| {
            n.borrow().hits.iter().filter(|h| h.source == NavSource::Scrollback).map(|h| (h.pane.clone(), h.pid.clone())).collect()
        });
        for (pane, pid) in classic {
            let Some(ask) = read_state(&pid, false).and_then(|s| s.ask) else { continue };
            let done = NAV.with(|n| n.borrow().asked.get(&pid).copied().unwrap_or(0));
            if ask.seq <= done {
                continue;
            }
            NAV.with(|n| n.borrow_mut().asked.insert(pid.clone(), ask.seq));
            if (now_ms() as f64 - ask.at).abs() > 3000.0 {
                continue;
            }
            if let Some(op) = ask.op() {
                self.scrollback_go(&pane, op);
            }
        }
        // 그려진 막대의 칸은 mod 가 새로 쓴 것을 보면 다시 그린다.
        let shown: Vec<String> = NAV.with(|n| {
            n.borrow().hits.iter().filter(|h| h.source == NavSource::Mod).map(|h| h.pid.clone()).collect()
        });
        let mut changed = false;
        for pid in shown {
            let before = NAV.with(|n| n.borrow().cache.get(&pid).and_then(|c| c.mtime));
            let after = state_path(&pid).and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
            changed |= before != after;
        }
        if busy || changed {
            self.chrome_dirty = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }

    /// 막대 누름: 눈금이면 그 프롬프트로, 손잡이면 끌기 시작, 홈이면 그 자리가 가운데 오게.
    pub(crate) fn prompt_nav_press(&mut self, x: f32, y: f32) -> bool {
        let Some(hit) = hit_at(x, y) else { return false };
        if let Some(i) = tick_at(&hit, y) {
            self.nav_go(&hit.pane, hit.source, NavOp::Prompt(i));
            return true;
        }
        let (ty, th) = hit.thumb;
        let grab = if y >= ty && y <= ty + th {
            y - ty
        } else {
            let row = row_at(&hit, y) - hit.visible / 2;
            self.nav_go(&hit.pane, hit.source, NavOp::Row(row.max(0)));
            th / 2.0
        };
        let drag = Drag { pane: hit.pane.clone(), pid: hit.pid.clone(), source: hit.source, grab };
        NAV.with(|n| n.borrow_mut().drag = Some(drag));
        self.chrome_dirty = true;
        true
    }

    pub(crate) fn prompt_nav_drag_move(&mut self) -> bool {
        let Some((pane, pid, source, grab)) =
            NAV.with(|n| n.borrow().drag.as_ref().map(|d| (d.pane.clone(), d.pid.clone(), d.source, d.grab)))
        else {
            return false;
        };
        let hit = NAV.with(|n| n.borrow().hits.iter().find(|h| h.pid == pid).cloned());
        if let Some(hit) = hit {
            let row = row_at(&hit, self.cursor_px.1 - grab);
            self.nav_go(&pane, source, NavOp::Row(row));
        }
        true
    }

    pub(crate) fn prompt_nav_release(&mut self) -> bool {
        let had = NAV.with(|n| n.borrow_mut().drag.take().is_some());
        if had {
            self.chrome_dirty = true;
        }
        had
    }

    /// 막대 위에 있나. 있으면 다른 손가락 커서·TUI 호버 전달을 건너뛴다.
    pub(crate) fn prompt_nav_hover(&mut self, x: f32, y: f32) -> bool {
        let over = hit_at(x, y).map(|h| h.pane);
        let changed = NAV.with(|n| {
            let mut n = n.borrow_mut();
            let changed = n.hover != over;
            n.hover = over.clone();
            changed
        });
        if changed {
            self.chrome_dirty = true;
        }
        over.is_some()
    }
}

thread_local! {
    /// `KASATERM_AUTONAV` 예약 — (발사 시각, 할 일).
    static AUTO: RefCell<Vec<(Instant, String)>> = const { RefCell::new(Vec::new()) };
}

impl crate::App {
    /// `KASATERM_AUTONAV="6000:tick=3;8000:cap=a;9000:track=0.2;11000:drag=0.2>0.8;13000:key=prev"`
    /// (+ `KASATERM_AUTONAV_CAP_DIR`). 누를 자리는 **이번 프레임에 실제로 그린 막대**에서 읽는다 —
    /// 좌표를 손으로 적으면 막대가 안 떠도 「눌렀다」고 적힌다(턴 헤더 하네스와 같은 이유).
    pub(crate) fn arm_autonav(&mut self) {
        let Ok(plan) = std::env::var("KASATERM_AUTONAV") else { return };
        let start = Instant::now();
        let mut steps = Vec::new();
        for part in plan.split(';') {
            let Some((ms, step)) = part.split_once(':') else { continue };
            let Ok(ms) = ms.trim().parse::<u64>() else { continue };
            if let Some(span) = step.strip_prefix("drag=") {
                // 끌기는 잡기 → 여덟 걸음 → 놓기로 펼친다. 걸음 사이가 있어야 끄는 중의 화면이 보인다.
                let Some((a, b)) = span.split_once('>') else { continue };
                let (Ok(a), Ok(b)) = (a.parse::<f32>(), b.parse::<f32>()) else { continue };
                steps.push((start + Duration::from_millis(ms), format!("grab={a}")));
                for i in 1..=8 {
                    let f = a + (b - a) * i as f32 / 8.0;
                    steps.push((start + Duration::from_millis(ms + i * 150), format!("move={f}")));
                }
                steps.push((start + Duration::from_millis(ms + 9 * 150), "release".into()));
            } else {
                steps.push((start + Duration::from_millis(ms), step.trim().to_string()));
            }
        }
        eprintln!("[autonav] {} steps", steps.len());
        AUTO.with(|a| *a.borrow_mut() = steps);
    }

    pub(crate) fn run_pending_autonav(&mut self) {
        let due: Vec<String> = AUTO.with(|a| {
            let mut a = a.borrow_mut();
            let now = Instant::now();
            let (ready, rest): (Vec<_>, Vec<_>) = a.drain(..).partition(|(at, _)| *at <= now);
            *a = rest;
            ready.into_iter().map(|(_, s)| s).collect()
        });
        for step in due {
            let hit = NAV.with(|n| n.borrow().hits.first().cloned());
            let (kind, arg) = step.split_once('=').unwrap_or((step.as_str(), ""));
            let frac = arg.parse::<f32>().unwrap_or(0.5);
            let at = |hit: &NavHit, f: f32| (hit.track.0 + hit.track.2 / 2.0, hit.track.1 + hit.track.3 * f);
            match (kind, hit) {
                ("cap", _) => {
                    if let (Ok(dir), Some(gpu)) = (std::env::var("KASATERM_AUTONAV_CAP_DIR"), self.gpu.as_mut()) {
                        gpu.capture_next = Some(format!("{dir}/{arg}.png"));
                    }
                    self.chrome_dirty = true;
                }
                ("state", _) => {
                    let hit = NAV.with(|n| n.borrow().hits.first().cloned());
                    let s = match &hit {
                        Some(h) if h.source == NavSource::Scrollback => self.scrollback_nav(&h.pane),
                        Some(h) => read_state(&h.pid, true),
                        None => None,
                    };
                    if let Some(s) = s {
                        let src = hit.map(|h| h.source);
                        eprintln!(
                            "[autonav] state {src:?} top={:?} total={} current={} prompts={} {:?}",
                            s.top,
                            s.total,
                            s.current,
                            s.prompts.len(),
                            s.prompts.iter().map(|(r, t)| format!("{r}:{}", t.chars().take(12).collect::<String>())).collect::<Vec<_>>()
                        );
                    }
                }
                // 사람의 Option+↑↓ 와 같은 길 — classic 칸은 여기서 옮기고, 풀스크린은 claude 로 보낸다.
                ("key", _) => {
                    if !self.prompt_nav_key(arg == "next") {
                        let bytes: &[u8] = if arg == "next" { b"\x1b[1;3B" } else { b"\x1b[1;3A" };
                        self.send_bytes(bytes);
                    }
                }
                ("tick", Some(hit)) => {
                    let i = arg.parse::<usize>().unwrap_or(0);
                    if let Some((y, _)) = hit.ticks.iter().find(|(_, t)| *t == i) {
                        let (x, y) = (hit.track.0 + hit.track.2 / 2.0, y + TICK_H / 2.0);
                        let ok = self.prompt_nav_press(x, y);
                        self.prompt_nav_release();
                        eprintln!("[autonav] tick {i} at ({x:.0},{y:.0}) handled={ok}");
                    }
                }
                ("track", Some(hit)) => {
                    let (x, y) = at(&hit, frac);
                    let ok = self.prompt_nav_press(x, y);
                    self.prompt_nav_release();
                    eprintln!("[autonav] track {frac} at ({x:.0},{y:.0}) handled={ok}");
                }
                ("grab", Some(hit)) => {
                    let (x, _) = at(&hit, frac);
                    let y = hit.thumb.0 + hit.thumb.1 / 2.0;
                    self.cursor_px = (x, y);
                    let ok = self.prompt_nav_press(x, y);
                    eprintln!("[autonav] grab at ({x:.0},{y:.0}) handled={ok}");
                }
                ("move", Some(hit)) => {
                    self.cursor_px = at(&hit, frac);
                    self.prompt_nav_drag_move();
                }
                ("release", _) => {
                    self.prompt_nav_release();
                }
                (other, None) => eprintln!("[autonav] {other}: 막대가 이번 프레임에 없다"),
                (other, _) => eprintln!("[autonav] 모르는 걸음 {other}"),
            }
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }
}

/// 한 칸의 막대를 그린다. 쉴 때는 여백 안의 가는 손잡이와 눈금만, 올리거나 끄는 중엔
/// 굵어지고 홈이 깔린다.
pub(crate) fn paint(
    g: &mut crate::gpu::GpuRenderer,
    x: f32,
    geo: &BarGeometry,
    track: (f32, f32),
    current: i64,
    active: bool,
) {
    use crate::theme;
    let w = if active { BAR_W_ACTIVE } else { BAR_W };
    let (ty, th) = track;
    if active {
        g.rect(x - w / 2.0, ty, w, th, theme::with_alpha(theme::text(), 0x14));
    }
    let (y, h) = geo.thumb;
    crate::pill_rect(
        g,
        x - w / 2.0,
        y,
        w,
        h,
        theme::with_alpha(theme::text(), if active { 0x99 } else { 0x66 }),
    );
    for (ty, i) in &geo.ticks {
        let on = *i as i64 == current;
        let color = if on { theme::accent() } else { theme::with_alpha(theme::accent(), 0x80) };
        g.rect(x - TICK_W / 2.0, *ty, TICK_W, TICK_H, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(total: i64, top: i64, prompts: &[i64]) -> NavState {
        NavState {
            session: "s".into(),
            fullscreen: true,
            total,
            top: Some(top),
            prompts: prompts.iter().map(|r| (*r, String::new())).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn reads_what_the_mod_writes() {
        let text = r#"{"v":1,"seq":4,"at":1,"session":"abc","armed":7,"done":6,"fullscreen":true,"cols":110,
            "total":379,"top":350,"current":12,"prompts":[[0,"질문 1"],[25,"질문 2"]]}"#;
        let s: NavState = serde_json::from_str(text).unwrap();
        assert_eq!((s.armed, s.done, s.total, s.top, s.current), (7, 6, 379, Some(350), 12));
        assert_eq!(s.prompts[1], (25, "질문 2".to_string()));
        assert!(s.usable());
        let main_screen: NavState = serde_json::from_str(r#"{"session":"abc","fullscreen":false,"total":10}"#).unwrap();
        assert!(!main_screen.usable(), "대체 화면이 아닌 claude 엔 막대를 안 그린다");
    }

    #[test]
    fn requests_carry_seq_and_op() {
        assert_eq!(NavOp::Prompt(3).json(9, 100), r#"{"seq":9,"at":100,"op":"prompt","index":3}"#);
        assert_eq!(NavOp::Row(42).json(1, 2), r#"{"seq":1,"at":2,"op":"row","row":42}"#);
        assert_eq!(NavOp::Prev.json(1, 2), r#"{"seq":1,"at":2,"op":"prev"}"#);
    }

    #[test]
    fn thumb_and_ticks_are_proportional_to_rows() {
        let geo = geometry(&state(400, 100, &[0, 100, 300]), 10.0, 400.0, 40).unwrap();
        assert_eq!(geo.thumb, (110.0, 40.0));
        assert_eq!(geo.ticks, vec![(10.0, 0), (110.0, 1), (310.0, 2)]);
    }

    #[test]
    fn a_short_thumb_grows_around_its_spot_and_stays_in_the_track() {
        let geo = geometry(&state(4000, 0, &[]), 0.0, 400.0, 40).unwrap();
        assert_eq!(geo.thumb, (0.0, THUMB_MIN_H));
        let geo = geometry(&state(4000, 3960, &[]), 0.0, 400.0, 40).unwrap();
        assert_eq!(geo.thumb.0 + geo.thumb.1, 400.0);
    }

    #[test]
    fn no_bar_when_everything_fits_or_the_top_is_unknown() {
        assert!(geometry(&state(30, 0, &[]), 0.0, 400.0, 40).is_none());
        let mut s = state(400, 0, &[]);
        s.top = None;
        assert!(geometry(&s, 0.0, 400.0, 40).is_none());
    }

    fn anchor(abs_line: i64) -> kasa_pty::PromptAnchor {
        kasa_pty::PromptAnchor { abs_line, text: format!("질문 {abs_line}") }
    }

    /// classic 칸은 스크롤백 전체가 행이고, 화면 맨 윗줄은 `hist - offset` 이다.
    #[test]
    fn scrollback_is_drawn_as_the_same_bar() {
        let s = scrollback_state(300, 50, 40, &[anchor(10), anchor(200), anchor(280)]);
        assert_eq!((s.total, s.top, s.current), (340, Some(250), 1));
        assert_eq!(s.prompts.iter().map(|(r, _)| *r).collect::<Vec<_>>(), vec![10, 200, 280]);
        let geo = geometry(&s, 0.0, 340.0, 40).unwrap();
        assert_eq!(geo.thumb, (250.0, 40.0));
        let bottom = scrollback_state(300, 0, 40, &[anchor(10)]);
        assert_eq!((bottom.top, bottom.current), (Some(300), 0));
        assert_eq!(scrollback_state(0, 0, 40, &[]).current, -1);
    }

    /// 앞뒤 프롬프트는 mod 의 `target`(nav.ts)과 같은 규칙 — 같은 키가 두 렌더러에서 같게 움직인다.
    #[test]
    fn prev_goes_to_the_turn_head_first_and_next_runs_out_at_the_end() {
        let at = |top| scrollback_state(top as usize, 0, 40, &[anchor(10), anchor(100), anchor(200)]);
        // 둘째 턴 안을 보는 중 — 먼저 그 턴의 머리로, 머리에 서 있으면 하나 앞으로.
        assert_eq!(step(&at(150), false), Some(1));
        assert_eq!(step(&at(100), false), Some(0));
        assert_eq!(step(&at(10), false), None);
        assert_eq!(step(&at(150), true), Some(2));
        assert_eq!(step(&at(250), true), None, "마지막 다음은 대화 끝(맨 아래로)");
        // 첫 프롬프트보다 위(로고)에서는 다음이 첫 프롬프트다.
        assert_eq!(step(&at(5), true), Some(0));
        assert_eq!(step(&at(5), false), None);
    }

    #[test]
    fn a_classic_ask_from_the_mod_names_an_op() {
        let s: NavState = serde_json::from_str(
            r#"{"session":"s","fullscreen":false,"total":0,"ask":{"op":"prompt","index":2,"seq":4,"at":1791263279302}}"#,
        )
        .unwrap();
        let ask = s.ask.unwrap();
        assert_eq!((ask.seq, ask.op()), (4, Some(NavOp::Prompt(2))));
        let next: NavAsk = serde_json::from_str(r#"{"op":"next","seq":1,"at":1}"#).unwrap();
        assert_eq!(next.op(), Some(NavOp::Next));
        let odd: NavAsk = serde_json::from_str(r#"{"op":"jump","seq":1,"at":1}"#).unwrap();
        assert_eq!(odd.op(), None);
    }

    #[test]
    fn a_press_maps_back_to_rows_and_ticks() {
        let geo = geometry(&state(400, 100, &[0, 100, 300]), 0.0, 400.0, 40).unwrap();
        let hit = NavHit {
            pane: "%1".into(),
            pid: "%1".into(),
            source: NavSource::Mod,
            track: (0.0, 0.0, 9.0, 400.0),
            thumb: geo.thumb,
            ticks: geo.ticks,
            total: 400,
            visible: 40,
        };
        assert_eq!(row_at(&hit, 200.0), 200);
        assert_eq!(row_at(&hit, -50.0), 0);
        assert_eq!(tick_at(&hit, 301.0), Some(2));
        assert_eq!(tick_at(&hit, 103.5), Some(1));
        assert_eq!(tick_at(&hit, 200.0), None);
    }
}
