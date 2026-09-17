//! 폰이 못 여는 주소를 바깥 주소로 — cloudflared 임시 터널(trycloudflare.com).
//!
//! 「폰」 도착지로 `open` 한 주소가 `localhost:3000` 이면 폰에서는 아무것도 안 열린다.
//! 학생이 도는 이 기계에서 그 포트로 임시 터널을 하나 세우고, 폰 쪽지에는 그 터널
//! 주소를 넣는다(2026-09-17 지시 「폰으로 설정하면 링크 주게」). 이름 붙은 터널
//! (tunnel.rs)과 달리 로그인·DNS 가 필요 없고, 앱과 함께 죽는다 — 잠깐 보여 주는
//! 용도라 상시 노출로 남기지 않는다.
//!
//! 포트(origin)마다 하나만 띄우고 다시 쓴다. 죽어 있으면 다시 띄운다.
//!
//! ⚠️ `--config <빈 파일>` 이 필수다. 안 주면 cloudflared 가 `~/.cloudflared/config.yml`
//! (이름 붙은 터널의 ingress)을 기본으로 읽어 `--url` 을 덮고, 주소는 잘 찍히는데
//! 엣지가 404 만 돌려준다(2026-09-17 실측 — 연결은 등록돼 있어 전파 지연으로 오해한다).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::IpAddr;
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::Duration;

use reqwest::Url;

struct Entry {
    child: Child,
    public: String,
}

fn registry() -> &'static Mutex<HashMap<String, Entry>> {
    static R: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 띄우기는 한 번에 하나 — 같은 포트로 두 요청이 겹쳐 터널 둘이 서는 것을 막는다.
fn spawn_gate() -> &'static Mutex<()> {
    static G: OnceLock<Mutex<()>> = OnceLock::new();
    G.get_or_init(|| Mutex::new(()))
}

/// 주소를 받기까지 보통 3~8초. 그 뒤 연결 등록까지 몇 초 더 — 등록 전엔 엣지가 404 다.
const URL_WAIT: Duration = Duration::from_secs(25);
const REGISTER_WAIT: Duration = Duration::from_secs(20);

enum Progress {
    Url(String),
    Registered,
    Exited,
}

/// 폰이 못 여는 주소인가 — loopback·0.0.0.0·사설망·`.local`. 사설망 주소는 같은
/// 와이파이면 열리지만 밖에서는 안 열리므로 함께 터널로 보낸다.
fn needs_tunnel(url: &Url) -> bool {
    let Some(host) = url.host_str() else { return false };
    let host = host.trim_matches(['[', ']']);
    if matches!(host, "localhost" | "localhost.") || host.ends_with(".local") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => ip.is_loopback() || ip.is_unspecified() || ip.is_private() || ip.is_link_local(),
        Ok(IpAddr::V6(ip)) => ip.is_loopback() || ip.is_unspecified(),
        Err(_) => false,
    }
}

/// 터널이 붙을 이쪽 원점 — 이 기계를 가리키는 호스트는 127.0.0.1 로, 다른 사설망
/// 기계면 그대로(터널은 이 기계에서 그 기계로 프록시한다).
fn origin_of(url: &Url) -> Option<String> {
    let host = url.host_str()?.trim_matches(['[', ']']).to_string();
    let port = url.port_or_known_default()?;
    let this_machine = matches!(host.as_str(), "localhost" | "localhost.")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback() || ip.is_unspecified());
    let host = if this_machine { "127.0.0.1".to_string() } else if host.contains(':') { format!("[{host}]") } else { host };
    Some(format!("{}://{host}:{port}", url.scheme()))
}

/// 터널 주소에 원래 주소의 경로·쿼리·조각을 그대로 얹는다.
fn splice(public_base: &str, url: &Url) -> Option<String> {
    let mut out = Url::parse(public_base).ok()?;
    out.set_path(url.path());
    out.set_query(url.query());
    out.set_fragment(url.fragment());
    Some(out.to_string())
}

/// 폰이 열 수 있는 주소. 이미 바깥 주소면 그대로, 이 기계 주소면 임시 터널 주소.
/// 터널을 새로 띄우면 수십 초 걸리니 GUI 스레드에서 부르지 않는다.
pub fn public_url(raw: &str) -> Result<String, String> {
    let url = Url::parse(raw).map_err(|_| "URL 을 읽을 수 없어요".to_string())?;
    if !needs_tunnel(&url) {
        return Ok(raw.to_string());
    }
    if !matches!(url.scheme(), "http" | "https") {
        return Err("HTTP·HTTPS 주소만 터널로 보낼 수 있어요".to_string());
    }
    let origin = origin_of(&url).ok_or_else(|| "포트를 알 수 없어요".to_string())?;
    let base = ensure(&origin)?;
    splice(&base, &url).ok_or_else(|| "터널 주소를 만들지 못했어요".to_string())
}

fn alive(origin: &str) -> Option<String> {
    let mut reg = registry().lock().ok()?;
    let entry = reg.get_mut(origin)?;
    match entry.child.try_wait() {
        Ok(None) => Some(entry.public.clone()),
        _ => {
            reg.remove(origin);
            None
        }
    }
}

fn empty_config_path() -> std::path::PathBuf {
    let p = std::env::temp_dir().join("kasaterm-quicktunnel-empty.yml");
    if !p.exists() {
        let _ = std::fs::write(&p, "");
    }
    p
}

fn log_path(origin: &str) -> std::path::PathBuf {
    let port = origin.rsplit(':').next().unwrap_or("0");
    std::env::temp_dir().join(format!("kasaterm-quicktunnel-{port}.log"))
}

fn ensure(origin: &str) -> Result<String, String> {
    let _gate = spawn_gate().lock().map_err(|_| "터널 잠금 실패".to_string())?;
    if let Some(public) = alive(origin) {
        return Ok(public);
    }
    let mut cmd = Command::new(crate::tunnel::cloudflared_bin());
    cmd.arg("--config")
        .arg(empty_config_path())
        .args(["tunnel", "--no-autoupdate", "--protocol", "http2", "--edge-ip-version", "4", "--url", origin])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cloudflared 실행 실패: {e} (brew install cloudflared)"))?;
    let stderr = child.stderr.take().ok_or_else(|| "cloudflared 출력을 못 잡았어요".to_string())?;
    let (tx, rx) = mpsc::channel::<Progress>();
    let log = std::fs::OpenOptions::new().create(true).append(true).open(log_path(origin)).ok();
    std::thread::spawn(move || {
        let mut log = log;
        let mut url_sent = false;
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if let Some(f) = log.as_mut() {
                let _ = writeln!(f, "{line}");
            }
            if !url_sent {
                if let Some(u) = find_trycloudflare(&line) {
                    url_sent = true;
                    let _ = tx.send(Progress::Url(u));
                }
            }
            if line.contains("Registered tunnel connection") {
                let _ = tx.send(Progress::Registered);
            }
        }
        let _ = tx.send(Progress::Exited);
    });
    let public = match rx.recv_timeout(URL_WAIT) {
        Ok(Progress::Url(u)) => u,
        _ => {
            let _ = child.kill();
            return Err(format!(
                "cloudflared 가 주소를 못 받았어요 (로그 {})",
                log_path(origin).display()
            ));
        }
    };
    // 등록 전에 폰이 누르면 404 라 잠깐 기다린다. 못 봐도 주소는 준다 — 로그가 늦었을 수 있다.
    let deadline = std::time::Instant::now() + REGISTER_WAIT;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(left) {
            Ok(Progress::Registered) => break,
            Ok(Progress::Exited) => {
                let _ = child.kill();
                return Err("cloudflared 가 바로 죽었어요".to_string());
            }
            Ok(Progress::Url(_)) => continue,
            Err(_) => break,
        }
    }
    if let Ok(mut reg) = registry().lock() {
        reg.insert(origin.to_string(), Entry { child, public: public.clone() });
    }
    Ok(public)
}

fn find_trycloudflare(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let rest = &line[start..];
    let end = rest.find(|c: char| c.is_whitespace() || c == '|').unwrap_or(rest.len());
    let u = &rest[..end];
    u.ends_with(".trycloudflare.com").then(|| u.to_string())
}

/// 앱을 끌 때 — 임시 터널은 앱보다 오래 살 이유가 없다. SIGKILL 이지만 cloudflared 는
/// 연결이 끊기면 엣지가 곧 주소를 거둔다.
pub fn shutdown() {
    if let Ok(mut reg) = registry().lock() {
        for (_, mut e) in reg.drain() {
            let _ = e.child.kill();
            let _ = e.child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn local_hosts_need_tunnel_public_do_not() {
        for s in ["http://localhost:3000/", "http://127.0.0.1:8080/x", "http://0.0.0.0:3000/", "http://10.1.177.135:3000/", "http://192.168.1.2/", "http://[::1]:5173/", "http://mini.local:3000/"] {
            assert!(needs_tunnel(&u(s)), "{s}");
        }
        for s in ["https://example.com/", "https://kasaterm.debimarlene.com/u/x/", "http://8.8.8.8/"] {
            assert!(!needs_tunnel(&u(s)), "{s}");
        }
    }

    #[test]
    fn origin_collapses_this_machine_to_loopback() {
        assert_eq!(origin_of(&u("http://localhost:3000/a?b")).unwrap(), "http://127.0.0.1:3000");
        assert_eq!(origin_of(&u("http://0.0.0.0:3000/")).unwrap(), "http://127.0.0.1:3000");
        assert_eq!(origin_of(&u("http://[::1]:3000/")).unwrap(), "http://127.0.0.1:3000");
        assert_eq!(origin_of(&u("http://10.1.2.3:8080/")).unwrap(), "http://10.1.2.3:8080");
        assert_eq!(origin_of(&u("https://localhost/")).unwrap(), "https://127.0.0.1:443");
    }

    #[test]
    fn splice_keeps_path_query_fragment() {
        let out = splice("https://a-b-c.trycloudflare.com", &u("http://localhost:3000/deals/7?tab=notes#top")).unwrap();
        assert_eq!(out, "https://a-b-c.trycloudflare.com/deals/7?tab=notes#top");
    }

    #[test]
    fn public_url_passes_public_addresses_through() {
        assert_eq!(public_url("https://example.com/x").unwrap(), "https://example.com/x");
    }

    /// 진짜 cloudflared 를 띄운다 — 3000 번에 뭔가 떠 있어야 하고 수십 초 걸린다.
    /// `cargo test -p kasa-mcp real_tunnel -- --ignored --nocapture`. 이 기계 DNS 가
    /// trycloudflare 를 못 풀 수 있어(넷버드 우선) 엣지 IP 를 못 박아 붙는다.
    #[test]
    #[ignore]
    fn real_tunnel_reaches_local_port_3000() {
        let public = public_url("http://localhost:3000/login").expect("tunnel");
        eprintln!("public = {public}");
        let url = u(&public);
        assert!(url.host_str().unwrap().ends_with(".trycloudflare.com"));
        assert_eq!(url.path(), "/login");
        let host = url.host_str().unwrap().to_string();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let status = rt.block_on(async {
            let client = reqwest::Client::builder()
                .resolve(&host, "104.16.230.132:443".parse().unwrap())
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(20))
                .build()
                .unwrap();
            client.get(&public).send().await.unwrap().status()
        });
        eprintln!("status = {status}");
        assert!(status.is_success() || status.is_redirection(), "{status}");
        assert_eq!(public_url("http://localhost:3000/").unwrap().trim_end_matches('/'), public.trim_end_matches("/login"), "같은 포트는 같은 터널을 다시 쓴다");
        shutdown();
    }

    #[test]
    fn finds_trycloudflare_url_in_cloudflared_banner() {
        let line = "2026-09-17T04:35:04Z INF |  https://ontario-search-bottom-seafood.trycloudflare.com                                     |";
        assert_eq!(find_trycloudflare(line).as_deref(), Some("https://ontario-search-bottom-seafood.trycloudflare.com"));
        assert!(find_trycloudflare("INF Requesting new quick Tunnel on trycloudflare.com...").is_none());
    }
}
