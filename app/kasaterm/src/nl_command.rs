//! 셸 입력줄의 `# 자연어 요청` 을 명령 한 줄로 바꿔 그 자리에 넣는다.
//!
//! **넣기만 하고 돌리지 않는다.** 모델이 지은 명령은 사람이 읽고 Enter 를 한 번 더 눌러야
//! 돈다 — 확인 없이 실행하는 길은 만들지 않는다. 그래서 넣는 글에서 줄바꿈·제어 문자를
//! 전부 걷는다(PTY 에 들어간 줄바꿈은 곧 실행이다).
//!
//! 셸 프롬프트에서만 받는다. 셸이 알려 준 입력 시작(OSC 133 `B`, ZDOTDIR 심이 PS1 끝에
//! 심는다)이 있어야 하고, 마지막 명령 블록이 닫혀 있어야 한다 — `C` 뒤 `D` 전이면 셸이
//! 아니라 그 명령(claude·vim…)이 키를 받는 중이다. claude 입력창의 `#` 을 가로채지 않는
//! 근거가 이것이다. 마크가 없는 pane(심 없는 셸·Windows·거울)에서는 Enter 를 그대로 둔다.
//!
//! 모델은 OpenGateway(`~/.config/opengateway.key`, 테마 굽기와 같은 키)로 부른다.
//! settings.json 의 `nl_command: false` 로 끄고, `nl_command_model` 로 모델을 바꾼다.

use super::*;

const DEFAULT_MODEL: &str = "anthropic/claude-haiku-5-5";
const ENDPOINT: &str = "https://apis.opengateway.ai/v1/chat/completions";
const TIMEOUT_SECS: u32 = 30;
/// 줄바꿈된 긴 요청을 이어 읽는 한도. 마크가 낡아(입력줄이 화면 끝에서 랩돼 위로 밀림)
/// 엉뚱한 줄에서 읽기 시작해도 멀리 가지 않게 한다.
const MAX_WRAPPED_ROWS: u16 = 6;
const MAX_COMMAND_CHARS: usize = 4000;

#[derive(Default)]
pub(crate) struct NlState {
    seq: u64,
    pending: Option<Pending>,
}

struct Pending {
    seq: u64,
    pane: String,
    /// 보낼 때의 입력줄. 답이 왔을 때 줄이 이것과 다르면 사람이 그새 고친 것이라 안 덮는다.
    line: String,
}

/// `#` 으로 시작하는 줄이면 그 뒤 글을 요청으로 본다.
pub(crate) fn request_of(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix('#')?;
    if rest.starts_with('!') {
        return None;
    }
    let rest = rest.trim();
    (!rest.is_empty()).then_some(rest)
}

/// 프롬프트 마크부터 커서까지의 입력줄. 소프트 랩된 행만 이어 붙인다.
fn prompt_text(cells: &[Vec<GridCell>], mark: (u16, u16), cursor: (u16, u16)) -> Option<String> {
    let ((mr, mc), (cr, cc)) = (mark, cursor);
    if mr > cr || cr - mr > MAX_WRAPPED_ROWS || (mr == cr && mc > cc) {
        return None;
    }
    let mut out = String::new();
    for r in mr..=cr {
        let row = cells.get(r as usize)?;
        if r < cr && !row.last().is_some_and(|c| c.wrapped) {
            return None;
        }
        let from = if r == mr { mc as usize } else { 0 };
        let to = if r == cr { cc as usize } else { row.len() };
        append_cells_text(row.iter().take(to).skip(from), &mut out);
    }
    Some(out)
}

/// 화면 줄과 친 글을 합친다. 화면은 셸 에코가 한 박자 늦을 수 있고, 조합기가 Enter 에서
/// 내놓은 마지막 음절(`flushed`)은 아직 셸에 안 갔다. 친 글이 화면 줄로 시작하면 친 글이
/// 더 최신이고, 아니면(탭 완성·기록 불러오기처럼 친 글 밖에서 바뀜) 화면이 맞다.
fn merge_typed(grid: &str, input_buf: &str, flushed: &str) -> String {
    let typed = format!("{input_buf}{flushed}");
    if !input_buf.is_empty() && typed.starts_with(grid) {
        typed
    } else {
        format!("{grid}{flushed}")
    }
}

/// 모델 답에서 넣을 명령 한 줄을 뽑는다. `# 까닭` 으로 답했으면 못 바꾼 것이다.
pub(crate) fn command_from_reply(reply: &str) -> Result<String, String> {
    let mut cmd = String::new();
    for raw in reply.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("```") {
            continue;
        }
        if cmd.is_empty() {
            if let Some(why) = line.strip_prefix('#') {
                let why = why.trim();
                return Err(if why.is_empty() { "셸 명령으로 바꿀 수 없는 요청이에요".to_string() } else { why.to_string() });
            }
        } else if line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("$ ").unwrap_or(line);
        let line = match line.strip_prefix('`').and_then(|l| l.strip_suffix('`')) {
            Some(inner) if !inner.contains('`') => inner,
            _ => line,
        };
        if let Some(kept) = cmd.strip_suffix('\\').map(|s| s.trim_end().len()) {
            cmd.truncate(kept);
            cmd.push(' ');
        } else if !cmd.is_empty() {
            cmd.push_str("; ");
        }
        cmd.extend(line.chars().map(|c| if c == '\t' { ' ' } else { c }).filter(|c| !c.is_control()));
    }
    let cmd = cmd.trim().to_string();
    if cmd.is_empty() {
        return Err("모델이 빈 답을 줬어요".to_string());
    }
    if cmd.chars().count() > MAX_COMMAND_CHARS {
        return Err("명령이 너무 길어 안 넣었어요".to_string());
    }
    Ok(cmd)
}

fn system_prompt(cwd: Option<&std::path::Path>) -> String {
    let os = if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(windows) {
        "Windows"
    } else {
        "Linux"
    };
    let shell = std::env::var("SHELL")
        .ok()
        .and_then(|s| std::path::Path::new(&s).file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "zsh".to_string());
    let place = cwd.map(|p| format!(", current directory {}", p.display())).unwrap_or_default();
    let userland = if cfg!(target_os = "macos") {
        "- Use tools that ship with macOS (BSD userland) unless the request names another tool.\n"
    } else {
        ""
    };
    format!(
        "You turn a natural-language request into exactly one shell command line.\n\
         Environment: {os}, shell {shell}{place}.\n\
         Rules:\n\
         - Output only the command on a single line. No explanation, no markdown, no code fences, no leading $.\n\
         {userland}\
         - Use paths relative to the current directory unless the request names another place.\n\
         - Chain multiple steps with && on the same line.\n\
         - Do not add sudo unless the request asks for it.\n\
         - If no shell command can do it, or the request is too vague, output one line starting with \"# \" and a short reason in the request's language."
    )
}

fn ask(key: &str, model: &str, system: &str, request: &str) -> Result<String, String> {
    let body = serde_json::json!({
        "model": model,
        "max_tokens": 1024,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": request}
        ]
    })
    .to_string();
    let txt = crate::themegen::curl_post(ENDPOINT, &format!("Authorization: Bearer {key}"), &body, TIMEOUT_SECS)?;
    let v: serde_json::Value = serde_json::from_str(&txt).map_err(|_| "답을 읽지 못했어요".to_string())?;
    if let Some(msg) = v["error"]["message"].as_str() {
        return Err(msg.chars().take(120).collect());
    }
    v["choices"][0]["message"]["content"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "모델이 빈 답을 줬어요".to_string())
}

impl App {
    /// 그 pane 이 셸 프롬프트에 앉아 있으면 마크부터 커서까지의 줄.
    pub(crate) fn shell_prompt_grid(&self, pid: &str) -> Option<String> {
        let pty = self.pty.get(pid)?;
        let idle = pty.blocks_arc().lock().ok()?.back().is_none_or(|b| b.exit_code.is_some());
        if !idle {
            return None;
        }
        let ws = self.ws.lock().ok()?;
        let outer = ws.outer_for_pty(pid)?;
        let t = ws.panes.get(&outer)?.tab_for_pid(pid).term()?;
        if t.alt_screen {
            return None;
        }
        prompt_text(&t.cells, t.prompt_end?, (t.cursor_row, t.cursor_col))
    }

    /// 맨 Enter 를 받기 직전에 부른다. `# 요청` 줄이면 Enter 를 가져가고(셸에 안 보낸다)
    /// 명령을 지으러 보낸다. `commit_prefix` 는 조합기가 방금 내놓은, 아직 안 보낸 글자다.
    pub(crate) fn nl_command_enter(&mut self, commit_prefix: &[u8]) -> bool {
        let Some(pid) = self.target_surface() else { return false };
        let Some(grid) = self.shell_prompt_grid(&pid) else { return false };
        let line = merge_typed(&grid, &self.input_buf, &String::from_utf8_lossy(commit_prefix));
        let Some(request) = request_of(&line).map(str::to_string) else { return false };
        let settings = socket::read_settings();
        if settings.get("nl_command").and_then(serde_json::Value::as_bool) == Some(false) {
            return false;
        }
        self.send_bytes_to_surface(Some(&pid), commit_prefix);
        self.input_buf = line.clone();
        self.current_suggestion = None;
        if self.nl.pending.as_ref().is_some_and(|p| p.pane == pid && p.line == line) {
            self.set_toast("아직 명령을 짓는 중이에요".to_string());
            return true;
        }
        let Some(key) = crate::themegen::og_key() else {
            self.set_toast("자연어 명령에 쓸 OpenGateway 키가 없어요 — ~/.config/opengateway.key".to_string());
            return true;
        };
        let model = settings
            .get("nl_command_model")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .unwrap_or(DEFAULT_MODEL)
            .to_string();
        let system = system_prompt(self.pane_cwd_cache.get(&pid).map(|p| p.as_path()));
        self.nl.seq += 1;
        let seq = self.nl.seq;
        self.nl.pending = Some(Pending { seq, pane: pid.clone(), line });
        let proxy = self.proxy.clone();
        let asked = request.clone();
        std::thread::spawn(move || {
            let result = ask(&key, &model, &system, &asked).and_then(|reply| command_from_reply(&reply));
            let _ = proxy.send_event(UserEvent::NlCommand { pane: pid, seq, result });
        });
        self.set_toast(format!("명령 짓는 중 — {request}"));
        true
    }

    pub(crate) fn nl_command_done(&mut self, pane: String, seq: u64, result: Result<String, String>) {
        if !self.nl.pending.as_ref().is_some_and(|p| p.seq == seq && p.pane == pane) {
            return;
        }
        let Some(pending) = self.nl.pending.take() else { return };
        let cmd = match result {
            Ok(cmd) => cmd,
            Err(why) => {
                self.set_toast(format!("자연어 명령 — {why}"));
                return;
            }
        };
        let now = self.shell_prompt_grid(&pending.pane);
        if now.as_deref().map(str::trim_end) != Some(pending.line.trim_end()) {
            self.set_toast(format!("입력줄이 바뀌어 안 넣었어요 — {cmd}"));
            return;
        }
        // ^E^U: 줄 끝으로 가서 통째로 지운다(zsh·bash emacs 키맵). 커서가 줄 중간이어도 남는 글이 없다.
        let mut bytes = b"\x05\x15".to_vec();
        bytes.extend_from_slice(cmd.as_bytes());
        self.send_bytes_to_surface(Some(&pending.pane), &bytes);
        if self.target_surface().as_deref() == Some(pending.pane.as_str()) {
            self.input_buf = cmd;
            self.current_suggestion = None;
        }
        self.set_toast("명령을 넣었어요 — 읽어 보고 Enter 로 실행".to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str, width: usize, wrapped: bool) -> Vec<GridCell> {
        let mut cells: Vec<GridCell> = Vec::new();
        for ch in text.chars() {
            cells.push(GridCell { ch, ..GridCell::blank() });
            if gpu::is_wide_char(ch) {
                cells.push(GridCell { ch: '\0', ..GridCell::blank() });
            }
        }
        cells.resize(width, GridCell::blank());
        if let Some(last) = cells.last_mut() {
            last.wrapped = wrapped;
        }
        cells
    }

    #[test]
    fn only_hash_lines_with_text_are_requests() {
        assert_eq!(request_of("# 큰 파일 찾아줘"), Some("큰 파일 찾아줘"));
        assert_eq!(request_of("  #ls 정리 "), Some("ls 정리"));
        assert_eq!(request_of("#"), None);
        assert_eq!(request_of("#   "), None);
        assert_eq!(request_of("#!/bin/sh"), None);
        assert_eq!(request_of("ls # 주석"), None);
    }

    #[test]
    fn prompt_text_reads_from_the_mark_and_skips_wide_spacers() {
        let cells = vec![row("~ % # 큰 파일", 20, false)];
        assert_eq!(prompt_text(&cells, (0, 4), (0, 13)).as_deref(), Some("# 큰 파일"));
    }

    #[test]
    fn prompt_text_joins_only_soft_wrapped_rows() {
        let wrapped = vec![row("% # abc", 7, true), row("def", 7, false)];
        assert_eq!(prompt_text(&wrapped, (0, 2), (1, 3)).as_deref(), Some("# abcdef"));
        let separate = vec![row("% # abc", 7, false), row("def", 7, false)];
        assert_eq!(prompt_text(&separate, (0, 2), (1, 3)), None);
        assert_eq!(prompt_text(&separate, (1, 0), (0, 3)), None);
    }

    #[test]
    fn typed_text_wins_only_while_it_extends_the_screen() {
        assert_eq!(merge_typed("# 큰 파", "# 큰 파일", ""), "# 큰 파일");
        assert_eq!(merge_typed("# 큰 파일 찾아", "# 큰 파일 찾아", "줘"), "# 큰 파일 찾아줘");
        assert_eq!(merge_typed("# 기록에서 불러온 줄", "", "줘"), "# 기록에서 불러온 줄줘");
        assert_eq!(merge_typed("# 탭 완성된 경로/", "# 탭", ""), "# 탭 완성된 경로/");
    }

    #[test]
    fn replies_become_one_line_without_control_bytes() {
        assert_eq!(command_from_reply("find . -size +100M\n").unwrap(), "find . -size +100M");
        assert_eq!(command_from_reply("```bash\ndu -sh * | sort -rh\n```").unwrap(), "du -sh * | sort -rh");
        assert_eq!(command_from_reply("$ ls -la").unwrap(), "ls -la");
        assert_eq!(command_from_reply("`git status`").unwrap(), "git status");
        assert_eq!(command_from_reply("cd src\nls").unwrap(), "cd src; ls");
        assert_eq!(command_from_reply("find . \\\n  -name x").unwrap(), "find . -name x");
        assert_eq!(command_from_reply("ls\n# 설명 줄").unwrap(), "ls");
        assert_eq!(command_from_reply("echo a\u{1b}[201~b\tc").unwrap(), "echo a[201~b c");
    }

    #[test]
    fn hash_reply_is_a_refusal_not_a_command() {
        assert_eq!(command_from_reply("# 셸로 할 수 없어요").unwrap_err(), "셸로 할 수 없어요");
        assert!(command_from_reply("#").is_err());
        assert!(command_from_reply("  \n```\n```").is_err());
    }
}
