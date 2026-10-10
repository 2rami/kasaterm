//! claude 래퍼 — 협업 훅·연결 mod·계정 경로를 싣는 `claude` shim 과 그 재료.
use super::*;

/// Locate the canonical collab-hooks directory the generated hook settings
/// point at. The scripts resolve their siblings via `dirname $0`, so pointing
/// at any one complete copy works.
/// 레포의 claude mod(`collab-hooks/claude-mods/<이름>/`, `.claude-plugin/plugin.json` 이 있는 폴더)를
/// 전부 shim 자리로 옮기고 옮긴 이름을 돌려준다. 번들 안을 그대로 가리키면 claude 가 그 폴더에
/// 타입 파일(`.claude-plugin/types`)을 써 서명된 앱이 바뀌므로 쓸 수 있는 자리에 복사본을 둔다.
/// 시험·엔진이 깐 타입은 싣지 않는다.
pub(crate) fn install_claude_mods(hooks_dir: &std::path::Path, shim_dir: &std::path::Path) -> Vec<String> {
    fn copy(from: &std::path::Path, to: &std::path::Path, skip: &[&str]) -> std::io::Result<()> {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            let name = entry.file_name();
            if name.to_str().is_some_and(|n| skip.contains(&n)) {
                continue;
            }
            let src = entry.path();
            let dst = to.join(&name);
            if src.is_dir() {
                let inner: &[&str] = if name == ".claude-plugin" { &["types"] } else { &[] };
                copy(&src, &dst, inner)?;
            } else {
                // 같은 내용이면 건드리지 않는다 — claude 는 이 폴더를 지켜보다 파일이 바뀌면 mod 를
                // 다시 싣는데, 다시 실린 mod 는 쥐고 있던 것을 잃는다(shim 은 설정을 바꿀 때마다 다시 굽는다).
                let body = std::fs::read(&src)?;
                if std::fs::read(&dst).ok().as_deref() != Some(body.as_slice()) {
                    std::fs::write(&dst, body)?;
                }
            }
        }
        Ok(())
    }
    crate::prompt_nav::set_dir(shim_dir.join("prompt-nav"));
    let Ok(entries) = std::fs::read_dir(hooks_dir.join("claude-mods")) else { return Vec::new() };
    let mut names: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let from = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
        if !from.join(".claude-plugin/plugin.json").is_file() {
            continue;
        }
        match copy(&from, &shim_dir.join("claude-mods").join(&name), &["tests", ".gitignore"]) {
            Ok(()) => names.push(name),
            Err(e) => eprintln!("[shim] claude mod {name} copy failed: {e}"),
        }
    }
    names.sort();
    names
}

/// claude shim 에 얹는 줄 — 옮긴 mod 폴더마다 `--plugin-dir` 하나. prompt-nav 의 상태 파일은 그
/// 칸 몫이라 뜰 때 지운다 — 남아 있으면 mod 없이 뜬 claude 에도 낡은 막대가 그려진다.
pub(crate) fn claude_mods_block(names: &[String]) -> String {
    if names.is_empty() {
        return String::new();
    }
    let mut block = String::from("if [ -n \"$PERSONA_OK\" ]; then\n");
    if names.iter().any(|n| n == "prompt-nav") {
        block.push_str(
            "  export KASATERM_PROMPT_NAV_DIR=\"$SELF_DIR/prompt-nav\"\n\
  [ -n \"$KASATERM_PANE_ID\" ] && rm -f \"$KASATERM_PROMPT_NAV_DIR/$KASATERM_PANE_ID.json\" \"$KASATERM_PROMPT_NAV_DIR/$KASATERM_PANE_ID.req.json\"\n",
        );
    }
    block.push_str(
        "  for MOD in \"$SELF_DIR\"/claude-mods/*/; do\n\
    [ -f \"${MOD}.claude-plugin/plugin.json\" ] && set -- --plugin-dir \"${MOD%/}\" \"$@\"\n\
  done\n\
fi\n",
    );
    block
}

pub(crate) fn locate_collab_hooks_dir() -> Option<std::path::PathBuf> {
    resolve_collab_hooks_dir(
        std::env::current_exe().ok().as_deref(),
        std::env::var("KASATERM_COLLAB_HOOKS_DIR").ok().as_deref(),
    )
}

/// Pure resolution (split out so the priority is unit-testable). Priority:
/// 1. the **.app bundle's own Resources** — a release binary must run the hooks
///    it shipped with, so this WINS over the env override. Otherwise a leaked
///    `KASATERM_COLLAB_HOOKS_DIR` (e.g. inherited from a dev shell) would point
///    a release `.app` at version-skewed repo hooks (the bug this guards).
///    Windows MSI 는 exe 옆 `bin\collab-hooks\` — arona-ui 번들과 같은 자리,
///    같은 이유로 env 보다 우선.
/// 2. `KASATERM_COLLAB_HOOKS_DIR` — dev convenience for non-bundle `cargo run`,
///    where no bundle Resources sits next to `target/{debug,release}/kasaterm`.
/// 3. the repo source next to this crate (`CARGO_MANIFEST_DIR`) — plain dev run.
pub(crate) fn resolve_collab_hooks_dir(
    current_exe: Option<&std::path::Path>,
    env_dir: Option<&str>,
) -> Option<std::path::PathBuf> {
    if let Some(exe) = current_exe {
        // <bundle>/Contents/MacOS/kasaterm → <bundle>/Contents/Resources/collab-hooks
        if let Some(res) = exe
            .parent()
            .and_then(|m| m.parent())
            .map(|c| c.join("Resources/collab-hooks"))
        {
            if res.is_dir() {
                return Some(res);
            }
        }
        // Windows MSI: <bin>\kasaterm.exe → <bin>\collab-hooks\
        if let Some(adj) = exe.parent().map(|d| d.join("collab-hooks")) {
            if adj.is_dir() {
                return Some(adj);
            }
        }
    }
    if let Some(p) = env_dir {
        let p = std::path::PathBuf::from(p);
        if p.is_dir() {
            return Some(p);
        }
    }
    let dev = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("collab-hooks");
    if dev.is_dir() {
        return Some(dev);
    }
    None
}

/// The one shim line that switches Claude accounts, or `""` for the default
/// login.
///
/// `CLAUDE_SECURESTORAGE_CONFIG_DIR` moves *only* the credential store — Claude
/// Code hashes it into its Keychain item name — while `CLAUDE_CONFIG_DIR` stays
/// unset so `~/.claude` keeps holding transcripts, agents, teams and MCP config.
/// Switching the config dir instead would fracture all of that, since kasaterm
/// hardcodes `~/.claude` in the statusline, the board reader and the shim's own
/// `--continue` inference.
///
/// No account selected emits **nothing**: the default login is the absence of
/// the override, so an untouched install behaves exactly as before. An inherited
/// value wins, which is what lets the add-account flow log in to a brand new
/// store by exporting it explicitly.
///
/// The guard tests `${VAR+x}`, not `$VAR`, because **empty is not unset here**.
/// Claude Code reads a defined-but-empty value as "use the unsuffixed store" and
/// deliberately forwards it to child processes (its env allowlist special-cases
/// this one name so the empty string survives), so a claude that was told to
/// stay on the default login must keep that instruction. `[ -z "$VAR" ]` cannot
/// tell the two apart and silently re-points such a child at our account —
/// verified against 2.1.220: with the value set to `""`, `claude auth status`
/// reports the default login as signed in.
pub(crate) fn claude_account_export_line(dir: Option<&std::path::Path>) -> String {
    // 빈 경로 = 작업대(기본 자리) — env 를 아예 안 붙여 순정 claude 와 같게 띄운다.
    // 빈 값을 export 해도 claude 는 기본으로 동작하지만, 안 붙이는 쪽이 실측(ps 의
    // env 판독)에서도 「기본」으로 읽혀 깔끔하다.
    let Some(dir) = dir.filter(|d| !d.as_os_str().is_empty()) else {
        return String::new();
    };
    let q = dir.display().to_string().replace('\'', "'\\''");
    format!(
        "[ -z \"${{CLAUDE_SECURESTORAGE_CONFIG_DIR+x}}\" ] && \
         export CLAUDE_SECURESTORAGE_CONFIG_DIR='{q}'\n"
    )
}

/// Stage a `claude` wrapper + a session-scoped hook settings file on the pane
/// PATH (munder-difflin pattern). Collab hooks ride in via `claude --settings`
/// instead of edits to ~/.claude/settings.json, so claude outside a kasaterm
/// pane runs exactly as the user configured it and install-hooks.sh is no
/// longer needed.
/// claude shim 의 teammate 이름/색 case 분기(`미도리) AGENT=midori; ACOLOR=green ;;`) —
/// 배정 캐릭터(한글)를 ASCII agent 이름과 8색 --agent-color 로 사상한다. 로마자 슬러그가
/// 정본(inbox 파일명이 agent-name 슬러그라 한글은 "---" 로 붕괴), 슬러그 없는 커스텀
/// 캐릭터는 해시 축약으로 방어. 색은 characters.json claude_color 를 8색으로 정규화 —
/// --agent-color 는 teammate TUI 전체 테마라 pane accent 와 결이 맞아야 한다(team.rs).
pub(crate) fn teammate_case_arms() -> String {
    let Some(chars) = kasa_mcp::character::characters_json() else {
        return String::new();
    };
    let mut arms = String::new();
    for name in kasa_mcp::character::member_names(&chars) {
        // case 패턴 자리에 그대로 박히므로 sh 특수문자가 든 이름은 건너뛴다(사용자 편집
        // characters.json 방어 — 그 캐릭터만 팀모드 없이 부팅될 뿐 스크립트는 안 깨진다).
        if name
            .chars()
            .any(|c| c.is_whitespace() || "|)('\"`;&<>*?[]{}$!\\#~".contains(c))
        {
            continue;
        }
        let slug = theme::agent_slug(&name);
        let color = kasa_mcp::character::claude_color_for(&chars, &name)
            .map(|c| kasa_mcp::team::normalize_agent_color(&c).to_string())
            .unwrap_or_default();
        arms.push_str(&format!("      {name}) AGENT={slug}; ACOLOR={color} ;;\n"));
    }
    arms
}

/// 우리가 쓴 파일인지 알아보는 표식. 이게 없으면 사용자가 손수 쓴 것으로 보고
/// 건드리지 않는다 — `~/.claude/commands` 는 사용자 개인 설정이지 우리 것이 아니다.
pub(crate) const RENAME_CMD_MARK: &str = "<!-- kasaterm-managed -->";

/// 예전에 심어 둔 `~/.claude/commands/rename.md` 를 **지운다**.
///
/// 내장 `/rename` 이 거부되던 건("Teammate names are set by the team leader") 셰임이
/// 트리플을 붙여 pane 의 claude 가 전부 팀원이었기 때문이다. 이제 트리플을 걷어냈으니
/// 내장이 다시 돈다 — 2026-08-09 실측: `Session renamed to: RENAMED-OK` 이 뜨고 그
/// 이름이 입력박스 상단 보더(칩이 덮던 그 자리)에 표시됐다. 그러면 커스텀 커맨드는
/// 이득 없이 내장을 가리기만 한다(커스텀이 내장을 이긴다).
///
/// 레포 밖 파일이라 심는 코드를 지우는 것만으론 이미 깔린 파일이 안 없어진다. 그래서
/// 부팅마다 우리 표식이 붙은 것만 골라 지운다. 표식 없는 파일 = 사용자가 쓴 것이라
/// 그대로 둔다 — 자동 정리가 남의 편집을 지우면 그게 더 나쁘다.
pub(crate) fn remove_rename_command() {
    let Some(home) = kasa_socket::home_dir() else {
        return;
    };
    let path = home.join(".claude/commands/rename.md");
    if !rename_cmd_is_ours(std::fs::read_to_string(&path).ok().as_deref()) {
        return;
    }
    if let Err(e) = std::fs::remove_file(&path) {
        eprintln!("[shim] /rename 대체 커맨드 제거 실패 {path:?}: {e}");
    }
}

/// 지울지 판정만 — 파일시스템을 안 타야 테스트가 `$HOME` 을 흔들지 않는다.
pub(crate) fn rename_cmd_is_ours(existing: Option<&str>) -> bool {
    existing.is_some_and(|cur| cur.contains(RENAME_CMD_MARK))
}

pub(crate) fn install_claude_hook_shim(shim_dir: &std::path::Path) {
    let Some(hooks_dir) = locate_collab_hooks_dir() else {
        eprintln!("[shim] collab-hooks dir not found — claude hook shim skipped");
        return;
    };
    // The Claude wrapper and character roster use separate crates and path
    // resolvers. Publish the path already proven here so Windows dev builds
    // do not lose the roster while still finding the hook scripts.
    std::env::set_var("KASATERM_COLLAB_HOOKS_DIR", &hooks_dir);
    // Windows pane 셸은 Git bash(sh 있음) — wrapper 는 그대로 쓴다. sh 더블쿼트 안
    // 백슬래시는 케이스별로 씹히므로 경로는 슬래시로 통일(Git bash 는 C:/ 혼용 허용).
    let hd = if cfg!(windows) {
        hooks_dir.display().to_string().replace('\\', "/")
    } else {
        hooks_dir.display().to_string()
    };
    // Windows Claude 는 훅을 네이티브 프로세스로 띄워 Git Bash 의 명령 탐색을
    // 거치지 않는다. PATH 에 `sh` 이름이 없으면 SessionStart 가 무음 실패해 학생
    // 배정만 사라지므로 설치된 sh.exe 절대경로를 설정에 굽는다.
    let hook_sh = hook_shell_program();
    let cmd = |script: &str, timeout: u64| {
        // 「kasaterm-turn.sh end」처럼 인자가 붙은 것은 **경로만** 따옴표에 넣는다 — 통째로
        // 감싸면 sh 가 「…/kasaterm-turn.sh end」라는 파일을 찾아 No such file 로 죽고,
        // 턴 경계가 앱에 한 번도 안 닿았다(2026-09-18 실측: Stop 훅 오류가 매 턴 떴다).
        let (file, args) = match script.split_once(' ') {
            Some((f, a)) => (f, format!(" {a}")),
            None => (script, String::new()),
        };
        let run = if cfg!(windows) {
            match file.strip_suffix(".py") {
                Some(_) => format!(
                    "{} -X utf8 \"{hd}/{file}\"{args}",
                    python3_program().unwrap_or("python3")
                ),
                None => format!("\"{hook_sh}\" \"{hd}/{file}\"{args}"),
            }
        } else {
            format!("\"{hd}/{file}\"{args}")
        };
        serde_json::json!({ "type": "command", "command": run, "timeout": timeout })
    };
    // Mirrors what install-hooks.sh used to register globally — same matcher
    // and timeouts, so in-pane behavior is unchanged.
    let mut settings = serde_json::json!({
        "hooks": {
            // 세션 시작/재개 즉시 bind → 첫 프롬프트 전에도 board 에 뜬다. SessionStart 는
            // startup·resume·clear 에 모두 발화하므로 relaunch 후 claude --resume 재바인딩도
            // 커버. transcript 자체는 discover_transcript(cwd→projects, --session-id)로
            // hook-free 라 이 bind 는 roster(복구)·즉시성 보조일 뿐.
            "SessionStart": [{ "hooks": [cmd("kasaterm-bind-transcript.sh", 5000)] }],
            // UserPromptSubmit(board-context.py) 제거 — 프롬프트마다 persona+board+inbox 를
            // additionalContext 로 주입해 소넷 워커 컨텍스트가 누적·과대했다(사용자 06-14).
            // persona 는 스폰 시 `--append-system-prompt`로 1회(캐시돼 per-turn 0) 대체.
            // board/inbox 자동인지는 폐기 — 조율은 GUI(SCHALE OS) 와 명시적 kasacollab 으로.
            // 같은 방 다른 pane 이 같은 파일을 작업 중이면 Edit 직전에 막는다
            // (transcript 직접 비교, 데몬 무관). 모든 pane 공통 안전망.
            // 진행 표시 정본(`kasaterm-agent-status.sh`) — 서브에이전트·백그라운드의
            // 시작과 끝을 그 순간 받는다. matcher 를 안 거는 이유: 걸러야 할 것이
            // 도구 **이름**이 아니라 `tool_input.run_in_background` 라 matcher 로는
            // 표현이 안 되고, 스크립트가 첫 줄에서 bash 만으로 관심 밖을 쳐낸다.
            // timeout 은 초 단위라 5 초 — 표시가 한 번 빠지는 것이 도구 호출이
            // 늦어지는 것보다 낫다(옆의 5000 은 사실상 무제한이다).
            // 닫힌 pane 으로 가는 SendMessage 를 그 자리서 막는다. 사용자가 닫아도
            // 그 안의 claude 는 계속 도는데 **명부(ListAgents)에는 닫힘이 안 보여서**,
            // 학생이 멀쩡한 줄 알고 일을 시키고 그 작업이 사용자 눈 밖에서 돌았다
            // (사용자 2026-08-15). board 의 `detached` 로 이미 알 수 있지만 그건 보러
            // 가야 보이고, 안 보고 보내는 것이 사고의 형태다.
            // 서브에이전트(Agent)는 보드·화면에 안 보여 사람이 진행을 못 지켜본다 — 막고
            // `kasaterm-cli summon` 으로 학생을 세우게 한다(읽기 전용 탐색·설계는 통과).
            "PreToolUse": [
                { "matcher": "Edit|Write|MultiEdit", "hooks": [cmd("kasaterm-conflict-guard.py", 5000)] },
                { "matcher": "Agent|Task", "hooks": [cmd("kasaterm-subagent-guard.py", 5)] },
                // TestFlight 빌드 만료는 되돌릴 수 없고 깔아 둔 폰 앱이 멈춘다 — 학생은 못 하게(2026-09-28 사고).
                { "matcher": "Bash", "hooks": [cmd("kasaterm-build-expiry-guard.py", 5)] },
                { "hooks": [cmd("kasaterm-closed-pane-guard.py", 5000)] },
                { "hooks": [cmd("kasaterm-agent-status.sh", 5)] }
            ],
            "PostToolUse": [
                { "matcher": "SendUserFile", "hooks": [cmd("auto-imgopen.sh", 10)] },
                // 목록에 뜨는 이름도 전부 세션 이름이라, 학생이 그것을 사람 이름으로 쓰게
                // 된다. 목록 자체는 안 건드리고 옆에 이름↔캐릭터 표만 붙인다 —
                // **부를 때는 캐릭터, `to:` 주소는 목록의 그 이름**이라는 구분이 요점이다.
                { "matcher": "ListAgents", "hooks": [cmd("kasaterm-peer-name.py", 5000)] },
                { "hooks": [cmd("kasaterm-steer-hook.sh", 5000)] },
                { "hooks": [cmd("kasaterm-agent-status.sh", 5)] }
            ],
            // ultracode 는 effort 와 별개 상태인데 claude 가 statusline 에 안 실어 준다
            // (payload 스펙의 effort 는 low|medium|high|xhigh|max 뿐). 여러 에이전트를
            // 푸는 턴인지가 화면에 안 보이므로, 프롬프트를 보고 마커를 남겨 statusline 이
            // 읽게 한다. 턴 단위 opt-in 이라 마커도 프롬프트마다 다시 쓰고 지운다.
            // 도착한 cross-session 메시지의 `from-name` 은 **세션 이름**이라 화면의 캐릭터
            // 이름과 다르다. 태그는 claude 가 직접 만들고 명부도 claude 가 안에 들고 있어
            // (밖에서 파일을 고쳐도 안 먹는다 — 실측) 앱이 끼어들 자리가 여기뿐이다.
            // ⚠️ 위에서 걷어낸 board-context.py 와 혼동하지 말 것: 그건 프롬프트마다
            // board 전체를 밀어 넣어 컨텍스트를 부풀렸고, 이건 **메시지가 온 턴에만**
            // **발신자 한 줄만** 낸다. 그 좁힘을 풀면 같은 비용이 그대로 돌아온다.
            // stdout 에 JSON 을 내므로 ultracode-mark 와 **다른 그룹**에 둔다(Stop 의
            // stop-drain 과 같은 이유 — 한 그룹에 섞이면 결정이 깨질 수 있다).
            // 턴 경계(`kasaterm-turn.sh`) — pane 상태의 정본. 프롬프트 제출이 열고 Stop 이
            // 닫으며, PreCompact 가 압축 시작을 알린다(끝은 SessionStart(source=compact) 를
            // bind-transcript 가 받는다). 화면의 스피너를 읽어 상태를 짐작하던 것을 대신한다.
            // bash+sed 뿐이라 프롬프트 핫패스에 인터프리터가 안 뜬다.
            "UserPromptSubmit": [
                { "hooks": [cmd("kasaterm-turn.sh start", 5)] },
                { "hooks": [cmd("kasaterm-closed-pane-guard.py", 5000)] },
                { "hooks": [cmd("ultracode-mark.py", 3000)] },
                { "hooks": [cmd("kasaterm-peer-name.py", 5000)] }
            ],
            "PreCompact": [
                { "hooks": [cmd("kasaterm-turn.sh compact_start", 5)] }
            ],
            // 두 훅을 **다른 그룹**으로 나눠 둔다 — stop-drain 은 인박스가 있으면
            // stdout 에 block JSON 을 내는데, 같은 그룹이면 진행 표시 훅의 출력과
            // 섞여 그 결정이 깨질 수 있다. agent-status 는 stdout 을 안 쓰지만
            // 나란히 두는 것 자체가 나중에 그 규칙을 잊게 만든다.
            "Stop": [
                { "hooks": [cmd("kasaterm-turn.sh end", 5)] },
                { "hooks": [cmd("kasaterm-stop-drain.sh", 5000)] },
                { "hooks": [cmd("kasaterm-agent-status.sh", 5)] }
            ],
            // 종류별로 나눠 건다 — 승인(permission) · 질문/선택(question) · 답 없이 방치
            // (idle). auth_success·agent_completed 같은 것은 사람을 기다리는 게 아니라
            // 안 건다. 스크립트는 페이로드의 종류를 먼저 보고, 없으면 인자를 쓴다(옛
            // claude 가 matcher 를 무시해도 종류가 안 섞인다).
            "Notification": [
                { "matcher": "permission_prompt", "hooks": [cmd("kasaterm-notify-attention.sh permission", 5000)] },
                { "matcher": "elicitation_dialog|elicitation_url_dialog|agent_needs_input", "hooks": [cmd("kasaterm-notify-attention.sh question", 5000)] },
                { "matcher": "idle_prompt", "hooks": [cmd("kasaterm-notify-attention.sh idle", 5000)] },
            ],
        },
        // statusLine 도 세션 스코프 --settings 로 주입 — 배정 학생 프사(U+FFFC)·model·git·
        // ctx%·effort + 내부 cd 보고(report-cwd). pane 안에서만 우리 것, 밖 claude 는
        // 사용자 ~/.claude/settings.json statusLine 그대로(--settings 는 pane PATH 한정).
        // claude 는 대화가 움직일 때만 상태줄을 다시 그려, 쉬는 pane 은 다른 곳에서 바꾼
        // 브랜치를 계속 옛 이름으로 보였다. 매초 다시 그린다 — 그래서 한 번에 수 ms 인
        // kasaterm-cli(shim dir 에 스테이징돼 pane PATH 로 잡힌다)로 짓는다.
        "statusLine": {
            "type": "command",
            "command": "kasaterm-cli statusline",
            "padding": 0,
            "refreshInterval": 1,
        },
        // 다른 방 pane 이 보낸 메시지를 승인 대기로 잡지 않는다. 기본값은 권한 프롬프트를
        // 건너뛰는 세션의 인바운드를 붙잡는데, pane claude 는 전부 그 모드라 기본값이면
        // 학생을 굴리는 흐름이 매 메시지 사용자 클릭에서 끊긴다(08-09: accept 로 도달 확인).
        "crossSessionInbound": "accept",
    });
    if cfg!(windows) {
        // conflict-guard 는 python 의존 — 기본 Windows 엔 쓸 만한 인터프리터가 없어
        // 훅이 매 Edit 마다 실패 노이즈를 낸다. 실제로 찾았을 때만 유지한다
        // (`python3` 이라는 이름만 보면 안 된다 — Windows 의 그 이름은 exit 49 로
        // 죽는 MS Store 스텁이라, 실행해서 "Python 3" 을 확인하는 쪽이 정본이다).
        //
        // 같은 remove 에 진행 표시 훅(`agent-status`)의 `PreToolUse` 도 함께 빠지는데,
        // 그게 맞다 — 그 스크립트도 payload 파싱에 python 을 쓰므로 남겨 봐야 아무
        // 일도 못 한다. Windows 는 transcript 폴백으로 표시가 이어진다(꼬리 한계는
        // 그대로 남지만, 훅이 없는 편보다 낫다).
        if python3_program().is_none() {
            settings["hooks"]
                .as_object_mut()
                .unwrap()
                .remove("PreToolUse");
        }
    }
    let settings_path = shim_dir.join("claude-hooks-settings.json");
    {
        match serde_json::to_string_pretty(&settings) {
            Ok(s) => {
                if let Err(e) = write_shim_data(&settings_path, s) {
                    eprintln!("[shim] write claude-hooks-settings.json failed: {e}");
                    return;
                }
            }
            Err(e) => {
                eprintln!("[shim] serialize claude hook settings failed: {e}");
                return;
            }
        }
    }
    // 설정창 "클로드" 탭 노브를 shim 에 인라인 — 파싱은 여기 Rust 가 하고 shim 은 순수 sh 로
    // 남긴다(hot path 가벼움). 아래 라인들은 전부 PERSONA_OK 게이트를 공유하므로
    // attach/agents/subcommand(case 가 PERSONA_OK 를 비움)엔 안 붙어 서브커맨드를 오염 안
    // 시킨다. 불변식(session-id/--settings/task-list)은 노브가 아니라 계속 하드코딩.
    let model = socket::read_claude_model();
    let effort = socket::read_claude_effort();
    let extra = socket::read_claude_extra();
    let extra = extra.trim();
    // 「말투」 토글로 이 줄을 빼지 않는다 — 꺼지면 신원 쪽이 `KASATERM_PERSONA` 를 규약만으로 내려 준다.
    // 여기서 한 번 더 막으면 말투를 끈 채 부팅한 앱의 claude 만 보드·전달·done 규약 없이 떴다
    // (2026-10-02 codex pane 과 견주다 확인 — codex 는 같은 값을 AGENTS.md 로 받아 규약이 실렸다).
    let persona_line = "[ -n \"$PERSONA_OK\" ] && [ -n \"$KASATERM_PERSONA\" ] && set -- --append-system-prompt \"$KASATERM_PERSONA\" \"$@\"\n".to_string();
    // 학생별 실행 통로(`KASATERM_BACKEND`) — kimi·glm 처럼 claude 를 감싸 게이트웨이로
    // 보내는 런처다. 이 줄이 persona 블록의 **맨 앞**인 것이 설계의 핵심이다: 런처는
    // 환경을 씌운 뒤 다시 PATH 의 claude(= 이 shim)를 부르므로, 플래그를 붙인 다음에
    // 넘기면 재진입에서 한 번 더 붙어 persona 와 --settings 가 두 벌이 된다. 원본
    // 인자를 그대로 넘기고 주입은 재진입 쪽에 맡기면 정확히 한 번만 붙는다.
    //
    // 재귀 가드(`KASATERM_VIA_BACKEND`)가 없으면 무한루프다 — 런처가 `command claude`
    // 로 부르는데 `command` 는 함수·별칭만 건너뛸 뿐 PATH 는 그대로 타서 이 shim 으로
    // 되돌아온다. 런처가 PATH 에 없으면 줄 전체가 조용히 통과해 순정 claude 로 뜬다.
    let backend_line = "[ -n \"$PERSONA_OK\" ] && [ -n \"$KASATERM_BACKEND\" ] \
&& [ -z \"$KASATERM_VIA_BACKEND\" ] && command -v \"$KASATERM_BACKEND\" >/dev/null 2>&1 \
&& { KASATERM_VIA_BACKEND=1; export KASATERM_VIA_BACKEND; exec \"$KASATERM_BACKEND\" claude \"$@\"; }\n"
        .to_string();
    // 학생별 모델(`KASATERM_MODEL`)이 설정창 전역 노브를 이긴다(2026-08-24 지시:
    // 학생 한 명당 모델 선택). 전역이 비어 있어도 줄을 굽는다 — 학생 값이 들어올
    // 자리를 남겨야 하고, 둘 다 비면 셸에서 걸러져 `--model` 이 안 붙는다.
    //
    // 게이트웨이 런처를 거쳐 돌아온 경우엔 손대지 않는다: 그 런처가 이미 자기
    // `--model` 을 붙였으므로 여기서 덧붙이면 플래그가 두 번이 되어, 게이트웨이가
    // 모르는 이름이 이겨 엉뚱한 모델로 붙을 수 있다.
    let model_line = {
        // 전역값만 작은따옴표로 감싼다 — `claude-opus-5[1m]` 의 `[1m]` 이 zsh 글롭이라
        // 무인용이면 "no matches found" 로 대입이 통째 실패해 --model 이 아예 안 붙고
        // claude 가 기본 모델(구세대 Opus)로 떨어졌다(사용자 2026-07-27 실사고: 학생이
        // 전부 4.8). env 쪽은 큰따옴표 확장이라 글롭을 안 탄다.
        let q = model.replace('\'', "'\\''");
        format!(
            "if [ -n \"$PERSONA_OK\" ] && [ -z \"$KASATERM_VIA_BACKEND\" ]; then\n\
             _KTM=\"$KASATERM_MODEL\"\n\
             [ -z \"$_KTM\" ] && _KTM='{q}'\n\
             [ -n \"$_KTM\" ] && set -- --model \"$_KTM\" \"$@\"\n\
             fi\n"
        )
    };
    let effort_line = if effort.is_empty() {
        String::new()
    } else {
        format!("[ -n \"$PERSONA_OK\" ] && export CLAUDE_EFFORT={effort}\n")
    };
    let extra_line = if extra.is_empty() {
        String::new()
    } else {
        format!("[ -n \"$PERSONA_OK\" ] && set -- {extra} \"$@\"\n")
    };
    let persona_block =
        format!("{backend_line}{persona_line}{model_line}{effort_line}{extra_line}");
    // MCP 자동 주입. 위 노브들과 달리 **prepend 가 아니라 append** 라서 블록이 따로다 —
    // `--mcp-config` 는 variadic 이라 뒤따르는 non-flag 를 값으로 삼켜, 앞에 두면 사용자
    // 프롬프트가 통째로 사라진다. 반복 지정은 누적되므로 사용자가 자기 것을 줘도 안 부딪힌다.
    let mcp_block = match socket::claude_mcp_config_path() {
        Some(p) if socket::read_claude_mcp() => {
            let q = p
                .to_string_lossy()
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('$', "\\$")
                .replace('`', "\\`");
            format!(
                "MCPCFG=\"{q}\"\n\
                 # 파일이 비었거나 mcpServers 키가 없으면 claude 가 **부팅 자체를 거부**한다.\n\
                 # 문법 오류까지는 못 잡지만(복구는 그 파일을 지우는 것 한 줄이고 claude 가\n\
                 # 경로를 그대로 찍어준다), 빈 파일 하나로 모든 pane 을 벽돌로 만드는 사고는\n\
                 # 이 얕은 검사로 막힌다.\n\
                 if [ -n \"$PERSONA_OK\" ] && [ -s \"$MCPCFG\" ] && grep -q '\"mcpServers\"' \"$MCPCFG\" 2>/dev/null; then\n\
                 case \" $* \" in *\" --strict-mcp-config \"*|*\" -- \"*) ;; *) set -- \"$@\" --mcp-config \"$MCPCFG\" ;; esac\n\
                 fi\n"
            )
        }
        _ => String::new(),
    };
    // 계정 전환. persona/model 노브와 달리 **PERSONA_OK 게이트 밖**이다 — 저것들은
    // attach·agents·-p·stop/logs 에서 일부러 빠지지만, 인증이 서브커맨드마다 다른
    // 계정을 보면 그건 그냥 고장이다. 디렉터리는 여기서 만들어 둔다: macOS 는 경로를
    // 해시해 Keychain 항목명만 가르지만, 다른 OS 는 이 안에 .credentials.json 을 쓴다.
    // pane 이 가리킬 자리는 **작업대 = claude 의 기본 자리**다(claude_auth 머리말) —
    // ensure_active 가 성공하면 빈 경로를 주고, 그때 pane 은 env 없이 순정 claude 와
    // 똑같이 뜬다(기본 자리의 keychain 항목은 claude 가 만든 것이라 암호 창이 없다).
    // 계정마다 다른 금고를 직접 가리키면 그 pane 은 뜰 때 그 계정에 못 박혀, 나중에
    // 계정을 바꿔도 재시작 말고는 길이 없다. 채우지 못하면(로그인 없는 슬롯 등) 금고를
    // 그대로 가리키는 옛 방식으로 폴백한다 — 빈 자리를 주면 로그인 화면으로 뜬다.
    let account_id = socket::read_claude_account();
    let account_dir = crate::claude_auth::ensure_active(&account_id, socket::claude_account_dir)
        .or_else(|| socket::claude_account_dir(&account_id));
    if let Some(ref d) = account_dir {
        if !d.as_os_str().is_empty() {
            if let Err(e) = std::fs::create_dir_all(d) {
                eprintln!("[shim] claude account dir 생성 실패: {e}");
            }
        }
    }
    // 사용량 pill 도 같은 계정을 봐야 한다. `/claude-usage` 핸들러는 이 프로세스 안에서
    // 돌므로 우리 env 로 알린다 — shim 을 다시 깔 때마다(=계정을 바꿀 때마다) 갱신되니
    // pill 이 다음 폴링부터 새 계정 한도를 읽는다. 자식 pane 도 이 값을 물려받지만
    // 읽는 쪽이 없고, shim 이 보는 이름과 달라 서로 간섭하지 않는다.
    std::env::set_var(
        "KASATERM_CLAUDE_ACCOUNT_DIR",
        account_dir
            .as_deref()
            .map_or(String::new(), |d| d.display().to_string()),
    );
    // /status 의 Email/Organization 은 `~/.claude.json` 캐시다 — 작업대를 채워도
    // 캐시가 옛 계정이면 pane 은 새 계정으로 돌면서 /status 는 옛말을 한다. 전환
    // 때만 갱신하고 부팅 재구성 때는 안 갱신해, 표시가 하루 넘게 낡은 실사고가
    // 있었다(2026-08-17: 실물은 사이오닉인데 /status 는 gmail). 여기서 맞춘다.
    if !account_id.is_empty()
        && account_dir
            .as_ref()
            .is_some_and(|d| d.as_os_str().is_empty())
    {
        crate::claude_auth::adopt_oauth_account_cache(
            crate::mcp_panel_port(),
            socket::claude_account_dir(&account_id),
        );
    }
    let account_block = claude_account_export_line(account_dir.as_deref());
    // teammate 트리플 자동 부착 — pane 의 claude 를 전부 팀원으로 부팅해 SendMessage 를
    // 상시 연다. 인박스 폴러는 트리플만으로 arm 되고 config.json 은 필요 없다.
    //
    // 이 블록은 2026-07-24 에 한 번 통째로 제거됐다가 08-04 에 되살아났다. 제거 사유는
    // 둘이었는데 **둘 다 이름 꼬리가 세션 id 였던 탓**이다: `--resume` 마다 sid 가 바뀌니
    // 이름이 바뀌고, 옛 이름 인박스로 간 SendMessage 는 아무도 안 읽는 파일에 쌓였다(조용한
    // 유실). 유령 인박스도 재시작 횟수만큼 늘었다. 그래서 꼬리를 **pane 번호**로 바꿨다 —
    // pane 은 자기 생애 동안 번호가 안 바뀌므로 같은 pane 에서 몇 번을 resume 해도 같은
    // 이름·같은 인박스고, 인박스 개수는 방의 pane 슬롯 수로 묶인다.
    //
    // 되살린 이유: 제거의 진짜 동기는 `@이름` 칩이 입력박스 구분선에서 /rename 세션 이름
    // 자리를 뺏는 것이었는데(사용자), 그건 이제 render.rs 의 strip_teammate_chip 이 칩만
    // 지워서 해결한다 — 통신을 끄지 않고도 화면이 조용해진다.
    //
    // 나머지 규칙:
    // - 이름 = <로마자 슬러그>-p<pane 번호>. 한글은 inbox 파일명 슬러그가 "---" 로 붕괴해
    //   충돌한다(team.rs). 같은 캐릭터가 여러 pane 에 있어도 번호로 갈린다.
    // - 팀 = 방(cwd) 단위, /teamname 엔드포인트가 계산(fnv 해시는 순수 sh 재현 불가).
    //   서버가 죽어 팀명이 비면 플래그 전체 생략 — 순정 claude 부팅으로 조용히 폴백.
    // - pane 밖(--bg detach 포크 등)은 트리플을 안 붙인다. 데몬이 argv 를 재구성하는
    //   경로라 어차피 유실되고, pane 번호도 없어 이름이 안 선다.
    // - agent-name=목표작업명 규칙(team.rs, 다이얼로그 스폰용)과 공존: 사용자가 --agent-*
    //   를 직접 주면 우리 트리플은 통째 생략된다.
    // - agent-id 는 전원 고정 문자열 "team-lead": claude 의 승인 포워딩 게이트가
    //   agent-id=="team-lead" 를 리더로 판정해 꺼지므로, AskUserQuestion·권한 요청이
    //   "Waiting for team lead approval"(존재하지 않는 리더 무한대기 + 요청 유실)로 새지
    //   않고 그 pane 에 네이티브 렌더된다. 수신 폴러·인박스 파일명·SendMessage 주소는 전부
    //   agent-name 기준이라 id 중복은 무해하다. 비공개 인터페이스 문자열 비교라 claude
    //   버전 업 시 재검증 필요.
    //
    // 함께 남아 있는 resume 연속성 처리(트리플과 무관):
    // - TSID 파싱(--session-id/--resume 값, --continue 는 cwd 프로젝트 최신 transcript 추론)
    // - KASATERM_RESUMED_SID/RESUME_PICKER 마커(statusline ⑂bg 오발화 방지)
    // - resume 부팅 캐릭터 정합 교정(사용자: 모모이 세션이 프라나 배지·persona 로 부팅)
    let team_arms = teammate_case_arms();
    install_agent_identity_helper(shim_dir);
    let identity_block = identity_bootstrap_sh("claude", "$TSID");
    let team_block = format!(
        "AGENT=\"\"; ACOLOR=\"\"; TSID=\"$SID\"; prev=\"\"\n\
for a in \"$@\"; do case \"$prev\" in --session-id|--resume|-r) case \"$a\" in -*) ;; *) TSID=\"$a\" ;; esac ;; esac; prev=\"$a\"; done\n\
# id 없는 --continue 는 claude 와 같은 기준(cwd 프로젝트 최신 transcript)으로 sid 를\n\
# 추론해 캐릭터 정합·RESUMED_SID 마커의 연속성을 유지한다(추론 실패는 마커 없이 부팅).\n\
case \" $* \" in\n\
*\" --continue \"*|*\" -c \"*) if [ -z \"$TSID\" ]; then\n\
  RSLUG=$(printf %s \"$PWD\" | sed 's![/.]!-!g')\n\
  LATEST=$(ls -t \"$HOME/.claude/projects/$RSLUG\"/*.jsonl 2>/dev/null | head -1)\n\
  [ -n \"$LATEST\" ] && TSID=$(basename \"$LATEST\" .jsonl)\n\
  case \"$TSID\" in ????????-????-????-????-????????????) ;; *) TSID=\"\" ;; esac\n\
fi ;;\n\
esac\n\
# 사용자 주도 resume 마커 — statusline 의 ⑂bg 배지가 anchor 불일치 휴리스틱이라\n\
# resume 세션 전부에 오발화한다(사용자). id 있으면 그 sid 를, 피커/continue 는 플래그를\n\
# export 해 statusline 이 포크/attach 뷰(마커 없음)와 구분하게 한다. anchor\n\
# (KASATERM_SESSION_ID) 자체는 state.rs 캐릭터 복원이 원본을 요구해 안 덮는다.\n\
[ -n \"$TSID\" ] && export KASATERM_RESUMED_SID=\"$TSID\"\n\
case \" $* \" in *\" --resume \"*|*\" -r \"*|*\" --continue \"*|*\" -c \"*) [ -z \"$TSID\" ] && export KASATERM_RESUME_PICKER=1 ;; esac\n\
# resume/명시 sid 부팅 — pane 상속 캐릭터 대신 그 세션의 정본(바인딩) 캐릭터로 정체성 교정\n\
# 이름과 지침을 한 응답으로 받는다. 실패하면 낡은 pane env 로 실행하지 않는다.\n\
if [ -n \"$PERSONA_OK\" ] && [ -n \"$KASATERM_PANE_ID\" ]; then\n\
{identity_block}\
fi\n\
if [ -n \"$PERSONA_OK\" ] && [ -n \"$KASATERM_PANE_ID\" ] && [ -n \"$KASATERM_CHARACTER\" ]; then\n\
  case \" $* \" in *\" --agent-id \"*|*\" --agent-name \"*|*\" --team-name \"*) : ;; *)\n\
    # 앱이 학생을 앉히는 그 순간에 같은 함수로 계산해 내려 준 값이 정본이다.\n\
    # 아래 표는 앱 **부팅 시점 스냅샷**이라, 그 뒤 테마를 바꾸거나 명단 밖 학생이\n\
    # 앉으면 이름이 표에 없어 통째로 안 붙었다 — 그러면 claude 가 폴더 이름으로\n\
    # 지은 이름이 남아, 미리 알려 준 이름으로 말을 걸면 「그런 이름 없다」가 된다.\n\
    AGENT=\"$KASATERM_AGENT_SLUG\"\n\
    if [ -z \"$AGENT\" ]; then\n\
      case \"$KASATERM_CHARACTER\" in\n\
{team_arms}\
      esac\n\
    fi\n\
  ;; esac\n\
fi\n\
if [ -n \"$AGENT\" ]; then\n\
  TEAM=$(curl -s --max-time 2 --get --data-urlencode \"cwd=$PWD\" \"http://127.0.0.1:${{KASASPACE_MCP_PORT:-8765}}/teamname\" 2>/dev/null)\n\
  if [ -n \"$TEAM\" ]; then\n\
    AGENT=\"$AGENT-p${{KASATERM_PANE_ID#%}}${{KASATERM_AGENT_SUFFIX}}\"\n\
    export KASATERM_TEAM=\"$TEAM\" KASATERM_AGENT=\"$AGENT\"\n\
    # 트리플(--agent-id/--agent-name/--team-name) 대신 세션 이름만 준다. 트리플을 붙이면\n\
    # claude 가 이 세션을 cross-session 명부에서 통째로 제외해(등록 함수 첫 줄이\n\
    # `if(W4()!=null) return false`, W4()=--agent-id) 다른 방 pane 과 서로 못 찾는다.\n\
    # 이름만 주면 방(cwd) 경계 없이 ListAgents→SendMessage 가 닿는다(08-09 실측).\n\
    export CLAUDE_CODE_SESSION_NAME=\"$AGENT\"\n\
  fi\n\
fi\n"
    );
    let wrapper = format!("#!/bin/sh\n\
# kasaterm pane-only claude wrapper — injects the collab hooks session-scoped\n\
# (--settings) so ~/.claude/settings.json stays untouched. Outside a pane this\n\
# wrapper isn't on PATH and claude runs exactly as the user configured it.\n\
HOOKS_DIR=\"{hd}\"\n\
SELF_DIR=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd)\n\
CLEAN_PATH=$(printf '%s' \"$PATH\" | tr ':' '\\n' | grep -vxF \"$SELF_DIR\" | grep -vE '(^|/)kasaterm-shim-[0-9]+/?$' | paste -sd: -)\n\
REAL=$(PATH=\"$CLEAN_PATH\" command -v claude 2>/dev/null)\n\
# 같은 디렉터리가 다른 표기로 PATH 에 남으면 위 grep 이 못 지운다 — Git Bash 는\n\
# /tmp 를 AppData/Local/Temp 의 별칭으로 두어, PATH 엔 `/tmp/kasaterm-shim-N` 이\n\
# 들어 있는데 $0 로 잰 SELF_DIR 은 `/c/Users/.../Temp/kasaterm-shim-N` 이다. 그러면\n\
# REAL 이 이 스크립트 자신이 되어 무한 재귀한다(2026-09-01 Windows 실측: --version\n\
# 조차 안 끝났다). 문자열이 달라도 같은 파일인지로 한 번 더 거른다.\n\
while [ -n \"$REAL\" ] && [ \"$REAL\" -ef \"$0\" ]; do\n\
  CLEAN_PATH=$(printf '%s' \"$CLEAN_PATH\" | tr ':' '\\n' | grep -vxF \"$(dirname -- \"$REAL\")\" | paste -sd: -)\n\
  REAL=$(PATH=\"$CLEAN_PATH\" command -v claude 2>/dev/null)\n\
done\n\
if [ -z \"$REAL\" ]; then\n\
  echo \"kasaterm claude shim: real claude not found on PATH\" >&2\n\
  exit 127\n\
fi\n\
# `claude kimi` ≡ `kimi claude` — 첫 인자가 모델 이름이면 런처로 넘긴다. 런처는\n\
# 게이트웨이 env 를 얹고 다시 PATH 의 claude(=이 shim)를 부르므로, 그때는 첫 인자가\n\
# --model 이라 여기 안 걸리고 훅·페르소나 주입이 정상으로 걸린다.\n\
case \"$1\" in\n\
  kimi|glm|agy) command -v kasa-ai >/dev/null 2>&1 && exec kasa-ai claude \"$@\" ;;\n\
esac\n\
# 낱말 스위치는 없다 — 기계 오가기는 별도 셰임 `to` 하나다(2026-09-07). 옛\n\
# `claude mini`·`local`·`classic`·`noflicker`·`tasks` 는 전부 걷었다.\n\
{ablk}\
# 세션끼리 서로를 찾게 한다(ListAgents → SendMessage). 게이트는 서버 플래그\n\
# tengu_harbor_kite 이거나 이 env 인데, 08-09 확인 시점엔 그 플래그가 이미 켜져 있었다\n\
# — 그러니 이 줄은 지금 당장 필요한 것이 아니라 플래그가 회수돼도 pane 통신이 안 끊기게\n\
# 하는 잠금장치다. 켜지면 세션이 ~/.claude/sessions/<pid>.json 에 등록되고\n\
# /tmp/cc-socks/<pid>.sock 로 오간다. (아침에 ListAgents 가 비었던 건 플래그가 아니라\n\
# 트리플 때문이었다 — 명부 등록이 --agent-id 있는 세션을 거부한다.)\n\
export CLAUDE_CODE_HARBOR_KITE=1\n\
SETTINGS=\"$SELF_DIR/claude-hooks-settings.json\"\n\
# 백엔드가 이 pane 에 심은 캐릭터 정체성 적용(사용자): persona = 시스템프롬프트 prefix(캐시,\n\
# per-turn 0), session-id = transcript 파일명 고정. 사용자가 --session-id/--resume 를\n\
# 직접 주면 그게 우선(우리 건 생략). --settings 도 사용자 지정이면 우리 걸 안 얹는다.\n\
USER_SETTINGS=0\n\
for a in \"$@\"; do [ \"$a\" = \"--settings\" ] && USER_SETTINGS=1 && break; done\n\
PERSONA_OK=1\n\
BGSUF=\"\"\n\
# attach/agents 는 서브커맨드, -p/--print 는 헤드리스 일회성 — persona·session-id 얹으면 깨진다(사용자: 이어받기\n\
# 안 붙던 원인 · Bash 도구의 claude -p 가 pane session-id 강탈→board 가 그 pane 을 학생으로 둔갑·⑂bg 오발화).\n\
# --bg 는 session-id 를 자기가 관리(명시 지정은 무시+경고 실측)하지만 persona 는 새 세션이라 붙이고,\n\
# --agent-* 트리플은 데몬 스폰까지 전달된다(07-16 실측) — 이름 접미사만 랜덤 BGSUF(비-hex, bridge 매칭 회피).\n\
case \" $* \" in *\" attach \"*|*\" agents \"*|*\" -p \"*|*\" --print \"*) SID=\"\"; PERSONA_OK=\"\" ;; *\" --bg \"*|*\" --background \"*) SID=\"\"; BGSUF=$(od -An -N2 -tx1 /dev/urandom | tr -d ' \\n' | tr '0123456789abcdef' 'ghjkmnpqrstvwxyz') ;; *\" --session-id \"*|*\" --resume \"*|*\" -r \"*|*\" --continue \"*|*\" -c \"*) SID=\"\" ;; *) SID=\"$KASATERM_SESSION_ID\" ;; esac\n\
# stop/logs 도 세션 지정 서브커맨드 — session-id/persona/트리플을 얹으면 claude 가 서브커맨드를\n\
# 프롬프트 positional 로 소비해 유령 세션 부팅/\"already in use\"(실측 07-16). $1 정확 일치는\n\
# zshrc claude() 알리아스(--dangerously-skip-permissions prepend)에 깨진다(실측) — 첫 non-flag\n\
# 인자로 판정. 값 받는 플래그가 stop/logs 앞에 오는 조합은 비현실적이라 허용 리스크.\n\
SUB=\"\"; for a in \"$@\"; do case \"$a\" in -*) ;; *) SUB=\"$a\"; break ;; esac; done\n\
# 서브커맨드에는 노브를 하나도 얹지 않는다 — `--mcp-config` 처럼 대화형 실행에만 있는 플래그를\n\
# 붙이면 `claude mcp list` 가 통째로 `unknown option` 으로 죽는다(2026-08-03 실사고). 첫 non-flag\n\
# 인자가 **정확히** 이 이름일 때만이라 프롬프트(`claude \"mcp 어떻게 써\"`)는 안 걸린다.\n\
# claude 가 새 서브커맨드를 내면 여기 추가할 것.\n\
case \"$SUB\" in stop|logs|mcp|config|configuration|doctor|update|upgrade|install|installation|plugin|plugins|project|auth|gateway|setup-token|auto-mode|ultrareview|migrate-installer) SID=\"\"; PERSONA_OK=\"\" ;; esac\n\
# 학생 명령(`시로코`)의 pane 별 정체성 override — env 는 셸 spawn 시 고정이라 재배정은\n\
# 파일로 온다. 있으면 persona/character 를 덮는다(빈 파일 = persona 없는 학생 = 미적용).\n\
OVP=\"$SELF_DIR/repersona-${{KASATERM_PANE_ID}}.persona\"\n\
if [ -n \"$KASATERM_PANE_ID\" ] && [ -f \"$OVP\" ]; then\n\
  KASATERM_PERSONA=$(cat \"$OVP\")\n\
  [ -f \"${{OVP%.persona}}.character\" ] && export KASATERM_CHARACTER=\"$(cat \"${{OVP%.persona}}.character\")\"\n\
  # 모델·통로도 새 학생 것으로 갈아탄다 — 안 그러면 이름과 얼굴만 바뀌고 앞
  # 학생의 모델로 계속 돈다. 파일이 있으면 빈 내용도 존중한다(= 지정 없음 →\n\
  # 전역 기본). 재배정을 안 한 pane 은 파일이 없어 spawn 때의 env 그대로다.\n\
  [ -f \"${{OVP%.persona}}.model\" ] && KASATERM_MODEL=$(cat \"${{OVP%.persona}}.model\")\n\
  [ -f \"${{OVP%.persona}}.backend\" ] && KASATERM_BACKEND=$(cat \"${{OVP%.persona}}.backend\")\n\
  # 말 거는 이름의 로마자 머리도 새 학생 것으로 — 안 갈아 끼우면 얼굴과 말투만\n\
  # 바뀌고 이름은 앞 학생으로 남는다.\n\
  [ -f \"${{OVP%.persona}}.slug\" ] && KASATERM_AGENT_SLUG=$(cat \"${{OVP%.persona}}.slug\")\n\
fi\n\
{tblk}\
{pblk}\
{nblk}\
[ -n \"$SID\" ] && set -- --session-id \"$SID\" \"$@\"\n\
# task store(~/.claude/tasks/<id>)를 transcript session 과 같은 키로 묶는다 — 없으면 claude\n\
# 가 매 실행 임의 session-<hex8> 로 task 를 저장해 pane↔task 매핑이 끊긴다(사용자: 유즈\n\
# 업무탭 빔). SID 비면(사용자 --resume) claude 기본.\n\
# 이름은 CLAUDE_CODE_ 접두어다. CLAUDE_TASK_LIST_ID 로 주면 claude 가 안 읽고, 그러면\n\
# 목록 키가 안 잡히면서 Task 도구 자체가 세션에 안 실린다(2026-09-15 실측).\n\
[ -n \"$SID\" ] && export CLAUDE_CODE_TASK_LIST_ID=\"$SID\"\n\
{mblk}\
{permissions}\
# 폴더 신뢰 화면(「Is this a project you trust?」) 선해결 — 무인으로 띄운 학생이 거기서\n\
# 멈춘다. codex shim 의 config.toml 신뢰와 같은 자리다. 서브커맨드·-p 엔 그 화면이 없다.\n\
[ -n \"$PERSONA_OK\" ] && kasaterm-cli claude-trust \"$PWD\" >/dev/null 2>&1\n\
if [ \"$USER_SETTINGS\" = 1 ] || [ ! -f \"$SETTINGS\" ]; then\n\
  exec \"$REAL\" \"$@\"\n\
fi\n\
exec \"$REAL\" --settings \"$SETTINGS\" \"$@\"\n",
        hd = hd, tblk = team_block, pblk = persona_block, ablk = account_block, mblk = mcp_block,
        nblk = claude_mods_block(&install_claude_mods(&hooks_dir, shim_dir)),
        permissions = format!("if [ -n \"$PERSONA_OK\" ]; then\n{}fi\n", agent_preferences::permission_shell("claude", agent_preferences::permission("claude")) + ":\n"));
    let wrapper_path = shim_dir.join("claude");
    if let Err(e) = write_shim(&wrapper_path, wrapper) {
        eprintln!("[shim] write claude wrapper failed: {e}");
        return;
    }
    // `to` — 기계 사이를 cd 처럼 오간다(2026-09-07 지시 「cd·ls 처럼 하고 싶은데」,
    // 동사는 2026-09-07 지시로 `to`). `to` = ls(명부, 여기가 어디인지 *), `to <기계>`
    // = cd(그 기계 창에 셸 pane 을 세우고 이 pane 이 비춘다 — 레포를 안 건드린다), `to <기계> <명령...>`
    // 은 그 셸에서 그 명령을(to nacho codex), `to ..` 는 이 기계로 돌아오기. 옛
    // `mini`·`book` 두 낱말이 이 하나로 합쳐졌다.
    //
    // `to <기계>` 는 **이사(`migrate`)가 아니라 거울(`remote --here`)** 이다 — 처음엔
    // 이사에 물려 있어서 저쪽 레포를 맞추다 「커밋 안 한 변경」에 막혀 셸 하나도 못
    // 열었다(2026-09-07 지적 「그냥 미러링인데 왜 안닿았던거야?」). 도는 학생을
    // 통째로 옮기는 것은 Info 의 이사 메뉴가 한다.
    //
    // `..` 는 원격 셸 거울 **안에서** 치는 것이라 명령이 저쪽 기계에서 돈다 — 그래서
    // 소켓을 부르지 않고 예약 알림 마커(OSC 777)만 뱉는다. 그 pane 을 소유한 앱
    // (맥북)의 화면 펌프가 그걸 잡아 로컬 셸로 스왑한다(2026-09-02 「book 치면 다시
    // 로컬로」). 원격 연결이 raw 바이트 모드라 이 OSC 가 맥북 파서까지 그대로 오고,
    // 맥북이 이미 OSC 777 을 읽는다. 로컬 pane 에서 쳐도 앱이 「원격 아님」으로
    // 무시한다. sh 한 줄이라 셸 종류를 안 탄다.
    #[cfg(unix)]
    {
        let to = format!(
            "#!/bin/sh\n\
case \"$1\" in\n\
  \"\") exec kasaterm-cli machines ;;\n\
  ..) printf '\\033]777;notify;{};\\033\\\\'; exit 0 ;;\n\
  -h|--help)\n\
    echo 'to                    기계 목록 (여기가 어디인지 *)'\n\
    echo 'to <기계>             그 기계 창에 셸 pane 을 세우고 이 pane 이 비춘다 (레포는 안 건드림)'\n\
    echo 'to <기계> <명령...>   그 셸에서 그 명령을 (to nacho codex)'\n\
    echo 'to ..                 이 기계로 돌아오기'\n\
    exit 0 ;;\n\
esac\n\
M=$1; shift\n\
if [ $# -eq 0 ]; then\n\
  exec kasaterm-cli machines connect \"$M\" --here ${{KASATERM_PANE_ID:+\"$KASATERM_PANE_ID\"}}\n\
fi\n\
exec kasaterm-cli machines connect \"$M\" --here ${{KASATERM_PANE_ID:+\"$KASATERM_PANE_ID\"}} --run \"$*\"\n",
            crate::BRING_HOME_MARKER
        );
        if let Err(e) = write_shim(&shim_dir.join("to"), to) {
            eprintln!("[shim] write to failed: {e}");
        }
    }
    write_cmd_launcher(shim_dir, "claude");
    // kasacollab(협업 CLI)도 pane PATH 에 스테이징 — 훅이 아니라 셸/claude 가
    // 직접 부르는 명령이라 settings 주입으로는 못 싣는다. 예전엔 ~/.local/bin
    // 수동 설치(개인 설정 오염 + 정본 이동 시 무음 고장)였다.
    let py = python3_program().unwrap_or("python3");
    let collab = format!("#!/bin/sh\nexec {py} -X utf8 \"{hd}/kasacollab.py\" \"$@\"\n");
    let collab_path = shim_dir.join("kasacollab");
    if let Err(e) = write_shim(&collab_path, collab) {
        eprintln!("[shim] write kasacollab wrapper failed: {e}");
        return;
    }
    write_cmd_launcher(shim_dir, "kasacollab");
    // sh 훅 아홉 개가 전부 `python3` 을 이름으로 부른다(bind-transcript·steer·
    // stop-drain·notify·auto-imgopen …). Windows 에서 그 이름은 MS Store 스텁이라
    // exit 49 로 죽고, 훅들은 죄다 `2>/dev/null || true` 라 **무음으로 통째 정지**
    // 했다 — bind·steer·drain 이 전부 안 도는 상태가 눈에 안 보였다. 훅을 하나씩
    // 고치는 대신 pane PATH 맨 앞(shim_dir)에 진짜를 가리키는 `python3` 를 놓는다.
    // 이름이 이미 맞으면(unix) 아무것도 안 만든다.
    if cfg!(windows) && py != "python3" && python3_program().is_some() {
        let bridge = format!("#!/bin/sh\nexec {py} -X utf8 \"$@\"\n");
        match write_shim(&shim_dir.join("python3"), bridge) {
            Ok(()) => write_cmd_launcher(shim_dir, "python3"),
            Err(e) => eprintln!("[shim] write python3 bridge failed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane_shims::tests::test_posix_shell;

    /// No account selected must emit *nothing*. An `export` with an empty value
    /// would not be inert — Claude Code treats a defined-but-empty
    /// `CLAUDE_SECURESTORAGE_CONFIG_DIR` as an explicit "use the unsuffixed
    /// store", which is a different code path from leaving it unset.
    #[test]
    fn no_claude_account_emits_no_shim_line() {
        assert_eq!(claude_account_export_line(None), "");
    }

    /// The guard must key on `+x` (set-ness), not the value. A child claude that
    /// inherited an explicit empty value is being told to stay on the default
    /// login; `[ -z "$VAR" ]` would read that as "unset" and hijack it.
    #[test]
    fn claude_account_line_guards_on_set_ness_not_emptiness() {
        let line = claude_account_export_line(Some(std::path::Path::new("/tmp/acct/a1")));
        assert_eq!(
            line,
            "[ -z \"${CLAUDE_SECURESTORAGE_CONFIG_DIR+x}\" ] && \
             export CLAUDE_SECURESTORAGE_CONFIG_DIR='/tmp/acct/a1'\n"
        );
        // The behaviour the string is there for, exercised through a real shell.
        let probe = format!("{line}printf %s \"${{CLAUDE_SECURESTORAGE_CONFIG_DIR-UNSET}}\"");
        let Some(sh) = test_posix_shell() else { return };
        let run = |env: Option<&str>| {
            let mut c = std::process::Command::new(&sh);
            c.arg("-c")
                .arg(&probe)
                .env_remove("CLAUDE_SECURESTORAGE_CONFIG_DIR");
            if let Some(v) = env {
                c.env("CLAUDE_SECURESTORAGE_CONFIG_DIR", v);
            }
            String::from_utf8(c.output().expect("sh").stdout).expect("utf8")
        };
        assert_eq!(
            run(None),
            "/tmp/acct/a1",
            "미설정이면 우리 계정을 심어야 한다"
        );
        assert_eq!(
            run(Some("")),
            "",
            "빈 값 = '기본 저장소' 지시라 존중해야 한다"
        );
        assert_eq!(run(Some("/other")), "/other", "명시 값이 우선이어야 한다");
    }

    /// Paths are single-quoted, so an apostrophe in a directory name would end
    /// the quote and let the rest of the path run as shell words.
    #[test]
    fn claude_account_line_escapes_a_quote_in_the_path() {
        let line = claude_account_export_line(Some(std::path::Path::new("/tmp/geo'no/a1")));
        assert!(line.ends_with("='/tmp/geo'\\''no/a1'\n"), "{line}");
    }

    #[test]
    fn rename_command_removed_only_when_it_is_ours() {
        // 트리플을 걷어내 내장 /rename 이 다시 도니, 우리가 심었던 대체 커맨드는 지운다.
        assert!(rename_cmd_is_ours(Some(&format!(
            "---\ndescription: 옛 버전\n---\n{RENAME_CMD_MARK}\n"
        ))));
        // 표식 없는 파일은 사용자 것 — 자동 정리가 남의 편집을 지우면 더 나쁘다.
        assert!(!rename_cmd_is_ours(Some(
            "---\ndescription: 내가 쓴 rename\n---\n직접 만든 커맨드\n"
        )));
        // 애초에 없으면 지울 것도 없다.
        assert!(!rename_cmd_is_ours(None));
    }

    /// 설정에 적힌 훅 스크립트가 **실제로 있는지**. 이름을 잘못 적으면 claude 는
    /// 그 훅을 조용히 건너뛴다 — 오류도 안 나고, 막으려던 것이 안 막히는데 화면은
    /// 평소와 똑같다. 가드류 훅은 그 침묵이 곧 통과라 특히 위험하다.
    #[test]
    fn every_hook_script_named_in_the_settings_exists() {
        let dir = std::env::temp_dir().join(format!("kt-shim-scripts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        install_claude_hook_shim(&dir);
        let Ok(raw) = std::fs::read(dir.join("claude-hooks-settings.json")) else {
            // collab-hooks 미해석 환경(번들 밖 CI)이면 생성 자체가 스킵된다.
            return;
        };
        let settings: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        let mut seen = 0usize;
        let mut walk = |v: &serde_json::Value| {
            let Some(c) = v.as_str() else { return };
            for tok in c.split('"') {
                if !(tok.ends_with(".py") || tok.ends_with(".sh")) {
                    continue;
                }
                // 인터프리터 앞머리(`python3 -X utf8 `)가 붙은 형태도 있어 마지막
                // 조각만 본다. 대조는 **레포의** collab-hooks 로 한다 — 설정이 가리키는
                // 것은 설치된 번들이고, 그쪽이 낡은 건 다시 구우면 되는 별개 문제다.
                // 여기서 잡으려는 것은 이름 오타 하나로 훅이 통째로 안 도는 쪽이다.
                let path = tok.rsplit(' ').next().unwrap_or(tok);
                let name = path.rsplit('/').next().unwrap_or(path);
                seen += 1;
                let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("collab-hooks")
                    .join(name);
                assert!(
                    src.is_file(),
                    "설정이 가리키는 훅 스크립트가 레포에 없다: {name}"
                );
            }
        };
        for group in [
            "SessionStart",
            "PreToolUse",
            "PostToolUse",
            "UserPromptSubmit",
            "PreCompact",
            "Stop",
            "Notification",
        ] {
            let Some(entries) = settings
                .pointer(&format!("/hooks/{group}"))
                .and_then(|v| v.as_array())
            else {
                continue;
            };
            for e in entries {
                let Some(hooks) = e.get("hooks").and_then(|v| v.as_array()) else {
                    continue;
                };
                for h in hooks {
                    if let Some(c) = h.get("command") {
                        walk(c);
                    }
                }
            }
        }
        walk(
            settings
                .pointer("/statusLine/command")
                .unwrap_or(&serde_json::Value::Null),
        );
        assert!(
            seen >= 5,
            "훅 경로를 하나도 못 읽었다 — 설정 모양이 바뀌었나 ({seen})"
        );
    }

    #[test]
    fn claude_wrapper_is_valid_sh_and_free_of_teammate_triple() {
        // 실제 생성물(팀모드 블록 포함)이 POSIX sh 로 파싱되는지 — 문자열 조립이라
        // 이스케이프 하나로 전체 pane claude 부팅이 깨질 수 있는 지점의 안전망.
        let dir = std::env::temp_dir().join(format!("kt-shim-syntax-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        install_claude_hook_shim(&dir);
        let wrapper = dir.join("claude");
        let Ok(body) = std::fs::read_to_string(&wrapper) else {
            // collab-hooks 미해석 환경(번들 밖 CI)이면 생성 자체가 스킵된다.
            return;
        };
        #[cfg(windows)]
        {
            let settings: serde_json::Value = serde_json::from_slice(
                &std::fs::read(dir.join("claude-hooks-settings.json")).unwrap(),
            )
            .unwrap();
            let session_start = settings
                .pointer("/hooks/SessionStart/0/hooks/0/command")
                .and_then(|v| v.as_str())
                .unwrap();
            let sh = hook_shell_program();
            assert!(
                std::path::Path::new(&sh).is_file(),
                "Git for Windows sh.exe must exist for Claude hooks"
            );
            assert!(
                session_start.starts_with(&format!("\"{sh}\" ")),
                "Windows hook must use the absolute sh.exe path: {session_start}"
            );
            let cmd = std::fs::read_to_string(dir.join("claude.cmd")).unwrap();
            assert!(cmd.contains("sh.exe\" \"%~dpn0\" %*"), "{cmd}");
        }
        let Some(sh) = test_posix_shell() else { return };
        let ok = std::process::Command::new(sh)
            .arg("-n")
            .arg(&wrapper)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "generated claude wrapper failed sh -n");
        // 자동 트리플은 2026-08-09 에 걷어냈다 — `--agent-id` 가 붙은 세션은 claude 가
        // cross-session 명부에서 통째로 제외해(등록 함수 첫 줄 `if(W4()!=null) return false`)
        // 방(cwd)이 다른 pane 끼리 서로 찾지도 못하고 메시지도 안 간다. 발신·수신 양쪽이
        // 다 죽으므로, 트리플이 되살아나면 그 순간 pane 간 협업이 조용히 끊긴다.
        assert!(
            !body.contains("set -- --agent-id"),
            "teammate triple came back — cross-session 명부 등록이 거부된다"
        );
        // 트리플 대신 이름만 넘기고, 명부 등록 게이트를 env 로 연다.
        assert!(
            body.contains("CLAUDE_CODE_SESSION_NAME"),
            "session name went missing — 상대가 이름으로 못 부른다"
        );
        assert!(
            body.contains("CLAUDE_CODE_HARBOR_KITE=1"),
            "harbor kite env went missing — 명부에 안 오른다"
        );
        assert!(
            body.contains("KASATERM_AGENT="),
            "board 가 읽는 AGENT env 가 사라졌다"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn claude_wrapper_loads_the_claude_mods() {
        // 레포의 claude mod 는 폴더만 두면 모든 claude 칸에 실린다. 번들 안을 가리키면 claude 가
        // 거기에 타입 파일을 써 서명이 깨지므로 shim 자리의 복사본을 싣는다.
        let dir = std::env::temp_dir().join(format!("kt-shim-mods-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let hooks = dir.join("hooks");
        let shim = dir.join("shim");
        let real = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("collab-hooks/claude-mods/prompt-nav");
        let fake = hooks.join("claude-mods/other");
        std::fs::create_dir_all(fake.join(".claude-plugin/types/claude-code")).unwrap();
        std::fs::create_dir_all(fake.join("types")).unwrap();
        std::fs::create_dir_all(hooks.join("claude-mods/not-a-mod")).unwrap();
        std::fs::create_dir_all(&shim).unwrap();
        std::fs::write(fake.join(".claude-plugin/plugin.json"), "{}").unwrap();
        std::fs::write(fake.join(".claude-plugin/types/claude-code/index.d.ts"), "x").unwrap();
        std::fs::write(fake.join("types/index.d.ts"), "contract").unwrap();
        let status = std::process::Command::new("cp")
            .args(["-R", &real.display().to_string(), &hooks.join("claude-mods/prompt-nav").display().to_string()])
            .status();
        if !status.is_ok_and(|s| s.success()) {
            return;
        }
        assert_eq!(install_claude_mods(&hooks, &shim), vec!["other".to_string(), "prompt-nav".to_string()]);
        let copy = shim.join("claude-mods/prompt-nav");
        assert!(copy.join("hooks/register.tsx").is_file());
        assert!(copy.join("bin/transcript-items.py").is_file(), "기록 차례 도우미가 빠졌다");
        assert!(!copy.join("tests").exists(), "시험은 싣지 않는다");
        let other = shim.join("claude-mods/other");
        assert!(other.join("types/index.d.ts").is_file(), "mod 의 타입 계약은 싣는다");
        assert!(!other.join(".claude-plugin/types").exists(), "엔진이 깐 타입은 싣지 않는다");
        assert!(!shim.join("claude-mods/not-a-mod").exists(), "plugin.json 없는 폴더는 mod 가 아니다");
        let block = claude_mods_block(&["other".into(), "prompt-nav".into()]);
        assert!(block.contains("set -- --plugin-dir \"${MOD%/}\""), "--plugin-dir 줄이 빠졌다");
        assert!(block.contains("KASATERM_PROMPT_NAV_DIR="), "prompt-nav 가 상태를 쓸 자리를 모른다");
        assert!(!claude_mods_block(&["other".into()]).contains("KASATERM_PROMPT_NAV_DIR"));
        assert!(claude_mods_block(&[]).is_empty());
        // 실제 wrapper 에 실렸다면 진짜 claude 를 부르기 전이어야 한다(훅 폴더가 낡은 번들이면 안 실린다).
        install_claude_hook_shim(&shim);
        if let Ok(body) = std::fs::read_to_string(shim.join("claude")) {
            if let Some(at) = body.find("--plugin-dir") {
                assert!(at < body.find("exec \"$REAL\"").unwrap(), "--plugin-dir 가 exec 뒤에 있다");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn claude_wrapper_appends_mcp_config_after_user_args() {
        // `--mcp-config` 는 variadic 이라 **뒤따르는 non-flag 를 값으로 삼킨다** — 다른 노브들처럼
        // prepend 로 바꾸면 사용자 프롬프트가 통째로 인자에 먹혀 조용히 사라진다. 그리고 exec 이
        // 두 갈래(사용자 --settings 유무)라 분기 뒤에 붙이면 한쪽에서만 빠진다.
        let dir = std::env::temp_dir().join(format!("kt-shim-mcp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        install_claude_hook_shim(&dir);
        let Ok(body) = std::fs::read_to_string(dir.join("claude")) else {
            return;
        };
        if !body.contains("--mcp-config") {
            // 설정에서 껐거나 홈을 못 찾은 환경 — 블록 자체가 안 실린 것이라 검사할 대상이 없다.
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        assert!(
            body.contains("set -- \"$@\" --mcp-config"),
            "--mcp-config 가 prepend 로 돌아갔다 — 사용자 프롬프트를 삼킨다"
        );
        let at = body.find("--mcp-config").unwrap();
        // 닻은 **진짜 claude 를 부르는 exec**($REAL)다 — 그보다 앞의 exec(kimi 런처)는
        // claude 에 닿지 않는 갈래라 이 검사의 대상이 아니다. 아무 `exec` 나 잡으면 그 갈래가 하나 늘 때마다 헛경보가 선다.
        let exec_at = body.find("exec \"$REAL\"").unwrap();
        assert!(at < exec_at, "--mcp-config 주입이 exec 분기보다 뒤에 있다");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_collab_hooks_dir_prefers_bundle_over_env() {
        // Build a throwaway .app-shaped tree + a separate env-pointed dir so the
        // priority is proven on real filesystem state (is_dir checks).
        let base = std::env::temp_dir().join(format!("kt-hooks-prio-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let exe = base.join("Bundle.app/Contents/MacOS/kasaterm");
        let bundle_res = base.join("Bundle.app/Contents/Resources/collab-hooks");
        let env_dir = base.join("repo/collab-hooks");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "x").unwrap();
        std::fs::create_dir_all(&bundle_res).unwrap();
        std::fs::create_dir_all(&env_dir).unwrap();
        let env_str = env_dir.to_str().unwrap();

        // 1. In a bundle, Resources WINS over the env override (the leak guard).
        assert_eq!(
            resolve_collab_hooks_dir(Some(&exe), Some(env_str)).as_deref(),
            Some(bundle_res.as_path()),
            "bundle Resources must beat a leaked KASATERM_COLLAB_HOOKS_DIR",
        );

        // 1b. Windows MSI 모양(bin\kasaterm.exe + bin\collab-hooks\) — exe 옆
        //     번들이 env 를 이긴다(Resources 와 같은 leak 가드).
        let msi_exe = base.join("bin/kasaterm");
        let msi_adj = base.join("bin/collab-hooks");
        std::fs::create_dir_all(&msi_adj).unwrap();
        std::fs::write(&msi_exe, "x").unwrap();
        assert_eq!(
            resolve_collab_hooks_dir(Some(&msi_exe), Some(env_str)).as_deref(),
            Some(msi_adj.as_path()),
            "exe-adjacent collab-hooks must beat the env override",
        );

        // 2. No bundle Resources (dev exe under target/) → env override applies.
        let dev_exe = base.join("target/debug/kasaterm");
        std::fs::create_dir_all(dev_exe.parent().unwrap()).unwrap();
        std::fs::write(&dev_exe, "x").unwrap();
        assert_eq!(
            resolve_collab_hooks_dir(Some(&dev_exe), Some(env_str)).as_deref(),
            Some(env_dir.as_path()),
            "without bundle Resources, the env override should win",
        );

        // 3. Neither bundle nor env → repo dev fallback (CARGO_MANIFEST_DIR,
        //    which really exists for this crate).
        let dev_fallback = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("collab-hooks");
        assert_eq!(
            resolve_collab_hooks_dir(Some(&dev_exe), None).as_deref(),
            Some(dev_fallback.as_path()),
            "with no bundle and no env, fall back to the repo source",
        );

        // A bogus env dir that doesn't exist is ignored (falls through to dev).
        assert_eq!(
            resolve_collab_hooks_dir(Some(&dev_exe), Some("/nonexistent/kt/hooks")).as_deref(),
            Some(dev_fallback.as_path()),
            "a non-existent env dir must not be returned",
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
