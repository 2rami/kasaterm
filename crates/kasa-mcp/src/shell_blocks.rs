//! 거울(PC 거울·폰)이 원본 셸 칸을 「명령 + 결과」 묶음으로 다시 그리는 데이터 창구.
//! 원본 PTY 크기는 건드리지 않는다 — 결과는 원본 폭과 상관없는 논리 줄로 주고, 접기는
//! 보는 쪽이 제 폭으로 한다. 정본 규칙은 `docs/mirror-render.md`.
//!
//! `GET /term/blocks?pane=%7&since=<stamp>&wait=<ms>&have=<id>&lines=<n>` →
//! `{ok, pane, since, integration, alt, cols, rows, oldest, newest, blocks:[…]}`.
//! `since` 가 지금 도장과 같으면 바뀔 때까지(최대 `wait`) 기다렸다 답한다.
//! `have` 보다 id 가 큰 블록과, 아직 도는 블록은 늘 싣는다. `block=<id>` 는 그 블록 하나를
//! 줄 상한 없이 준다(접힌 긴 결과 펼치기).
//!
//! 블록마다 `cwd`(명령이 돈 폴더)와 `links`(`[[줄, 글자 시작, 글자 수, 절대 경로], …]`)를 싣는다.
//! 결과 속 낱말 가운데 이 기계에 실제로 있는 폴더만 고리다 — 보는 쪽은 그 자리를 눌러 `cd` 한다.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use axum::extract::{Query, RawQuery};
use axum::Json;
use kasa_bridge::screen::Color;
use kasa_pty::block_lines::{block_lines, StyledLine};
use kasa_pty::{CommandBlock, PtySession};
use serde_json::{json, Value};

const WAIT_MAX_MS: u64 = 25_000;
const POLL_EVERY: Duration = Duration::from_millis(100);
/// 묶음 목록에서 블록 하나에 싣는 줄 상한. 넘으면 앞 1/4 · 뒤 3/4 만 싣고 가운데를 비운다.
const LINES_DEFAULT: usize = 160;
const LINES_FULL_MAX: usize = 20_000;
/// 블록 하나에서 폴더인지 확인해 보는 낱말 수와 싣는 고리 수 — 도는 명령은 250ms 마다
/// 다시 실리므로 stat 을 무한정 돌리지 않는다.
const LINK_CHECKS: usize = 400;
const LINKS_MAX: usize = 200;

pub(crate) async fn term_blocks_get(
    RawQuery(raw): RawQuery,
    Query(q): Query<HashMap<String, String>>,
) -> Json<Value> {
    let pane = q
        .get("pane")
        .filter(|p| kasa_pty::lookup_session(p).is_some())
        .cloned()
        .or_else(|| raw_param(raw.as_deref(), "pane"))
        .or_else(|| q.get("pane").cloned());
    let Some(pane) = pane.filter(|p| !p.is_empty()) else {
        return Json(json!({ "ok": false, "error": "`pane` 이 필요해요" }));
    };
    let num = |k: &str| q.get(k).and_then(|v| v.parse::<u64>().ok());
    let since = num("since");
    let wait = num("wait").unwrap_or(0).min(WAIT_MAX_MS);
    let deadline = Instant::now() + Duration::from_millis(wait);
    loop {
        let Some(sess) = kasa_pty::lookup_session(&pane) else {
            return Json(json!({ "ok": false, "gone": true, "error": format!("세션 {pane} 이 없다") }));
        };
        let stamp = stamp(&sess);
        if since != Some(stamp) || Instant::now() >= deadline {
            return Json(answer(&pane, &sess, stamp, &q));
        }
        drop(sess);
        tokio::time::sleep(POLL_EVERY).await;
    }
}

/// 블록 바뀜 번호와 대체 화면 여부를 한 수로 — 둘 중 하나만 바뀌어도 깬다.
fn stamp(sess: &PtySession) -> u64 {
    sess.blocks_rev() * 2 + sess.alt_screen() as u64
}

fn answer(pane: &str, sess: &PtySession, stamp: u64, q: &HashMap<String, String>) -> Value {
    let num = |k: &str| q.get(k).and_then(|v| v.parse::<u64>().ok());
    let (cols, rows) = sess.size();
    let store = sess.blocks_arc();
    let blocks: Vec<CommandBlock> = {
        let all = store.lock().unwrap();
        match num("block") {
            Some(id) => all.iter().filter(|b| b.id == id).cloned().collect(),
            None => {
                let have = num("have").unwrap_or(0);
                all.iter()
                    .filter(|b| b.id > have || b.duration_ms.is_none())
                    .cloned()
                    .collect()
            }
        }
    };
    let (oldest, newest) = {
        let all = store.lock().unwrap();
        (all.front().map_or(0, |b| b.id), all.back().map_or(0, |b| b.id))
    };
    let max_lines = match (num("block"), num("lines")) {
        (Some(_), _) | (_, Some(0)) => LINES_FULL_MAX,
        (_, Some(n)) => (n as usize).clamp(8, LINES_FULL_MAX),
        _ => LINES_DEFAULT,
    };
    let blocks: Vec<Value> = blocks.iter().map(|b| block_json(b, max_lines)).collect();
    json!({
        "ok": true,
        "pane": pane,
        "since": stamp,
        "integration": sess.blocks_rev() > 0,
        "alt": sess.alt_screen(),
        "cols": cols,
        "rows": rows,
        "oldest": oldest,
        // 보는 쪽이 가진 것보다 작으면 원본 셸이 새로 떠 번호가 처음부터다.
        "newest": newest,
        "blocks": blocks,
    })
}

fn block_json(b: &CommandBlock, max_lines: usize) -> Value {
    let lines = block_lines(&b.output);
    let count = lines.len();
    let mut out = json!({
        "id": b.id,
        "cmd": b.command,
        "start_ms": b.started_ms,
        "running": b.duration_ms.is_none(),
        "count": count,
    });
    let o = out.as_object_mut().unwrap();
    if let Some(code) = b.exit_code {
        o.insert("exit".into(), json!(code));
    }
    if let Some(ms) = b.duration_ms {
        o.insert("ms".into(), json!(ms));
    }
    if b.is_tui {
        o.insert("tui".into(), json!(true));
    }
    if b.dropped_lines > 0 {
        o.insert("dropped".into(), json!(b.dropped_lines));
    }
    let sent: Vec<&StyledLine> = if count > max_lines {
        let head = max_lines / 4;
        let tail = max_lines - head;
        o.insert("gap_at".into(), json!(head));
        o.insert("gap".into(), json!(count - head - tail));
        lines[..head].iter().chain(lines[count - tail..].iter()).collect()
    } else {
        lines.iter().collect()
    };
    if let Some(cwd) = &b.cwd {
        o.insert("cwd".into(), json!(cwd));
    }
    let links = dir_links(&sent, b.cwd.as_deref().map(Path::new));
    if !links.is_empty() {
        o.insert("links".into(), Value::Array(links));
    }
    o.insert("lines".into(), Value::Array(sent.into_iter().map(line_json).collect()));
    out
}

/// 결과 줄 속 폴더 이름 — 빈칸으로 가른 낱말을 명령이 돈 폴더 기준으로 풀어 실제로 있는
/// 폴더만 고른다. 앞뒤 따옴표·괄호·쌍점과 끝의 `/` 는 떼고 잰다(`ls -F`·`grep` 의 `경로:`).
/// 폴더를 모르는 블록은 절대 경로·`~` 만 푼다.
fn dir_links(lines: &[&StyledLine], cwd: Option<&Path>) -> Vec<Value> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let mut checks = 0;
    let mut out = Vec::new();
    for (row, line) in lines.iter().enumerate() {
        let text: String = line.iter().map(|s| s.text.as_str()).collect();
        let chars: Vec<char> = text.chars().collect();
        // `ls -1` 의 「with space」처럼 빈칸 든 이름은 낱말로 가르면 못 찾는다 — 줄 통째가 폴더인지 먼저 본다.
        let whole = text.trim();
        if whole.contains(char::is_whitespace) && whole.chars().count() <= 255 && checks < LINK_CHECKS {
            checks += 1;
            let path = if whole.starts_with('/') { Some(std::path::PathBuf::from(whole)) } else { cwd.map(|c| c.join(whole)) };
            if let Some(path) = path.filter(|p| p.is_dir()) {
                let a = text.chars().take_while(|c| c.is_whitespace()).count();
                let abs = std::fs::canonicalize(&path).unwrap_or(path);
                out.push(json!([row, a, whole.chars().count(), abs.display().to_string()]));
                if out.len() >= LINKS_MAX {
                    return out;
                }
                continue;
            }
        }
        let mut i = 0;
        while i < chars.len() {
            if chars[i].is_whitespace() {
                i += 1;
                continue;
            }
            let start = i;
            while i < chars.len() && !chars[i].is_whitespace() {
                i += 1;
            }
            let lead = chars[start..i].iter().take_while(|c| "\"'(<[`".contains(**c)).count();
            let trail = chars[start + lead..i].iter().rev().take_while(|c| "\"'),;:>]`".contains(**c)).count();
            let (a, z) = (start + lead, i - trail);
            if a >= z || z - a > 255 {
                continue;
            }
            let word: String = chars[a..z].iter().collect();
            if word.contains("://") || word.chars().all(|c| c == '-') {
                continue;
            }
            let bare = word.trim_end_matches('/');
            let path = if word.starts_with('/') {
                Some(std::path::PathBuf::from(if bare.is_empty() { "/" } else { bare }))
            } else if bare == "~" || bare.starts_with("~/") {
                home.as_ref().map(|h| h.join(bare.trim_start_matches('~').trim_start_matches('/')))
            } else {
                cwd.map(|c| c.join(bare))
            };
            let Some(path) = path else { continue };
            if checks >= LINK_CHECKS {
                return out;
            }
            checks += 1;
            if path.is_dir() {
                let abs = std::fs::canonicalize(&path).unwrap_or(path);
                out.push(json!([row, a, z - a, abs.display().to_string()]));
                if out.len() >= LINKS_MAX {
                    return out;
                }
            }
        }
    }
    out
}

/// 한 줄 = 조각 목록. 조각은 `{t, f?, b?, s?}` — 색은 0~255 팔레트 번호, 트루컬러는
/// `0x1000000 | rgb`, 기본색·꾸밈 없음은 키를 뺀다.
fn line_json(line: &StyledLine) -> Value {
    Value::Array(
        line.iter()
            .map(|span| {
                let mut v = json!({ "t": span.text });
                let o = v.as_object_mut().unwrap();
                if let Some(f) = color_num(&span.fg) {
                    o.insert("f".into(), json!(f));
                }
                if let Some(b) = color_num(&span.bg) {
                    o.insert("b".into(), json!(b));
                }
                if span.flags != 0 {
                    o.insert("s".into(), json!(span.flags));
                }
                v
            })
            .collect(),
    )
}

fn color_num(c: &Color) -> Option<u32> {
    match c {
        Color::Default => None,
        Color::Idx(i) => Some(*i as u32),
        Color::Rgb(r, g, b) => Some(0x100_0000 | (*r as u32) << 16 | (*g as u32) << 8 | *b as u32),
    }
}

/// `?pane=%7` 을 인코딩 없이 친 주소는 디코딩되며 깨진다 — 원문에서 한 번 더 본다.
fn raw_param(raw: Option<&str>, key: &str) -> Option<String> {
    raw?.split('&')
        .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_output_sends_head_and_tail_with_a_gap() {
        let output: String = (0..300).map(|i| format!("line {i}\r\n")).collect();
        let b = CommandBlock {
            id: 3,
            command: "seq".into(),
            output,
            exit_code: Some(0),
            started_ms: 1,
            duration_ms: Some(12),
            is_tui: false,
            dropped_lines: 0,
            cwd: None,
        };
        let v = block_json(&b, 160);
        assert_eq!(v["count"], 300);
        assert_eq!(v["gap_at"], 40);
        assert_eq!(v["gap"], 140);
        let lines = v["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 160);
        assert_eq!(lines[0][0]["t"], "line 0");
        assert_eq!(lines[40][0]["t"], "line 180");
        assert_eq!(v["running"], false);
        assert_eq!(v["exit"], 0);
    }

    #[test]
    fn spans_carry_palette_and_truecolor() {
        let line = &block_lines("\x1b[1;31mE\x1b[0m\x1b[38;2;1;2;3mx\x1b[0m.")[0];
        let v = line_json(line);
        assert_eq!(v[0], json!({"t": "E", "f": 1, "s": 1}));
        assert_eq!(v[1], json!({"t": "x", "f": 0x100_0000 | 0x010203}));
        assert_eq!(v[2], json!({"t": "."}));
    }

    #[test]
    fn folder_words_become_links_with_absolute_paths() {
        let dir = std::env::temp_dir().join(format!("kt-links-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("with space")).unwrap();
        std::fs::write(dir.join("a.txt"), "").unwrap();
        let lines = block_lines("src/  a.txt  'src':\r\n없는폴더 ..\r\n");
        let refs: Vec<&StyledLine> = lines.iter().collect();
        let links = dir_links(&refs, Some(&dir));
        let canon = std::fs::canonicalize(&dir).unwrap();
        let src = canon.join("src").display().to_string();
        let parent = canon.parent().unwrap().display().to_string();
        assert_eq!(links, vec![json!([0, 0, 4, src]), json!([0, 14, 3, src]), json!([1, 5, 2, parent])]);
        assert!(dir_links(&refs, None).is_empty(), "폴더를 모르면 상대 경로는 안 푼다");
        let spaced = block_lines("with space\r\n");
        let spaced: Vec<&StyledLine> = spaced.iter().collect();
        assert_eq!(dir_links(&spaced, Some(&dir)), vec![json!([0, 0, 10, canon.join("with space").display().to_string()])]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn raw_pane_survives_unencoded_percent() {
        assert_eq!(raw_param(Some("since=4&pane=%116&wait=0"), "pane").as_deref(), Some("%116"));
        assert_eq!(raw_param(Some("panes=1"), "pane"), None);
    }
}
