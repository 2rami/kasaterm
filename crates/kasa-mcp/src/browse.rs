//! 브라우징 대상 — 사람이 볼 페이지가 **어느 기기의 무엇**으로 열리는지
//! (`docs/browse-target.md`). 설정 두 키(`browse_device`·`browse_open`)를 읽고,
//! 폰이 알려 온 화면 크기(`mobile-devices.json`)와 폰 제어 소켓(`/mobile/ws`)의
//! 등록부를 쥔다. 쓰기는 앱 쪽(`socket.rs` 의 settings_action)이 한다 — 설정
//! 파일의 주인은 앱이고, 이 크레이트는 KasaChrome 처럼 읽기만 한다.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// 사람이 볼 페이지의 목적지.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Open {
    /// 내장 웹 — 맥은 요청 pane 의 탭, 폰은 앱 안 웹 화면.
    Web,
    /// 그 기기의 브라우저.
    Chrome,
}

impl Open {
    pub fn as_str(self) -> &'static str {
        match self {
            Open::Web => "web",
            Open::Chrome => "chrome",
        }
    }
    pub fn parse(s: &str) -> Option<Open> {
        match s.trim() {
            "web" => Some(Open::Web),
            "chrome" | "browser" => Some(Open::Chrome),
            _ => None,
        }
    }
}

/// 어느 기기로.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Device {
    /// 키 없음 — 거울로 보는 사람이 있으면 그쪽, 없으면 이 기계(2026-09-02 규칙).
    Auto,
    ThisMachine,
    /// 명부의 기계 라벨.
    Machine(String),
    /// 폰 사용자 이름(`mobile-users.json`).
    Phone(String),
}

impl Device {
    /// 설정·HTTP 에서 쓰는 id. `Auto` 는 `"auto"`.
    pub fn id(&self) -> String {
        match self {
            Device::Auto => "auto".to_string(),
            Device::ThisMachine => String::new(),
            Device::Machine(label) => crate::machines::find(label)
                .and_then(|m| m.machine_id)
                .map(|id| format!("~{id}"))
                .unwrap_or_else(|| label.clone()),
            Device::Phone(name) => format!("phone:{name}"),
        }
    }

    /// id → 기기. `~<machine_id>` 는 명부에서 라벨로 푼다. 모르는 값은 None.
    pub fn from_id(id: &str) -> Option<Device> {
        let id = id.trim();
        if id == "auto" {
            return Some(Device::Auto);
        }
        if id.is_empty() {
            return Some(Device::ThisMachine);
        }
        if let Some(name) = id.strip_prefix("phone:") {
            return (!name.is_empty()).then(|| Device::Phone(name.to_string()));
        }
        if id.starts_with('~') {
            return crate::machines::find_route(id).map(|m| Device::Machine(m.label));
        }
        crate::machines::find(id).map(|m| Device::Machine(m.label))
    }
}

/// 설정 `browse_open`. 없거나 이상하면 브라우저.
pub fn open_mode() -> Open {
    crate::character::read_setting_str("browse_open")
        .as_deref()
        .and_then(Open::parse)
        .unwrap_or(Open::Chrome)
}

/// 설정 `browse_device`. 키가 없으면 Auto. 있는데 못 푸는 값(명부에서 사라진
/// 기계)도 Auto 로 — 사라진 기계를 고집하다 아무 데도 안 열리는 것보다 낫다.
pub fn device() -> Device {
    match crate::character::read_setting_str("browse_device") {
        None => Device::Auto,
        Some(raw) => Device::from_id(&raw).unwrap_or(Device::Auto),
    }
}

/// 폰이 알려 온 화면 — 논리 픽셀(CSS px).
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct PhoneScreen {
    pub width: u32,
    pub height: u32,
    #[serde(default = "one")]
    pub dpr: f64,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub platform: String,
    /// 마지막 등록 시각(unix 초).
    #[serde(default)]
    pub seen: u64,
}

fn one() -> f64 {
    1.0
}

/// 폰 화면 기본값 — 등록 전 폰을 고르면 이걸로 흉내 낸다(아이폰 15 급).
pub fn default_phone_screen() -> PhoneScreen {
    PhoneScreen { width: 393, height: 852, dpr: 3.0, model: String::new(), platform: "ios".into(), seen: 0 }
}

fn devices_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("KASATERM_MOBILE_DEVICES") {
        return Some(PathBuf::from(p));
    }
    Some(kasa_socket::home_dir()?.join(".config/kasaterm/mobile-devices.json"))
}

static DEVICES_WRITE: Mutex<()> = Mutex::new(());

fn load_devices() -> HashMap<String, PhoneScreen> {
    devices_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// 폰이 `POST /mobile/device` 로 알린 화면을 남긴다.
pub fn register_phone(name: &str, mut screen: PhoneScreen) -> std::io::Result<()> {
    let _g = DEVICES_WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(path) = devices_path() else { return Ok(()) };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    screen.seen = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut all = load_devices();
    all.insert(name.to_string(), screen);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&all)?)?;
    std::fs::rename(tmp, path)
}

/// 등록된 폰 화면. 없으면 None — 부르는 쪽이 `default_phone_screen` 로 메운다.
pub fn phone_screen(name: &str) -> Option<PhoneScreen> {
    load_devices().remove(name)
}

/// 고른 기기가 폰이면 그 화면 — KasaChrome·내장 웹 pane 이 흉내 낼 크기.
pub fn selected_phone_screen() -> Option<PhoneScreen> {
    match device() {
        Device::Phone(name) => Some(phone_screen(&name).unwrap_or_else(default_phone_screen)),
        _ => None,
    }
}

// ── 폰 제어 소켓 등록부 ──────────────────────────────────────────────────

type PhoneTx = tokio::sync::mpsc::Sender<String>;

fn phone_ctls() -> &'static Mutex<HashMap<String, Vec<(u64, PhoneTx)>>> {
    static V: OnceLock<Mutex<HashMap<String, Vec<(u64, PhoneTx)>>>> = OnceLock::new();
    V.get_or_init(Default::default)
}

pub fn register_phone_ctl(name: &str, tx: PhoneTx) -> u64 {
    static TOKEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let token = TOKEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut g) = phone_ctls().lock() {
        g.entry(name.to_string()).or_default().push((token, tx));
    }
    token
}

pub fn unregister_phone_ctl(name: &str, token: u64) {
    if let Ok(mut g) = phone_ctls().lock() {
        if let Some(v) = g.get_mut(name) {
            v.retain(|(t, _)| *t != token);
            if v.is_empty() {
                g.remove(name);
            }
        }
    }
}

/// 제어 소켓이 붙어 있는 폰인가.
pub fn phone_online(name: &str) -> bool {
    phone_ctls()
        .lock()
        .ok()
        .and_then(|g| g.get(name).map(|v| v.iter().any(|(_, tx)| !tx.is_closed())))
        .unwrap_or(false)
}

/// 폰의 모든 제어 접속에 JSON 한 줄. 닿은 수.
pub fn push_phone_control(name: &str, text: &str) -> usize {
    let Ok(mut g) = phone_ctls().lock() else { return 0 };
    let Some(v) = g.get_mut(name) else { return 0 };
    v.retain(|(_, tx)| !tx.is_closed());
    let n = v.iter().filter(|(_, tx)| tx.try_send(text.to_string()).is_ok()).count();
    if v.is_empty() {
        g.remove(name);
    }
    n
}

// ── 열기 요청과 응답 ─────────────────────────────────────────────────────

/// 요청 번호 — 폰의 `opened` 응답을 어느 요청에 붙일지.
pub fn next_request() -> u64 {
    static REQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    REQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// 폰이 `opened` 를 보내면 앱에 알리는 자리(`remote::OpenUrlSink` 와 같은 꼴).
pub type OpenAckSink = Box<dyn Fn(u64, bool, Option<String>) + Send + Sync>;

fn ack_sink() -> &'static Mutex<Option<OpenAckSink>> {
    static S: OnceLock<Mutex<Option<OpenAckSink>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

pub fn set_open_ack_sink(sink: OpenAckSink) {
    if let Ok(mut g) = ack_sink().lock() {
        *g = Some(sink);
    }
}

pub fn fire_open_ack(req: u64, ok: bool, error: Option<String>) {
    if let Ok(g) = ack_sink().lock() {
        if let Some(sink) = g.as_ref() {
            sink(req, ok, error);
        }
    }
}

/// 폰에 페이지를 보낸다. 제어 소켓이 붙어 있으면 그리로(즉시, 응답이 온다),
/// 아니면 푸시 알림으로(누르면 앱이 연다). 돌려주는 값은 (요청 번호, 어느 길).
pub fn open_on_phone(name: &str, url: &str, mode: Open) -> (u64, PhoneRoute) {
    let req = next_request();
    let msg = serde_json::json!({
        "t": "open-url", "url": url, "mode": mode.as_str(), "req": req.to_string(),
    })
    .to_string();
    if push_phone_control(name, &msg) > 0 {
        return (req, PhoneRoute::Socket);
    }
    if crate::push::configured() {
        let alert = crate::push::Alert {
            title: "페이지 열기".to_string(),
            body: url.chars().take(180).collect(),
            machine: None,
            pane: String::new(),
            kind: format!("open-url:{}", mode.as_str()),
            collapse: Some("open-url".to_string()),
            sender: None,
            avatar_slug: None,
        };
        // GUI 스레드(tokio 밖)에서도 불린다 — 자체 런타임을 짧게 띄운다.
        std::thread::spawn(move || {
            if let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
                let _ = rt.block_on(crate::push::send(&alert));
            }
        });
        return (req, PhoneRoute::Push);
    }
    (req, PhoneRoute::Unreachable)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PhoneRoute {
    Socket,
    Push,
    Unreachable,
}

// ── 목록 ────────────────────────────────────────────────────────────────

/// `GET /browse/devices` 본문.
pub fn devices_json() -> serde_json::Value {
    let selected = device();
    let selected_id = selected.id();
    let mut devices = vec![serde_json::json!({
        "id": "", "label": crate::machines::self_label(), "kind": "desktop", "online": true,
        "this": true,
    })];
    let snap = crate::machines::snapshot();
    for m in crate::machines::listed_machines() {
        let online = snap
            .iter()
            .find(|v| v.get("label").and_then(|l| l.as_str()) == Some(m.label.as_str()))
            .and_then(|v| v.get("online").and_then(|o| o.as_bool()))
            .unwrap_or(false);
        devices.push(serde_json::json!({
            "id": Device::Machine(m.label.clone()).id(),
            "label": m.label, "kind": "desktop", "online": online,
        }));
    }
    let screens = load_devices();
    for u in crate::mobile::users() {
        let screen = screens.get(&u.name);
        devices.push(serde_json::json!({
            "id": format!("phone:{}", u.name),
            "label": format!("{} 폰", u.name),
            "kind": "phone",
            "online": phone_online(&u.name),
            "model": screen.map(|s| s.model.clone()).unwrap_or_default(),
            "viewport": screen.map(|s| serde_json::json!({
                "width": s.width, "height": s.height, "dpr": s.dpr,
            })),
        }));
    }
    serde_json::json!({
        "ok": true,
        "open": open_mode().as_str(),
        "selected": selected_id,
        "auto": selected == Device::Auto,
        "devices": devices,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_parses_both_spellings() {
        assert_eq!(Open::parse("web"), Some(Open::Web));
        assert_eq!(Open::parse(" chrome "), Some(Open::Chrome));
        assert_eq!(Open::parse("browser"), Some(Open::Chrome));
        assert_eq!(Open::parse("safari"), None);
    }

    #[test]
    fn device_ids_round_trip_without_registry() {
        assert_eq!(Device::from_id("auto"), Some(Device::Auto));
        assert_eq!(Device::from_id(""), Some(Device::ThisMachine));
        assert_eq!(Device::from_id("phone:geono"), Some(Device::Phone("geono".into())));
        assert_eq!(Device::from_id("phone:"), None);
        assert_eq!(Device::Phone("geono".into()).id(), "phone:geono");
        assert_eq!(Device::ThisMachine.id(), "");
        assert_eq!(Device::Auto.id(), "auto");
    }

    #[test]
    fn phone_control_registry_counts_live_sockets() {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(4);
        let name = "t-registry";
        assert!(!phone_online(name));
        let token = register_phone_ctl(name, tx);
        assert!(phone_online(name));
        assert_eq!(push_phone_control(name, "{\"t\":\"x\"}"), 1);
        assert_eq!(rx.try_recv().ok().as_deref(), Some("{\"t\":\"x\"}"));
        unregister_phone_ctl(name, token);
        assert!(!phone_online(name));
        assert_eq!(push_phone_control(name, "{}"), 0);
    }

    #[test]
    fn phone_screen_persists_in_scoped_file() {
        let dir = std::env::temp_dir().join(format!("kasa-browse-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("devices.json");
        std::env::set_var("KASATERM_MOBILE_DEVICES", &path);
        let screen = PhoneScreen { width: 393, height: 852, dpr: 3.0, model: "iPhone".into(), platform: "ios".into(), seen: 0 };
        register_phone("geono", screen.clone()).unwrap();
        let back = phone_screen("geono").unwrap();
        assert_eq!((back.width, back.height, back.dpr), (393, 852, 3.0));
        assert!(back.seen > 0);
        std::env::remove_var("KASATERM_MOBILE_DEVICES");
        let _ = std::fs::remove_dir_all(dir);
    }
}
