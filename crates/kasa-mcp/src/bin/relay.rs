//! `kasa-relay` — 폰 관문 서버. 앱이 업링크로 붙고 폰은 `/u/<slug>/…` 로 들어온다.
//! 배포 위치(넷버드망·클러스터)와 무관 — 어디서 실행하든 코드는 같다.
//!
//!   kasa-relay [--bind <주소>] [--port <n>] [--state <파일>]
//!   (기본 127.0.0.1 · 8790 · ~/.config/kasaterm/relay-state.json)
//!
//! 관문 자체는 인증하지 않는다 — 자격은 각 앱이 주소(slug)로 매긴다(`gateway.rs` 머리말).

fn main() -> anyhow::Result<()> {
    let mut bind = "127.0.0.1".to_string();
    let mut port: u16 = 8790;
    // 관문 slug 소유 기록. 기본 ~/.config/kasaterm/relay-state.json.
    let mut state: Option<std::path::PathBuf> = std::env::var_os("HOME")
        .map(|h| std::path::PathBuf::from(h).join(".config/kasaterm/relay-state.json"));
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--port" {
            if let Some(p) = args.next().and_then(|s| s.parse().ok()) {
                port = p;
            }
        } else if a == "--bind" {
            if let Some(b) = args.next() {
                bind = b;
            }
        } else if a == "--state" {
            state = args.next().map(std::path::PathBuf::from);
        }
    }
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(kasa_mcp::relay::serve(&bind, port, state))
}
