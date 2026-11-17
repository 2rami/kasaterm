//! 키 하나가 창에 들어와 PTY·거울 연결로 나가기까지의 발자국.
//!
//! 「키가 가끔 씹힌다」는 말만으로는 어디서 없어졌는지 가를 수 없다 — OS·IME 가 안 넘겼는지, 앞을 가린
//! 화면이 삼켰는지, tell 붙여넣기가 붙들었는지, 거울 연결이 잃었는지. 늘 켜 둔 메모리 고리에 단계마다
//! 한 줄씩 남기고 `kasaterm-cli input-trace` 로 그 순간 꺼내 본다. 파일로는 쓰지 않는다.
//!
//! 친 글자는 남기지 않는다(비밀번호도 키다). 글자는 종류만(`a`·`A`·`0`·`가`·`?`), 이름 있는 키는 이름,
//! 바이트는 길이만 남긴다 — 「다섯 자 쳤는데 넷만 갔다」를 세는 데는 그것으로 충분하다.
use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CAPACITY: usize = 4000;

struct Entry {
    at: Instant,
    wall_ms: u64,
    key: u64,
    step: &'static str,
    surface: String,
    detail: String,
}

static RING: Mutex<VecDeque<Entry>> = Mutex::new(VecDeque::new());
static NEXT_KEY: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static CURRENT: Cell<u64> = const { Cell::new(0) };
    static SETTLED: Cell<bool> = const { Cell::new(false) };
}

fn push(key: u64, step: &'static str, surface: &str, detail: String) {
    let wall_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    let mut ring = RING.lock().unwrap_or_else(|e| e.into_inner());
    if ring.len() >= CAPACITY {
        ring.pop_front();
    }
    ring.push_back(Entry { at: Instant::now(), wall_ms, key, step, surface: surface.to_string(), detail });
}

/// 창이 키(또는 IME 확정)를 받았다. 이 스레드에서 `end` 까지 일어난 쓰기는 이 키의 것으로 묶인다.
pub fn begin(detail: String) -> u64 {
    let key = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
    CURRENT.with(|c| c.set(key));
    SETTLED.with(|s| s.set(false));
    push(key, "recv", "", detail);
    key
}

/// 키 처리가 끝났다. 어디에도 쓰이지 않았으면 그때의 화면 상태를 붙여 「안 보냄」으로 남긴다 —
/// 수십 갈래의 이른 반환마다 이유를 달지 않아도, 무엇이 앞을 가리고 있었는지가 곧 이유다.
pub fn end(key: u64, state: impl FnOnce() -> String) {
    let settled = SETTLED.with(|s| s.replace(false));
    CURRENT.with(|c| c.set(0));
    if !settled {
        push(key, "unsent", "", state());
    }
}

/// 지금 처리 중인 키의 단계 하나. 키 밖(tell·거울 송신 스레드)에서 불리면 번호 없이 남는다.
pub fn note(step: &'static str, surface: &str, detail: String) {
    let key = CURRENT.with(Cell::get);
    if key != 0 {
        SETTLED.with(|s| s.set(true));
    }
    push(key, step, surface, detail);
}

/// 키와 무관한 사건(메인 스레드 멈춤 등). 처리 중인 키를 「보냈다」로 치지 않는다.
pub fn event(step: &'static str, detail: String) {
    push(0, step, "", detail);
}

/// 글자는 종류로만 바꾼다.
pub fn mask(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            'a'..='z' => 'a',
            'A'..='Z' => 'A',
            '0'..='9' => '0',
            ' ' => '_',
            '\u{1100}'..='\u{11FF}' | '\u{3130}'..='\u{318F}' | '\u{AC00}'..='\u{D7A3}' => '가',
            c if c.is_control() => '^',
            _ => '?',
        })
        .collect()
}

/// 최근 `within` 안의 발자국을 오래된 것부터.
pub fn dump(within: Duration) -> Vec<String> {
    let now = Instant::now();
    let ring = RING.lock().unwrap_or_else(|e| e.into_inner());
    ring.iter()
        .filter(|e| now.duration_since(e.at) <= within)
        .map(|e| {
            let secs = e.wall_ms / 1000;
            let local = clock_of_day(secs);
            let key = if e.key == 0 { "    -".to_string() } else { format!("#{:>4}", e.key) };
            let surface = if e.surface.is_empty() { String::new() } else { format!(" {}", e.surface) };
            format!("{local}.{:03} {key} {}{surface} {}", e.wall_ms % 1000, e.step, e.detail)
        })
        .collect()
}

/// 사람이 말한 시각(「방금 3시 12분에」)과 맞춰 보려고 현지 시각으로 찍는다. 시간대 라이브러리를 들이지 않고
/// 한국 시각(일광 절약 없음)을 기본으로, 다른 곳은 `KASATERM_TRACE_UTC_OFFSET`(초)로 바꾼다.
fn clock_of_day(unix_secs: u64) -> String {
    let offset: i64 = std::env::var("KASATERM_TRACE_UTC_OFFSET").ok().and_then(|v| v.parse().ok()).unwrap_or(9 * 3600);
    let s = (unix_secs as i64 + offset).rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_text_never_reaches_the_ring() {
        assert_eq!(mask("pw: Hunter2 가나"), "aa?_Aaaaaa0_가가");
        let key = begin(format!("key {}", mask("s3cret")));
        note("pty", "%5", "6B ok".into());
        end(key, || "never".into());
        let lines = dump(Duration::from_secs(60));
        assert!(lines.iter().all(|l| !l.contains("s3cret")));
        assert!(lines.iter().any(|l| l.contains("recv") && l.contains("a0aaaa")));
        assert!(!lines.iter().any(|l| l.contains(&format!("#{key:>4} unsent"))), "a written key is settled");
    }

    #[test]
    fn a_key_that_went_nowhere_records_what_covered_the_pane() {
        let key = begin("key Enter".into());
        end(key, || "modal=restore".into());
        let lines = dump(Duration::from_secs(60));
        assert!(lines.iter().any(|l| l.contains(&format!("#{key:>4} unsent")) && l.contains("modal=restore")));
    }
}
