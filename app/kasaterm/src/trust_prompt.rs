//! claude 폴더 신뢰 화면(「Accessing workspace: … Quick safety check: Is this a project you
//! trust?」)을 사람 손 없이 지나가게 한다. 무인으로 띄운 학생이 이 화면에서 멈추면 브리프도
//! 못 받고 서 있다.
//!
//! 막는 길은 둘이다. 카사텀 pane 의 claude 는 shim 이 실행 직전에 신뢰를 심어(`claude-trust`)
//! 이 화면이 아예 안 뜬다. 그래도 뜬 화면(앱 밖에서 바뀐 설정, 다른 설정 폴더)은 여기서 넘긴다.
//!
//! claude 2.1.286 의 선택지는 「No, exit」가 먼저이고 처음 초점도 거기다 — Enter 만 치면 claude 가
//! 꺼진다. 그래서 초점이 「Yes, I trust this folder」에 있을 때만 Enter 를 보내고, No 에 있으면
//! 아래 화살표를 **한 번** 보내 다음 화면에서 초점이 옮겨졌는지 다시 확인한다. 옛 판의
//! 「❯ 1. Yes, I trust this folder」(Yes 가 먼저)도 같은 판정으로 바로 Enter 다.
//!
//! 입력 보호는 tell 과 같다: claude 가 아닌 pane(셸)·닫힌 입력·한글 조합 중·사람이 방금 친
//! pane 은 건드리지 않고, 쓰기는 읽은 입력 번호와 비교해 그사이 누가 쳤으면 버린다. 같은 화면에는
//! 화살표도 Enter 도 한 번씩만 — 화면이 사라졌다 다시 떠야 새로 센다.

use super::*;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

/// 화면 펌프가 이 낱말이 그려진 줄을 보면 그 pane 을 살펴볼 후보로 적는다.
const HINT: &str = "trust this folder";
/// 사람이 마지막으로 친 뒤 이만큼 조용해야 건드린다. claude 도 화면이 뜬 직후 150ms 안에 온
/// 키는 「미리 친 것」으로 버리므로 그보다 넉넉해야 한다.
const QUIET: Duration = Duration::from_millis(500);
/// 화면이 처음 보인 뒤 이만큼은 그리기가 끝나기를 기다린다.
const SETTLE: Duration = Duration::from_millis(500);
const TICK: Duration = Duration::from_millis(150);
const TRUST_LABEL: &str = "Yes, I trust this folder";
const EXIT_LABELS: [&str; 2] = ["No, exit", "No, continue without these permissions"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Focus {
    Trust,
    Exit,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TrustScreen {
    pub(crate) workspace: String,
    pub(crate) focus: Focus,
}

/// 선택 표시(❯)와 옛 판의 번호(`1. `)를 뗀 선택지 글.
fn option_label(line: &str) -> (bool, &str) {
    let (marked, rest) = match line.strip_prefix(['❯', '>', '›']) {
        Some(rest) => (true, rest.trim_start()),
        None => (false, line),
    };
    let rest = match rest.split_once(". ") {
        Some((n, label)) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => label,
        _ => rest,
    };
    (marked, rest)
}

/// 화면 글이 claude 신뢰 화면이면 그 폴더와 지금 초점. 대화 속에 같은 글이 인용된 화면은
/// 입력창의 ❯ 가 하나 더 있어 걸러진다 — 선택 표시가 정확히 하나, 그것도 두 선택지 중 하나여야 한다.
pub(crate) fn trust_screen(text: &str) -> Option<TrustScreen> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let head = lines.iter().position(|l| l.starts_with("Accessing workspace:"))?;
    let rest = &lines[head + 1..];
    if !rest.iter().any(|l| l.starts_with("Quick safety check:")) {
        return None;
    }
    let workspace = rest.iter().find(|l| !l.is_empty())?.to_string();
    let (mut trust, mut exit, mut markers) = (None, None, 0);
    for line in rest {
        let (marked, label) = option_label(line);
        markers += usize::from(marked);
        if label == TRUST_LABEL {
            trust = Some(marked);
        } else if EXIT_LABELS.contains(&label) {
            exit = Some(marked);
        }
    }
    let focus = match (trust?, exit?, markers) {
        (true, false, 1) => Focus::Trust,
        (false, true, 1) => Focus::Exit,
        _ => return None,
    };
    Some(TrustScreen { workspace, focus })
}

/// 펌프 스레드가 고친 줄마다 부르는 값싼 검사 — 할당 없이 낱말만 찾는다.
pub(crate) fn row_mentions_trust(row: &[GridCell]) -> bool {
    let n = HINT.len();
    row.len() >= n && row.windows(n).any(|w| w.iter().map(|c| c.ch).eq(HINT.chars()))
}

/// 펌프 스레드 → GUI 스레드. 판정과 쓰기는 한글 조합 상태를 아는 GUI 스레드가 한다.
pub(crate) fn note(surface: &str) {
    if let Ok(mut watch) = watch().lock() {
        watch.pending.insert(surface.to_string());
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    Down,
    Enter,
}

impl Key {
    fn bytes(self) -> &'static [u8] {
        match self {
            Key::Down => b"\x1b[B",
            Key::Enter => b"\r",
        }
    }
}

#[derive(Default)]
struct Gate {
    claude: bool,
    quiet: bool,
    composing: bool,
    closed: bool,
}

/// 한 화면(같은 PTY·같은 폴더)에서 이미 한 일.
struct Seen {
    pty: usize,
    workspace: String,
    since: Instant,
    moved: bool,
    answered: bool,
}

#[derive(Default)]
struct Watch {
    pending: HashSet<String>,
    seen: HashMap<String, Seen>,
    last_tick: Option<Instant>,
}

fn watch() -> &'static std::sync::Mutex<Watch> {
    static WATCH: std::sync::OnceLock<std::sync::Mutex<Watch>> = std::sync::OnceLock::new();
    WATCH.get_or_init(Default::default)
}

impl Watch {
    fn forget(&mut self, surface: &str) {
        self.pending.remove(surface);
        self.seen.remove(surface);
    }

    /// 지금 이 화면에 보낼 키. 보냈으면 `sent` 로 적는다 — 같은 화면에 같은 키는 다시 안 나온다.
    fn next_key(&mut self, surface: &str, pty: usize, screen: Option<TrustScreen>, gate: &Gate, now: Instant) -> Option<Key> {
        let Some(screen) = screen else {
            self.forget(surface);
            return None;
        };
        let seen = self.seen.entry(surface.to_string()).or_insert_with(|| Seen {
            pty,
            workspace: screen.workspace.clone(),
            since: now,
            moved: false,
            answered: false,
        });
        if seen.pty != pty || seen.workspace != screen.workspace {
            *seen = Seen { pty, workspace: screen.workspace, since: now, moved: false, answered: false };
        }
        if seen.answered || !gate.claude || !gate.quiet || gate.composing || gate.closed
            || now.duration_since(seen.since) < SETTLE {
            return None;
        }
        match screen.focus {
            Focus::Trust => Some(Key::Enter),
            Focus::Exit if !seen.moved => Some(Key::Down),
            Focus::Exit => None,
        }
    }

    fn sent(&mut self, surface: &str, key: Key) {
        if let Some(seen) = self.seen.get_mut(surface) {
            match key {
                Key::Down => seen.moved = true,
                Key::Enter => seen.answered = true,
            }
        }
    }
}

impl App {
    pub(crate) fn trust_prompt_tick(&mut self) {
        let now = Instant::now();
        let surfaces: Vec<String> = {
            let Ok(mut watch) = watch().lock() else { return };
            if watch.pending.is_empty() || watch.last_tick.is_some_and(|at| now.duration_since(at) < TICK) {
                return;
            }
            watch.last_tick = Some(now);
            watch.pending.iter().cloned().collect()
        };
        for surface in surfaces {
            let Some(pty) = self.pty.get(&surface).cloned().or_else(|| kasa_pty::lookup_session(&surface)) else {
                watch().lock().unwrap().forget(&surface);
                continue;
            };
            let revision = pty.input_revision();
            let screen = trust_screen(&pty.visible_text(usize::MAX));
            let gate = match screen {
                Some(_) => Gate {
                    claude: pty.active_agent() == Some(kasa_pty::AgentKind::Claude),
                    quiet: pty.input_quiet_for(QUIET),
                    composing: self.tell_composing(&surface),
                    closed: pty.input_closed(),
                },
                None => Gate::default(),
            };
            let workspace = screen.as_ref().map(|s| s.workspace.clone()).unwrap_or_default();
            let key = watch().lock().unwrap().next_key(&surface, Arc::as_ptr(&pty) as usize, screen, &gate, now);
            let Some(key) = key else { continue };
            if pty.send_bytes_guarded(key.bytes(), Some(revision)).is_ok() {
                watch().lock().unwrap().sent(&surface, key);
                eprintln!("[trust] {surface} 폴더 신뢰 화면 {key:?}: {workspace}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// claude 2.1.286 의 신뢰 화면 — 위 테두리만 있는 대화상자, No 가 먼저.
    fn screen(first: &str, second: &str) -> String {
        format!(
            "────────────────────────────────────────\n \
             Accessing workspace:\n\n \
             /Users/kasa/Desktop/new-repo\n\n \
             Quick safety check: Is this a project you created or one you trust? (Like your own\n \
             code, a well-known open source project, or work from your team).\n\n \
             Claude Code'll be able to read, edit, and execute files here.\n\n \
             Security guide\n\n \
             {first}\n \
             {second}\n\n \
             Enter to confirm · Esc to cancel\n"
        )
    }

    fn gate() -> Gate {
        Gate { claude: true, quiet: true, composing: false, closed: false }
    }

    #[test]
    fn current_screen_starts_on_exit_and_reads_the_folder() {
        let s = trust_screen(&screen("❯ No, exit", "  Yes, I trust this folder")).unwrap();
        assert_eq!(s, TrustScreen { workspace: "/Users/kasa/Desktop/new-repo".into(), focus: Focus::Exit });
        let s = trust_screen(&screen("  No, exit", "❯ Yes, I trust this folder")).unwrap();
        assert_eq!(s.focus, Focus::Trust);
        let s = trust_screen(&screen("❯ No, continue without these permissions", "  Yes, I trust this folder")).unwrap();
        assert_eq!(s.focus, Focus::Exit);
    }

    #[test]
    fn older_numbered_screen_with_yes_selected_is_trust() {
        let s = trust_screen(&screen("❯ 1. Yes, I trust this folder", "  2. No, exit")).unwrap();
        assert_eq!(s.focus, Focus::Trust);
    }

    #[test]
    fn only_the_exact_dialog_matches() {
        assert_eq!(trust_screen(&screen("❯ No, exit", "  Yes, I trust this folder please")), None);
        assert_eq!(trust_screen(&screen("  No, exit", "  Yes, I trust this folder")), None, "no focus");
        assert_eq!(trust_screen(&screen("❯ No, exit", "❯ Yes, I trust this folder")), None, "two foci");
        let without_check = screen("❯ No, exit", "  Yes, I trust this folder").replace("Quick safety check:", "Note:");
        assert_eq!(trust_screen(&without_check), None);
        // 지시문·대화에 인용된 문구는 화면 구성이 아니다.
        assert_eq!(trust_screen("「Yes, I trust this folder」 뜨는 거 다 할 수 있게\n❯ "), None);
    }

    #[test]
    fn dialog_quoted_inside_a_conversation_is_ignored() {
        let quoted = format!(
            "⏺ Bash(kasaterm-cli peek %3)\n  ⎿  {}\n────────────\n❯ \n────────────\n  ? for shortcuts\n",
            screen("❯ No, exit", "  Yes, I trust this folder")
        );
        assert_eq!(trust_screen(&quoted), None);
    }

    #[test]
    fn hint_scan_finds_the_label_in_a_grid_row() {
        let row = |s: &str| s.chars().map(|ch| GridCell { ch, ..GridCell::blank() }).collect::<Vec<_>>();
        assert!(row_mentions_trust(&row("   ❯ Yes, I trust this folder   ")));
        assert!(!row_mentions_trust(&row("Yes, I trust this")));
        assert!(!row_mentions_trust(&[]));
    }

    #[test]
    fn moves_to_trust_once_then_enters_once() {
        let mut w = Watch::default();
        let t0 = Instant::now();
        let exit = || trust_screen(&screen("❯ No, exit", "  Yes, I trust this folder"));
        let trust = || trust_screen(&screen("  No, exit", "❯ Yes, I trust this folder"));
        assert_eq!(w.next_key("%3", 1, exit(), &gate(), t0), None, "waits for the dialog to settle");
        let later = t0 + SETTLE;
        assert_eq!(w.next_key("%3", 1, exit(), &gate(), later), Some(Key::Down));
        w.sent("%3", Key::Down);
        assert_eq!(w.next_key("%3", 1, exit(), &gate(), later + SETTLE), None, "arrow is not repeated on the same screen");
        assert_eq!(w.next_key("%3", 1, trust(), &gate(), later + SETTLE), Some(Key::Enter));
        w.sent("%3", Key::Enter);
        assert_eq!(w.next_key("%3", 1, trust(), &gate(), later + SETTLE * 2), None, "Enter is sent once");
        assert_eq!(w.next_key("%3", 1, exit(), &gate(), later + SETTLE * 2), None);
    }

    #[test]
    fn a_new_screen_starts_over_after_the_old_one_is_gone() {
        let mut w = Watch::default();
        let t0 = Instant::now();
        let trust = || trust_screen(&screen("  No, exit", "❯ Yes, I trust this folder"));
        assert_eq!(w.next_key("%3", 1, trust(), &gate(), t0 + SETTLE), None, "first sight only starts the clock");
        assert_eq!(w.next_key("%3", 1, trust(), &gate(), t0 + SETTLE * 2), Some(Key::Enter));
        w.sent("%3", Key::Enter);
        assert_eq!(w.next_key("%3", 1, None, &gate(), t0 + SETTLE * 3), None);
        assert!(!w.pending.contains("%3") && !w.seen.contains_key("%3"));
        let t1 = t0 + SETTLE * 4;
        assert_eq!(w.next_key("%3", 1, trust(), &gate(), t1), None);
        assert_eq!(w.next_key("%3", 1, trust(), &gate(), t1 + SETTLE), Some(Key::Enter));
        // 같은 pane 번호에 새 PTY 가 앉으면 그것도 새 화면이다.
        w.sent("%3", Key::Enter);
        assert_eq!(w.next_key("%3", 2, trust(), &gate(), t1 + SETTLE * 2), None);
        assert_eq!(w.next_key("%3", 2, trust(), &gate(), t1 + SETTLE * 3), Some(Key::Enter));
    }

    #[test]
    fn shells_closed_input_typing_and_composition_are_left_alone() {
        let trust = || trust_screen(&screen("  No, exit", "❯ Yes, I trust this folder"));
        for blocked in [
            Gate { claude: false, ..gate() },
            Gate { quiet: false, ..gate() },
            Gate { composing: true, ..gate() },
            Gate { closed: true, ..gate() },
        ] {
            let mut w = Watch::default();
            let t0 = Instant::now();
            assert_eq!(w.next_key("%3", 1, trust(), &blocked, t0), None);
            assert_eq!(w.next_key("%3", 1, trust(), &blocked, t0 + SETTLE * 4), None);
            assert_eq!(w.next_key("%3", 1, trust(), &gate(), t0 + SETTLE * 4), Some(Key::Enter), "proceeds once the guard clears");
        }
    }
}
