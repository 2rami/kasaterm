//! `kasa-relay` — 사내 세션 소통 중계 서버(2단계). 기계들이 여기 등록하면 세션
//! 목록을 모아 주고 메시지를 대상 기계로 라우팅한다. 배포 위치(넷버드망·클러스터)와
//! 무관 — 어디서 실행하든 코드는 같다.
//!
//!   kasa-relay [--port <n>] [--state <파일>]   (기본 8790 · ~/.config/kasaterm/relay-state.json)
//!
//! 관문(폰 주소 `/u/<slug>/`·앱 업링크 `/relay/uplink`)도 여기 얹힌다 — gateway.rs.
//!
//! 인증: `KASA_RELAY_TOKEN` 환경변수가 있으면 그 값을 `X-Relay-Token` 으로 요구한다.
//! 없으면 인증 없이 뜬다(로컬 테스트용).

fn main() -> anyhow::Result<()> {
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
        } else if a == "--state" {
            state = args.next().map(std::path::PathBuf::from);
        }
    }
    let token = std::env::var("KASA_RELAY_TOKEN").ok().filter(|s| !s.is_empty());
    if token.is_none() {
        eprintln!("[kasa-relay] ⚠️ KASA_RELAY_TOKEN 미설정 — 인증 없이 뜬다(로컬 테스트만).");
    }
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(kasa_mcp::relay::serve(port, token, state))
}
