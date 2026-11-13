//! 사람을 부를 때와 그때 할 말 — 데스크톱 OS 알림과 폰 푸시가 이것 하나를 쓴다(docs/design.md 「에이전트 알림 판정」).
//!
//! 부르는 것은 넷뿐이다: 승인 기다림·질문 기다림·막힘(막힘·승인 필요·재시작 필요·실패 보고, 오래 가는 오류)·정말 다
//! 끝남. 턴이 끝날 때마다 울리니 백그라운드 작업을 기다리며 턴을 몇 번이고 끝내는 학생 때문에 정작 봐야 할 승인·완료가
//! 묻혔고, 데스크톱과 폰이 서로 다른 규칙으로 울어 한쪽에만 오는 알림이 생겼다.
//!
//! 「정말 다 끝남」은 정상 종료한 턴 + 그 칸에 도는 백그라운드 작업·하위 에이전트·모니터 없음 + 스스로 깨어날 예약 없음이
//! [`SETTLE`] 동안 그대로일 때다. 학생의 완료 보고(`kasaterm-cli done`)가 가장 확실한 끝이라, 보고가 있으면 그 턴의 끝은
//! 보고한 일과 같은 일로 묶는다. 막힘 보고도 그 턴이 끝나야 울린다 — 학생이 선생님께 사연을 말하는 답 글이 그때 선다.
//!
//! 말은 그 학생이 마지막에 선생님께 한 답 글의 첫머리다. 학생은 페르소나 말투로 「선생님, …했어요」라고 쓰니 그것이 곧
//! 사람에게 하는 말이다. 완료 보고 요약은 판에 남기는 메모라(「… 완료(/term/walk 200), 남은 것: …」) 답 글을 못 찾을 때만 쓴다.

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
    /// 막힘·승인 필요·재시작 필요·실패 보고 — 그 턴이 끝난 뒤.
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
        if let Some(report) = facts.report.as_ref().filter(|r| Some(r.key) != self.report) {
            self.report = Some(report.key);
            self.pending = Some(Pending { key: report.key, report: Some(report.clone()), since: None });
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
        // 막힘 보고는 오류로도 지우지 않는다 — 사람이 손써야 하는 일이라고 학생이 직접 말했다.
        let reopened = facts.completion.is_none() && !facts.idle;
        let blocked = self.pending.as_ref().is_some_and(|p| p.report.as_ref().is_some_and(Report::blocked));
        if !blocked && (facts.error || reopened && self.pending.as_ref().is_some_and(|p| p.report.is_none())) {
            self.pending = None;
        }
        // 막힌 학생은 남은 일을 기다리지 않는다 — 턴이 끝나 사람 차례가 된 것으로 충분하다.
        let settled = facts.idle && (blocked || !facts.busy && !facts.questions && !facts.continues);
        let pending = self.pending.as_mut()?;
        if !settled {
            pending.since = None;
            return None;
        }
        if now.saturating_duration_since(*pending.since.get_or_insert(now)) < SETTLE {
            return None;
        }
        let Pending { key, report, .. } = self.pending.take()?;
        Some(match report {
            Some(report) if report.blocked() => Ring::Blocked { report },
            report => Ring::Finished { key, report },
        })
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

/// 괄호 속 덧붙임(「(/term/walk 200, 방 이름 확인)」)은 말이 아니라 메모라 뺀다. 짝이 안 맞으면 그대로 둔다.
fn without_asides(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut depth = 0usize;
    let mut held = String::new();
    for c in line.chars() {
        match c {
            '(' | '（' => {
                depth += 1;
                held.push(c);
            }
            ')' | '）' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    held.clear();
                } else {
                    held.push(c);
                }
            }
            _ if depth > 0 => held.push(c),
            _ => out.push(c),
        }
    }
    out.push_str(&held);
    out
}

/// 주소·경로는 사람에게 소리 내 읽을 말이 아니다 — 주소는 호스트, 경로는 마지막 이름만 남긴다.
fn without_paths(line: &str) -> String {
    line.split(' ')
        .map(|word| {
            let lead = word.len() - word.trim_start_matches(['「', '\'', '"']).len();
            let core_end = word.trim_end_matches([',', '.', '」', '!', '?', ':', ';', '\'', '"']).len().max(lead);
            let (head, core, tail) = (&word[..lead], &word[lead..core_end], &word[core_end..]);
            let named = if core.starts_with("http://") || core.starts_with("https://") {
                host(core)
            } else {
                let parts = core.split('/').filter(|p| !p.is_empty()).count();
                let rooted = core.starts_with('/') || core.starts_with("~/") || core.starts_with("./");
                let filed = core.rsplit('/').next().is_some_and(|last| last.contains('.'));
                if core.contains('/') && parts > 0 && (rooted || parts >= 3 || parts == 2 && filed) { file_name(core) } else { core }
            };
            format!("{head}{named}{tail}")
        })
        .collect::<Vec<_>>()
        .join(" ")
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

/// 글을 문장으로. 줄이 바뀌면 문장도 바뀌고(목록 한 줄은 한 마디다), 줄 안에서는 마침표·물음표·느낌표·물결 뒤에 빈칸이 와야
/// 끝으로 본다(`push.rs`·`0.2.1` 은 끝이 아니다). 코드 덩이·표·머리글·괄호 속 메모는 뺀다.
fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
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
        let words = without_paths(&without_asides(&plain_inline(strip_marker(line))));
        let words = words.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut start = 0;
        let mut chars = words.char_indices().peekable();
        while let Some((at, c)) = chars.next() {
            if matches!(c, '.' | '!' | '?' | '。' | '…' | '~') && chars.peek().is_none_or(|(_, next)| next.is_whitespace()) {
                let end = at + c.len_utf8();
                out.push(words[start..end].trim().to_string());
                start = end;
            }
        }
        out.push(words[start..].trim().to_string());
    }
    out.retain(|s| !s.trim_matches(['.', '!', '?', '~', '…', ' ']).is_empty());
    out
}

/// 인사·추임새뿐인 문장(「네, 선생님!」·「마키 왔어요~」·「다 됐어요!」) — 호칭·기호를 빼면 몇 자 안 남는다.
fn filler(sentence: &str) -> bool {
    sentence.replace("선생님", "").chars().filter(|c| c.is_alphanumeric()).count() <= 6
}

fn joined(parts: &[String]) -> String {
    let mut out = String::new();
    for part in parts {
        if !out.is_empty() {
            if !out.ends_with(['.', '!', '?', '~', '…', '。']) {
                out.push('.');
            }
            out.push(' ');
        }
        out.push_str(part);
    }
    out
}

/// 학생이 한 말을 알림 첫머리로 — 첫 문장, 그것이 인사·추임새뿐이면 알맹이가 나올 때까지 다음 문장까지(세 문장 안),
/// [`BODY_MAX`] 안으로. 코드 덩이·표·머리글·목록 기호·괄호 속 메모·경로를 걷는다.
pub fn spoken(text: &str) -> String {
    // 쪽지·전달 글의 보낸 이 표(`⟦유즈⟧`)는 말이 아니다.
    let text = text.trim_start();
    let text = text.strip_prefix('⟦').and_then(|rest| rest.split_once('⟧')).map_or(text, |(_, said)| said);
    let all = sentences(text);
    let mut taken = Vec::new();
    for sentence in all.into_iter().take(3) {
        let stop = !filler(&sentence);
        taken.push(sentence);
        if stop {
            break;
        }
    }
    clip(&joined(&taken), BODY_MAX)
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

/// 학생이 묻는 말 — 물음이 든 첫 문장. 비면 묻고 싶은 것이 있다는 말만.
pub fn question(prompt: &str) -> String {
    let asked = sentences(prompt).into_iter().find(|s| s.contains('?') || s.contains('？'));
    let said = asked.map(|s| clip(&s, BODY_MAX)).unwrap_or_else(|| spoken(prompt));
    if said.is_empty() {
        "선생님, 여쭤볼 게 있어요.".to_string()
    } else if said.starts_with("선생님") {
        said
    } else {
        format!("선생님, {said}")
    }
}

/// 막혔다는 말 — 학생이 그 턴 끝에 선생님께 한 답 글이 먼저다. 못 찾으면 보고에서 무엇 때문에 멈췄는지를 앞에 말한다.
pub fn blocked(report: &Report, last_words: Option<&str>) -> String {
    if let Some(said) = last_words.map(spoken).filter(|s| !s.is_empty()) {
        return said;
    }
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

/// 다 끝났다는 말 — 학생이 마지막에 선생님께 한 답 글이 먼저, 못 찾으면 보고 요약.
pub fn finished(report: Option<&Report>, last_words: Option<&str>) -> String {
    let said = last_words.map(spoken).filter(|s| !s.is_empty());
    said.or_else(|| report.map(|r| spoken(&r.summary)).filter(|s| !s.is_empty()))
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

    /// 막힘 보고는 그 턴이 끝나야 울린다 — 학생이 사연을 말하는 답 글이 그때 선다. 남은 백그라운드는 기다리지 않고, 한 번만.
    #[test]
    fn a_blocked_report_rings_once_when_its_turn_ends() {
        let stuck = report(9, "failed", "승인 필요: 배포");
        let reporting = Facts { idle: false, report: Some(stuck.clone()), ..facts(None) };
        assert!(run(&[facts(None), reporting.clone(), reporting.clone(), reporting.clone(), reporting], 3).is_empty(),
            "보고하고 아직 말하는 중");
        let ended = Facts { busy: true, report: Some(stuck.clone()), ..facts(Some(5)) };
        let reporting = Facts { idle: false, report: Some(stuck.clone()), ..facts(None) };
        let rang = run(&[facts(None), reporting, ended.clone(), ended.clone(), ended.clone(), ended], 3);
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
    fn spoken_is_the_opening_without_markdown_code_paths_or_asides() {
        let text = "## 결과\n\n**빌드**를 `cargo test` 로 확인했어요. [문서](https://x.y/z)도 고쳤습니다.\n\n```rust\nfn main() {}\n```";
        assert_eq!(spoken(text), "빌드를 cargo test 로 확인했어요.");
        assert_eq!(spoken("- crates/kasa-device/src/push.rs 를 고쳤어요(시험 3개)\n- 끝"), "push.rs 를 고쳤어요");
        assert_eq!(spoken("| a | b |\n|---|---|\n표 뒤 말."), "표 뒤 말.");
        assert_eq!(spoken("v0.2.187 과 home·news 를 봤어요. 다음 말."), "v0.2.187 과 home·news 를 봤어요.");
        assert_eq!(spoken("https://kasaterm.debimarlene.com/relay/install/abc 로 올렸어요."), "kasaterm.debimarlene.com 로 올렸어요.");
        let long = "가".repeat(400);
        assert_eq!(spoken(&long).chars().count(), BODY_MAX);
        assert!(spoken(&long).ends_with('…'));
    }

    #[test]
    fn a_greeting_alone_carries_on_to_the_next_sentence() {
        assert_eq!(spoken("네, 선생님! 마키 왔어요~ kontext-jp 페이지 다섯 장 다 새로 그렸어요. 부품도 셋 만들었고요."),
            "네, 선생님! 마키 왔어요~ kontext-jp 페이지 다섯 장 다 새로 그렸어요.");
        assert_eq!(spoken("다 됐어요!\n\n- 시험 통과"), "다 됐어요! 시험 통과");
        assert_eq!(spoken("⟦유즈⟧ 선생님, 올렸어요."), "선생님, 올렸어요.");
    }

    /// 실제로 받은 모양 — 판 메모 같은 보고 요약이 아니라 학생이 선생님께 한 답 글이 본문이 된다(2026-11-13 「알림이 나한테
    /// 하는 말처럼 안 와」).
    #[test]
    fn the_body_is_the_reply_to_the_teacher_not_the_board_memo() {
        let yuzu = report(1, "succeeded",
            "맥북·미니 0.2.223 교체·복원·이어가기 완료(/term/walk 200, 방 이름 확인), 옛 0.2.221 백업 정리. 남은 것: 집 PC");
        let yuzu_said = "선생님, 맥북이랑 미니를 0.2.223 으로 올리고 학생들 다 이어 놨어요. `/term/walk` 도 200 이 나와요.\n\n            - 옛 0.2.221 백업은 지웠어요\n- 남은 건 집 PC 예요";
        assert_eq!(finished(Some(&yuzu), Some(yuzu_said)), "선생님, 맥북이랑 미니를 0.2.223 으로 올리고 학생들 다 이어 놨어요.");
        let maki = report(2, "succeeded", "kontext-jp 페이지 5장(home·news·shops·about·detail) 높음 · 부품 3(새 3: hero·card·nav)");
        let maki_said = "네 선생님! kontext-jp 페이지 다섯 장 다 높은 쪽으로 다시 그렸어요. 부품은 세 개 새로 만들었어요.";
        assert_eq!(finished(Some(&maki), Some(maki_said)), "네 선생님! kontext-jp 페이지 다섯 장 다 높은 쪽으로 다시 그렸어요.");
        assert_eq!(finished(Some(&yuzu), None), "맥북·미니 0.2.223 교체·복원·이어가기 완료, 옛 0.2.221 백업 정리.",
            "답 글을 못 찾을 때만 보고 요약");
        assert_eq!(finished(None, None), "선생님, 다 끝났어요.");
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
    fn blocked_and_questions_speak_like_the_student() {
        let approval = report(1, "failed", "승인 필요: 프로덕션 배포 (kubectl apply -f deploy.yaml)");
        assert_eq!(blocked(&approval, Some("선생님, 프로덕션에 배포해도 될까요? 시험은 다 통과했어요.")),
            "선생님, 프로덕션에 배포해도 될까요?");
        assert_eq!(blocked(&approval, None), "선생님, 승인이 필요해요. 프로덕션 배포");
        assert_eq!(blocked(&report(1, "failed", "막힘: **열쇠**가 없어요"), None), "선생님, 막혔어요. 열쇠가 없어요");
        assert_eq!(blocked(&report(1, "failed", "테스트 실패"), Some("")), "선생님, 잘 안 됐어요. 테스트 실패");
        assert_eq!(question("어느 쪽으로 할까요?"), "선생님, 어느 쪽으로 할까요?");
        assert_eq!(question("배포 방식을 정해야 해요. 블루그린으로 할까요, 한 번에 바꿀까요?"),
            "선생님, 블루그린으로 할까요, 한 번에 바꿀까요?");
        assert_eq!(question("선생님, 이거요?"), "선생님, 이거요?");
        assert_eq!(question(""), "선생님, 여쭤볼 게 있어요.");
        assert_eq!(trouble("API 오류 (2분째)"), "선생님, 막혔어요. API 오류 (2분째)");
    }
}
