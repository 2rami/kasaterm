//! 카사넷 — 기기끼리 iroh P2P QUIC 직통 길. 설계는 `docs/kasanet.md`.

pub mod allow;
pub mod fwd;
pub mod identity;
pub mod link;
pub mod route;

pub use iroh;

use iroh::endpoint::presets::Preset;
use iroh::endpoint::Builder;
use iroh::{Endpoint, SecretKey};

pub use allow::AllowList;
pub use fwd::{Forward, FwdServer};
pub use link::{Link, LinkState, RelayTrust};
pub use route::{Route, Via};

/// 모든 카사넷 엔드포인트가 거치는 틀. 허용 목록 훅이 빠진 엔드포인트가 생기지 않게 여기 한 곳에서 건다.
/// 실사용은 `presets::N0`(공용 중계·주소 찾기), 시험은 `presets::Minimal` 에 중계를 끈다.
pub fn builder(preset: impl Preset, secret_key: SecretKey, allow: &AllowList) -> Builder {
    Endpoint::builder(preset)
        .secret_key(secret_key)
        .hooks(allow.hook())
}
