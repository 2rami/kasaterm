//! 하네스별 어댑터를 띄울 명령. 설치된 어댑터가 PATH 에 있으면 그것을, 없으면 npx 로 받는다.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    Claude,
    Codex,
}

impl Harness {
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    /// (설치됐을 때의 실행 파일 이름, npx 로 받을 패키지).
    /// 판은 실측한 것에 고정한다 — 어댑터는 판마다 `_meta` 확장과 도구 출력 자리를 바꾼다.
    fn adapter(self) -> (&'static str, &'static str) {
        match self {
            Self::Claude => ("claude-agent-acp", "@agentclientprotocol/claude-agent-acp@0.101.0"),
            Self::Codex => ("codex-acp", "@agentclientprotocol/codex-acp@2.5.0"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    /// 물려받은 환경 위에 덧씌운다.
    pub env: Vec<(OsString, OsString)>,
}

impl LaunchSpec {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self { program: program.into(), args: Vec::new(), env: Vec::new() }
    }

    /// `path` 는 찾을 PATH 다. 어댑터는 `#!/usr/bin/env node` 스크립트라 자식도 같은 PATH 로
    /// node 를 찾아야 해서 그대로 넘긴다 — Finder 로 띄운 앱의 PATH 에는 node 가 없다.
    pub fn resolve(harness: Harness, path: &OsStr) -> Option<Self> {
        let (bin, package) = harness.adapter();
        let spec = match find_program(bin, path) {
            Some(installed) => Self::new(installed),
            None => {
                let mut spec = Self::new(find_program("npx", path)?);
                spec.args = vec!["-y".into(), package.into()];
                spec
            }
        };
        Some(spec.env("PATH", path))
    }

    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        let key = key.into();
        self.env.retain(|(k, _)| *k != key);
        self.env.push((key, value.into()));
        self
    }

    /// claude 어댑터가 띄울 Claude Code. 비우면 어댑터에 딸린 판이 돌아 사용자의 판·설정과 어긋난다.
    pub fn claude_executable(self, claude: &Path) -> Self {
        self.env("CLAUDE_CODE_EXECUTABLE", claude)
    }
}

/// codex 는 도구 셸의 env 를 `shell_environment_policy` 로 새로 짓는다. 사용자가 `inherit = "core"` 를 쓰면
/// `KASATERM_*` 가 빠져 학생의 `kasaterm-cli` 가 앱 소켓을 못 찾는다 — 칸 shim 이 `-c` 로 막는 것과 같은 문제다.
/// codex-acp 는 `CODEX_CONFIG`(JSON) 의 키를 세션 설정 덮어쓰기로 넘기므로, 칸의 값을 점 경로 키로 못박는다.
/// 고를 이름은 shim 과 같다: `KASA*`·`CODEX_HOME`, 단 `KASATERM_PERSONA`·KEY·TOKEN·SECRET 이 든 이름·여러 줄 값은 뺀다.
/// 넣을 것이 없거나 이미 있는 `CODEX_CONFIG` 를 못 읽으면 `None` — 남의 설정을 덮지 않는다.
pub fn codex_pane_env_config(vars: impl IntoIterator<Item = (String, String)>, existing: Option<&str>) -> Option<String> {
    let mut config: Map<String, Value> = match existing {
        Some(text) => serde_json::from_str(text).ok()?,
        None => Map::new(),
    };
    let mut added = false;
    for (name, value) in vars {
        let ours = name.starts_with("KASA") || name == "CODEX_HOME";
        let secret = ["KEY", "TOKEN", "SECRET"].iter().any(|w| name.contains(w));
        if !ours || secret || name == "KASATERM_PERSONA" || value.contains('\n') {
            continue;
        }
        config.entry(format!("shell_environment_policy.set.{name}")).or_insert_with(|| {
            added = true;
            Value::String(value)
        });
    }
    added.then(|| Value::Object(config).to_string())
}

/// PATH 에서 실행 파일을 찾되 카사텀 칸의 shim 폴더(`kasaterm-shim-N`)는 건너뛴다.
/// shim 의 `claude`·`codex` 는 대화형 실행에 훅·페르소나·session-id 플래그를 얹는데,
/// 어댑터가 SDK 모드로 부르는 호출에 그것이 섞이면 안 된다.
pub fn find_program(name: &str, path: &OsStr) -> Option<PathBuf> {
    let names: Vec<String> = if cfg!(windows) {
        vec![format!("{name}.cmd"), format!("{name}.exe"), name.to_owned()]
    } else {
        vec![name.to_owned()]
    };
    std::env::split_paths(path)
        .filter(|dir| !is_shim_dir(dir))
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|p| p.is_file())
}

fn is_shim_dir(dir: &Path) -> bool {
    dir.file_name()
        .and_then(OsStr::to_str)
        .and_then(|n| n.strip_prefix("kasaterm-shim-"))
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kasa-acp-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn installed_adapter_wins_over_npx() {
        let dir = scratch("installed");
        let exe = if cfg!(windows) { "codex-acp.cmd" } else { "codex-acp" };
        let npx = if cfg!(windows) { "npx.cmd" } else { "npx" };
        fs::write(dir.join(exe), "").expect("write");
        fs::write(dir.join(npx), "").expect("write");
        let spec = LaunchSpec::resolve(Harness::Codex, dir.as_os_str()).expect("resolved");
        assert_eq!(spec.program, dir.join(exe));
        assert!(spec.args.is_empty());
        assert_eq!(spec.env, [(OsString::from("PATH"), dir.clone().into_os_string())]);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn codex_gets_the_pane_env_pinned_like_the_shim_does() {
        let vars = [
            ("KASATERM_PANE_ID", "%1"),
            ("KASATERM_SOCKET_PATH", "/tmp/rig.sock"),
            ("KASATERM_PERSONA", "persona"),
            ("KASA_API_KEY", "secret"),
            ("KASATERM_NOTE", "two\nlines"),
            ("CODEX_HOME", "/h"),
            ("HOME", "/Users/x"),
        ]
        .map(|(k, v)| (k.to_owned(), v.to_owned()));
        let config: Value =
            serde_json::from_str(&codex_pane_env_config(vars.clone(), None).expect("something to pin")).expect("json");
        assert_eq!(
            config,
            serde_json::json!({
                "shell_environment_policy.set.KASATERM_PANE_ID": "%1",
                "shell_environment_policy.set.KASATERM_SOCKET_PATH": "/tmp/rig.sock",
                "shell_environment_policy.set.CODEX_HOME": "/h",
            })
        );
        let kept = codex_pane_env_config(vars.clone(), Some(r#"{"model":"m","shell_environment_policy.set.CODEX_HOME":"/mine"}"#))
            .expect("merged");
        let kept: Value = serde_json::from_str(&kept).expect("json");
        assert_eq!(kept["model"], "m");
        assert_eq!(kept["shell_environment_policy.set.CODEX_HOME"], "/mine", "an explicit value wins");
        assert_eq!(codex_pane_env_config(vars, Some("not json")), None);
        assert_eq!(codex_pane_env_config([("HOME".to_owned(), "/x".to_owned())], None), None);
    }

    #[test]
    fn pane_shim_dirs_are_skipped() {
        let root = scratch("shim");
        let shim = root.join("kasaterm-shim-74204");
        let real = root.join("bin");
        let not_shim = root.join("kasaterm-shim-notes");
        for d in [&shim, &real, &not_shim] {
            fs::create_dir_all(d).expect("dir");
        }
        let exe = if cfg!(windows) { "claude.cmd" } else { "claude" };
        fs::write(shim.join(exe), "").expect("write");
        fs::write(real.join(exe), "").expect("write");
        let path = std::env::join_paths([&shim, &real]).expect("path");
        assert_eq!(find_program("claude", &path), Some(real.join(exe)));
        fs::write(not_shim.join(exe), "").expect("write");
        let path = std::env::join_paths([&not_shim, &real]).expect("path");
        assert_eq!(find_program("claude", &path), Some(not_shim.join(exe)));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn falls_back_to_pinned_npx_package_or_nothing() {
        let dir = scratch("npx");
        let npx = if cfg!(windows) { "npx.cmd" } else { "npx" };
        assert_eq!(LaunchSpec::resolve(Harness::Claude, dir.as_os_str()), None);
        fs::write(dir.join(npx), "").expect("write");
        let spec = LaunchSpec::resolve(Harness::Claude, dir.as_os_str())
            .expect("resolved")
            .claude_executable(Path::new("/opt/claude"));
        assert_eq!(spec.program, dir.join(npx));
        assert_eq!(spec.args, [OsString::from("-y"), "@agentclientprotocol/claude-agent-acp@0.101.0".into()]);
        assert!(spec.env.contains(&("CLAUDE_CODE_EXECUTABLE".into(), "/opt/claude".into())));
        fs::remove_dir_all(dir).ok();
    }
}
