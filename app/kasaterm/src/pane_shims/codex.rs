//! codex 래퍼 — 칸 env 를 고정하는 `codex` shim, 계정 파일, GUI 에서 안 보이는 npm 설치 찾기.
use super::*;

/// codex 판 pane shim. pane 안에서만 계정·페르소나를 얹고 사용자 개인 설정은 안
/// 건드린다 — 수단이 셋 다르다. 전부 2026-08-05 실측 확정:
///
/// 1. **`--settings` 등가물이 없다.** 세션 스코프 주입 자리가 `CODEX_HOME` 자체다.
///    pane 별 홈을 세우고 `~/.codex` 를 심볼릭으로 미러한다
///    (세션·플러그인·스킬·캐시·인증 공유).
/// 2. **`config.toml` 만 복사한다.** codex 가 신뢰 목록을 여기 되쓰는데,
///    심볼릭이면 그 쓰기가 사용자 개인 설정으로 샌다. 매 실행 다시 복사해 안 낡는다.
/// Permission defaults stay pane-local so they never rewrite the user's config.
pub(crate) fn install_codex_shim(shim_dir: &std::path::Path) {
    install_agent_identity_helper(shim_dir);
    // Named replacements keep shell quoting separate from Rust formatting.
    let wrapper = r#"#!/bin/sh
# kasaterm pane-only codex wrapper — pane 전용 CODEX_HOME 을 세워 훅·페르소나를 얹는다.
# ~/.codex 는 읽기만 한다. pane 밖에선 이 래퍼가 PATH 에 없어 순정 codex 가 돈다.
SELF_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
CLEAN_PATH=$(printf '%s' "$PATH" | tr ':' '\n' | grep -vxF "$SELF_DIR" | grep -vE '(^|/)kasaterm-shim-[0-9]+/?$' | paste -sd: -)
REAL=$(PATH="$CLEAN_PATH" command -v codex 2>/dev/null)
# 같은 디렉터리가 다른 표기로 PATH 에 남으면 위 grep 이 못 지운다 — Git Bash 는
# /tmp 를 AppData/Local/Temp 의 별칭으로 두어, PATH 엔 `/tmp/kasaterm-shim-N` 이
# 들어 있는데 $0 로 잰 SELF_DIR 은 `/c/Users/.../Temp/kasaterm-shim-N` 이다. 그러면
# REAL 이 이 스크립트 자신이 되어 무한 재귀한다(2026-09-01 Windows 실측: --version
# 조차 안 끝났다). 문자열이 달라도 같은 파일인지로 한 번 더 거른다.
while [ -n "$REAL" ] && [ "$REAL" -ef "$0" ]; do
  CLEAN_PATH=$(printf '%s' "$CLEAN_PATH" | tr ':' '\n' | grep -vxF "$(dirname -- "$REAL")" | paste -sd: -)
  REAL=$(PATH="$CLEAN_PATH" command -v codex 2>/dev/null)
done
if [ -z "$REAL" ]; then
  echo "kasaterm codex shim: real codex not found on PATH" >&2
  exit 127
fi
# 관리 서브커맨드는 순정으로 통과 — 우리 홈을 씌우면 `login` 이 엉뚱한 자리를 보고
# `plugin add` 는 다음 실행에 사라진다(config 를 매번 새로 복사하므로). 대화 계열
# (서브커맨드 없음=TUI · exec · resume · fork · review)만 우리 홈을 쓴다.
SUB=""; for a in "$@"; do case "$a" in -*) ;; *) SUB="$a"; break ;; esac; done
case "$SUB" in
  login|logout|mcp|plugin|app|app-server|remote-control|completion|update|doctor|sandbox|debug|apply|cloud|exec-server|features|help|archive|delete|unarchive)
    exec "$REAL" "$@" ;;
esac
# Nested utility calls do not allocate another student or rewrite the parent's
# pane home. A new shell/harness run does not inherit this child-only marker.
if [ "$KASATERM_LAUNCH_OWNER" = "$SELF_DIR:$KASATERM_PANE_ID" ]; then
  exec "$REAL" "$@"
fi
SRC="$HOME/.codex"
CH="$SELF_DIR/codex-home-${KASATERM_PANE_ID:-solo}"
# macOS 에서는 실체를 /tmp 의 짧은 경로에 두고 여기엔 링크만 건다. codex 는 CODEX_HOME 을
# 실경로로 풀어 그 아래에 제어 소켓을 여는데, $TMPDIR 밑 경로는 소켓 주소 상한(104바이트)을
# 넘어 데몬이 못 뜬다(2026-09-25 실측: 132바이트, "path must be shorter than SUN_LEN").
# 이 이름을 그대로 두는 건 pane 홈을 이 경로로 찾는 쪽(rollout 결속·상태 db 수리) 때문이다.
if [ ! -e "$CH" ] && [ "$(uname)" = Darwin ]; then
  SD="/tmp/kasaterm-shim-${SELF_DIR##*kasaterm-shim-}-$(id -u)"
  mkdir -m 700 "$SD" 2>/dev/null
  if [ -O "$SD" ] && [ ! -L "$SD" ]; then
    SH="$SD/$(printf '%s' "${KASATERM_PANE_ID:-solo}" | cksum | cut -d' ' -f1)"
    mkdir -p "$SH" 2>/dev/null && ln -sfn "$SH" "$CH" 2>/dev/null
  fi
fi
mkdir -p "$CH" 2>/dev/null || exec "$REAL" "$@"
# ~/.codex 를 심볼릭으로 미러 — 세션·플러그인·스킬·캐시·인증을 원본과 공유해 pane 안
# codex 가 pane 밖 codex 와 같은 것을 본다. auth.json 도 심볼릭이라 토큰 갱신이 원본에
# 그대로 써져 로그인이 안 갈린다(실측: doctor 가 stored ChatGPT tokens=true). 매 실행
# ln -sfn 이라 pane 안에서 생긴 드리프트는 다음 실행에 원상복구된다.
for e in "$SRC"/* "$SRC"/.[!.]*; do
  [ -e "$e" ] || continue
  n=${e##*/}
  case "$n" in config.toml|hooks.json|AGENTS.md) continue ;; esac
  ln -sfn "$e" "$CH/$n" 2>/dev/null
done
# 계정 슬롯 — 이 파일 한 줄이 활성 슬롯의 auth 디렉터리다. **매 실행 읽으므로**
# 설정에서 계정을 바꾸면 이미 열려 있는 pane 도 다음 codex 부터 그 계정으로 뜬다.
# 갈아 끼우는 건 auth.json 하나뿐 — 세션·플러그인·스킬·캐시는 위 미러 그대로라
# pane 안 codex 가 pane 밖과 같은 것을 계속 본다. 빈 파일/없는 파일 = 기본 로그인.
# 아직 로그인 안 한 슬롯은 링크가 대상 없이 걸리는데, 그게 맞다: codex 는 로그인
# 필요로 보고, `codex login` 이 쓰는 순간 그 파일이 슬롯 안에 생긴다.
ACCT=$(cat "$SELF_DIR/codex-account" 2>/dev/null)
if [ -n "$ACCT" ]; then
  mkdir -p "$ACCT" 2>/dev/null
  ln -sfn "$ACCT/auth.json" "$CH/auth.json" 2>/dev/null
fi
cp "$SRC/config.toml" "$CH/config.toml" 2>/dev/null
# 디렉터리 신뢰 프롬프트 선해결 — 무인 스폰이 여기서 멈춘다("Do you trust the contents
# of this directory?", 실측). 같은 경로를 또 쓰면 TOML 중복 테이블이라 config 가 통째로
# 안 읽히므로 없을 때만 붙인다.
if [ -n "$PWD" ] && ! grep -qF "[projects.\"$PWD\"]" "$CH/config.toml" 2>/dev/null; then
  printf '\n[projects."%s"]\ntrust_level = "trusted"\n' "$PWD" >> "$CH/config.toml"
fi
# 예전 버전이 만든 pane 전용 hooks.json 은 신뢰 경고를 되살리므로 걷는다.
rm -f "$CH/hooks.json"
# 페르소나 — codex 엔 --append-system-prompt 등가물이 없어 CODEX_HOME/AGENTS.md 로 준다
# (pane 별 홈이라 격리). 사용자 전역 AGENTS.md 를 먼저 깔고 뒤에 얹는다 — 통째로 갈아치우면
# 그의 전역 지시가 pane 안에서만 조용히 사라진다. 매 실행 다시 복사라 누적되지 않는다.
OVP="$SELF_DIR/repersona-${KASATERM_PANE_ID}.persona"
if [ -n "$KASATERM_PANE_ID" ] && [ -f "$OVP" ]; then
  KASATERM_PERSONA=$(cat "$OVP")
  [ -f "${OVP%.persona}.character" ] && export KASATERM_CHARACTER="$(cat "${OVP%.persona}.character")"
fi
# KASATERM_LAUNCH_IDENTITY
INSTRUCTIONS=$(mktemp "$CH/AGENTS.md.XXXXXX") || exit 1
cp "$SRC/AGENTS.md" "$INSTRUCTIONS" 2>/dev/null || : > "$INSTRUCTIONS"
[ -n "$KASATERM_PERSONA" ] && printf '\n%s\n' "$KASATERM_PERSONA" >> "$INSTRUCTIONS"
mv "$INSTRUCTIONS" "$CH/AGENTS.md" || exit 1
export CODEX_HOME="$CH"
# KASATERM_PERMISSION_DEFAULT
# 계정 슬롯은 auth.json 하나로 갈린다. keyring/auto 저장이면 CODEX_HOME이 달라도
# OS 키링 하나를 함께 보므로, 슬롯을 쓰는 pane만 공식 file 저장 모드로 고정한다.
# 끝에 붙여 사용자가 앞에서 준 같은 키보다 이 값이 이긴다.
if [ -n "$ACCT" ]; then
  set -- "$@" -c 'cli_auth_credentials_store="file"'
fi
# codex 는 도구 셸의 env 를 shell_environment_policy 로 새로 짓는다. 사용자가 inherit="core" 를 쓰면
# KASATERM_* 가 통째로 빠져, 학생의 kasaterm-cli 가 앱 소켓을 못 찾고 board·tell·done 이 전부
# 끊긴다(2026-10-02 실측: connect to "/tmp/cmux.sock"). 이 실행의 값을 `set` 으로 못박는다.
# 값은 TOML 문자열로 감싼다 — 맨값이면 포트 같은 숫자가 정수로 읽혀 설정 전체가 거부된다.
# 페르소나는 AGENTS.md 로 이미 갔고, KEY·TOKEN·SECRET 이 든 이름은 codex 기본 제외 규칙을 따른다.
NL='
'
for n in CODEX_HOME $(env | sed -n 's/^\(KASA[A-Z0-9_]*\)=.*/\1/p' | sort -u); do
  case "$n" in KASATERM_PERSONA|*KEY*|*TOKEN*|*SECRET*) continue ;; esac
  v=$(printenv "$n") || continue
  case "$v" in *"$NL"*) continue ;; esac
  v=$(printf '%s' "$v" | sed 's/\\/\\\\/g; s/"/\\"/g')
  set -- "$@" -c "shell_environment_policy.set.$n=\"$v\""
done
# codex 는 홈마다 app-server 데몬을 띄우고(기능 daemon_auto_start) pane 이 닫혀도 남겨 둔다.
# 홈이 pane 마다 따로라 codex pane 을 열 때마다 하나씩 쌓였다(2026-09-25 실측: 도는 codex 0,
# 데몬 3). 그래서 이 codex 를 띄운 셸(pane 의 셸)이 사라지면 그 홈의 데몬을 내린다.
# codex 가 꺼질 때 내리지 않는 건 codex 의 계약 때문이다 — 화면을 나가면 「작업은 계속
# 돈다, codex resume 으로 다시 붙어라」라고 안내한다. pane 이 살아 있는 동안은 그걸 지킨다.
# 감시자는 두 번 fork 해 pane 프로세스 트리 밖(ppid 1)에 둔다 — pane 을 닫을 때 트리째
# SIGKILL 하고(terminate_local), 에이전트 판별은 셸 자손 중 가장 새 것을 codex 로 읽는다.
# 같은 pane 에서 다시 켜면 새 실행이 표식을 덮고, 옛 감시자는 그걸 보고 손을 뗀다.
# `daemon stop` 은 업데이터(pid-update-loop)를 남기므로 그 pid 파일로 따로 거둔다.
case "$(uname)" in
  Darwin|Linux)
    OWNER="$CH/kasaterm-launch.pid"
    echo $$ > "$OWNER"
    PANE_SH=$PPID
    [ "$PANE_SH" -gt 1 ] 2>/dev/null || PANE_SH=$$
    ( ( trap '' HUP INT
        while kill -0 "$PANE_SH" 2>/dev/null && [ "$(cat "$OWNER" 2>/dev/null)" = "$$" ]; do sleep 3; done
        [ "$(cat "$OWNER" 2>/dev/null)" = "$$" ] || exit 0
        "$REAL" app-server daemon stop >/dev/null 2>&1
        UP=$(sed -n 's/.*"pid":\([0-9]*\).*/\1/p' "$CH/app-server-daemon/daemon-updater.pid" 2>/dev/null)
        [ -n "$UP" ] && ps -o command= -p "$UP" 2>/dev/null | grep -q 'pid-update-loop' && kill "$UP" 2>/dev/null
      ) </dev/null >/dev/null 2>&1 & ) ;;
esac
exec "$REAL" "$@"
"#;
    let wrapper = wrapper.replace("# KASATERM_LAUNCH_IDENTITY", &identity_bootstrap_sh("codex", ""))
        .replace("# KASATERM_PERMISSION_DEFAULT", &(agent_preferences::permission_shell("codex", agent_preferences::permission("codex")) + &agent_preferences::codex_statusline_shell()));
    let wrapper_path = shim_dir.join("codex");
    if let Err(e) = write_shim(&wrapper_path, wrapper) {
        eprintln!("[shim] write codex wrapper failed: {e}");
        return;
    }
    write_codex_account_file(shim_dir);
}

/// 활성 codex 슬롯의 auth 디렉터리를 위 래퍼가 읽는 파일에 적는다(빈 줄 = 기본 로그인).
///
/// claude 는 이 정보를 shim 본문에 `export` 로 굽지만 codex 는 파일로 넘긴다 — 래퍼가
/// 값 하나 없는 정적 문자열이라 계정을 바꿀 때마다 다시 구울 이유가 없고, 파일이면
/// **이미 떠 있는 pane 도 다음 codex 실행부터** 새 계정을 본다.
pub(crate) fn write_codex_account_file(shim_dir: &std::path::Path) {
    let dir = socket::codex_account_dir(&socket::read_codex_account());
    // 슬롯 디렉터리는 여기서 만들어 둔다 — 없으면 래퍼의 auth.json 링크가 걸릴 자리가
    // 없어, `codex login` 이 그 슬롯에 토큰을 못 쓴다.
    if let Some(ref d) = dir {
        if let Err(e) = std::fs::create_dir_all(d) {
            eprintln!("[shim] codex account dir 생성 실패: {e}");
        }
    }
    let body = dir.map_or(String::new(), |d| d.display().to_string());
    if let Err(e) = write_shim_data(&shim_dir.join("codex-account"), body) {
        eprintln!("[shim] write codex-account failed: {e}");
    }
}

/// GUI 앱의 PATH에는 npm 전역 설치 폴더가 빠질 수 있다. 셸에 `codex`를 맡기면
/// Finder에서 띄운 앱만 `command not found`가 되므로, 흔한 설치 위치를 직접 찾는다.
/// `CODEX_BIN`은 패키지 매니저 밖의 설치를 위한 명시적 탈출구다.
pub(crate) fn codex_binary() -> std::path::PathBuf {
    if let Some(path) = std::env::var_os("CODEX_BIN").filter(|value| !value.is_empty()) {
        return path.into();
    }
    let name = if cfg!(windows) { "codex.cmd" } else { "codex" };
    if let Some(home) = kasa_socket::home_dir() {
        for root in [
            home.join(".npm-global/lib/node_modules/@openai/codex"),
            home.join(".local/lib/node_modules/@openai/codex"),
            home.join(".npm/lib/node_modules/@openai/codex"),
        ] {
            if let Some(path) = npm_codex_binary(&root) {
                return path;
            }
        }
        for rel in [".npm-global/bin", ".local/bin", ".bun/bin", ".npm/bin"] {
            let path = home.join(rel).join(name);
            if path.is_file() {
                return path;
            }
        }
    }
    for root in [
        std::path::PathBuf::from("/opt/homebrew/lib/node_modules/@openai/codex"),
        std::path::PathBuf::from("/usr/local/lib/node_modules/@openai/codex"),
    ] {
        if let Some(path) = npm_codex_binary(&root) {
            return path;
        }
    }
    for dir in std::env::var_os("PATH")
        .as_deref()
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
    {
        // pane shim은 다시 순정 codex를 찾는 래퍼다. GUI의 짧은 PATH에서 그 래퍼를
        // 고르면 내부 탐색도 같은 이유로 실패하므로 실제 설치 후보로 세지 않는다.
        if dir.to_string_lossy().contains("kasaterm-shim-") {
            continue;
        }
        let path = dir.join(name);
        if path.is_file() {
            return path;
        }
    }
    for dir in ["/opt/homebrew/bin", "/usr/local/bin"] {
        let path = std::path::Path::new(dir).join(name);
        if path.is_file() {
            return path;
        }
    }
    name.into()
}

pub(crate) fn npm_codex_binary(package_root: &std::path::Path) -> Option<std::path::PathBuf> {
    let relative = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "node_modules/@openai/codex-darwin-arm64/vendor/aarch64-apple-darwin/bin/codex"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "node_modules/@openai/codex-darwin-x64/vendor/x86_64-apple-darwin/bin/codex"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "node_modules/@openai/codex-linux-arm64/vendor/aarch64-unknown-linux-musl/bin/codex"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "node_modules/@openai/codex-linux-x64/vendor/x86_64-unknown-linux-musl/bin/codex"
    } else if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
        "node_modules/@openai/codex-win32-arm64/vendor/aarch64-pc-windows-msvc/codex.exe"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "node_modules/@openai/codex-win32-x64/vendor/x86_64-pc-windows-msvc/codex.exe"
    } else {
        return None;
    };
    let path = package_root.join(relative);
    path.is_file().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// codex shim 의 불변식. claude 판에서 값을 복붙하다 깨지기 쉬운 것들만 못박는다.
    #[test]
    fn codex_shim_wrapper_holds_its_invariants() {
        let dir = std::env::temp_dir().join(format!("kt-shim-codex-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        install_codex_shim(&dir);
        let body = std::fs::read_to_string(dir.join("codex")).unwrap();
        assert!(body.contains("kasaterm-shim-[0-9]+"), "exclude every older wrapper, not just this directory");
        assert!(body.contains("agent-identity.py\" codex"), "resolve resume identity before loading AGENTS.md");
        assert!(body.find("agent-identity.py\" codex").unwrap() < body.find("cp \"$SRC/AGENTS.md\"").unwrap());
        assert!(
            !body.contains("--dangerously-bypass-hook-trust"),
            "명령 승인과 무관한 훅 trust 우회는 시작 경고만 만들므로 강제하지 않는다"
        );
        assert!(
            body.contains(&agent_preferences::permission_shell("codex", agent_preferences::permission("codex"))),
            "The wrapper must use the configured permission policy"
        );
        assert!(
            body.contains("export CODEX_HOME="),
            "pane 전용 홈을 안 내보내면 계정과 페르소나가 안 붙는다"
        );
        assert!(
            body.contains("cp \"$SRC/config.toml\""),
            "config 는 반드시 복사 — 심볼릭이면 codex 의 신뢰 상태 쓰기가 사용자 개인 설정으로 샌다"
        );
        assert!(
            body.contains("trust_level = \\\"trusted\\\"") || body.contains("trust_level"),
            "디렉터리 신뢰를 선주입하지 않으면 무인 스폰이 프롬프트에서 멈춘다"
        );
        assert!(
            body.contains("grep -qF \"[projects.\\\"$PWD\\\"]\""),
            "중복 [projects] 테이블은 TOML 파싱을 통째로 깬다 — 없을 때만 붙여야 한다"
        );
        assert!(
            body.contains("rm -f \"$CH/hooks.json\""),
            "이전 pane 훅을 걷지 않으면 다음 실행에서 신뢰 경고가 되살아난다"
        );
        assert!(
            body.contains("cli_auth_credentials_store=\"file\""),
            "계정 슬롯이 keyring을 쓰면 CODEX_HOME을 갈라도 같은 로그인을 공유한다"
        );
        assert!(
            body.contains("( ( trap '' HUP INT")
                && body.find("kasaterm-launch.pid").unwrap() < body.rfind("exec \"$REAL\" \"$@\"").unwrap(),
            "데몬 감시자가 exec 전에 pane 트리 밖에 서지 않으면 pane 을 닫을 때 같이 죽어 데몬이 남는다"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn harness_path_filter_excludes_all_inherited_app_generations() {
        let dir = std::env::temp_dir().join(format!("kt-shim-path-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        install_codex_shim(&dir);
        let body = std::fs::read_to_string(dir.join("codex")).unwrap();
        let filter = body.lines().find(|line| line.starts_with("CLEAN_PATH=")).unwrap();
        let result = std::process::Command::new("/bin/sh")
            .args(["-c", &format!("{filter}\nprintf '%s' \"$CLEAN_PATH\"")])
            .env("SELF_DIR", &dir)
            .env("PATH", format!("{}:/tmp/kasaterm-shim-11:/private/tmp/kasaterm-shim-22/:/usr/bin:/bin", dir.display()))
            .output().unwrap();
        assert!(result.status.success());
        assert_eq!(String::from_utf8(result.stdout).unwrap(), "/usr/bin:/bin");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// inherit="core" 인 사용자 설정에서도 학생의 도구 셸이 pane 신원을 보게 하는 블록.
    #[cfg(unix)]
    #[test]
    fn codex_wrapper_pins_pane_env_for_tool_shells() {
        let dir = std::env::temp_dir().join(format!("kt-shim-env-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        install_codex_shim(&dir);
        let body = std::fs::read_to_string(dir.join("codex")).unwrap();
        let start = body.find("NL='").unwrap();
        let block = &body[start..];
        let block = &block[..block.find("\ndone\n").unwrap() + 6];
        let result = std::process::Command::new("/bin/sh")
            .args(["-c", &format!("set -- resume\n{block}printf '%s\\n' \"$@\"")])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("CODEX_HOME", "/tmp/h")
            .env("KASATERM_PANE_ID", "%3")
            .env("KASATERM_CHARACTER", "시로코")
            .env("KASASPACE_MCP_PORT", "50992")
            .env("KASATERM_SOCKET_PATH", "/tmp/a \"b\"\\c.sock")
            .env("KASATERM_PERSONA", "말투\n규약")
            .env("KASATERM_KASANET_KEY", "/tmp/k")
            .output().unwrap();
        assert!(result.status.success());
        let args: Vec<String> = String::from_utf8(result.stdout).unwrap().lines().map(str::to_string).collect();
        assert_eq!(args[0], "resume", "원래 인자가 앞에 남아야 서브커맨드가 안 깨진다");
        for want in [
            "shell_environment_policy.set.CODEX_HOME=\"/tmp/h\"",
            "shell_environment_policy.set.KASATERM_PANE_ID=\"%3\"",
            "shell_environment_policy.set.KASATERM_CHARACTER=\"시로코\"",
            "shell_environment_policy.set.KASASPACE_MCP_PORT=\"50992\"",
            "shell_environment_policy.set.KASATERM_SOCKET_PATH=\"/tmp/a \\\"b\\\"\\\\c.sock\"",
        ] {
            assert!(args.iter().any(|a| a == want), "{want} 이 빠졌다: {args:?}");
        }
        assert!(!args.iter().any(|a| a.contains("PERSONA") || a.contains("KASANET_KEY")), "{args:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn codex_binary_finds_the_gui_invisible_npm_install() {
        let Some(home) = kasa_socket::home_dir() else {
            return;
        };
        let package = home.join(".npm-global/lib/node_modules/@openai/codex");
        if let Some(native) = npm_codex_binary(&package) {
            if std::env::var_os("CODEX_BIN").is_none() {
                assert_eq!(codex_binary(), native);
            }
        }
    }
}
