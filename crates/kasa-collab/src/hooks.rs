//! GUI 없는 호스트(`kasa tui`)의 claude 훅 설치. 칸 안 claude 가 턴 경계·승인 대기·대화 기록
//! 자리를 호스트에 알려야 보드의 학생 상태·done·summon 이 돈다.
//!
//! 훅 스크립트의 원본은 본판 `app/kasaterm/collab-hooks/` 다(본판은 앱 묶음에서 그 폴더를 직접
//! 가리킨다). 여기서는 같은 파일을 묶어 shim 폴더에 풀어 쓴다 — cargo git 의존은 레포 전체를
//! 받으므로 git rev 로 받는 kasalite 에서도 그대로 묶인다(LFS 아닌 글 파일들이다).
//!
//! 지금은 유닉스(bash)만이다. 윈도우 훅(Git bash·python 찾기)은 본판 설치기에만 있다.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::json;

macro_rules! hook {
    ($name:literal) => {
        ($name, include_bytes!(concat!("../../../app/kasaterm/collab-hooks/", $name)).as_slice())
    };
}

/// 묶는 훅. 보드 상태에 필요한 것(대화 기록 묶기·턴·승인 대기·도구 진행)과 협업 가드(남이 잡은
/// 파일 덮어쓰기 막기)다.
const HOOKS: [(&str, &[u8]); 6] = [
    hook!("kasaterm-bind-transcript.sh"),
    hook!("kasaterm-turn.sh"),
    hook!("kasaterm-notify-attention.sh"),
    hook!("kasaterm-agent-status.sh"),
    hook!("kasaterm-stop-drain.sh"),
    hook!("kasaterm-conflict-guard.py"),
];

/// `dir/hooks/` 에 훅을 풀고 `dir/claude-hooks-settings.json` 을 쓴다. 설정 파일 경로를 돌려준다.
pub fn install(dir: &Path) -> Result<PathBuf> {
    let hooks = dir.join("hooks");
    std::fs::create_dir_all(&hooks).with_context(|| format!("훅 폴더 {}", hooks.display()))?;
    for (name, bytes) in HOOKS {
        let path = hooks.join(name);
        std::fs::write(&path, bytes).with_context(|| format!("훅 {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    let hd = hooks.display().to_string();
    let cmd = |script: &str, timeout: u64| {
        let (file, args) = match script.split_once(' ') {
            Some((f, a)) => (f, format!(" {a}")),
            None => (script, String::new()),
        };
        json!({ "type": "command", "command": format!("\"{hd}/{file}\"{args}"), "timeout": timeout })
    };
    let settings = json!({
        "hooks": {
            "SessionStart": [{ "hooks": [cmd("kasaterm-bind-transcript.sh", 5000)] }],
            "PreToolUse": [
                { "matcher": "Edit|Write|MultiEdit", "hooks": [cmd("kasaterm-conflict-guard.py", 5000)] },
                { "hooks": [cmd("kasaterm-agent-status.sh", 5)] }
            ],
            "PostToolUse": [{ "hooks": [cmd("kasaterm-agent-status.sh", 5)] }],
            "UserPromptSubmit": [{ "hooks": [cmd("kasaterm-turn.sh start", 5)] }],
            "PreCompact": [{ "hooks": [cmd("kasaterm-turn.sh compact_start", 5)] }],
            "Stop": [
                { "hooks": [cmd("kasaterm-turn.sh end", 5)] },
                { "hooks": [cmd("kasaterm-stop-drain.sh", 5000)] },
                { "hooks": [cmd("kasaterm-agent-status.sh", 5)] }
            ],
            "Notification": [
                { "matcher": "permission_prompt", "hooks": [cmd("kasaterm-notify-attention.sh permission", 5000)] },
                { "matcher": "elicitation_dialog|elicitation_url_dialog|agent_needs_input", "hooks": [cmd("kasaterm-notify-attention.sh question", 5000)] },
                { "matcher": "idle_prompt", "hooks": [cmd("kasaterm-notify-attention.sh idle", 5000)] }
            ]
        },
        "crossSessionInbound": "accept"
    });
    let path = dir.join("claude-hooks-settings.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&settings)?)?;
    Ok(path)
}

/// shim 폴더의 `claude` — 진짜 claude 를 PATH 에서 shim 폴더를 빼고 찾아 훅 설정을 얹어 부른다.
#[cfg(unix)]
pub fn write_claude_wrapper(dir: &Path, settings: &Path) -> Result<()> {
    let script = format!(
        "#!/bin/sh\n\
         # kasa tui 칸의 claude — 훅 설정을 얹는다. 진짜 claude 는 이 폴더를 뺀 PATH 에서 찾는다.\n\
         SELF_DIR='{dir}'\n\
         P=$(printf '%s' \"$PATH\" | tr ':' '\\n' | grep -vxF \"$SELF_DIR\" | paste -sd: -)\n\
         REAL=$(PATH=\"$P\" command -v claude)\n\
         [ -z \"$REAL\" ] && {{ echo 'claude 를 찾지 못했다' >&2; exit 127; }}\n\
         exec \"$REAL\" --settings '{settings}' \"$@\"\n",
        dir = dir.display(),
        settings = settings.display(),
    );
    let path = dir.join("claude");
    std::fs::write(&path, script)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn installs_hooks_and_settings() {
        let dir = std::env::temp_dir().join(format!("kasa-collab-hooks-{}", std::process::id()));
        let settings = super::install(&dir).unwrap();
        let text = std::fs::read_to_string(&settings).unwrap();
        assert!(text.contains("kasaterm-turn.sh\\\" start"));
        assert!(dir.join("hooks/kasaterm-bind-transcript.sh").exists());
        #[cfg(unix)]
        {
            super::write_claude_wrapper(&dir, &settings).unwrap();
            let wrapper = std::fs::read_to_string(dir.join("claude")).unwrap();
            assert!(wrapper.contains("--settings"));
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
