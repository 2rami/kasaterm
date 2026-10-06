//! 본판·kasa-serve-web 의 협업 호스트 — 기기 명부·관문·원격 칸을 kasa-collab 에 꽂는다.

use std::sync::{Arc, Once};

use kasa_collab::env::{CollabEnv, Route};

struct McpEnv;

impl CollabEnv for McpEnv {
    fn self_label(&self) -> String {
        crate::machines::self_label()
    }
    fn machines(&self) -> Vec<Route> {
        crate::machines::machines()
            .into_iter()
            .map(|m| Route { label: m.label, machine_id: m.machine_id, base: m.base })
            .collect()
    }
    fn relay(&self) -> Option<(String, String)> {
        Some((crate::mobile::gateway()?, crate::mobile::owner()?.slug))
    }
    fn auth_token(&self, base: &str) -> Option<String> {
        crate::remote::connection_auth_token(base)
    }
    fn is_remote_pane(&self, surface: &str) -> bool {
        crate::remote::is_remote_pane(surface)
    }
}

/// 보드·tell 을 쓰기 전에 한 번. 여러 번 불러도 된다.
pub fn install_collab_env() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| kasa_collab::env::set_env(Arc::new(McpEnv)));
}
