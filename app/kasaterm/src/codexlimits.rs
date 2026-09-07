//! 코덱스 한도를 **코덱스에게 직접 묻는다.**
//!
//! 예전에는 rollout(대화 기록)의 `token_count` 줄에서 읽었는데, 코덱스가
//! 2026-09-05 무렵부터 그 줄에 한도를 안 싣는다(그 뒤 세션 넷을 훑어 `used_percent`
//! 가 한 건도 없었다). 기록이 말해 주지 않으니 화면도 빈칸이 됐다.
//!
//! codex CLI 는 `app-server` 라는 JSON-RPC 서버를 품고 있고, 데스크톱 앱이 쓰는
//! 그 창구에 `account/rateLimits/read` 가 있다. 인증도 헤더도 CLI 가 알아서 하므로
//! 우리가 토큰을 만질 일이 없다 — HTTP 로 직접 치던 길은 403 이었다.

use std::io::{BufRead, Write};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 한 창의 한도. `(창 길이 분, 쓴 비율, 풀리는 시각)`.
pub(crate) type Window = (u32, f32, Option<i64>);

#[derive(Clone, Default)]
pub(crate) struct CodexLimits {
    pub windows: Vec<Window>,
    /// 구독 플랜(`pro` 등). 같은 퍼센트도 플랜에 따라 뜻이 다르다.
    pub plan: Option<String>,
    /// 한도 초기화권 잔액 — 코덱스가 문자열로 준다("0"·"2").
    pub reset_credits: Option<String>,
}

#[derive(Clone)]
struct CachedLimits {
    at: Instant,
    account_id: String,
    limits: CodexLimits,
}

fn cache() -> &'static Mutex<Option<CachedLimits>> {
    static C: OnceLock<Mutex<Option<CachedLimits>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn current_account_id() -> String {
    crate::socket::read_codex_account()
}

fn cached_snapshot(cached: Option<&CachedLimits>, account_id: &str) -> Option<CodexLimits> {
    cached
        .filter(|value| value.account_id == account_id)
        .map(|value| value.limits.clone())
}

fn cached_stale(cached: Option<&CachedLimits>, account_id: &str, every: Duration) -> bool {
    cached.is_none_or(|value| value.account_id != account_id || value.at.elapsed() >= every)
}

/// 마지막으로 읽은 값. **현재 고른 계정에서 읽은 값만** 내준다.
///
/// 코덱스 계정을 바꾼 직후 옛 계정의 퍼센트를 새 이메일 옆에 그리면, 한도를 보고
/// 옮기는 기능이 정반대 답을 준다. Claude 배지가 `account_dir` 을 대조하는 것과 같은
/// 경계다.
pub(crate) fn snapshot() -> Option<CodexLimits> {
    let account_id = current_account_id();
    let cached = cache().lock().ok()?;
    cached_snapshot(cached.as_ref(), &account_id)
}

/// 다시 물을 때가 됐나. app-server 를 띄우는 값이라 자주 부를 자리가 아니다.
pub(crate) fn stale(every: Duration) -> bool {
    let account_id = current_account_id();
    cache()
        .lock()
        .ok()
        .map(|cached| cached_stale(cached.as_ref(), &account_id, every))
        .unwrap_or(false)
}

/// 로그인 완료 직후에는 같은 계정이어도 옛 한도를 버리고 바로 다시 묻는다.
pub(crate) fn invalidate() {
    if let Ok(mut cached) = cache().lock() {
        *cached = None;
    }
}

/// 코덱스에게 물어 캐시를 채운다. **블로킹이라 폴러 스레드에서만 부른다.**
pub(crate) fn refresh() -> bool {
    let account_id = current_account_id();
    let account_home = crate::socket::codex_account_dir(&account_id);
    let Some(limits) = ask(account_home.as_deref()) else {
        return false;
    };
    if let Ok(mut g) = cache().lock() {
        *g = Some(CachedLimits {
            at: Instant::now(),
            account_id,
            limits,
        });
    }
    true
}

fn app_server_command(slot: bool) -> &'static str {
    if slot {
        "codex app-server -c 'cli_auth_credentials_store=\"file\"'"
    } else {
        "codex app-server"
    }
}

fn ask(account_home: Option<&std::path::Path>) -> Option<CodexLimits> {
    // 로그인 셸을 거치는 이유는 auth_probe 와 같다 — Finder 로 뜬 .app 의 PATH 에는
    // codex 가 없어 직접 spawn 하면 늘 실패한다.
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let mut command = crate::proc::command(shell);
    command.arg("-lc").arg(app_server_command(account_home.is_some()));
    match account_home {
        // `auth.json` 은 CODEX_HOME 아래에 있다는 Codex의 공식 계약을 그대로 쓴다.
        // 설정에서 고른 슬롯을 여기에도 주지 않으면 하단바만 늘 기본 로그인의
        // 한도를 읽고, 실제 pane 은 선택한 계정으로 뜨는 두 세계가 된다.
        Some(home) => {
            command.env("CODEX_HOME", home);
        }
        None => {
            // kasaterm 을 Codex pane 안에서 띄웠더라도 기본 로그인은 ~/.codex 여야 한다.
            command.env_remove("CODEX_HOME");
        }
    }
    let mut child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    {
        let stdin = child.stdin.as_mut()?;
        // initialize 를 먼저 보내지 않으면 그 뒤 요청이 통째로 거부된다.
        let _ = writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"clientInfo":{{"name":"kasaterm","title":"kasaterm","version":"{}"}}}}}}"#,
            env!("CARGO_PKG_VERSION")
        );
        let _ = writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":2,"method":"account/rateLimits/read","params":{{}}}}"#
        );
        let _ = stdin.flush();
    }
    let stdout = child.stdout.take()?;
    let mut out = None;
    // 응답 사이에 알림(remoteControl/status 등)이 섞여 오므로 id 로 고른다.
    for line in std::io::BufReader::new(stdout)
        .lines()
        .map_while(Result::ok)
    {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if v.get("id").and_then(|x| x.as_i64()) == Some(2) {
            out = v.get("result").cloned();
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    parse(&out?)
}

fn parse(result: &serde_json::Value) -> Option<CodexLimits> {
    let rl = result.get("rateLimits")?;
    let mut windows = Vec::new();
    for key in ["primary", "secondary"] {
        let Some(w) = rl.get(key).filter(|v| !v.is_null()) else {
            continue;
        };
        let pct = w.get("usedPercent").and_then(|v| v.as_f64())? as f32;
        let mins = w
            .get("windowDurationMins")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        if mins == 0 {
            continue;
        }
        windows.push((mins, pct, w.get("resetsAt").and_then(|v| v.as_i64())));
    }
    // 짧은 창이 왼쪽 — 「지금 당장」이 먼저 읽혀야 한다.
    windows.sort_by_key(|(m, _, _)| *m);
    Some(CodexLimits {
        windows,
        plan: rl
            .get("planType")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        reset_credits: rl
            .pointer("/credits/balance")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_slots_read_the_same_file_auth_that_login_writes() {
        assert_eq!(app_server_command(false), "codex app-server");
        assert!(app_server_command(true).contains("cli_auth_credentials_store=\"file\""));
    }

    #[test]
    fn 다른_계정의_캐시는_보이지_않고_즉시_낡는다() {
        let cached = CachedLimits {
            at: Instant::now(),
            account_id: "codex-1".to_string(),
            limits: CodexLimits::default(),
        };
        assert!(cached_snapshot(Some(&cached), "codex-1").is_some());
        assert!(cached_snapshot(Some(&cached), "codex-2").is_none());
        assert!(!cached_stale(
            Some(&cached),
            "codex-1",
            Duration::from_secs(300)
        ));
        assert!(cached_stale(
            Some(&cached),
            "codex-2",
            Duration::from_secs(300)
        ));
    }

    /// 실측 응답(2026-09-07). 창을 짧은 것부터 담고, 플랜과 초기화권도 함께 든다.
    #[test]
    fn 실측_응답을_창으로_읽는다() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"rateLimits":{"limitId":"codex","primary":{"usedPercent":37.5,"windowDurationMins":10080,"resetsAt":1789376333},"secondary":{"usedPercent":4.0,"windowDurationMins":300,"resetsAt":1788789533},"credits":{"hasCredits":false,"unlimited":false,"balance":"2"},"planType":"pro"}}"#,
        )
        .unwrap();
        let got = parse(&v).expect("읽혀야 한다");
        assert_eq!(
            got.windows,
            vec![
                (300, 4.0, Some(1788789533)),
                (10080, 37.5, Some(1789376333))
            ],
            "5시간이 먼저"
        );
        assert_eq!(got.plan.as_deref(), Some("pro"));
        assert_eq!(got.reset_credits.as_deref(), Some("2"));
    }

    /// 창이 하나뿐이거나 null 이어도 나머지를 버리지 않는다.
    #[test]
    fn 빈_창은_건너뛴다() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"rateLimits":{"primary":{"usedPercent":0,"windowDurationMins":10080},"secondary":null,"planType":"pro"}}"#,
        )
        .unwrap();
        let got = parse(&v).unwrap();
        assert_eq!(got.windows, vec![(10080, 0.0, None)]);
        assert!(got.reset_credits.is_none());
    }
}
