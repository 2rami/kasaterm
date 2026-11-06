//! 하네스별 어댑터를 띄울 명령. 설치된 어댑터가 PATH 에 있으면 그것을, 없으면 npx 로 받는다.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

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

fn find_program(name: &str, path: &OsStr) -> Option<PathBuf> {
    let names: Vec<String> = if cfg!(windows) {
        vec![format!("{name}.cmd"), format!("{name}.exe"), name.to_owned()]
    } else {
        vec![name.to_owned()]
    };
    std::env::split_paths(path)
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|p| p.is_file())
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
