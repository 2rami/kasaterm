//! 코덱스 한도를 **코덱스에게 직접 묻는다.**
//!
//! 예전에는 rollout(대화 기록)의 `token_count` 줄에서 읽었는데, 코덱스가
//! 2026-09-05 무렵부터 그 줄에 한도를 안 싣는다(그 뒤 세션 넷을 훑어 `used_percent`
//! 가 한 건도 없었다). 기록이 말해 주지 않으니 화면도 빈칸이 됐다.
//!
//! codex CLI 는 `app-server` 라는 JSON-RPC 서버를 품고 있고, 데스크톱 앱이 쓰는
//! 그 창구에 `account/rateLimits/read` 가 있다. 인증도 헤더도 CLI 가 알아서 하므로
//! 우리가 토큰을 만질 일이 없다 — HTTP 로 직접 치던 길은 403 이었다.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// 한 창의 한도. `(창 길이 분, 쓴 비율, 풀리는 시각)`.
pub(crate) type Window = (u32, f32, Option<i64>);

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NamedWindow {
    /// 서버 `limitName` 원문. 모델 이름으로 추측하거나 바꾸지 않는다.
    pub name: String,
    pub minutes: u32,
    pub pct: f32,
    pub resets_at: Option<i64>,
}

#[derive(Clone, Default)]
pub(crate) struct CodexLimits {
    pub windows: Vec<Window>,
    /// `rateLimitsByLimitId`의 이름 달린 추가 bucket. 기본 하단에는 숨고 상세만 쓴다.
    pub named_windows: Vec<NamedWindow>,
    /// 마지막 조회가 실패해 이전 성공값을 보여 주는 중이다.
    pub stale: bool,
    /// 구독 플랜(`pro` 등). 같은 퍼센트도 플랜에 따라 뜻이 다르다.
    pub plan: Option<String>,
    /// 한도 초기화권 잔액 — 코덱스가 문자열로 준다("0"·"2").
    pub reset_credits: Option<String>,
}

#[derive(Clone)]
struct CachedLimits {
    at: Instant,
    /// 실패도 한 번의 조회다. `None`을 시각과 함께 기억해야 로그인 안 된 슬롯을
    /// 메뉴가 열린 5초마다 다시 띄우지 않는다.
    limits: Option<CodexLimits>,
    probe: bool,
}

fn cache() -> &'static Mutex<HashMap<String, CachedLimits>> {
    static C: OnceLock<Mutex<HashMap<String, CachedLimits>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn generation() -> &'static AtomicU64 {
    static G: AtomicU64 = AtomicU64::new(0);
    &G
}

fn current_account_id() -> String {
    crate::socket::read_codex_account()
}

fn cached_snapshot(
    cached: &HashMap<String, CachedLimits>,
    account_id: &str,
) -> Option<CodexLimits> {
    cached
        .get(account_id)
        .and_then(|value| value.limits.clone())
}

fn cached_stale(cached: &HashMap<String, CachedLimits>, account_id: &str, every: Duration) -> bool {
    cached
        .get(account_id)
        .is_none_or(|value| value.at.elapsed() >= every)
}

fn store_result(
    cached: &mut HashMap<String, CachedLimits>,
    account_id: &str,
    limits: Option<CodexLimits>,
    started_generation: u64,
    current_generation: u64,
) -> bool {
    if started_generation != current_generation {
        return false;
    }
    let now = Instant::now();
    match limits {
        Some(mut limits) => {
            limits.stale = false;
            cached.insert(
                account_id.to_string(),
                CachedLimits {
                    at: now,
                    limits: Some(limits),
                    probe: false,
                },
            );
            true
        }
        None => match cached.get_mut(account_id) {
            Some(previous) => {
                previous.at = now;
                previous.probe = false;
                let changed = previous
                    .limits
                    .as_ref()
                    .is_some_and(|limits| !limits.stale);
                if let Some(limits) = previous.limits.as_mut() {
                    limits.stale = true;
                }
                changed
            }
            None => {
                // 값이 없는 실패도 기록한다. 화면은 이를 첫 조회 전과 구분하고,
                // 폴러는 메뉴가 열려 있어도 같은 로그아웃 슬롯을 5초마다 안 띄운다.
                cached.insert(
                    account_id.to_string(),
                    CachedLimits {
                        at: now,
                        limits: None,
                        probe: false,
                    },
                );
                true
            }
        },
    }
}

/// 마지막으로 읽은 값. **현재 고른 계정에서 읽은 값만** 내준다.
///
/// 코덱스 계정을 바꾼 직후 옛 계정의 퍼센트를 새 이메일 옆에 그리면, 한도를 보고
/// 옮기는 기능이 정반대 답을 준다. Claude 배지가 `account_dir` 을 대조하는 것과 같은
/// 경계다.
pub(crate) fn snapshot() -> Option<CodexLimits> {
    let account_id = current_account_id();
    snapshot_for(&account_id)
}

/// 마지막으로 읽은 특정 계정의 값. 빈 id는 기본 `~/.codex` 로그인이다.
pub(crate) fn snapshot_for(account_id: &str) -> Option<CodexLimits> {
    let cached = cache().lock().ok()?;
    cached_snapshot(&cached, account_id)
}

/// 이 계정에 한 번이라도 물어봤나. `snapshot_for == None`만으로는 아직 조회 전과
/// 조회 실패를 가를 수 없어 설정 카드가 영원히 「확인 중」으로 남는다.
pub(crate) fn attempted_for(account_id: &str) -> bool {
    cache()
        .lock()
        .ok()
        .is_some_and(|cached| cached.contains_key(account_id))
}

/// 화면 리그가 계정별 그래프를 확인할 때만 쓰는 값 주입구.
pub(crate) fn seed_for_probe(account_id: &str, windows: Vec<Window>) -> bool {
    if std::env::var_os("KASATERM_AUTOPORTPOP_MS").is_none() {
        return false;
    }
    let Some(mut cached) = cache().lock().ok() else {
        return false;
    };
    cached.insert(
        account_id.to_string(),
        CachedLimits {
            at: Instant::now(),
            limits: Some(CodexLimits {
                windows,
                stale: std::env::var_os("KASATERM_TEST_USAGE_STALE").is_some(),
                ..Default::default()
            }),
            probe: true,
        },
    );
    true
}

/// 화면 리그가 서버 이름을 보존한 추가 bucket까지 상세 카드에 심는 입구.
pub(crate) fn seed_named_for_probe(account_id: &str, windows: Vec<NamedWindow>) -> bool {
    if std::env::var_os("KASATERM_AUTOPORTPOP_MS").is_none() {
        return false;
    }
    let Some(mut cached) = cache().lock().ok() else {
        return false;
    };
    let entry = cached
        .entry(account_id.to_string())
        .or_insert_with(|| CachedLimits {
            at: Instant::now(),
            limits: Some(CodexLimits::default()),
            probe: true,
        });
    entry.at = Instant::now();
    entry.probe = true;
    entry
        .limits
        .get_or_insert_with(CodexLimits::default)
        .named_windows = windows;
    true
}

/// 격리 화면 리그가 심은 계정은 실제 자격증명 없이도 그래프만 그릴 수 있다.
pub(crate) fn seeded_for_probe(account_id: &str) -> bool {
    std::env::var_os("KASATERM_AUTOPORTPOP_MS").is_some()
        && cache()
            .lock()
            .ok()
            .and_then(|cached| cached.get(account_id).map(|value| value.probe))
            .unwrap_or(false)
}

/// 다시 물을 때가 됐나. app-server 를 띄우는 값이라 자주 부를 자리가 아니다.
pub(crate) fn stale(every: Duration) -> bool {
    let account_id = current_account_id();
    stale_for(&account_id, every)
}

/// 특정 계정에 다시 물을 때가 됐나. 계정 메뉴의 비활성 행이 쓴다.
pub(crate) fn stale_for(account_id: &str, every: Duration) -> bool {
    cache()
        .lock()
        .ok()
        .map(|cached| cached_stale(&cached, account_id, every))
        .unwrap_or(false)
}

/// 로그인 완료 직후에는 같은 계정이어도 옛 한도를 버리고 바로 다시 묻는다.
pub(crate) fn invalidate() {
    // 먼저 세대를 올린다. 이미 app-server 응답을 기다리는 폴러가 있으면, 아래 clear
    // 뒤에 옛 값을 다시 넣지 못하고 자기 응답을 버린다.
    generation().fetch_add(1, Ordering::AcqRel);
    if let Ok(mut cached) = cache().lock() {
        cached.clear();
    }
}

/// 코덱스에게 물어 캐시를 채운다. **블로킹이라 폴러 스레드에서만 부른다.**
pub(crate) fn refresh() -> bool {
    let account_id = current_account_id();
    refresh_for(&account_id)
}

/// 특정 계정의 코덱스를 띄워 캐시를 채운다. **블로킹이라 폴러 스레드에서만 부른다.**
pub(crate) fn refresh_for(account_id: &str) -> bool {
    let started = generation().load(Ordering::Acquire);
    let account_home = crate::socket::codex_account_dir(account_id);
    let limits = ask(account_home.as_deref());
    if let Ok(mut g) = cache().lock() {
        store_result(
            &mut g,
            account_id,
            limits,
            started,
            generation().load(Ordering::Acquire),
        )
    } else {
        false
    }
}

fn app_server_args(slot: bool) -> Vec<&'static str> {
    let mut args = vec!["app-server"];
    if slot {
        args.extend(["-c", "cli_auth_credentials_store=\"file\""]);
    }
    args
}

fn ask(account_home: Option<&std::path::Path>) -> Option<CodexLimits> {
    // Finder에서 띄운 앱의 PATH에는 npm 전역 설치가 없다. 로그인과 같은 탐색기를
    // 써야 계정은 붙었는데 한도만 영영 비는 두 경로가 생기지 않는다.
    let mut command = crate::proc::command(crate::codex_binary());
    command.args(app_server_args(account_home.is_some()));
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
    // `rateLimits`는 옛 단일 창구이고 `rateLimitsByLimitId.codex`가 명시적인 공용
    // Codex bucket이다. Astra처럼 공용 한도를 쓰는 모델을 이름으로 추측하지 않고,
    // 서버가 `codex`로 분류한 값만 그린다. 별도 모델 bucket은 이 화면의 범위가 아니다.
    let rl = result
        .pointer("/rateLimitsByLimitId/codex")
        .filter(|value| value.is_object())
        .or_else(|| result.get("rateLimits").filter(|value| value.is_object()))?;
    let mut windows = Vec::new();
    for key in ["primary", "secondary"] {
        let Some(w) = rl.get(key).filter(|v| !v.is_null()) else {
            continue;
        };
        let Some(pct) = w.get("usedPercent").and_then(|v| v.as_f64()) else {
            continue;
        };
        let pct = pct as f32;
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
    let mut named_windows = Vec::new();
    if let Some(buckets) = result
        .get("rateLimitsByLimitId")
        .and_then(|value| value.as_object())
    {
        for (id, bucket) in buckets {
            if id == "codex" {
                continue;
            }
            let Some(name) = bucket
                .get("limitName")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            for key in ["primary", "secondary"] {
                let Some(window) = bucket.get(key).filter(|value| value.is_object()) else {
                    continue;
                };
                let Some(pct) = window.get("usedPercent").and_then(|value| value.as_f64()) else {
                    continue;
                };
                let minutes = window
                    .get("windowDurationMins")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0) as u32;
                if minutes == 0 {
                    continue;
                }
                named_windows.push(NamedWindow {
                    name: name.to_string(),
                    minutes,
                    pct: pct as f32,
                    resets_at: window.get("resetsAt").and_then(|value| value.as_i64()),
                });
            }
        }
    }
    named_windows.sort_by(|a, b| a.name.cmp(&b.name).then(a.minutes.cmp(&b.minutes)));
    Some(CodexLimits {
        windows,
        named_windows,
        stale: false,
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
        assert_eq!(app_server_args(false), vec!["app-server"]);
        assert!(app_server_args(true).contains(&"cli_auth_credentials_store=\"file\""));
    }

    #[test]
    fn 다른_계정의_캐시는_보이지_않고_즉시_낡는다() {
        let mut cached = HashMap::new();
        cached.insert(
            "codex-1".to_string(),
            CachedLimits {
                at: Instant::now(),
                limits: Some(CodexLimits::default()),
                probe: false,
            },
        );
        assert!(cached_snapshot(&cached, "codex-1").is_some());
        assert!(cached_snapshot(&cached, "codex-2").is_none());
        assert!(!cached_stale(&cached, "codex-1", Duration::from_secs(300)));
        assert!(cached_stale(&cached, "codex-2", Duration::from_secs(300)));
    }

    #[test]
    fn 실패한_계정도_조회_간격은_지킨다() {
        let mut cached = HashMap::new();
        cached.insert(
            "logged-out".to_string(),
            CachedLimits {
                at: Instant::now(),
                limits: None,
                probe: false,
            },
        );
        assert!(cached_snapshot(&cached, "logged-out").is_none());
        assert!(!cached_stale(
            &cached,
            "logged-out",
            Duration::from_secs(60)
        ));
    }

    #[test]
    fn 일시_실패는_다른_계정이나_빈값으로_바꾸지_않는다() {
        let mut cached = HashMap::new();
        assert!(store_result(
            &mut cached,
            "codex-1",
            Some(CodexLimits {
                windows: vec![(300, 17.0, Some(123))],
                ..Default::default()
            }),
            1,
            1,
        ));
        assert!(store_result(&mut cached, "codex-1", None, 1, 1));
        let kept = cached_snapshot(&cached, "codex-1").expect("마지막 성공값 유지");
        assert_eq!(kept.windows, vec![(300, 17.0, Some(123))]);
        assert!(kept.stale, "실패 뒤에는 낡은 값임을 표시");
        assert!(cached_snapshot(&cached, "codex-2").is_none());
    }

    #[test]
    fn 첫_실패도_조회완료로_기록한다() {
        let mut cached = HashMap::new();
        assert!(store_result(&mut cached, "logged-out", None, 1, 1));
        assert!(cached.contains_key("logged-out"));
        assert!(cached_snapshot(&cached, "logged-out").is_none());
    }

    #[test]
    fn invalidate_뒤에_도착한_응답은_버린다() {
        let mut cached = HashMap::new();
        assert!(!store_result(
            &mut cached,
            "codex-1",
            Some(CodexLimits::default()),
            4,
            5,
        ));
        assert!(cached.is_empty(), "옛 세대 응답이 다시 들어왔다");

        assert!(store_result(
            &mut cached,
            "codex-1",
            Some(CodexLimits::default()),
            5,
            5,
        ));
        assert!(cached_snapshot(&cached, "codex-1").is_some());
    }

    /// 공용 Codex는 명시적인 `codex`를 고르고, 이름 달린 추가 bucket은 서버 원문을
    /// 보존한다. 어떤 모델에 해당하는지는 추측하지 않는다.
    #[test]
    fn 다중_응답에서_codex_bucket만_고른다() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{
                "rateLimits":{"primary":{"usedPercent":99,"windowDurationMins":300}},
                "rateLimitsByLimitId":{
                    "codex":{
                        "primary":{"usedPercent":7,"windowDurationMins":300,"resetsAt":1788789533},
                        "secondary":{"usedPercent":31,"windowDurationMins":10080,"resetsAt":1789376333},
                        "planType":"pro"
                    },
                    "base_model_inference":{
                        "limitName":"gpt-reserve",
                        "primary":{"usedPercent":88,"windowDurationMins":10080}
                    },
                    "nameless":{"primary":{"usedPercent":55,"windowDurationMins":300}}
                    }
            }"#,
        )
        .unwrap();
        let got = parse(&v).expect("codex bucket을 읽어야 한다");
        assert_eq!(
            got.windows,
            vec![
                (300, 7.0, Some(1788789533)),
                (10080, 31.0, Some(1789376333))
            ]
        );
        assert_eq!(got.plan.as_deref(), Some("pro"));
        assert_eq!(
            got.named_windows,
            vec![NamedWindow {
                name: "gpt-reserve".to_string(),
                minutes: 10080,
                pct: 88.0,
                resets_at: None,
            }]
        );
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

    /// 서버가 5시간 창을 안 주면 주간 하나만 남긴다. 없는 값을 0%로 만들면
    /// 계정을 옮길지 판단하는 화면이 거꾸로 답한다.
    #[test]
    fn 없는_시간창은_만들지_않는다() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"rateLimitsByLimitId":{"codex":{"primary":null,"secondary":{"usedPercent":42,"windowDurationMins":10080}}}}"#,
        )
        .unwrap();
        let got = parse(&v).unwrap();
        assert_eq!(got.windows, vec![(10080, 42.0, None)]);
    }
}
