//! 사람을 부를 때와 그때 할 말 — 데스크톱 OS 알림과 폰 푸시가 이것 하나를 쓴다(docs/design.md 「에이전트 알림 판정」).
//!
//! 부르는 것은 넷뿐이다: 승인 기다림·질문 기다림·막힘(막힘·승인 필요·재시작 필요·실패 보고, 오래 가는 오류)·정말 다
//! 끝남. 턴이 끝날 때마다 울리니 백그라운드 작업을 기다리며 턴을 몇 번이고 끝내는 학생 때문에 정작 봐야 할 승인·완료가
//! 묻혔고, 데스크톱과 폰이 서로 다른 규칙으로 울어 한쪽에만 오는 알림이 생겼다.
//!
//! 「정말 다 끝남」은 정상 종료한 턴 + 그 칸에 도는 백그라운드 작업·하위 에이전트·모니터 없음 + 스스로 깨어날 예약 없음이
//! [`SETTLE`] 동안 그대로일 때다. 학생의 완료 보고(`kasaterm-cli done`)가 가장 확실한 끝이라, 보고가 있으면 그 턴의 끝은
//! 보고한 일과 같은 일로 묶는다.

use std::time::{Duration, Instant};

/// 끝남은 이만큼 그대로여야 울린다. Stop 훅이 턴을 바로 잇거나 끝나자마자 백그라운드 통보·다른 학생의 말로 다음 턴이
/// 서는 잔걸음을 하나로 묶는다 — 마지막 끝만 울린다.
pub const SETTLE: Duration = Duration::from_secs(5);
/// 알림 본문 상한(글자). 잠금화면 두세 줄이다.
pub const BODY_MAX: usize = 140;

/// 사람을 부르는 대기 — 사람이 골라야 하는 것뿐이다(답 기다림·오래 쉼은 부르지 않는다).
pub fn wait_rings(kind: &str) -> bool {
    matches!(kind, "permission" | "question")
}

/// 학생의 완료 보고 한 건. `key` 는 보고마다 다르다(보고 시각).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub key: u64,
    pub outcome: String,
    pub summary: String,
}

impl Report {
    /// 성공이 아닌 보고 — 막힘·승인 필요·재시작 필요도 판에는 실패로 적힌다(`kasa_socket::cli::done_mark`).
    pub fn blocked(&self) -> bool {
        self.outcome != "succeeded"
    }
}

/// 한 칸의 지금 사실.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    /// 세션 신원. 없으면 증거가 없어 끝남을 판정하지 않는다.
    pub source: Option<String>,
    pub idle: bool,
    /// 정상 종료한 턴의 열쇠(새 종료면 바뀐다).
    pub completion: Option<u64>,
    pub error: bool,
    /// 백그라운드 작업·하위 에이전트·모니터가 돈다.
    pub busy: bool,
    pub questions: bool,
    /// 스스로 깨어날 예약이 있다(ScheduleWakeup 등).
    pub continues: bool,
    /// 지금 칸에 붙은 완료 보고 — 다음 일을 받으면 걷힌다.
    pub report: Option<Report>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ring {
    /// 정말 다 끝남. 보고로 끝났으면 그 보고, `key` 는 알림 겹침을 가르는 열쇠.
    Finished { key: u64, report: Option<Report> },
    Blocked { report: Report },
}

/// 칸 하나의 끝남 판정. 처음 보는 칸·세션이 바뀐 칸은 기준만 잡고 울리지 않는다(켜자마자 지난 끝이 울리지 않게).
#[derive(Default)]
pub struct Finish {
    seen: bool,
    source: Option<String>,
    completion: Option<u64>,
    report: Option<u64>,
    pending: Option<Pending>,
}

struct Pending {
    key: u64,
    report: Option<Report>,
    since: Option<Instant>,
}

impl Finish {
    /// 끝남이 그대로 있으면 울릴 때 — 부르는 쪽이 그때 다시 깨어 본다.
    pub fn deadline(&self) -> Option<Instant> {
        self.pending.as_ref().and_then(|p| p.since).map(|at| at + SETTLE)
    }

    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn update(&mut self, facts: &Facts, now: Instant) -> Option<Ring> {
        if !self.seen || self.source != facts.source {
            *self = Self {
                seen: true,
                source: facts.source.clone(),
                completion: facts.completion,
                report: facts.report.as_ref().map(|r| r.key),
                pending: None,
            };
            return None;
        }
        if facts.source.is_none() {
            return None;
        }
        let mut ring = None;
        if let Some(report) = facts.report.as_ref().filter(|r| Some(r.key) != self.report) {
            self.report = Some(report.key);
            if report.blocked() {
                self.pending = None;
                ring = Some(Ring::Blocked { report: report.clone() });
            } else {
                self.pending = Some(Pending { key: report.key, report: Some(report.clone()), since: None });
            }
        }
        if let Some(key) = facts.completion.filter(|key| Some(*key) != self.completion) {
            self.completion = Some(key);
            // 보고가 붙어 있으면 이 턴 끝은 보고한 그 일의 끝이다 — 보고로 울리거나 이미 울렸다.
            let reported = facts.report.is_some() || self.pending.as_ref().is_some_and(|p| p.report.is_some());
            if !reported {
                self.pending = Some(Pending { key, report: None, since: None });
            }
        }
        // 일하는 중인데 종료 열쇠가 없다 — 그 끝은 마지막 끝이 아니었다(Stop 훅이 이었거나 다음 턴이 섰다). 조용해진 뒤
        // 옛 열쇠가 다시 보여도 새 끝이 아니다.
        let reopened = facts.completion.is_none() && !facts.idle;
        if facts.error || reopened && self.pending.as_ref().is_some_and(|p| p.report.is_none()) {
            self.pending = None;
        }
        let settled = facts.idle && !facts.busy && !facts.questions && !facts.continues;
        if let Some(pending) = self.pending.as_mut() {
            if !settled {
                pending.since = None;
            } else if now.saturating_duration_since(*pending.since.get_or_insert(now)) >= SETTLE {
                let done = self.pending.take().map(|p| Ring::Finished { key: p.key, report: p.report });
                return ring.or(done);
            }
        }
        ring
    }
}

/// 다른 기기 `/term/panes` 행의 완료 보고. 보고 시각이 없는 옛 원본은 같은 보고를 가를 열쇠가 없어 없는 것으로 본다.
pub fn row_report(row: &serde_json::Value) -> Option<Report> {
    Some(Report {
        key: row.get("done_at_ms")?.as_u64()?,
        outcome: row.get("done_outcome")?.as_str()?.to_string(),
        summary: row.get("done_summary").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
    })
}

/// 다른 기기 행을 판정 사실로. 원본이 실은 `response_end` 가 그 행·세션의 것이고 낡지 않았을 때만 출처를 준다 — 증거가
/// 없으면(옛 원본·끊긴 캐시) 쉼만으로 끝남을 추측하지 않는다.
pub fn row_facts(row: &serde_json::Value) -> Facts {
    let end = row
        .get("response_end")
        .filter(|v| v.is_object())
        .and_then(|v| serde_json::from_value::<kasa_socket::backend::ResponseEnd>(v.clone()).ok())
        .filter(|end| {
            !end.source.is_empty()
                && end.session_id.as_deref().is_some_and(|sid| !sid.is_empty())
                && row.get("id").and_then(|v| v.as_str()) == Some(end.surface_id.as_str())
                && row.get("state_fresh").and_then(|v| v.as_bool()) != Some(false)
        });
    let Some(end) = end else { return Facts::default() };
    Facts {
        source: Some(format!("{}:{}", end.session_id.unwrap_or_default(), end.source)),
        idle: end.idle,
        completion: end.completion.and_then(|key| u64::from_str_radix(&key, 16).ok()),
        error: end.error,
        busy: end.background,
        questions: !end.questions.is_empty(),
        continues: end.continues,
        report: row_report(row),
    }
}

// ── 할 말 ──────────────────────────────────────────────────────────────────────

fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out = out.trim_end().to_string();
    out.push('…');
    out
}

/// 링크·그림·강조·코드 표시를 걷고 안의 글은 남긴다.
fn plain_inline(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('[') {
        let (head, tail) = rest.split_at(open);
        out.push_str(head.strip_suffix('!').unwrap_or(head));
        let Some(close) = tail.find("](") else {
            out.push_str(tail);
            rest = "";
            break;
        };
        let Some(end) = tail[close..].find(')') else {
            out.push_str(tail);
            rest = "";
            break;
        };
        out.push_str(&tail[1..close]);
        rest = &tail[close + end + 1..];
    }
    out.push_str(rest);
    out.replace("**", "").replace("__", "").replace(['`', '*'], "")
}

fn strip_marker(line: &str) -> &str {
    let line = line.trim_start_matches('>').trim_start();
    if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")).or_else(|| line.strip_prefix("+ ")) {
        return rest.trim_start();
    }
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 {
        if let Some(rest) = line[digits..].strip_prefix(". ").or_else(|| line[digits..].strip_prefix(") ")) {
            return rest.trim_start();
        }
    }
    line
}

/// 앞에서부터 `n` 문장. 마침표 뒤에 빈칸이 와야 문장 끝으로 본다(`push.rs`·`0.2.1` 은 끝이 아니다).
fn first_sentences(text: &str, n: usize) -> &str {
    let mut count = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if matches!(c, '.' | '!' | '?' | '。' | '…') {
            let end = at + c.len_utf8();
            if chars.peek().is_none_or(|(_, next)| next.is_whitespace()) {
                count += 1;
                if count == n {
                    return &text[..end];
                }
            }
        }
    }
    text
}

/// 학생이 한 말을 알림 한두 문장으로 — 코드 덩이·표·마크다운 표시를 걷고 첫 두 문장만, [`BODY_MAX`] 안으로.
pub fn spoken(text: &str) -> String {
    let mut parts = Vec::new();
    let mut fenced = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with("```") || line.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        let rule = line.chars().all(|c| matches!(c, '-' | '=' | '*' | '_' | ' '));
        // 머리글은 말이 아니라 문서의 차례다.
        if fenced || rule || line.starts_with('|') || line.starts_with('#') {
            continue;
        }
        let words = plain_inline(strip_marker(line));
        let words = words.trim();
        if !words.is_empty() {
            parts.push(words.to_string());
        }
    }
    let flat = parts.join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
    clip(first_sentences(&flat, 2), BODY_MAX)
}

fn file_name(path: &str) -> &str {
    path.trim_end_matches('/').rsplit(['/', '\\']).next().unwrap_or(path)
}

fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split(['/', '?', '#']).next().unwrap_or(rest)
}

/// 승인을 구하는 말 — 무엇을 하려는지가 드러나게(도구 이름이 아니라 그 도구로 할 일).
pub fn ask(tool: &str, input: Option<&serde_json::Value>) -> String {
    let field = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| input?.get(*k)?.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let line = |s: &str, max| clip(s.lines().next().unwrap_or(s), max);
    let what = match tool {
        "Bash" | "shell" | "exec_command" | "local_shell" => {
            field(&["command", "cmd"]).map(|c| format!("「{}」 실행해도 될까요?", line(c, 60)))
        }
        "Edit" | "MultiEdit" | "NotebookEdit" | "apply_patch" => {
            field(&["file_path", "notebook_path", "path"]).map(|p| format!("「{}」 고쳐도 될까요?", file_name(p)))
        }
        "Write" => field(&["file_path", "path"]).map(|p| format!("「{}」 파일을 써도 될까요?", file_name(p))),
        "Read" => field(&["file_path", "path"]).map(|p| format!("「{}」 읽어 봐도 될까요?", file_name(p))),
        "WebFetch" => field(&["url"]).map(|u| format!("「{}」 열어 봐도 될까요?", host(u))),
        "WebSearch" => field(&["query"]).map(|q| format!("「{}」 찾아봐도 될까요?", line(q, 40))),
        _ => None,
    };
    let what = what.unwrap_or_else(|| match tool {
        "" => "이거 해도 될까요?".to_string(),
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" | "apply_patch" => "파일을 고쳐도 될까요?".to_string(),
        "Bash" | "shell" | "exec_command" | "local_shell" => "명령을 실행해도 될까요?".to_string(),
        _ if tool.starts_with("mcp__") => format!("「{}」 도구를 써도 될까요?", tool.rsplit("__").next().unwrap_or(tool)),
        _ => format!("「{tool}」 써도 될까요?"),
    });
    format!("선생님, {what}")
}

/// 대기 이유 글(`"Bash 승인 대기"`·훅의 `"Claude needs your permission to use Bash"`)에서 도구 이름.
pub fn tool_in_reason(reason: &str) -> &str {
    let reason = reason.trim();
    if let Some(tool) = reason.strip_suffix("승인 대기") {
        return tool.trim();
    }
    reason.split_once("permission to use ").map_or("", |(_, tool)| tool.split_whitespace().next().unwrap_or(""))
}

/// 학생이 묻는 말. 비면 묻고 싶은 것이 있다는 말만.
pub fn question(prompt: &str) -> String {
    let said = spoken(prompt);
    if said.is_empty() {
        "선생님, 여쭤볼 게 있어요.".to_string()
    } else if said.starts_with("선생님") {
        said
    } else {
        format!("선생님, {said}")
    }
}

/// 막힌 보고 — 무엇 때문에 멈췄는지를 앞에 말한다.
pub fn blocked(report: &Report) -> String {
    let (mark, rest) = kasa_socket::cli::done_mark(&report.outcome, &report.summary);
    let head = match mark {
        "막힘" => "막혔어요.",
        "승인 필요" => "승인이 필요해요.",
        "재시작 필요" => "다시 시작해 주셔야 해요.",
        _ => "잘 안 됐어요.",
    };
    let rest = spoken(rest);
    clip(&format!("선생님, {head} {rest}"), BODY_MAX)
}

/// 저절로 안 풀리는 오류로 멈췄다.
pub fn trouble(label: &str) -> String {
    clip(&format!("선생님, 막혔어요. {}", label.trim()), BODY_MAX)
}

/// 다 끝났다는 말 — 보고 요약이 먼저, 없으면 학생이 마지막에 한 말.
pub fn finished(report: Option<&Report>, last_words: Option<&str>) -> String {
    let said = report.map(|r| spoken(&r.summary)).filter(|s| !s.is_empty());
    said.or_else(|| last_words.map(spoken).filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "선생님, 다 끝났어요.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(completion: Option<u64>) -> Facts {
        Facts { source: Some("s1".into()), idle: true, completion, ..Default::default() }
    }

    fn report(key: u64, outcome: &str, summary: &str) -> Report {
        Report { key, outcome: outcome.into(), summary: summary.into() }
    }

    /// `step` 초 간격으로 사실을 차례로 넣고 울린 것을 모은다.
    fn run(seq: &[Facts], step: u64) -> Vec<Ring> {
        let (mut judge, start) = (Finish::default(), Instant::now());
        seq.iter()
            .enumerate()
            .filter_map(|(i, f)| judge.update(f, start + Duration::from_secs(i as u64 * step)))
            .collect()
    }

    #[test]
    fn a_normal_end_rings_once_after_it_settles_and_the_first_sight_is_silent() {
        assert!(run(&[facts(Some(1)), facts(Some(1)), facts(Some(1))], 10).is_empty(), "켜자마자 본 지난 끝");
        let rang = run(&[facts(None), facts(Some(1)), facts(Some(1)), facts(Some(1))], 3);
        assert_eq!(rang, vec![Ring::Finished { key: 1, report: None }]);
        let short = run(&[facts(None), facts(Some(1)), facts(Some(1))], 2);
        assert!(short.is_empty(), "{SETTLE:?} 전에는 울리지 않는다");
    }

    #[test]
    fn background_work_holds_the_end_and_rings_once_when_it_is_all_over() {
        let busy = Facts { busy: true, ..facts(Some(1)) };
        let rang = run(&[facts(None), busy.clone(), busy.clone(), busy, facts(Some(1)), facts(Some(1)), facts(Some(1))], 3);
        assert_eq!(rang, vec![Ring::Finished { key: 1, report: None }], "턴만 끝나고 bg 가 돌면 기다렸다가 다 끝난 순간 한 번");

        let woken = Facts { idle: false, ..facts(Some(1)) };
        let rang = run(&[facts(None), Facts { busy: true, ..facts(Some(1)) }, woken, facts(Some(2)), facts(Some(2)), facts(Some(2))], 3);
        assert_eq!(rang, vec![Ring::Finished { key: 2, report: None }], "통보로 깬 다음 턴의 끝 하나만");
    }

    #[test]
    fn a_scheduled_wakeup_or_an_open_question_is_not_the_end() {
        let parked = Facts { continues: true, ..facts(Some(1)) };
        assert!(run(&[facts(None), parked.clone(), parked.clone(), parked], 10).is_empty());
        let asking = Facts { questions: true, ..facts(Some(1)) };
        assert!(run(&[facts(None), asking.clone(), asking], 10).is_empty());
        let broken = Facts { error: true, ..facts(Some(1)) };
        assert!(run(&[facts(None), broken, facts(Some(1)), facts(Some(1))], 10).is_empty(), "오류는 끝남이 아니다");
    }

    #[test]
    fn an_end_that_a_stop_hook_continues_is_not_the_last_end() {
        let reopened = Facts { idle: false, ..facts(None) };
        assert!(run(&[facts(None), facts(Some(1)), reopened, facts(Some(1)), facts(Some(1)), facts(Some(1))], 2).is_empty(),
            "다시 보인 옛 열쇠는 새 끝이 아니다");
    }

    #[test]
    fn quick_steps_are_bundled_into_the_last_end() {
        let working = |key| Facts { idle: false, ..facts(Some(key)) };
        let rang = run(&[facts(None), facts(Some(1)), working(1), facts(Some(2)), working(2), facts(Some(3)), facts(Some(3)), facts(Some(3)), facts(Some(3))], 2);
        assert_eq!(rang, vec![Ring::Finished { key: 3, report: None }]);
    }

    #[test]
    fn a_done_report_is_the_end_and_its_turn_end_does_not_ring_again() {
        let done = report(77, "succeeded", "빌드 끝");
        let reporting = Facts { idle: false, report: Some(done.clone()), ..facts(None) };
        let ended = Facts { report: Some(done.clone()), ..facts(Some(5)) };
        let rang = run(&[facts(None), reporting, ended.clone(), ended.clone(), ended.clone(), ended], 2);
        assert_eq!(rang, vec![Ring::Finished { key: 77, report: Some(done.clone()) }]);

        // 보고가 걷힌(다음 일을 받은) 뒤의 끝은 새 일이다.
        let ended = Facts { report: Some(done.clone()), ..facts(Some(5)) };
        let next_brief = Facts { idle: false, ..facts(Some(5)) };
        let next = run(&[facts(None), ended.clone(), ended.clone(), ended, next_brief, facts(Some(6)), facts(Some(6)), facts(Some(6))], 3);
        assert_eq!(next, vec![Ring::Finished { key: 77, report: Some(done) }, Ring::Finished { key: 6, report: None }]);
    }

    #[test]
    fn a_blocked_report_rings_at_once_and_its_turn_end_is_silent() {
        let stuck = report(9, "failed", "막힘: 열쇠가 없어요");
        let reporting = Facts { idle: false, report: Some(stuck.clone()), ..facts(None) };
        let ended = Facts { report: Some(stuck.clone()), ..facts(Some(5)) };
        let rang = run(&[facts(None), reporting, ended.clone(), ended.clone(), ended], 3);
        assert_eq!(rang, vec![Ring::Blocked { report: stuck }]);
    }

    #[test]
    fn a_remote_row_needs_its_own_fresh_evidence() {
        let row = |end: serde_json::Value| serde_json::json!({"id": "%3", "done_outcome": "failed", "done_at_ms": 7, "response_end": end});
        let good = row(serde_json::json!({"surface_id": "%3", "session_id": "s", "source": "a", "completion": "0b",
            "idle": true, "error": false, "background": false, "continues": true}));
        let f = row_facts(&good);
        assert_eq!((f.source.as_deref(), f.completion, f.continues), (Some("s:a"), Some(0x0b), true));
        assert_eq!(f.report.map(|r| (r.key, r.blocked())), Some((7, true)));
        let other = row(serde_json::json!({"surface_id": "%9", "session_id": "s", "source": "a", "idle": true, "error": false, "background": false}));
        assert!(row_facts(&other).source.is_none());
        let mut stale = good.clone();
        stale["state_fresh"] = false.into();
        assert!(row_facts(&stale).source.is_none());
        assert!(row_report(&serde_json::json!({"done_outcome": "succeeded"})).is_none(), "보고 시각 없는 옛 원본");
    }

    #[test]
    fn a_new_session_starts_quiet() {
        let other = Facts { source: Some("s2".into()), ..facts(Some(9)) };
        assert!(run(&[facts(None), other.clone(), other.clone(), other], 3).is_empty());
    }

    #[test]
    fn spoken_keeps_the_first_two_sentences_without_markdown_or_code() {
        let text = "## 결과\n\n**빌드**를 `cargo test` 로 확인했어요. [문서](https://x.y/z)도 고쳤습니다.\n\n```rust\nfn main() {}\n```\n세 번째 문장은 빠져요.";
        assert_eq!(spoken(text), "빌드를 cargo test 로 확인했어요. 문서도 고쳤습니다.");
        assert_eq!(spoken("- push.rs 와 v0.2.187 을 봤어요\n- 끝"), "push.rs 와 v0.2.187 을 봤어요 끝");
        assert_eq!(spoken("| a | b |\n|---|---|\n표 뒤 말."), "표 뒤 말.");
        let long = "가".repeat(400);
        assert_eq!(spoken(&long).chars().count(), BODY_MAX);
        assert!(spoken(&long).ends_with('…'));
    }

    #[test]
    fn asking_says_what_the_student_wants_to_do() {
        let bash = serde_json::json!({"command": "cargo test -p kasa-device\n--nocapture"});
        assert_eq!(ask("Bash", Some(&bash)), "선생님, 「cargo test -p kasa-device」 실행해도 될까요?");
        let edit = serde_json::json!({"file_path": "/a/b/push.rs"});
        assert_eq!(ask("Edit", Some(&edit)), "선생님, 「push.rs」 고쳐도 될까요?");
        let web = serde_json::json!({"url": "https://docs.rs/axum/latest"});
        assert_eq!(ask("WebFetch", Some(&web)), "선생님, 「docs.rs」 열어 봐도 될까요?");
        assert_eq!(ask("Bash", None), "선생님, 명령을 실행해도 될까요?");
        assert_eq!(ask("mcp__github__create_issue", None), "선생님, 「create_issue」 도구를 써도 될까요?");
        assert_eq!(ask("", None), "선생님, 이거 해도 될까요?");
        assert_eq!(tool_in_reason("Bash 승인 대기"), "Bash");
        assert_eq!(tool_in_reason("Claude needs your permission to use WebFetch"), "WebFetch");
        assert_eq!(tool_in_reason("무언가"), "");
    }

    #[test]
    fn blocked_finished_and_questions_speak_like_the_student() {
        assert_eq!(blocked(&report(1, "failed", "막힘: **열쇠**가 없어요")), "선생님, 막혔어요. 열쇠가 없어요");
        assert_eq!(blocked(&report(1, "failed", "승인 필요: 배포")), "선생님, 승인이 필요해요. 배포");
        assert_eq!(blocked(&report(1, "failed", "테스트 실패")), "선생님, 잘 안 됐어요. 테스트 실패");
        assert_eq!(finished(Some(&report(1, "succeeded", "다 됐어요")), Some("무시")), "다 됐어요");
        assert_eq!(finished(None, Some("```\nx\n```\n끝냈어요.")), "끝냈어요.");
        assert_eq!(finished(None, None), "선생님, 다 끝났어요.");
        assert_eq!(question("어느 쪽으로 할까요?"), "선생님, 어느 쪽으로 할까요?");
        assert_eq!(question("선생님, 이거요?"), "선생님, 이거요?");
        assert_eq!(question(""), "선생님, 여쭤볼 게 있어요.");
        assert_eq!(trouble("API 오류 (2분째)"), "선생님, 막혔어요. API 오류 (2분째)");
    }
}
