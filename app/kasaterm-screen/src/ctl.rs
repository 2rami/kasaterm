//! 앱 ↔ 서비스 제어의 서비스 쪽 — 틀과 판은 `kasa_cast::service`, 길은 OS 마다(Windows 이름 파이프 `win.rs`).
//!
//! - `hello` → `{t:"hello", proto, version, node, viewers, settings}`
//! - `allow {machines, phones}` — 앱이 명부 기기·주인 폰을 통째 넘긴다. 서비스는 디스크에 두고 앱이 없을 때도 그대로 받는다.
//! - `settings {input?, audio?, source_mute?, logon_screen?}` — 없는 칸은 그대로.
//! - `status` → `{t:"status", node, viewers, list}`

use std::sync::{Arc, Mutex};

use kasa_cast::mirror::{Denied, Input};
use kasa_cast::{Capture, Config, Frame, Session};

use serde_json::{json, Value};

use crate::node::Node;
use crate::state::{unix_now, Allowed, Settings, State};

pub use kasa_cast::service::PROTO;

pub struct Service {
    pub state: State,
    pub node: Node,
    pub settings: Arc<Mutex<Settings>>,
    allowed: Mutex<Allowed>,
}

impl Service {
    pub fn new(state: State, node: Node, settings: Arc<Mutex<Settings>>, allowed: Allowed) -> Self {
        Self { state, node, settings, allowed: Mutex::new(allowed) }
    }

    pub fn handle(&self, req: &Value) -> Value {
        match req.get("t").and_then(Value::as_str) {
            Some("hello") => json!({
                "t": "hello",
                "proto": PROTO,
                "version": crate::VERSION,
                "node": self.node.info(crate::VERSION, PROTO),
                "viewers": self.node.viewers.count(),
                "settings": self.settings().to_json(),
            }),
            Some("allow") => {
                let a = Allowed::from_json(req);
                self.state.save_allowed(&a);
                self.node.set_allowed(&a.live(unix_now()));
                *self.allowed.lock().unwrap_or_else(|e| e.into_inner()) = a;
                json!({ "t": "ok" })
            }
            Some("settings") => {
                let mut cur = self.settings();
                let merged = Settings::from_json(&merge(cur.to_json(), req));
                if merged != cur {
                    cur = merged;
                    self.state.save_settings(cur);
                    apply(cur);
                    *self.settings.lock().unwrap_or_else(|e| e.into_inner()) = cur;
                }
                json!({ "t": "ok", "settings": cur.to_json() })
            }
            Some("status") => json!({
                "t": "status",
                "node": self.node.info(crate::VERSION, PROTO),
                "viewers": self.node.viewers.count(),
                "list": self.node.viewers.list(),
            }),
            _ => json!({ "t": "error", "why": "unknown" }),
        }
    }

    fn settings(&self) -> Settings {
        *self.settings.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 몇 초마다 — 만료된 폰을 빼고 공개 상태를 쓴다.
    pub fn tick(&self) {
        let live = self.allowed.lock().unwrap_or_else(|e| e.into_inner()).live(unix_now());
        self.node.set_allowed(&live);
        self.state.write_public_status(crate::VERSION, self.node.viewers.count(), &self.node.info(crate::VERSION, PROTO));
    }
}

/// 콘솔에 로그인한 사용자가 있나 — 없으면 로그인 화면이다. 맥 도우미는 로그인한 세션에서만 돈다.
fn user_present() -> bool {
    #[cfg(windows)]
    return kasa_cast_host::service::console_has_user();
    #[cfg(not(windows))]
    true
}

/// 서비스 설정을 따르는 보내기 — 조작 허용·로그인 화면 허용을 여기서 가른다.
pub struct Gate {
    pub inner: Arc<dyn Capture>,
    pub settings: Arc<Mutex<Settings>>,
}

impl Gate {
    fn settings(&self) -> Settings {
        *self.settings.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Capture for Gate {
    fn start(&self, config: &Config, out: Box<dyn Fn(Frame) + Send + Sync>) -> Result<Box<dyn Session>, Denied> {
        if !self.settings().logon_screen && !user_present() {
            return Err(Denied::Unavailable);
        }
        self.inner.start(config, out)
    }

    fn request_permission(&self) {
        self.inner.request_permission();
    }

    fn input(&self, display: u32, input: &Input) -> Result<(), Denied> {
        if !self.settings().input {
            return Err(Denied::Unavailable);
        }
        self.inner.input(display, input)
    }

    fn request_input_permission(&self) {
        self.inner.request_input_permission();
    }

    fn now(&self) -> f64 {
        self.inner.now()
    }
}

/// 설정을 프로세스에 건다 — 조작·로그인 화면은 `Gate`, 소리는 노드가 연결마다 읽는다.
pub fn apply(s: Settings) {
    kasa_cast_host::set_source_mute(s.source_mute);
}

fn merge(mut base: Value, over: &Value) -> Value {
    if let (Some(b), Some(o)) = (base.as_object_mut(), over.as_object()) {
        for (k, v) in o {
            if k != "t" {
                b.insert(k.clone(), v.clone());
            }
        }
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_merge_keeps_fields_the_app_did_not_send() {
        let base = Settings { audio: false, ..Settings::default() }.to_json();
        let merged = Settings::from_json(&merge(base, &json!({ "t": "settings", "input": false })));
        assert!(!merged.audio && !merged.input && merged.source_mute);
    }
}
