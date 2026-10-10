//! 칸 shim — 칸 셸의 PATH 앞에 놓이는 래퍼·훅·보조 명령을 설치한다. 에이전트(claude·codex·agy)는
//! 이 래퍼를 지나 뜨고, 칸 env(프록시·이름 꼬리·신원)도 여기서 정한다. 하네스별 래퍼는 자식 모듈.
use super::*;

mod claude;
mod codex;
mod agy;

pub(crate) use self::agy::install_agy_hook_shim;
pub(crate) use self::codex::{codex_binary, install_codex_shim, write_codex_account_file};
pub(crate) use self::claude::{
    install_claude_hook_shim, locate_collab_hooks_dir, remove_rename_command,
};

/// 셰임 파일을 **원자적으로** 갈아 끼운다 — 같은 디렉터리의 임시 파일에 쓰고
/// 실행 권한까지 준 뒤 `rename` 으로 제자리에 놓는다.
///
/// 제자리 덮어쓰기(`fs::write`)면 안 되는 이유: 셸은 스크립트를 **읽어가며** 실행한다
/// (열어 둔 fd 의 오프셋을 들고 다음 줄을 그때그때 읽는다). 실행 중인 파일을 통째로
/// 덮으면 그 오프셋이 새 내용의 엉뚱한 자리를 가리켜 줄 한가운데부터 읽고 죽는다.
/// 2026-08-27 실측: 설정 화면 조작 → `regen_pane_shims` 가 claude 래퍼를 덮어썼고,
/// 하필 그때 claude 를 띄우던 pane 이 `line 92: syntax error near unexpected token
/// '|'` 로 셸만 남았다(파일 자체는 멀쩡해 `bash -n` 은 통과 — 순수한 경합이다).
///
/// `rename` 은 새 inode 를 그 이름에 얹으므로 이미 실행 중인 셸은 옛 inode 를 끝까지
/// 읽고, 다음 실행부터 새 것을 본다. "덮어쓰기만으로 이미 뜬 pane 에도 반영"이라는
/// 원래 의도는 그대로다 — 반영 시점이 실행 경계로 옮겨질 뿐이다.
pub(crate) fn write_shim_inner(path: &std::path::Path, body: &[u8], exec: bool) -> std::io::Result<()> {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("shim");
    let tmp = path.with_file_name(format!(".{name}.tmp{}", std::process::id()));
    std::fs::write(&tmp, body)?;
    #[cfg(unix)]
    if exec {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    }
    #[cfg(not(unix))]
    let _ = exec;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// 실행되는 셰임 스크립트 — 원자 교체 + `0o755`.
pub(crate) fn write_shim(path: &std::path::Path, body: impl AsRef<[u8]>) -> std::io::Result<()> {
    write_shim_inner(path, body.as_ref(), true)
}

/// 셰임이 읽기만 하는 파일(rc·hooks json·계정 경로) — 원자 교체, 실행 권한 없음.
/// 소스되는 rc 도 셸이 읽어가며 실행하므로 스크립트와 같은 경합에 걸린다.
pub(crate) fn write_shim_data(path: &std::path::Path, body: impl AsRef<[u8]>) -> std::io::Result<()> {
    write_shim_inner(path, body.as_ref(), false)
}

/// Windows 에서 sh 셰임 `name` 옆에 `.cmd` 짝을 둔다. PowerShell 은 PATH 앞쪽의 확장자 없는
/// 파일을 실행하지 않고 「연결 프로그램 선택」 창으로 열고, cmd 는 그 파일을 못 봐 셰임을
/// 건너뛰고 진짜를 띄운다. 같은 폴더에 `.cmd` 가 있으면 둘 다 그것을 먼저 고른다.
pub(crate) fn write_cmd_launcher(shim_dir: &std::path::Path, name: &str) {
    if !cfg!(windows) {
        return;
    }
    let path = shim_dir.join(format!("{name}.cmd"));
    let body = cmd_launcher_body(&hook_shell_program());
    // 설정을 바꿀 때마다 셰임을 다시 굽는데, 본문은 sh 경로뿐이라 거의 안 바뀐다. 실행 중인 배치를
    // 갈아 끼우는 rename 은 Windows 에서 막힐 수 있으니 같으면 손대지 않는다.
    if std::fs::read(&path).is_ok_and(|cur| cur == body.as_bytes()) {
        return;
    }
    if let Err(e) = write_shim(&path, body) {
        eprintln!("[shim] write {name}.cmd failed: {e}");
    }
}

/// 셰임 이름을 본문에 안 적고 `%~dpn0`(이 파일 경로에서 확장자만 뺀 것)으로 부른다 — cmd 는
/// 배치 본문을 콘솔 코드페이지로 읽어서, UTF-8 로 적은 학생 이름(`시로코`)이 깨진다.
pub(crate) fn cmd_launcher_body(sh: &str) -> String {
    format!("@echo off\r\n\"{}\" \"%~dpn0\" %*\r\n", sh.replace('/', "\\"))
}

/// `lite` 면 최소 shim 만 — rc 셋 + `kasaterm-cli`. claude 래퍼·훅·미리보기
/// 셰임은 전부 뺀다. CLI 는 이 shim dir 로만 pane PATH 에 오르므로 shim 을 통째로
/// 끄는 `shim_inject=off` 와는 다르다(그러면 lite 안에서 split/send 가 안 된다).
pub(crate) fn install_pane_shims(lite: bool) {
    // 전역 shim 스위치 OFF → shim dir 자체를 안 만든다. KASATERM_TMUX_SHIM_DIR 이 미설정
    // 이면 pty-backend(state.rs)가 PATH prepend·ZDOTDIR 를 건드리지 않아 자식 셸이 순정
    // 이 된다 — claude wrapper·imgopen·훅·프록시 배선 전무(진짜 독립). 기본 ON(하위호환).
    // install 은 부팅 1회라 이 스위치 변경은 재시작 후 적용된다.
    if !lite && !socket::read_shim_inject() {
        eprintln!("[shim] shim_inject=off — 순정 모드, pane shim 미설치");
        return;
    }
    let shim_dir = std::env::temp_dir().join(format!(
        "kasaterm{}-shim-{}",
        if lite { "-lite" } else { "" },
        std::process::id()
    ));
    if let Err(e) = std::fs::create_dir_all(&shim_dir) {
        eprintln!("[shim] mkdir {shim_dir:?} failed: {e}");
        return;
    }
    // Cross-pane RPC: stage kasaterm-cli on the child shell's PATH so it is
    // discoverable on the child shell's PATH. A pane can then run
    // `kasaterm-cli tell --raw %1 "..."` to drive a sibling pane
    // without needing to know the absolute target/debug path. Failure
    // is non-fatal — the shim already works without it.
    if let Some(cmux_src) = locate_cmux_compat_binary() {
        let cmux_name = if cfg!(windows) {
            "kasaterm-cli.exe"
        } else {
            "kasaterm-cli"
        };
        let cmux_target = shim_dir.join(cmux_name);
        let _ = std::fs::remove_file(&cmux_target);
        if let Err(e) = stage_shim(&cmux_src, &cmux_target) {
            eprintln!("[shim] stage kasaterm-cli {cmux_src:?} -> {cmux_target:?} failed: {e}");
        }
    }
    if !lite {
    // Drop `imgopen` / `mdopen` on the pane PATH so the user (or Claude) can
    // open an image viewer / markdown editor in the current workspace with
    // zero install — each just curls the host's MCP open-preview endpoint.
    install_preview_shims(&shim_dir);
    install_open_shim(&shim_dir);
    // Stage a `claude` wrapper that injects the collab hooks session-scoped
    // (`--settings`) so ~/.claude/settings.json is never modified.
    install_claude_hook_shim(&shim_dir);
    // codex 도 같은 대접 — pane 전용 CODEX_HOME 에 우리 훅·페르소나를 얹는다.
    install_codex_shim(&shim_dir);
    // agy 도 학생이 된다. 셋 중 유일하게 사용자 홈에 파일을 쓰는데(그 CLI 가
    // 에이전트를 이름으로만 찾는다), 접두 `kasaterm-` 밖은 안 건드린다.
    install_agy_hook_shim(&shim_dir);
    // pane 마다 rust-analyzer 가 하나씩 뜨던 것을 하나로 모은다(ra-multiplex 가
    // 있을 때만 — 없으면 shim 이 진짜를 그대로 exec 한다).
    install_rust_analyzer_shim(&shim_dir);
    install_tmux_swarm_shim(&shim_dir);
    // 학생 이름 자체를 명령으로(`시로코`/`shiroko`) — 이 pane 을 그 학생으로
    // 재배정하고 하네스(claude 기본, `시로코 codex`)를 띄운다. characters.json
    // 기준 부팅 1회 생성.
    install_student_shims(&shim_dir);
    // 내장 `/rename` 을 가리는 대체 커맨드. 셰임이 아니라 `~/.claude/commands` 라
    // shim_dir 을 안 받는다.
    remove_rename_command();
    }
    // Force our shim dir to the FRONT of PATH even after the user's rc
    // files run. A login+interactive zsh sources brew's zprofile, which
    // prepends /opt/homebrew/bin (the real tmux) ahead of the PATH we
    // hand the shell — so `tmux` resolves to brew's, not ours, and
    // claude teammate's `split-window` misses the shim. We point ZDOTDIR
    // (set in pty-backend) at this dir and drop thin rc files that source
    // the real ones first, then re-prepend our dir LAST in .zshrc — so
    // it wins over brew. Non-zsh shells ignore ZDOTDIR and rely on the
    // plain PATH prepend pty-backend still does.
    let write_rc = |name: &str, body: String| {
        if let Err(e) = write_shim_data(&shim_dir.join(name), body) {
            eprintln!("[shim] write rc {name} failed: {e}");
        }
    };
    write_rc(
        ".zshenv",
        "[ -f \"${HOME}/.zshenv\" ] && source \"${HOME}/.zshenv\"\n".to_string(),
    );
    write_rc(
        ".zprofile",
        "[ -f \"${HOME}/.zprofile\" ] && source \"${HOME}/.zprofile\"\n".to_string(),
    );
    // After sourcing the user's real .zshrc we (1) re-prepend our shim
    // dir to PATH so it wins over brew, and (2) install the full OSC 133
    // prompt-mark protocol (A prompt-start / B input-start / C output-start
    // / D;exit command-end). A/B wrap PS1 with zero-width (`%{..%}`) marks;
    // the `B` mark is what pty-backend sniffs to locate the editable command
    // line. C/D delimit a command block (Warp-style): preexec emits C right
    // before a command runs and marks `_kasaterm_ran`; the next precmd emits
    // D with that command's exit code and clears the mark. Gating D on the
    // preexec mark keeps a bare Enter (no preexec) from leaking a C-less D,
    // so every D pairs with a real C. `$?` is captured on precmd's FIRST line
    // — any command after it (even `[[ ]]`) clobbers it. The PS1 guard skips
    // re-wrapping a static PS1 while still re-wrapping themes that rebuild it
    // each precmd (powerlevel10k / starship). zsh-only — other shells ignore
    // ZDOTDIR and just get the PATH prepend.
    // precmd 는 OSC 9;9 로 지금 폴더도 알린다 — 다음 명령 블록이 어디서 돌았는지(`CommandBlock::cwd`)가
    // 되어, 명령 묶음 보기에서 결과 속 폴더 이름을 눌러 그리로 갈 수 있다.
    // (3) `to` 의 탭 완성 — 첫 인자는 명부 기계 이름(+`..`), 둘째부터는 명령 이름.
    // 사용자 .zshrc 가 compinit 을 안 돌렸으면 우리 덤프 파일로 조용히 돌린다
    // (2026-09-07 지시 「자동완성 되나 → 붙여줘」).
    write_rc(
        ".zshrc",
        format!(
            "[ -f \"${{HOME}}/.zshrc\" ] && source \"${{HOME}}/.zshrc\"\n\
             export PATH=\"{0}:${{PATH}}\"\n\
             _kasaterm_osc133(){{ local __ec=$?; \
             [[ -n $_kasaterm_ran ]] && {{ printf $'\\e]133;D;%d\\a' \"$__ec\"; _kasaterm_ran=; }}; \
             printf $'\\e]9;9;%s\\a' \"$PWD\"; \
             [[ \"$PS1\" == *$'\\e]133;B'* ]] && return; \
             PS1=$'%{{\\e]133;A\\a%}}'\"$PS1\"$'%{{\\e]133;B\\a%}}'; }}\n\
             _kasaterm_preexec133(){{ printf $'\\e]133;C\\a'; _kasaterm_ran=1; }}\n\
             autoload -Uz add-zsh-hook 2>/dev/null && {{ \
             add-zsh-hook precmd _kasaterm_osc133 2>/dev/null; \
             add-zsh-hook preexec _kasaterm_preexec133 2>/dev/null; }}\n\
             _kasaterm_to_complete(){{ \
             if (( CURRENT == 2 )); then local -a names; \
             names=(${{(f)\"$(kasaterm-cli machines --names 2>/dev/null)\"}} '..'); \
             _describe -t machines '기계' names; \
             else _command_names -e; fi; }}\n\
             (( $+functions[compdef] )) || {{ autoload -Uz compinit 2>/dev/null && \
             compinit -C -d \"{0}/.zcompdump\" 2>/dev/null; }}\n\
             (( $+functions[compdef] )) && compdef _kasaterm_to_complete to 2>/dev/null\n",
            shim_dir.display()
        ),
    );
    write_rc(
        ".zlogin",
        "[ -f \"${HOME}/.zlogin\" ] && source \"${HOME}/.zlogin\"\n".to_string(),
    );
    std::env::set_var("KASATERM_TMUX_SHIM_DIR", &shim_dir);
    eprintln!("[shim] pane shim dir={shim_dir:?}");
}

/// 훅·CLI 가 쓸 python3 실행 이름. Windows 엔 `python3` 이 없는 게 보통이고,
/// 있어도 대개 MS Store 스텁이라 `--version` 이 exit 49 로 죽는다 — 실물은
/// `python` 이나 py 런처다. 후보를 순서대로 때려 보고 `Python 3` 이라 답하는
/// 첫 놈을 쓴다. 프로세스 스폰이라 부팅 1회로 굳힌다.
///
/// unix 에서도 같은 순서를 도는데, 거기선 `python3` 이 첫 후보에서 바로 잡혀
/// 동작이 바뀌지 않는다.
///
/// 부르는 쪽은 반드시 `-X utf8` 을 붙인다 — Windows python 은 파이프로 내보낼
/// 때도 stdout 을 로케일 코드페이지(한국어 Windows 는 cp949)로 잡아서, 훅이
/// 뱉는 한글이 UTF-8 파서인 우리 터미널에 깨져 들어오고 cp949 에 없는 글자엔
/// UnicodeEncodeError 로 훅째 죽는다. roster JSON 처럼 파일로 쓰는 것도 같다.
pub(crate) fn python3_program() -> Option<&'static str> {
    static PROG: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();
    *PROG.get_or_init(|| {
        ["python3", "python", "py"].into_iter().find(|p| {
            proc::command(p)
                .arg("--version")
                .stdin(std::process::Stdio::null())
                .output()
                .map(|o| {
                    o.status.success() && String::from_utf8_lossy(&o.stdout).starts_with("Python 3")
                })
                .unwrap_or(false)
        })
    })
}

/// Write `imgopen` and `mdopen` into the shim dir (on the pane PATH). Each
/// resolves its argument to an absolute path and curls the host's MCP
/// open-preview endpoint, which opens a main-workspace tab. No dependency
/// beyond `curl` (ships on macOS/Linux); the port comes from
/// KASASPACE_MCP_PORT (default 8765), inherited from the host process.
/// `rust-analyzer` 를 가로채 **서버 하나를 모든 pane 이 나눠 쓰게** 한다.
///
/// pane 마다 claude 가 자기 rust-analyzer 를 띄운다 — 실측(2026-08-11) pane 7개에
/// 서버 7 + proc-macro 서버 7 = **14 프로세스가 같은 워크스페이스를 각자 인덱싱**하고
/// 있었고, 인덱싱이 돌자 CPU 119% · RSS 2.43GB 였다. claude 는 PATH 에서
/// `rust-analyzer` 를 찾고 이 shim 디렉터리가 PATH 맨 앞이라, 여기서 가로채
/// [`ra-multiplex`](https://crates.io/crates/ra-multiplex) 로 넘긴다.
///
/// 멀티플렉서가 없으면 shim 이 진짜를 그대로 exec 한다 — 없다고 LSP 를 죽이는 것보다
/// pane 마다 하나가 뜨는 편이 낫다. `claude --bare` 로 LSP 를 끄는 길도 있지만 그건
/// 훅·플러그인까지 함께 꺼서 협업(bind-transcript·statusline·페르소나)이 통째로 죽는다.
pub(crate) fn install_rust_analyzer_shim(shim_dir: &std::path::Path) {
    if cfg!(windows) {
        return;
    }
    // 진짜 경로 탐색은 claude/codex/agy shim 과 같은 방식(`CLEAN_PATH`)이다 — 자기
    // 디렉터리를 PATH 에서 빼고 찾는다.
    let body = r#"#!/bin/sh
# kasaterm rust-analyzer shim — pane 마다 서버가 하나씩 뜨는 것을 막는다.
#
# ⚠️ --server-path 에 **진짜 경로**를 넘겨야 한다. 안 주면 ra-multiplex 가 PATH 에서
#    rust-analyzer 를 찾는데 그게 이 shim 이라 자기를 무한히 다시 부른다.
SELF_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
CLEAN_PATH=$(printf '%s' "$PATH" | tr ':' '\n' | grep -vxF "$SELF_DIR" | grep -vE '(^|/)kasaterm-shim-[0-9]+/?$' | paste -sd: -)
REAL=$(PATH="$CLEAN_PATH" command -v rust-analyzer 2>/dev/null)
# 같은 디렉터리가 다른 표기로 PATH 에 남으면 위 grep 이 못 지운다 — Git Bash 는
# /tmp 를 AppData/Local/Temp 의 별칭으로 두어, PATH 엔 `/tmp/kasaterm-shim-N` 이
# 들어 있는데 $0 로 잰 SELF_DIR 은 `/c/Users/.../Temp/kasaterm-shim-N` 이다. 그러면
# REAL 이 이 스크립트 자신이 되어 무한 재귀한다(2026-09-01 Windows 실측: --version
# 조차 안 끝났다). 문자열이 달라도 같은 파일인지로 한 번 더 거른다.
while [ -n "$REAL" ] && [ "$REAL" -ef "$0" ]; do
  CLEAN_PATH=$(printf '%s' "$CLEAN_PATH" | tr ':' '\n' | grep -vxF "$(dirname -- "$REAL")" | paste -sd: -)
  REAL=$(PATH="$CLEAN_PATH" command -v rust-analyzer 2>/dev/null)
done
if [ -z "$REAL" ]; then
echo "kasaterm rust-analyzer shim: real rust-analyzer not found on PATH" >&2
exit 127
fi
MUX=$(PATH="$CLEAN_PATH" command -v ra-multiplex 2>/dev/null)
# 멀티플렉서가 없으면 진짜를 그대로 — LSP 가 죽는 것보다 pane 마다 하나가 낫다.
[ -n "$MUX" ] || exec "$REAL" "$@"
# 서버는 첫 pane 이 올린다. 자동 시작을 안 한다(실측: status 가 "Error: connect").
# 여럿이 동시에 올려도 포트를 못 잡은 쪽이 조용히 죽으므로 무해하다.
if ! "$MUX" status >/dev/null 2>&1; then
"$MUX" server >/dev/null 2>&1 &
# client 는 재시도하지 않는다 — 포트가 열릴 때까지 잠깐 기다린다(최대 5초).
i=0
while [ "$i" -lt 50 ] && ! "$MUX" status >/dev/null 2>&1; do
sleep 0.1
i=$((i+1))
done
fi
exec "$MUX" client --server-path "$REAL" "$@"
"#;
    let path = shim_dir.join("rust-analyzer");
    if let Err(e) = write_shim(&path, body) {
        eprintln!("[shim] write rust-analyzer failed: {e}");
    }
}

/// Claude Code 팀원 창을 이 앱의 pane 으로 세운다. teammateMode=tmux 인 claude 는 tmux 밖이면
/// `tmux -L claude-swarm-<pid>` 로 전용 서버를 띄워 팀원을 거기 숨기고, 사람은 그 서버에 따로
/// attach 해야 보였다(2026-09-28). 그 소켓 호출만 `kasaterm-tmux-swarm.py` 로 보내고 나머지는
/// 진짜 tmux 로 넘긴다. `$TMUX` 는 여전히 안 건다 — 그게 truecolor 를 깨서 6월에 위장을 걷었다.
pub(crate) fn install_tmux_swarm_shim(shim_dir: &std::path::Path) {
    if cfg!(windows) {
        return;
    }
    let Some(hooks) = locate_collab_hooks_dir() else {
        return;
    };
    let body = tmux_swarm_shim(&hooks.join("kasaterm-tmux-swarm.py"));
    if let Err(e) = write_shim(&shim_dir.join("tmux"), body) {
        eprintln!("[shim] write tmux failed: {e}");
    }
}

pub(crate) fn tmux_swarm_shim(script: &std::path::Path) -> String {
    r#"#!/bin/sh
# kasaterm tmux shim — Claude Code 팀원 서버(`-L claude-swarm-<pid>`)만 kasaterm pane 으로
# 옮기고 나머지는 진짜 tmux 로 넘긴다. 번역기나 python3 가 없으면 예전처럼 숨은 서버로 간다.
case "$1 $2" in
"-L claude-swarm-"*) [ -f __SCRIPT__ ] && command -v python3 >/dev/null 2>&1 && exec python3 __SCRIPT__ "$@" ;;
esac
SELF_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
CLEAN_PATH=$(printf '%s' "$PATH" | tr ':' '\n' | grep -vxF "$SELF_DIR" | grep -vE '(^|/)kasaterm-shim-[0-9]+/?$' | paste -sd: -)
REAL=$(PATH="$CLEAN_PATH" command -v tmux 2>/dev/null)
while [ -n "$REAL" ] && [ "$REAL" -ef "$0" ]; do
  CLEAN_PATH=$(printf '%s' "$CLEAN_PATH" | tr ':' '\n' | grep -vxF "$(dirname -- "$REAL")" | paste -sd: -)
  REAL=$(PATH="$CLEAN_PATH" command -v tmux 2>/dev/null)
done
if [ -z "$REAL" ]; then
  # 진짜 tmux 가 없는 기계에서도 팀원이 pane 으로 선다 — claude 는 `tmux -V` 로 있는지만 본다.
  [ "$1" = "-V" ] && { echo "tmux 3.5 (kasaterm)"; exit 0; }
  echo "tmux: command not found" >&2
  exit 127
fi
exec "$REAL" "$@"
"#
    .replace("__SCRIPT__", &shell_quote_path(&script.to_string_lossy()))
}

pub(crate) fn install_preview_shims(shim_dir: &std::path::Path) {
    // Windows shells can't run /bin/sh scripts and the pane PATH there isn't
    // a POSIX shell; skip rather than drop broken files.
    if cfg!(windows) {
        return;
    }
    // `--get --data-urlencode` lets curl build the query string with the
    // path properly percent-encoded (spaces, unicode, etc.) — no hand-rolled
    // URL escaping in sh.
    let mk = |cmd: &str, endpoint: &str| -> String {
        format!(
            "#!/bin/sh\n\
# kasaterm {cmd} — open a file in the current kasaterm workspace.\n\
if [ \"$#\" -lt 1 ]; then echo \"usage: {cmd} FILE\" >&2; exit 1; fi\n\
f=$1\n\
if command -v realpath >/dev/null 2>&1; then abs=$(realpath \"$f\"); \
else case \"$f\" in /*) abs=\"$f\";; *) abs=\"$PWD/$f\";; esac; fi\n\
port=${{KASASPACE_MCP_PORT:-8765}}\n\
curl -s --get --data-urlencode \"path=$abs\" \
--data-urlencode \"pane=${{KASATERM_PANE_ID:-}}\" \
\"http://127.0.0.1:$port/{endpoint}\" >/dev/null \
|| {{ echo \"{cmd}: failed to reach kasaterm\" >&2; exit 1; }}\n"
        )
    };
    for (name, endpoint) in [("imgopen", "open-image"), ("mdopen", "open-markdown")] {
        let path = shim_dir.join(name);
        if let Err(e) = write_shim(&path, mk(name, endpoint)) {
            eprintln!("[shim] write {name} failed: {e}");
        }
    }
}

/// pane 셸의 `open` 을 가로채는 셰임 — **http(s) 주소 하나만 넘긴 호출**을 앱의
/// `/open-url` 로 돌리고, 그 밖(`open .`·`open -a Foo`·파일)은 `/usr/bin/open` 에
/// 그대로 넘긴다. 본진(맥미니)에서 학생·CLI 가 `open https://…` 를 치면 그 기계
/// 크롬이 아니라 거울로 보는 맥북 크롬에 뜨게 하는 길이다(2026-09-02 지시). 앱에
/// 못 닿으면 종전대로 `/usr/bin/open` 이라 잃는 것이 없다. macOS 전용 — 다른 OS 는
/// `open` 이 이 뜻이 아니다.
pub(crate) fn install_open_shim(shim_dir: &std::path::Path) {
    if !cfg!(target_os = "macos") {
        return;
    }
    let body = r##"#!/bin/sh
# kasaterm open — a single http(s) URL goes to the human's browser via the app;
# everything else is /usr/bin/open untouched.
if [ "$#" -eq 1 ]; then
  case "$1" in
    http://*|https://*)
      port=${KASASPACE_MCP_PORT:-8765}
      # 폰 도착지면 앱이 임시 터널을 세우느라 수십 초 걸릴 수 있다 — 그 링크를 찍어 준다.
      out=$(curl -s -m 75 --get --data-urlencode "url=$1" \
          --data-urlencode "pane=${KASATERM_PANE_ID:-}" \
          "http://127.0.0.1:$port/open-url" 2>/dev/null)
      case "$out" in
        *'"ok":true'*)
          case "$out" in
            *'"target":"phone"'*)
              link=$(printf '%s' "$out" | sed -n 's/.*"url":"\([^"]*\)".*/\1/p')
              [ -n "$link" ] && echo "폰 쪽지로 보냈어요: $link"
              ;;
          esac
          exit 0
          ;;
      esac
      ;;
  esac
fi
exec /usr/bin/open "$@"
"##;
    if let Err(e) = write_shim(&shim_dir.join("open"), body) {
        eprintln!("[shim] write open failed: {e}");
    }
}

/// 캡처 프록시로 claude API 라우팅 — pane 별 깨끗한 대화 캡처(ccglass 방식). claude 가
/// 이 base 로 `/v1/messages` 를 보내면 kasa-mcp 프록시가 본문 messages[] 를 캡처(peek·
/// jsonl 없이 구조화 대화)하고 api.anthropic.com 으로 투명 포워드한다. MCP 서버 포트
/// (KASASPACE_MCP_PORT, pane spawn 전에 동기 설정)를 쓴다. 포트 미설정이면 빈 env →
/// claude 가 api.anthropic.com 직행(안전 폴백, 프록시 의존 안 함).
pub(crate) fn proxy_env(pane_id: &str) -> Vec<(String, String)> {
    match std::env::var("KASASPACE_MCP_PORT") {
        Ok(port) if !port.is_empty() => vec![(
            "ANTHROPIC_BASE_URL".to_string(),
            format!(
                "http://127.0.0.1:{port}/p/{}",
                pane_id.trim_start_matches('%')
            ),
        )],
        _ => Vec::new(),
    }
}

/// teammate 이름 꼬리 — **이 앱 부팅 동안 고정**인 3자 토큰(`-k7q`).
///
/// 없을 때 무슨 일이 났나: 이름이 `<슬러그>-p<pane번호>` 뿐이라 인박스 파일명이
/// `midori-p3.json` 이었고, pane 번호는 앱을 껐다 켜면 낮은 수부터 다시 쓰인다.
/// 그래서 **새 학생이 죽은 학생의 인박스를 물려받았다** — 실측 2026-08-06: `koharu-p7`
/// 에 08-04 "영문 데모 쇼케이스 브리프", `momoi-p1` 에 "폴더 정리 제외 요청" 이 배달
/// 안 된 채 남아 있었고, 그 번호에 새 학생이 뜨면 이틀 전 지시를 자기 것으로 읽는다.
/// 같은 이유로 같은 레포에 인스턴스를 둘 띄우면 두 학생이 한 파일을 나눠 먹는다.
///
/// 부팅마다 달라지면 되고 **예측 가능할 필요는 없다** — 부른 쪽은 `split` 응답이
/// 알려 주는 이름을 그대로 쓴다(`Backend::pane_agent`). 셰임과 GUI 가 같은 값을 봐야
/// 하므로 pane env(`KASATERM_AGENT_SUFFIX`)로 건네고, 슬러그와 독립이라 `--resume` 이
/// 캐릭터를 바꿔 달아도 어긋나지 않는다.
pub(crate) fn agent_name_suffix() -> String {
    static TOK: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    TOK.get_or_init(|| {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        // 초 단위 base36 뒤 3자 — 46656초(약 13시간) 주기라 재시작 간 충돌이 사실상 없다.
        let mut n = secs % 46_656;
        let d: Vec<u8> = b"0123456789abcdefghijklmnopqrstuvwxyz".to_vec();
        let mut s = [0u8; 3];
        for i in (0..3).rev() {
            s[i] = d[(n % 36) as usize];
            n /= 36;
        }
        format!("-{}", String::from_utf8_lossy(&s))
    })
    .clone()
}

/// Install the shared, fail-closed pre-exec identity resolver.
pub(crate) fn install_agent_identity_helper(shim_dir: &std::path::Path) {
    if let Err(error) = write_shim_data(
        &shim_dir.join("agent-identity.py"),
        include_str!("../collab-hooks/kasaterm-agent-identity.py"),
    ) {
        eprintln!("[shim] identity helper failed: {error}");
    }
}

pub(crate) fn identity_bootstrap_sh(harness: &str, anchor: &str) -> String {
    r#"export PATH="$SELF_DIR:$CLEAN_PATH"
# 앱은 이 번호가 프로세스 표에 살아 있는 동안 자리를 지킨다. Git Bash 의 $$ 는 MSYS 번호라
# Windows 표에 없어서 학생이 뜨자마자 자리를 잃는다 — 그 셸의 Windows pid 를 쓴다.
# -X utf8: Windows python 은 로케일(cp949)로 읽고 써서 한글 이름·경로가 깨진다.
LAUNCH_PID=$$
[ -r "/proc/$$/winpid" ] && LAUNCH_PID=$(cat "/proc/$$/winpid")
if [ -n "$KASATERM_VIA_BACKEND" ] && [ "$KASATERM_LAUNCH_OWNER" = "$SELF_DIR:$KASATERM_PANE_ID" ] && [ -d "$KASATERM_IDENTITY_DIR" ]; then
  IDENTITY="$KASATERM_IDENTITY_DIR"
else
  IDENTITY=$(KASATERM_LAUNCH_PID=$LAUNCH_PID python3 -X utf8 "$SELF_DIR/agent-identity.py" HARNESS "$SELF_DIR" "ANCHOR" "$@") || {
    # 도우미는 실패를 스스로 알리고 1 로 끝난다. 다른 코드는 python3 자체가 못 돈 것이다 — Windows 는
    # 127(없음)이나 49(MS Store 스텁)라 스텁 안내만 남고 왜 칸이 안 뜨는지 안 보였다. 실행은 그대로 막는다.
    RC=$?
    [ "$RC" -ne 1 ] && echo "kasaterm: 학생 지침을 맞추려면 Python 3 이 필요한데 python3 을 실행하지 못했어요(종료 코드 $RC). Python 3 을 설치하고 kasaterm 을 다시 켜 주세요." >&2
    exit 1
  }
fi
export KASATERM_IDENTITY_DIR="$IDENTITY"
export KASATERM_CHARACTER="$(cat "$IDENTITY/character")"
export KASATERM_PERSONA="$(cat "$IDENTITY/persona")"
export KASATERM_AGENT_SLUG="$(cat "$IDENTITY/slug")"
export KASATERM_MODEL="$(cat "$IDENTITY/model")"
export KASATERM_BACKEND="$(cat "$IDENTITY/backend")"
SID=$(cat "$IDENTITY/session_id")
[ -n "$SID" ] && export KASATERM_SESSION_ID="$SID"
export KASATERM_LAUNCH_OWNER="$SELF_DIR:$KASATERM_PANE_ID"
"#.replace("HARNESS", harness).replace("ANCHOR", anchor)
}

/// 학생 이름을 pane 명령으로 스테이징 — `시로코`(또는 슬러그 `shiroko`)를 치면
/// 그 pane 을 해당 학생으로 재배정하고 claude 를 실행한다. persona 는 override
/// 파일(`repersona-<pane>.persona`)로 claude 래퍼에 전달(env 는 셸 spawn 시
/// 고정이라 늦게 못 바꿈), GUI 상태(헤더·테두리·board 마커·세션바인딩)는
/// `/repersona` 엔드포인트가 갱신한다. 중복 허용 — 같은 학생 pane 은 색 변주
/// (theme::accent_variant)로 구분. characters.json 기준 부팅 1회 생성(다른 shim
/// 노브와 동일하게 변경은 재시작 후 적용).
pub(crate) fn install_student_shims(shim_dir: &std::path::Path) {
    // POSIX sh 스크립트 — Windows 는 Git Bash 의 sh 로 돌고, PowerShell·cmd 칸은 `.cmd` 짝으로 닿는다.
    let Some(chars) = kasa_mcp::character::characters_json() else {
        return;
    };
    let sq = |s: &str| s.replace('\'', "'\\''");
    // 「말투」 토글이 꺼져 있으면 런처도 말투를 안 싣는다 — 이름·얼굴만 갈아 끼운다.
    let persona_on = socket::read_claude_persona();
    for name in kasa_mcp::character::member_names(&chars) {
        let persona = persona_on
            .then(|| kasa_mcp::character::persona_for(&chars, &name))
            .flatten()
            .unwrap_or_default();
        // 이 학생의 모델·통로를 스크립트에 굽는다. claude shim 과 달리 여기서는
        // 학생이 이미 정해져 있으므로 값을 직접 실을 수 있다.
        let model = kasa_mcp::character::model_for(&chars, &name).unwrap_or_default();
        let backend = kasa_mcp::character::backend_for(&chars, &name).unwrap_or_default();
        let script = format!(
            "#!/bin/sh\n\
# kasaterm 학생 런처 — 이 pane 을 '{name}' 로 재배정하고 하네스 실행.\n\
SELF_DIR=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd)\n\
if [ -n \"$KASATERM_PANE_ID\" ]; then\n\
  printf '%s' '{persona_sq}' > \"$SELF_DIR/repersona-$KASATERM_PANE_ID.persona\"\n\
  printf '%s' '{name_sq}' > \"$SELF_DIR/repersona-$KASATERM_PANE_ID.character\"\n\
  printf '%s' '{model_sq}' > \"$SELF_DIR/repersona-$KASATERM_PANE_ID.model\"\n\
  printf '%s' '{backend_sq}' > \"$SELF_DIR/repersona-$KASATERM_PANE_ID.backend\"\n\
  curl -s --get --data-urlencode \"surface=$KASATERM_PANE_ID\" \\\n\
    --data-urlencode \"character={name_sq}\" \\\n\
    \"http://127.0.0.1:${{KASASPACE_MCP_PORT:-8765}}/repersona\" >/dev/null 2>&1\n\
fi\n\
# An explicit provider must win over the saved launcher default.\n\
# `{name} kimi` 는 kimi 모델로 claude 를, `{name} kimi codex` 는 kimi 로 codex 를 띄운다.\n\
# 모델 이름은 ~/.local/bin/kasa-ai 심링크(kimi·glm)로 PATH 에 있어 exec 이 닿는다.\n\
# 아는 이름일 때만 소비하므로 `{name} \"버그 고쳐\"` 같은 프롬프트 전달은 그대로다.\n\
H={preferred_agent}\n\
M=\n\
case \"$1\" in\n\
  claude|codex|agy) H=$1; shift ;;\n\
  kimi|glm) M=$1; shift; case \"$1\" in claude|codex|agy) H=$1; shift ;; esac ;;\n\
esac\n\
# 손으로 통로를 지정했으면(`{name} kimi`) 재귀 가드를 세우고 나간다 — 방금 쓴\n\
# .backend 파일 때문에 claude shim 이 런처를 한 번 더 태우는 것을 막는다.\n\
[ -n \"$M\" ] && {{ KASATERM_VIA_BACKEND=1; export KASATERM_VIA_BACKEND; exec \"$M\" \"$H\" \"$@\"; }}\n\
exec \"$H\" \"$@\"\n",
            name_sq = sq(&name),
            persona_sq = sq(&persona),
            model_sq = sq(&model),
            backend_sq = sq(&backend),
            preferred_agent = agent_preferences::preferred_agent(),
        );
        // 한글 정식 이름 + 로마자 슬러그 별칭(IME 전환 없이도 실행) 둘 다 스테이징.
        let mut cmd_names: Vec<String> = vec![name.clone()];
        if let Some(slug) = theme::character_slug(&name) {
            cmd_names.push(slug.to_string());
        }
        for cmd in cmd_names {
            let path = shim_dir.join(&cmd);
            if let Err(e) = write_shim(&path, &script) {
                eprintln!("[shim] write student shim {cmd} failed: {e}");
                continue;
            }
            write_cmd_launcher(shim_dir, &cmd);
        }
    }
}

/// Locate the kasaterm-cli binary so we can stage it on the pane PATH
/// (install_pane_shims). Env override first, then sibling of the current exe.
pub(crate) fn locate_cmux_compat_binary() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("KASATERM_CMUX_COMPAT_BIN") {
        let p = std::path::PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    for name in ["kasaterm-cli.exe", "kasaterm-cli"] {
        let c = dir.join(name);
        if c.is_file() {
            return Some(c);
        }
    }
    None
}

/// Place the shim binary at `target` so child shells can find it.
/// Symlink first, fall back to a plain copy when the platform refuses
/// (Windows without Developer Mode or admin will reject CreateSymbolicLink).
pub(crate) fn stage_shim(src: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Copy the bytes (not a symlink) so the staged helper is a
        // standalone copy in $TMPDIR, fully decoupled from the app
        // bundle. That makes a *running* app survive an in-place bundle
        // replace (rm -rf + cp during `build-app.sh --install`): the
        // already-spawned panes keep exec'ing this stable copy, and the
        // next launch re-stages a fresh copy from the new bundle. A
        // symlink would dangle the instant the bundle's inode changed,
        // breaking `tmux` split / `imgcat` mid-session. Re-copying every
        // start (caller removes the old target first) also kills any
        // stale helper from a previous build.
        std::fs::copy(src, target)?;
        std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o755))?;
        Ok(())
    }
    #[cfg(windows)]
    {
        match std::os::windows::fs::symlink_file(src, target) {
            Ok(()) => Ok(()),
            Err(_) => {
                // Symlink path failed (likely a non-admin, non-dev-mode
                // user). Copy the bytes so we still end up with a
                // working tmux.exe in the shim dir.
                std::fs::copy(src, target).map(|_| ())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// 셰임을 돌려 볼 POSIX sh — Windows 는 칸의 훅·`.cmd` 짝이 실제로 부르는 Git 의 sh.exe 다.
    pub(crate) fn test_posix_shell() -> Option<std::path::PathBuf> {
        if cfg!(windows) {
            let sh = std::path::PathBuf::from(hook_shell_program());
            return sh.is_file().then_some(sh);
        }
        Some("sh".into())
    }

    /// 신원 확인에 넘기는 pid 는 **OS 프로세스 표의 번호**여야 한다 — 앱은 그 번호가 표에 살아 있는
    /// 동안만 학생 자리를 지킨다. Git Bash 의 `$$` 는 MSYS 번호라 Windows 표에 없어서, 학생이 뜨자마자
    /// 자리가 걷혔다. 그래서 「sh 를 띄운 쪽이 받은 pid 이거나 그 자손(Git 의 sh.exe 는 진짜 bash 를
    /// 자식으로 띄우는 런처다)이고, 실행 중 표에 있다」를 실제 셸로 잰다.
    #[test]
    fn identity_bootstrap_sends_the_os_pid_of_the_launching_shell() {
        let sh = test_posix_shell().expect("Windows 칸의 셰임은 Git for Windows 의 sh.exe 로 돈다");
        let dir = std::env::temp_dir().join(format!("kt-identity-pid-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let id = dir.join("id");
        std::fs::create_dir_all(&id).unwrap();
        for key in ["character", "persona", "slug", "model", "backend", "session_id"] {
            std::fs::write(id.join(key), key).unwrap();
        }
        // 가짜 python3 — 받은 pid·인자를 적고, 시험이 그 pid 를 표에서 찾을 때까지 셸을 붙잡아 둔다.
        write_shim(
            &dir.join("python3"),
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$OUT/args\"\n\
             printf '%s' \"$KASATERM_LAUNCH_PID\" > \"$OUT/pid.tmp\" && mv \"$OUT/pid.tmp\" \"$OUT/pid\"\n\
             i=0; while [ ! -f \"$OUT/go\" ] && [ $i -lt 600 ]; do sleep 0.1; i=$((i+1)); done\n\
             printf '%s' \"$OUT/id\"\n",
        )
        .unwrap();
        // Git Bash 의 sh 에는 슬래시 경로로 건넨다 — 칸 셰임이 경로를 굽는 방식과 같다.
        let out = dir.to_string_lossy().replace('\\', "/");
        let script = format!(
            "SELF_DIR=$(cd \"{out}\" && pwd)\nCLEAN_PATH=$PATH\n{}printf '%s' \"$KASATERM_CHARACTER\"\n",
            identity_bootstrap_sh("claude", "")
        );
        let mut child = std::process::Command::new(&sh)
            .arg("-c")
            .arg(&script)
            .env("OUT", &out)
            .env_remove("KASATERM_VIA_BACKEND")
            .env_remove("KASATERM_IDENTITY_DIR")
            .env_remove("KASATERM_LAUNCH_OWNER")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let pid_file = dir.join("pid");
        let started = Instant::now();
        while !pid_file.exists() {
            if child.try_wait().unwrap().is_some() {
                let out = child.wait_with_output().unwrap();
                panic!("신원 확인 전에 셸이 끝났다: {}", String::from_utf8_lossy(&out.stderr));
            }
            assert!(started.elapsed() < Duration::from_secs(60), "가짜 python3 에 60초 동안 안 닿았다");
            std::thread::sleep(Duration::from_millis(20));
        }
        let reported: u32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
        let table = kasa_pty::fresh_process_table();
        let launcher = child.id();
        let mut cur = reported;
        let mut under_launcher = cur == launcher;
        for _ in 0..16 {
            let Some(&(_, parent, _)) = table.iter().find(|(p, _, _)| *p == cur) else { break };
            if parent == launcher {
                under_launcher = true;
            }
            if under_launcher || parent <= 1 || parent == cur {
                break;
            }
            cur = parent;
        }
        let in_table = table.iter().any(|(p, _, _)| *p == reported);
        std::fs::write(dir.join("go"), "").unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            in_table && under_launcher,
            "넘긴 pid {reported} 가 띄운 셸({launcher})의 살아 있는 OS 프로세스가 아니다 — 앱이 자리를 바로 걷는다"
        );
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "character");
        // Windows python 은 -X utf8 없이 로케일(cp949)로 읽어 한글 학생 이름과 경로가 깨진다.
        let args = std::fs::read_to_string(dir.join("args")).unwrap();
        let args: Vec<&str> = args.lines().collect();
        assert_eq!(args[..2], ["-X", "utf8"], "{args:?}");
        assert!(args[2].ends_with("agent-identity.py") && args[3] == "claude", "{args:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Python 이 없는 설치 — 신원 확인을 못 하면 하네스를 띄우지 않는다(이전 학생 말투로 뜨는 것보다
    /// 낫다). 그 대신 python3 자체가 못 돈 경우(127 없음·49 Windows MS Store 스텁)는 이유를 알린다.
    /// 도우미가 스스로 실패한 경우(1)는 도우미가 이미 알렸으니 겹쳐 말하지 않는다.
    #[test]
    fn identity_bootstrap_stops_and_explains_when_python_cannot_run() {
        let sh = test_posix_shell().expect("셰임을 돌릴 POSIX sh");
        let dir = std::env::temp_dir().join(format!("kt-identity-nopy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.to_string_lossy().replace('\\', "/");
        let script = format!(
            "SELF_DIR=$(cd \"{out}\" && pwd)\nCLEAN_PATH=$PATH\n{}echo LAUNCHED\n",
            identity_bootstrap_sh("codex", "")
        );
        for (rc, explains) in [(127, true), (49, true), (1, false)] {
            write_shim(&dir.join("python3"), format!("#!/bin/sh\necho stub-said-something >&2\nexit {rc}\n")).unwrap();
            let run = std::process::Command::new(&sh)
                .arg("-c")
                .arg(&script)
                .env_remove("KASATERM_VIA_BACKEND")
                .env_remove("KASATERM_IDENTITY_DIR")
                .output()
                .unwrap();
            let (stdout, stderr) = (String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
            assert_eq!(run.status.code(), Some(1), "rc={rc}: {stderr}");
            assert!(!stdout.contains("LAUNCHED"), "rc={rc}: 신원 없이 하네스가 떴다");
            assert_eq!(stderr.contains("Python 3 이 필요한데"), explains, "rc={rc}: {stderr}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `.cmd` 짝은 셰임 이름을 본문에 안 적고 ASCII 로만 남는다 — cmd 가 배치 본문을 콘솔 코드페이지로
    /// 읽어서 한글 이름을 적으면 깨진다.
    #[test]
    fn cmd_launcher_reaches_its_sibling_without_naming_it() {
        let body = cmd_launcher_body("C:/Program Files/Git/bin/sh.exe");
        assert_eq!(body, "@echo off\r\n\"C:\\Program Files\\Git\\bin\\sh.exe\" \"%~dpn0\" %*\r\n");
        assert!(body.is_ascii());
    }

    /// PowerShell·cmd 칸에서 셰임이 실제로 닿는지 — 한글 이름, 공백·한글 인자 그대로.
    /// PowerShell 이 확장자 없는 셰임을 고르면 「연결 프로그램」 창이 떠 매달리므로 시간 상한을 둔다.
    #[cfg(windows)]
    #[test]
    fn cmd_launcher_runs_the_sh_shim_from_cmd_and_powershell() {
        assert!(test_posix_shell().is_some(), "Windows 칸의 셰임은 Git for Windows 의 sh.exe 로 돈다");
        let dir = std::env::temp_dir().join(format!("kt-cmd-launcher-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["시로코", "probe"] {
            write_shim(&dir.join(name), "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
            write_cmd_launcher(&dir, name);
            assert!(dir.join(format!("{name}.cmd")).is_file(), "{name}.cmd 를 안 썼다");
        }
        let run = |cmd: &mut std::process::Command| -> String {
            let mut child = cmd
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let started = Instant::now();
            while child.try_wait().unwrap().is_none() {
                if started.elapsed() > Duration::from_secs(60) {
                    let _ = child.kill();
                    panic!("60초 안에 안 끝났다 — 확장자 없는 셰임이 「연결 프로그램」 창으로 갔나");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let out = child.wait_with_output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8(out.stdout).unwrap().replace("\r\n", "\n")
        };
        // Rust 는 .cmd 를 cmd.exe 로 띄우고 인자를 배치 규칙대로 감싼다 — PowerShell 이 .cmd 를 부르는 자리와 같다.
        assert_eq!(run(std::process::Command::new(dir.join("시로코.cmd")).args(["a", "b c", "한글"])), "a\nb c\n한글\n");
        let path = format!("{};{}", dir.display(), std::env::var("PATH").unwrap_or_default());
        assert_eq!(
            run(std::process::Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", "probe a 'b c'"])
                .env("PATH", &path)),
            "a\nb c\n",
            "PowerShell 이 이름으로 찾을 때 같은 폴더의 확장자 없는 셰임이 아니라 .cmd 를 골라야 한다"
        );
        assert_eq!(
            run(std::process::Command::new("cmd.exe").args(["/d", "/c", "probe a"]).env("PATH", &path)),
            "a\n",
            "cmd 는 확장자 없는 셰임을 못 보니 .cmd 가 없으면 셰임을 건너뛴다"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 셰임 교체가 **제자리 덮어쓰기가 아니라 rename** 인지 — inode 로 잰다.
    ///
    /// 이 한 줄이 회귀의 정본이다: `write_shim` 을 `fs::write` 로 되돌리면 inode 가
    /// 그대로라 즉시 실패한다. 실행 중 셸이 안전한 이유가 정확히 "옛 inode 가 살아
    /// 있어서" 이므로, 검사할 것은 파일 내용이 아니라 inode 다.
    #[cfg(unix)]
    #[test]
    fn write_shim_replaces_the_inode_instead_of_overwriting() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = std::env::temp_dir().join(format!("kasaterm-shim-inode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("claude");

        write_shim(&path, "#!/bin/sh\nexit 0\n").unwrap();
        let first = std::fs::metadata(&path).unwrap();
        assert_eq!(
            first.permissions().mode() & 0o777,
            0o755,
            "실행 권한이 안 붙었다"
        );

        write_shim(&path, "#!/bin/sh\nexit 1\n").unwrap();
        let second = std::fs::metadata(&path).unwrap();
        assert_ne!(
            first.ino(),
            second.ino(),
            "제자리 덮어쓰기다 — 실행 중 셸이 깨진다"
        );
        assert_eq!(second.permissions().mode() & 0o777, 0o755);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "#!/bin/sh\nexit 1\n"
        );

        // 임시 파일을 남기면 PATH 인 셰임 dir 에 쓰레기가 쌓인다.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .filter(|n| n != "claude")
            .collect();
        assert!(leftovers.is_empty(), "임시 파일이 남았다: {leftovers:?}");

        // 읽기 전용 파일(실행 안 되는 데이터)은 권한을 안 건드린다.
        let data = dir.join("codex-account");
        write_shim_data(&data, "x").unwrap();
        assert_eq!(
            std::fs::metadata(&data).unwrap().permissions().mode() & 0o111,
            0
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 실행 **중인** 셰임을 갈아도 그 실행이 끝까지 간다.
    ///
    /// 2026-08-27 회귀: 설정 화면을 만지면 `regen_pane_shims` 가 claude 래퍼를 제자리에
    /// 덮어썼고, 하필 그때 claude 를 띄우던 pane 이 `syntax error` 로 셸만 남았다.
    /// 셸은 열어 둔 fd 의 오프셋으로 다음 줄을 그때그때 읽으므로, 내용이 통째로 갈리면
    /// 줄 한가운데부터 읽는다.
    ///
    /// 교체 시점은 **자식이 남기는 마커를 보고** 잡는다 — 고정 `sleep` 으로 재면 테스트가
    /// 605개와 함께 도는 부하에서 자식이 스크립트를 열기도 전에 갈아치워, rename 인데도
    /// 깨진 것처럼 보인다(실측으로 그렇게 한 번 틀렸다).
    ///
    /// 대조군(`fs::write`)을 먼저 돌려 **이 환경에서 실제로 깨지는지** 확인하고, 깨질 때만
    /// rename 쪽을 판정한다 — 셸이 스크립트를 통째로 버퍼에 담는 환경이면 어느 쪽도 안
    /// 깨지므로 그때의 통과는 아무것도 증명하지 않는다.
    #[cfg(unix)]
    #[test]
    fn a_shim_being_executed_survives_a_regen() {
        let other = "#!/bin/sh\nexit 9\n";

        let run_with = |name: &str, swap: &dyn Fn(&std::path::Path)| -> String {
            let dir = std::env::temp_dir()
                .join(format!("kasaterm-shim-live-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("claude");
            let mark = dir.join("started");
            // 마커를 찍은 뒤 잠들고, 깨어나서야 나머지를 읽는다. 패딩은 셸의 읽기
            // 버퍼(BUFSIZ, 보통 1~8KB)보다 확실히 커야 한다 — 작으면 한 번에 읽혀
            // 교체가 눈에 안 띈다. 실제 claude 래퍼도 수 KB다.
            let pad: String = (0..400)
                .map(|i| format!("# padding line {i} ————————————————\n"))
                .collect();
            let script = format!(
                "#!/bin/sh\n: > {mark:?}\nsleep 0.5\n{pad}echo OK\nexit 0\n",
                mark = mark.display().to_string()
            );
            write_shim(&path, script.as_bytes()).unwrap();
            let mut child = std::process::Command::new(&path)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            // 자식이 첫 줄에 닿기까지는 기계 부하에 달렸다(검사 여섯 벌을 함께 돌리면 1.8초,
            // 빌드가 겹친 날은 10초를 넘겨 이 자리에서 떨어졌다). 그래서 시계로 재지 않고
            // **자식이 살아 있는지**로 가른다 — 표식 없이 끝났으면 셰임이 못 돈 것이니 바로
            // 실패, 살아 있으면 기다린다. 상한은 매달림을 끊는 용도일 뿐이다.
            let waited = std::time::Instant::now();
            while !mark.exists() {
                if let Some(status) = child.try_wait().unwrap() {
                    if mark.exists() {
                        break;
                    }
                    let out = child.wait_with_output().unwrap();
                    panic!("자식이 셰임을 실행하지 못했다: {status} · {}", String::from_utf8_lossy(&out.stderr).trim());
                }
                assert!(waited.elapsed() < Duration::from_secs(60), "자식이 60초 동안 셰임 첫 줄에 닿지 못했다");
                std::thread::sleep(Duration::from_millis(5));
            }
            swap(&path);
            let out = child.wait_with_output().unwrap();
            let _ = std::fs::remove_dir_all(&dir);
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };

        let control = run_with("control", &|p| {
            std::fs::write(p, other).unwrap();
        });
        if control == "OK" {
            // 이 셸은 스크립트를 통째로 읽는다 — 제자리 덮어쓰기로도 안 깨지므로
            // rename 쪽 통과가 아무것도 말해 주지 않는다. 판정을 접는다.
            return;
        }

        let atomic = run_with("atomic", &|p| {
            write_shim(p, other).unwrap();
        });
        assert_eq!(atomic, "OK", "rename 으로 갈았는데도 실행 중 셰임이 깨졌다");
    }

    /// 팀원 소켓만 번역기로, 나머지 tmux 는 진짜로. 번역기로 새면 사람이 쓰는 tmux 가 망가지고,
    /// 셰임이 자기를 진짜로 착각하면 자기를 끝없이 부른다.
    #[cfg(unix)]
    #[test]
    fn tmux_shim_routes_only_the_teammate_socket() {
        let dir = std::env::temp_dir().join(format!("kasaterm-tmux-shim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (shim, bin) = (dir.join("kasaterm-shim-1"), dir.join("bin"));
        std::fs::create_dir_all(&shim).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        let script = dir.join("kasaterm-tmux-swarm.py");
        write_shim(&shim.join("tmux"), tmux_swarm_shim(&script)).unwrap();
        for (name, tag) in [("python3", "swarm"), ("tmux", "real")] {
            write_shim(&bin.join(name), format!("#!/bin/sh\necho {tag} \"$@\"\n")).unwrap();
        }
        let path = format!("{}:{}:/usr/bin:/bin", shim.display(), bin.display());
        let run = |args: &[&str]| {
            let out = std::process::Command::new(shim.join("tmux"))
                .args(args)
                .env("PATH", &path)
                .output()
                .unwrap();
            (out.status.code(), String::from_utf8_lossy(&out.stdout).trim().to_string())
        };
        // 번역기가 없는 판(예: 스크립트 없이 구운 번들)이면 팀원이 깨지는 대신 예전 길로 간다.
        assert_eq!(run(&["-L", "claude-swarm-42", "has-session"]).1, "real -L claude-swarm-42 has-session");
        std::fs::write(&script, "").unwrap();
        let (_, out) = run(&["-L", "claude-swarm-42", "has-session"]);
        assert_eq!(out, format!("swarm {} -L claude-swarm-42 has-session", script.display()));
        assert_eq!(run(&["-L", "mine", "ls"]).1, "real -L mine ls");
        assert_eq!(run(&["-V"]).1, "real -V");
        std::fs::remove_file(bin.join("tmux")).unwrap();
        if !["/usr/bin/tmux", "/bin/tmux"].iter().any(|p| std::path::Path::new(p).exists()) {
            assert_eq!(run(&["-V"]), (Some(0), "tmux 3.5 (kasaterm)".to_string()));
            assert_eq!(run(&["ls"]).0, Some(127));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
