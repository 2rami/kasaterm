//! 칸에 쳐 넣는 명령 한 줄을 그 칸 셸의 문법으로 짓는다 — 폴더 옮기기·인자 인용·Enter.
//!
//! 부르는 쪽과 받는 쪽 기기 OS 가 다를 수 있어(맥에서 Windows 칸으로 summon) 줄은 받는 기기가 자기 칸
//! 셸을 보고 짓는다(`surface.launch`). Windows 칸 기본 셸은 cmd 라, 작은따옴표·`&&`·`\n` 을 그대로 치면
//! 따옴표가 글자로 남고 Enter 도 안 눌린다(2026-10-23 실측: summon 이 90초 뒤 「안 떴어요」로 끝났다).

use anyhow::{bail, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellKind {
    Posix,
    Fish,
    PowerShell,
    Cmd,
}

impl ShellKind {
    /// 셸 프로세스 이름(`-zsh`·`pwsh.exe`·`CMD.EXE`)에서. 셸이 아니면 None.
    pub fn from_process_name(name: &str) -> Option<Self> {
        let name = name.trim_start_matches('-').to_ascii_lowercase();
        let base = name.rsplit(['/', '\\']).next().unwrap_or(&name);
        match base.strip_suffix(".exe").unwrap_or(base) {
            "zsh" | "bash" | "sh" | "dash" | "ksh" | "mksh" | "yash" | "tcsh" | "csh" => Some(Self::Posix),
            "fish" => Some(Self::Fish),
            "pwsh" | "powershell" => Some(Self::PowerShell),
            "cmd" => Some(Self::Cmd),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Posix => "posix",
            Self::Fish => "fish",
            Self::PowerShell => "powershell",
            Self::Cmd => "cmd",
        }
    }

    /// 인자 하나를 그 셸에서 글자 그대로 넘어가게 감싼다. cmd 는 `"` 를 감쌀 길이 없어 부르는 쪽이 먼저 거른다.
    pub fn quote(self, text: &str) -> String {
        match self {
            Self::Posix => format!("'{}'", text.replace('\'', r"'\''")),
            // fish 작은따옴표 안에서는 `\\`·`\'` 가 탈출이라 역슬래시도 겹쳐야 글자 그대로다.
            Self::Fish => format!("'{}'", text.replace('\\', r"\\").replace('\'', r"\'")),
            Self::PowerShell => powershell_quote(text),
            Self::Cmd => cmd_quote(text),
        }
    }

    /// `dir` 로 옮기는 데 성공했을 때만 `command` 를 돌린다.
    pub fn cd_then(self, dir: &str, command: &str) -> String {
        match self {
            Self::Posix | Self::Fish => format!("cd {} && {command}", self.quote(dir)),
            // Windows PowerShell 5.1 엔 `&&` 가 없다. 못 옮기면 멈춤 오류가 그 줄의 나머지를 건너뛴다.
            Self::PowerShell => format!("Set-Location -LiteralPath {} -ErrorAction Stop; {command}", self.quote(dir)),
            // `/d` 가 없으면 다른 드라이브 폴더로는 옮기지 않고 성공으로 끝난다.
            Self::Cmd => format!("cd /d {} && {command}", self.quote(dir)),
        }
    }

    /// 칸 입력줄에 남은 글자를 먼저 지우는 키. Ctrl+U 는 readline 의 줄 지우기라 POSIX 셸에만 보낸다.
    pub fn clear_line(self) -> &'static str {
        match self {
            Self::Posix | Self::Fish => "\x15",
            Self::PowerShell | Self::Cmd => "",
        }
    }

    /// 친 줄을 실행하는 키. ConPTY 는 `\r` 만 Enter 로 읽는다. 맥 POSIX 셸은 예전처럼 `\n`.
    pub fn enter(self) -> &'static str {
        match self {
            Self::Posix | Self::Fish if !cfg!(windows) => "\n",
            _ => "\r",
        }
    }
}

/// PowerShell 작은따옴표 문자열. 그 안에서 특별한 건 작은따옴표뿐인데, PowerShell 은 굽은 작은따옴표
/// (U+2018~U+201B)도 같은 따옴표로 읽어서 둘 다 겹쳐 쓴다.
pub fn powershell_quote(text: &str) -> String {
    let mut out = String::from("'");
    for c in text.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// cmd 큰따옴표 문자열. 따옴표 안에서도 `%이름%` 은 펼쳐지므로 `%` 만 따옴표 밖으로 꺼내 `^` 로 막는다
/// (`"50"^%" off"` → 받는 프로그램엔 `50% off`).
fn cmd_quote(text: &str) -> String {
    format!("\"{}\"", text.replace('%', "\"^%\""))
}

/// 셸 줄에 섞이면 줄을 끊거나 따옴표를 깨는 글자 — cmd·PowerShell 칸의 인자는 결국 `.cmd` 셰임의 `%*` 로
/// cmd 가 한 번 더 읽어서 둘 다 같은 제한을 진다.
fn windows_unsafe(text: &str) -> bool {
    text.chars().any(|c| c == '"' || c.is_control())
}

/// 감싸지 않아도 cmd·PowerShell 이 다르게 읽지 않는 단어.
fn windows_bare(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '\\' | '+'))
}

/// 명령 단어들을 cmd·PowerShell 줄로. 칸의 `claude`·`codex`·학생 이름은 `.cmd` 셰임이라 인자는 결국
/// cmd 가 읽는다 — PowerShell 은 감쌀 인자가 있으면 멈춤 기호 `--%` 뒤에 cmd 꼴 그대로 넘긴다. 그러지
/// 않으면 PowerShell 5.1 은 공백 없는 `a&b` 를 따옴표 없이 넘겨 cmd 가 `&` 에서 명령을 끊는다.
fn windows_command(shell: ShellKind, words: &[String]) -> String {
    let (program, args) = words.split_first().expect("빈 명령은 부르는 쪽이 거른다");
    let program = match (windows_bare(program), shell) {
        (true, _) => program.clone(),
        // 감싼 첫 단어는 PowerShell 이 문자열 값으로 읽고 실행하지 않는다.
        (false, ShellKind::PowerShell) => format!("& {}", powershell_quote(program)),
        (false, _) => cmd_quote(program),
    };
    let mut line = program;
    if shell == ShellKind::PowerShell && !args.iter().all(|a| windows_bare(a)) {
        line.push_str(" --%");
    }
    for arg in args {
        line.push(' ');
        line.push_str(&if windows_bare(arg) { arg.clone() } else { cmd_quote(arg) });
    }
    line
}

/// POSIX 셸 단어로 나눈다(`claude --model 'opus[1m]'` → 세 단어). 따옴표가 안 닫혔으면 None.
pub fn split_posix_words(text: &str) -> Option<Vec<String>> {
    let (mut words, mut cur, mut has) = (Vec::new(), String::new(), false);
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                has = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => cur.push(c),
                    }
                }
            }
            '"' => {
                has = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => match chars.next()? {
                            c @ ('"' | '\\' | '$' | '`') => cur.push(c),
                            '\n' => {}
                            c => {
                                cur.push('\\');
                                cur.push(c);
                            }
                        },
                        c => cur.push(c),
                    }
                }
            }
            '\\' => {
                has = true;
                match chars.next() {
                    Some('\n') | None => {}
                    Some(c) => cur.push(c),
                }
            }
            c if c.is_whitespace() => {
                if has {
                    words.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                has = true;
                cur.push(c);
            }
        }
    }
    if has {
        words.push(cur);
    }
    Some(words)
}

/// 사람이 보는 꼴의 경로 — Windows `canonicalize` 가 붙이는 `\\?\` 를 뗀다. cmd 는 `\\?\` 경로로
/// 옮기지 못하고, 그 경로를 작업 폴더로 받은 칸은 `C:\Windows` 로 떨어진다.
pub fn plain_path(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    match path.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_string(),
        _ => path.to_string(),
    }
}

/// 학생 부팅 한 줄(Enter 포함). `command` 는 POSIX 셸 꼴로 받은 그대로다(`claude`·`시로코 codex`·
/// `claude --model 'opus[1m]'`) — POSIX 칸엔 그대로 치고, 다른 셸이면 단어로 나눠 그 셸 문법으로 다시 감싼다.
///
/// `prompt` 는 codex 첫 입력이다. cmd·PowerShell 칸은 줄바꿈·따옴표를 명령줄로 넘길 수 없어서(`.cmd`
/// 셰임을 지나며 cmd 가 다시 읽는다) 받는 쪽이 파일에 적어 그 경로를 `prompt_file` 로 준다 — 칸의 codex
/// 셰임이 `--kasaterm-prompt-file` 을 읽어 원문 그대로 첫 입력으로 넘긴다.
pub fn launch_line(
    shell: ShellKind,
    cwd: Option<&str>,
    command: &str,
    prompt: Option<&str>,
    prompt_file: Option<&str>,
) -> Result<String> {
    let line = match shell {
        ShellKind::Posix | ShellKind::Fish => {
            let mut run = command.to_string();
            if let Some(prompt) = prompt {
                run.push(' ');
                run.push_str(&shell.quote(prompt));
            }
            match cwd {
                Some(dir) => shell.cd_then(dir, &run),
                None => run,
            }
        }
        ShellKind::PowerShell | ShellKind::Cmd => {
            let Some(mut words) = split_posix_words(command).filter(|w| !w.is_empty()) else {
                bail!("부팅 명령의 따옴표가 안 닫혔어요: {command}");
            };
            if prompt.is_some() {
                let Some(file) = prompt_file else {
                    bail!("{} 칸에 첫 입력을 넘길 셰임이 없어요", shell.name());
                };
                words.extend(["--kasaterm-prompt-file".to_string(), file.to_string()]);
            }
            if let Some(bad) = words.iter().find(|w| windows_unsafe(w)) {
                bail!("{} 칸엔 큰따옴표·줄바꿈이 든 인자를 넘길 수 없어요: {bad}", shell.name());
            }
            if cwd.is_some_and(windows_unsafe) {
                bail!("{} 칸엔 큰따옴표·줄바꿈이 든 폴더로 옮길 수 없어요", shell.name());
            }
            let run = windows_command(shell, &words);
            match cwd {
                Some(dir) => shell.cd_then(&plain_path(dir), &run),
                None => run,
            }
        }
    };
    Ok(line + shell.enter())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_names_map_to_their_syntax() {
        for (name, kind) in [
            ("-zsh", Some(ShellKind::Posix)),
            ("/bin/bash", Some(ShellKind::Posix)),
            (r"C:\Program Files\Git\usr\bin\bash.exe", Some(ShellKind::Posix)),
            ("fish", Some(ShellKind::Fish)),
            ("pwsh.exe", Some(ShellKind::PowerShell)),
            ("PowerShell.exe", Some(ShellKind::PowerShell)),
            ("CMD.EXE", Some(ShellKind::Cmd)),
            ("claude.exe", None),
        ] {
            assert_eq!(ShellKind::from_process_name(name), kind, "{name}");
        }
    }

    #[test]
    fn each_shell_quotes_its_own_way() {
        assert_eq!(ShellKind::Posix.quote("it's"), r"'it'\''s'");
        assert_eq!(ShellKind::Fish.quote(r"it's C:\x"), r"'it\'s C:\\x'");
        assert_eq!(ShellKind::PowerShell.quote("it's ‘x’"), "'it''s ‘‘x’’'");
        assert_eq!(ShellKind::Cmd.quote(r"C:\a b\50%"), r#""C:\a b\50"^%"""#);
    }

    #[test]
    fn posix_words_split_like_a_shell() {
        assert_eq!(split_posix_words("claude").unwrap(), ["claude"]);
        assert_eq!(
            split_posix_words(r#"시로코  codex --model 'opus[1m]' "a \"b\"" c\ d ''"#).unwrap(),
            ["시로코", "codex", "--model", "opus[1m]", r#"a "b""#, "c d", ""]
        );
        assert!(split_posix_words("claude 'open").is_none());
    }

    #[test]
    fn verbatim_windows_paths_become_plain() {
        assert_eq!(plain_path(r"\\?\C:\Users\x\wt\win-update"), r"C:\Users\x\wt\win-update");
        assert_eq!(plain_path(r"\\?\UNC\srv\share\x"), r"\\srv\share\x");
        assert_eq!(plain_path("/Users/kasa/proj"), "/Users/kasa/proj");
    }

    /// 맥 칸(POSIX)에 치는 줄은 예전 summon 과 글자 하나 다르지 않다.
    #[test]
    fn posix_boot_line_is_the_old_summon_line() {
        let line = launch_line(ShellKind::Posix, Some("/a b/it's"), "claude", None, None).unwrap();
        assert_eq!(line, format!("cd '/a b/it'\\''s' && claude{}", ShellKind::Posix.enter()));
        let codex = launch_line(ShellKind::Posix, Some("/r"), "시로코 codex", Some("할 일\n둘째 줄"), None).unwrap();
        assert_eq!(codex, format!("cd '/r' && 시로코 codex '할 일\n둘째 줄'{}", ShellKind::Posix.enter()));
    }

    #[test]
    fn windows_shells_get_their_own_cd_and_enter() {
        let cmd = launch_line(ShellKind::Cmd, Some(r"\\?\C:\Users\x\wt\win-update"), "claude", None, None).unwrap();
        assert_eq!(cmd, "cd /d \"C:\\Users\\x\\wt\\win-update\" && claude\r");
        let ps = launch_line(ShellKind::PowerShell, Some(r"C:\it's"), "claude --model 'opus[1m]'", None, None).unwrap();
        assert_eq!(ps, "Set-Location -LiteralPath 'C:\\it''s' -ErrorAction Stop; claude --% --model \"opus[1m]\"\r");
        assert_eq!(launch_line(ShellKind::PowerShell, None, "호시노 codex", None, None).unwrap(), "호시노 codex\r");
        let first = launch_line(ShellKind::Cmd, None, "'my tool' x", None, None).unwrap();
        assert_eq!(first, "\"my tool\" x\r");
        assert_eq!(launch_line(ShellKind::PowerShell, None, "'my tool'", None, None).unwrap(), "& 'my tool'\r");
        assert_eq!(ShellKind::Posix.clear_line(), "\x15");
        assert_eq!(ShellKind::Cmd.clear_line(), "");
    }

    #[test]
    fn windows_prompts_travel_by_file() {
        let line = launch_line(ShellKind::Cmd, Some(r"C:\r"), "codex", Some("a\n\"b\""), Some(r"C:\t\p 1.txt")).unwrap();
        assert_eq!(line, "cd /d \"C:\\r\" && codex --kasaterm-prompt-file \"C:\\t\\p 1.txt\"\r");
        assert!(launch_line(ShellKind::Cmd, None, "codex", Some("x"), None).is_err());
        assert!(launch_line(ShellKind::PowerShell, None, r#"claude "a\"b""#, None, None).is_err());
    }

    /// 실제 cmd·PowerShell 에 줄을 먹여, 공백·한글·`%`·작은따옴표가 든 폴더로 옮겨 인자를 글자 그대로
    /// 받는지 본다. 폴더가 없으면 명령을 돌리지 않는다.
    #[cfg(windows)]
    #[test]
    fn windows_boot_lines_run_in_real_shells() {
        use std::os::windows::process::CommandExt;
        let base = std::env::temp_dir().join(format!("kasa-shell-line-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let target = base.join("학생 it's 50%PATH% (x)");
        std::fs::create_dir_all(&target).unwrap();
        let bin = base.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        // 칸의 셰임처럼 `.cmd` 가 `%*` 로 다시 넘기는 길까지 탄다 — 받은 인자와 폴더를 파일로 남긴다.
        let probe = bin.join("probe.ps1");
        std::fs::write(&probe, "Set-Content -Encoding utf8 -LiteralPath (Join-Path $PWD 'got.txt') -Value ($args -join '|')\r\n").unwrap();
        std::fs::write(bin.join("kprobe.cmd"), "@powershell.exe -NoProfile -ExecutionPolicy Bypass -File \"%~dp0probe.ps1\" %*\r\n").unwrap();
        let path = format!("{};{}", bin.display(), std::env::var("PATH").unwrap_or_default());
        let command = "kprobe --model 'opus[1m]' 'a&b|c' '50%PATH%' 호시노 'x y'";
        let want = "--model|opus[1m]|a&b|c|50%PATH%|호시노|x y";
        let mut shells = vec![(ShellKind::Cmd, "cmd.exe".to_string()), (ShellKind::PowerShell, "powershell.exe".to_string())];
        match std::process::Command::new("where.exe").arg("pwsh").output().ok().filter(|o| o.status.success()) {
            Some(found) => shells.push((ShellKind::PowerShell, String::from_utf8_lossy(&found.stdout).lines().next().unwrap_or("pwsh").trim().to_string())),
            None => eprintln!("pwsh 가 없어 Windows PowerShell 로만 본다"),
        }
        for (kind, program) in shells {
            let program = program.as_str();
            let _ = std::fs::remove_file(target.join("got.txt"));
            let line = launch_line(kind, Some(&target.to_string_lossy()), command, None, None).unwrap();
            let line = line.trim_end_matches('\r');
            let mut run = std::process::Command::new(program);
            run.current_dir(&base).env("PATH", &path);
            match kind {
                ShellKind::Cmd => run.raw_arg(format!("/d /s /c \"{line}\"")),
                _ => run.args(["-NoProfile", "-NonInteractive", "-Command", line]),
            };
            let out = run.output().unwrap();
            let got = std::fs::read_to_string(target.join("got.txt"))
                .unwrap_or_else(|_| panic!("{program} `{line}` 가 그 폴더에서 안 돌았다: {}", String::from_utf8_lossy(&out.stderr)));
            assert_eq!(got.trim_start_matches('\u{feff}').trim_end(), want, "{program} `{line}`");
            let missing = launch_line(kind, Some(&base.join("없는 폴더").to_string_lossy()), "kprobe x", None, None).unwrap();
            let mut run = std::process::Command::new(program);
            run.current_dir(&base).env("PATH", &path);
            let line = missing.trim_end_matches('\r');
            match kind {
                ShellKind::Cmd => run.raw_arg(format!("/d /s /c \"{line}\"")),
                _ => run.args(["-NoProfile", "-NonInteractive", "-Command", line]),
            };
            let _ = run.output().unwrap();
            assert!(!base.join("got.txt").exists(), "{program}: 폴더가 없으면 명령을 안 돌린다");
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
