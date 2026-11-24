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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QueryFailure {
    Unavailable,
    ReauthRequired,
}

#[derive(Clone)]
struct CachedLimits {
    at: Instant,
    /// 실패도 한 번의 조회다. `None`을 시각과 함께 기억해야 로그인 안 된 슬롯을
    /// 메뉴가 열린 5초마다 다시 띄우지 않는다.
    limits: Option<CodexLimits>,
    probe: bool,
    reauth_required: bool,
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
    result: Result<CodexLimits, QueryFailure>,
    started_generation: u64,
    current_generation: u64,
) -> bool {
    if started_generation != current_generation {
        return false;
    }
    let now = Instant::now();
    match result {
        Ok(mut limits) => {
            limits.stale = false;
            cached.insert(account_id.to_string(), CachedLimits {
                at: now,
                limits: Some(limits),
                probe: false,
                reauth_required: false,
            });
            true
        }
        Err(failure) => {
            let entry = cached.entry(account_id.to_string()).or_insert(CachedLimits {
                at: now,
                limits: None,
                probe: false,
                reauth_required: false,
            });
            entry.at = now;
            entry.probe = false;
            if failure == QueryFailure::ReauthRequired {
                entry.reauth_required = true;
                entry.limits = None;
            } else if let Some(limits) = entry.limits.as_mut() {
                limits.stale = true;
            }
            true
        }
    }
}

pub(crate) fn reauth_required_for(account_id: &str) -> bool {
    cache().lock().ok()
        .is_some_and(|slots| slots.get(account_id).is_some_and(|slot| slot.reauth_required))
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

pub(crate) fn missing_note_for(account_id: &str) -> &'static str {
    if reauth_required_for(account_id) { "다시 로그인 필요" }
    else if attempted_for(account_id) { "한도 조회 실패" } else { "한도 확인 중…" }
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
            reauth_required: false,
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
            reauth_required: false,
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

/// 특정 계정에 다시 물을 때가 됐나. 계정 메뉴의 비활성 행이 쓴다.
pub(crate) fn stale_for(account_id: &str, every: Duration) -> bool {
    cache()
        .lock()
        .ok()
        .map(|cached| cached_stale(&cached, account_id, every))
        .unwrap_or(false)
}

/// 로그인 완료 직후에는 같은 계정이어도 옛 한도를 버리고 바로 다시 묻는다.
pub(crate) fn invalidate(account_id: &str) {
    // Reject app-server replies already in flight when this login finished.
    generation().fetch_add(1, Ordering::AcqRel);
    if let Ok(mut cached) = cache().lock() {
        cached.remove(account_id);
    }
}

/// 공용 Codex 창 중 가장 많이 쓴 비율. 이름 달린 모델 bucket 은 그 모델만의 한도라 뺀다.
fn pressure(limits: &CodexLimits) -> Option<f32> {
    limits.windows.iter().map(|(_, pct, _)| *pct).reduce(f32::max)
}

/// 한도 자동 전환이 옮겨 갈 Codex 로그인과 떠나는 계정의 사용률. `order` 는 기본
/// 로그인(`""`)부터 등록 차례대로다.
///
/// Plus·Pro 는 한도를 넘으면 크레딧 잔액에서 빠지므로 현재 계정이 기준에 닿으면 옮긴다.
pub(crate) fn autoswitch_target(current: &str, order: &[String], threshold: f32) -> Option<(String, f32)> {
    let cached = cache().lock().ok()?;
    pick_autoswitch(
        current,
        order.iter().map(|id| (id.as_str(), cached_snapshot(&cached, id))),
        threshold,
    )
}

fn pick_autoswitch<'a>(
    current: &str,
    slots: impl IntoIterator<Item = (&'a str, Option<CodexLimits>)>,
    threshold: f32,
) -> Option<(String, f32)> {
    // `pick_account_by_order` 는 빈 id 를 「계정 아님」으로 거르는데 Codex 는 빈 id 가 기본
    // 로그인이다. 표지를 붙여 넘기고 돌려받을 때 뗀다.
    const KEY: &str = "codex:";
    // 마지막 조회가 실패한 슬롯(로그아웃·토큰 만료)은 후보가 아니다. 그리로 옮기면 그 뒤
    // 뜨는 codex 가 전부 로그인 화면에서 멈춘다.
    let fresh: Vec<(String, f32)> = slots
        .into_iter()
        .filter_map(|(id, limits)| {
            let limits = limits.filter(|limits| !limits.stale)?;
            Some((format!("{KEY}{id}"), pressure(&limits)?))
        })
        .collect();
    let current = format!("{KEY}{current}");
    let pct = fresh.iter().find(|(id, _)| *id == current)?.1;
    let to = crate::socket::pick_account_by_order(&current, pct, &fresh, threshold, &HashMap::new(), 0)?;
    Some((to.strip_prefix(KEY)?.to_string(), pct))
}

pub(crate) fn spawn_poller(proxy: winit::event_loop::EventLoopProxy<crate::UserEvent>) {
    std::thread::spawn(move || {
        let mut last_switch: Option<Instant> = None;
        while !kasa_socket::session_end::is_ending() {
            let visible = crate::handler::watching_account_usage();
            let every = Duration::from_secs(if visible { 60 } else { 300 });
            let mut order = vec![String::new()];
            order.extend(crate::socket::read_codex_accounts().into_iter().map(|slot| slot.id));
            let current = current_account_id();
            let mut seen = std::collections::HashSet::new();
            // An unresponsive CLI in another slot must not delay the current account's result.
            for id in std::iter::once(&current).chain(&order).filter(|id| seen.insert(*id)) {
                if stale_for(id, every) && refresh_for(id)
                    && proxy.send_event(crate::UserEvent::Redraw).is_err()
                {
                    return;
                }
            }
            if crate::socket::read_account_autoswitch()
                && last_switch.is_none_or(|at| at.elapsed() >= Duration::from_secs(300))
            {
                let from = current_account_id();
                if let Some((to, pct)) = autoswitch_target(&from, &order, crate::socket::read_account_autoswitch_pct()) {
                    last_switch = Some(Instant::now());
                    if proxy.send_event(crate::UserEvent::CodexAccountAutoswitch { from, to, pct }).is_err() {
                        return;
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    });
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

fn ask(account_home: Option<&std::path::Path>) -> Result<CodexLimits, QueryFailure> {
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
        .map_err(|_| QueryFailure::Unavailable)?;
    let result = query_limits(&mut child);
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn query_limits(child: &mut std::process::Child) -> Result<CodexLimits, QueryFailure> {
    let stdin = child.stdin.as_mut().ok_or(QueryFailure::Unavailable)?;
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"clientInfo":{{"name":"kasaterm","title":"kasaterm","version":"{}"}}}}}}"#,
        env!("CARGO_PKG_VERSION")
    ).map_err(|_| QueryFailure::Unavailable)?;
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":2,"method":"account/rateLimits/read","params":{{}}}}"#
    ).and_then(|()| stdin.flush()).map_err(|_| QueryFailure::Unavailable)?;

    let stdout = child.stdout.take().ok_or(QueryFailure::Unavailable)?;
    let (send, response) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            if value["id"].as_i64() == Some(2) {
                let _ = send.send(parse_limits_reply(&value));
                break;
            }
        }
    });
    // The worker's pipe is closed when the caller kills this child on timeout.
    response.recv_timeout(Duration::from_secs(6)).unwrap_or(Err(QueryFailure::Unavailable))
}

fn parse_limits_reply(value: &serde_json::Value) -> Result<CodexLimits, QueryFailure> {
    if let Some(error) = value.get("error") {
        let message = error["message"].as_str().unwrap_or_default().to_ascii_lowercase();
        let code = error.pointer("/data/code").and_then(|code| code.as_str())
            .or_else(|| error.pointer("/data/error/code").and_then(|code| code.as_str()));
        let auth_codes = ["token_expired", "token_revoked", "invalid_grant", "refresh_token_reused", "refresh_token_expired", "refresh_token_invalidated"];
        if code.is_some_and(|code| auth_codes.contains(&code))
            || auth_codes.iter().any(|code| message.contains(code))
            || message.contains("not logged in")
            || message.contains("401 unauthorized")
            || message.contains("access token could not be refreshed")
        {
            return Err(QueryFailure::ReauthRequired);
        }
        return Err(QueryFailure::Unavailable);
    }
    value.get("result").and_then(parse).ok_or(QueryFailure::Unavailable)
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

    fn limits(pcts: &[f32], stale: bool) -> Option<CodexLimits> {
        Some(CodexLimits {
            windows: pcts.iter().map(|pct| (300, *pct, None)).collect(),
            stale,
            ..Default::default()
        })
    }

    #[test]
    fn 한도에_닿은_기본_로그인은_차례상_다음_슬롯으로_옮긴다() {
        let slots = [("", limits(&[12.0, 100.0], false)), ("codex-1", limits(&[0.0], false))];
        assert_eq!(pick_autoswitch("", slots, 100.0), Some(("codex-1".into(), 100.0)));
    }

    #[test]
    fn 슬롯이_한도에_닿으면_기본_로그인으로도_옮긴다() {
        let slots = [("", limits(&[30.0], false)), ("codex-1", limits(&[96.0], false))];
        assert_eq!(pick_autoswitch("codex-1", slots, 95.0), Some((String::new(), 96.0)));
    }

    #[test]
    fn 기준_아래면_위_차례에_여유가_있어도_그대로_둔다() {
        let slots = [("", limits(&[0.0], false)), ("codex-1", limits(&[40.0], false))];
        assert_eq!(pick_autoswitch("codex-1", slots, 90.0), None);
    }

    #[test]
    fn 조회가_실패했거나_여유가_모자란_슬롯으로는_안_간다() {
        let expired = [("", limits(&[100.0], false)), ("codex-1", limits(&[0.0], true))];
        assert_eq!(pick_autoswitch("", expired, 100.0), None);
        let never = [("", limits(&[100.0], false)), ("codex-1", None)];
        assert_eq!(pick_autoswitch("", never, 100.0), None);
        let tight = [("", limits(&[100.0], false)), ("codex-1", limits(&[97.0], false))];
        assert_eq!(pick_autoswitch("", tight, 100.0), None);
    }

    #[test]
    fn 지금_계정의_숫자가_낡았으면_판단하지_않는다() {
        let slots = [("", limits(&[100.0], true)), ("codex-1", limits(&[0.0], false))];
        assert_eq!(pick_autoswitch("", slots, 100.0), None);
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
                reauth_required: false,
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
                reauth_required: false,
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
            Ok(CodexLimits {
                windows: vec![(300, 17.0, Some(123))],
                ..Default::default()
            }),
            1,
            1,
        ));
        assert!(store_result(&mut cached, "codex-1", Err(QueryFailure::Unavailable), 1, 1));
        let kept = cached_snapshot(&cached, "codex-1").expect("마지막 성공값 유지");
        assert_eq!(kept.windows, vec![(300, 17.0, Some(123))]);
        assert!(kept.stale, "실패 뒤에는 낡은 값임을 표시");
        assert!(cached_snapshot(&cached, "codex-2").is_none());
    }

    #[test]
    fn 첫_실패도_조회완료로_기록한다() {
        let mut cached = HashMap::new();
        assert!(store_result(&mut cached, "logged-out", Err(QueryFailure::Unavailable), 1, 1));
        assert!(cached.contains_key("logged-out"));
        assert!(cached_snapshot(&cached, "logged-out").is_none());
    }

    #[test]
    fn invalidate_뒤에_도착한_응답은_버린다() {
        let mut cached = HashMap::new();
        assert!(!store_result(
            &mut cached,
            "codex-1",
            Ok(CodexLimits::default()),
            4,
            5,
        ));
        assert!(cached.is_empty(), "옛 세대 응답이 다시 들어왔다");

        assert!(store_result(
            &mut cached,
            "codex-1",
            Ok(CodexLimits::default()),
            5,
            5,
        ));
        assert!(cached_snapshot(&cached, "codex-1").is_some());
    }

    #[test]
    fn rejected_codex_credentials_clear_only_that_slots_quotas() {
        let mut cached = HashMap::new();
        let expired = serde_json::json!({"id":2,"error":{"code":-32000,"data":{"code":"token_expired"}}});
        let network = serde_json::json!({"id":2,"error":{"code":-32000,"message":"request timed out"}});
        let _ = store_result(&mut cached, "first", Ok(CodexLimits::default()), 1, 1);
        let _ = store_result(&mut cached, "next", Ok(CodexLimits::default()), 1, 1);
        let _ = store_result(&mut cached, "first", parse_limits_reply(&network), 1, 1);
        assert!(cached["first"].limits.as_ref().unwrap().stale);
        assert!(!cached["first"].reauth_required);
        let _ = store_result(&mut cached, "first", parse_limits_reply(&expired), 1, 1);
        assert!(cached["first"].reauth_required);
        assert!(cached["first"].limits.is_none());
        assert!(cached["next"].limits.is_some());
        let _ = store_result(&mut cached, "first", parse_limits_reply(&network), 1, 1);
        assert!(cached["first"].reauth_required);
        let _ = store_result(&mut cached, "first", Ok(CodexLimits::default()), 1, 1);
        assert!(!cached["first"].reauth_required);
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
