//! 풀스크린 claude 칸의 프롬프트 이동(Info 「내 프롬프트」 누름). claude 가 대체 화면에서 스크롤을 쥐고
//! 있어 터미널 스크롤백도 줄 번호도 없다. 그래서 칸마다 claude 안에 실은 mod
//! (`collab-hooks/claude-mods/prompt-nav`)가 눈이 되고 손이 된다. classic 렌더러는 받지 않는다 —
//! claude·codex 모두 풀스크린 TUI 로 돈다. 칸 오른쪽 스크롤바·프롬프트 눈금은 2026-10-19 걷었다 — 끌 때마다
//! 아래 두 번 누르기를 거쳐 느리고 튀었다. 스크롤바가 필요하면 직접 그리는 대화 보기(`chat_view.rs`)에 둔다.
//!
//! - **눈**: mod 가 대화 줄의 차례·높이·화면 맨 윗줄을 재서 `<dir>/<pane>.json` 에 쓴다
//!   (`total`·`top`·`current`·`prompts`). Info 「내 프롬프트」가 그것으로 지금 턴과 갈 자리를 안다.
//! - **손**: 대화 줄 스크롤은 claude 안에서 **사람이 누른 입력**의 응답일 때만 허락되고, 그
//!   누름 처리에서 파일을 읽으며 기다리면 그 자격을 잃는다. 그래서 두 번 누른다 — 요청 파일을
//!   쓰고 장전 화음(`ctrl+x b`)을 보내면 mod 가 읽어 쥐고 `armed` 로 알린다. 그것을 보면 앞
//!   단추 단축키(`ctrl+↑`)를 보내고, mod 는 쥔 요청으로 곧장 스크롤한다.
//!
//! 엔진은 아직 안 잰 줄을 짐작 높이로 건너 착지가 몇 줄 어긋날 수 있다. 프롬프트로 간
//! 요청은 도착한 위치를 보고 한 번 더 보내 바로 세운다(두 번째는 잰 구간이라 정확하다).
//!
//! 상태는 GUI 스레드의 지역 통에 둔다 — 그리기(불변 `&self`)에서도 읽어야 하고 App 을 안
//! 늘리려는 것이다.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

/// 장전 화음 — 엔진 동작 `app:cycleDiffBase`. diff 창 밖에선 엔진이 안 쥐어 mod 단추가 받는다.
const ARM: &[u8] = b"\x18b";
/// 앞 단추 — `app:diffFileListUp`(ctrl+↑). 사람이 직접 누르면 앞 프롬프트로 간다.
const FIRE: &[u8] = b"\x1b[1;5A";

const ARM_TIMEOUT: Duration = Duration::from_millis(700);
const DONE_TIMEOUT: Duration = Duration::from_millis(900);
/// mod 는 보고를 200ms 묶어 쓴다. 그 뒤라야 착지한 자리를 믿을 수 있다.
const SETTLE: Duration = Duration::from_millis(320);
const READ_EVERY: Duration = Duration::from_millis(90);

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
}

impl NavState {
    fn usable(&self) -> bool {
        self.fullscreen && !self.session.is_empty() && self.total > 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum NavOp {
    Prompt(usize),
}

impl NavOp {
    fn json(self, seq: u64, at: u128) -> String {
        let op = match self {
            NavOp::Prompt(i) => format!(r#""op":"prompt","index":{i}"#),
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

struct Nav {
    cache: HashMap<String, Cached>,
    /// pane(백엔드 pid)마다 한 번에 하나. 끄는 동안 쌓인 것은 마지막 것만 `queued` 에 둔다.
    pending: HashMap<String, Pending>,
    queued: HashMap<String, NavOp>,
    seq: u64,
}

thread_local! {
    static NAV: RefCell<Nav> = RefCell::new(Nav {
        cache: HashMap::new(),
        pending: HashMap::new(),
        queued: HashMap::new(),
        // mod 는 이미 한 번호 이하를 버린다. 앱을 다시 켜도 거꾸로 가지 않게 시각에서 시작한다.
        seq: now_ms() as u64,
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

impl crate::App {
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

    /// about_to_wait 매 틱 — 장전을 확인하면 쏘고, 도착을 보고 한 번 바로 세우고, 읽어 둔 칸의 상태가
    /// 바뀌었으면 다시 그린다(사람이 claude 안에서 휠로 굴리면 Info 의 지금 턴이 따라간다).
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
        let shown: Vec<String> = NAV.with(|n| n.borrow().cache.keys().cloned().collect());
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

}

thread_local! {
    /// `KASATERM_AUTONAV` 예약 — (발사 시각, 할 일).
    static AUTO: RefCell<Vec<(Instant, String)>> = const { RefCell::new(Vec::new()) };
}

impl crate::App {
    /// `KASATERM_AUTONAV="6000:prompt=3;8000:cap=a;9000:state;13000:key=prev"`(+ `KASATERM_AUTONAV_CAP_DIR`).
    /// 걸음은 초점 칸에 간다 — `prompt=` 는 Info 누름과 같은 요청, `key=` 는 사람의 Option+↑↓ 와 같은 키.
    pub(crate) fn arm_autonav(&mut self) {
        let Ok(plan) = std::env::var("KASATERM_AUTONAV") else { return };
        let start = Instant::now();
        let steps: Vec<(Instant, String)> = plan
            .split(';')
            .filter_map(|part| {
                let (ms, step) = part.split_once(':')?;
                Some((start + Duration::from_millis(ms.trim().parse().ok()?), step.trim().to_string()))
            })
            .collect();
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
            let pid = self.target_surface();
            let (kind, arg) = step.split_once('=').unwrap_or((step.as_str(), ""));
            match kind {
                "cap" => {
                    if let (Ok(dir), Some(gpu)) = (std::env::var("KASATERM_AUTONAV_CAP_DIR"), self.gpu.as_mut()) {
                        gpu.capture_next = Some(format!("{dir}/{arg}.png"));
                    }
                    self.chrome_dirty = true;
                }
                "state" => {
                    if let Some(s) = pid.as_deref().and_then(|p| read_state(p, true)) {
                        eprintln!(
                            "[autonav] state top={:?} total={} current={} prompts={} {:?}",
                            s.top,
                            s.total,
                            s.current,
                            s.prompts.len(),
                            s.prompts.iter().map(|(r, t)| format!("{r}:{}", t.chars().take(12).collect::<String>())).collect::<Vec<_>>()
                        );
                    }
                }
                "prompt" => {
                    let ok = pid.as_deref().filter(|p| live_state(p).is_some()).is_some_and(|p| {
                        self.prompt_nav_start(p, NavOp::Prompt(arg.parse().unwrap_or(0)), false)
                    });
                    eprintln!("[autonav] prompt {arg} sent={ok}");
                }
                // 사람의 Option+↑↓ 와 같은 길 — 키가 claude 로 가고 칸 안 mod 의 보이지 않는 단추가 받는다.
                "key" => {
                    let bytes: &[u8] = if arg == "next" { b"\x1b[1;3B" } else { b"\x1b[1;3A" };
                    self.send_bytes(bytes);
                }
                other => eprintln!("[autonav] 모르는 걸음 {other}"),
            }
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
