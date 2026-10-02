//! 카사넷을 폰 앱에 싣는 C ABI. 설계는 `docs/kasanet.md` 「P5」.
//!
//! 폰은 거는 쪽뿐이다 — 허용 목록이 비어 있어 들어오는 연결은 모두 끊는다. 데스크톱마다 `127.0.0.1` 입구
//! 하나를 열고(`kasa_net::Route::direct_only`), 입구는 직통일 때만 싣는다. 관문(HTTPS)으로 갈지 입구로 갈지는
//! 앱이 연결마다 `kasanet_state` 를 보고 고른다. 직통을 잃으면 입구가 실던 연결을 끊어 앱이 관문으로 다시 붙는다.
//!
//! 문자열은 모두 UTF-8 C 문자열. 이 라이브러리가 돌려준 문자열은 `kasanet_free_string` 으로 돌려준다.
//! 모든 함수는 어느 스레드에서 불러도 되고, 오래 막지 않는다(`kasanet_start`·`kasanet_stop` 만 바인드·닫기를 기다린다).

use std::collections::HashMap;
use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use kasa_net::iroh::endpoint::presets;
use kasa_net::iroh::{Endpoint, EndpointId, RelayMode};
use kasa_net::{identity, peer, AllowList, Link, LinkState, Route};

struct Entrance {
    route: Route,
    link: Link,
}

struct Node {
    rt: tokio::runtime::Runtime,
    endpoint: Endpoint,
    links: HashMap<EndpointId, Link>,
    /// 입구 로컬 포트 → 입구. 같은 데스크톱(같은 id)을 다시 열면 같은 입구를 준다.
    entrances: HashMap<u16, Entrance>,
}

static NODE: Mutex<Option<Node>> = Mutex::new(None);
const BIND_ENV: &str = "KASATERM_KASANET_BIND";
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

fn fail(why: impl Into<String>) -> i32 {
    *LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = Some(why.into());
    -1
}

fn guarded<T>(fallback: T, f: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| {
        fail("kasanet: 안에서 panic");
        fallback
    })
}

fn out(s: String) -> *mut c_char {
    CString::new(s).map_or(std::ptr::null_mut(), CString::into_raw)
}

unsafe fn arg<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    CStr::from_ptr(p).to_str().ok()
}

/// 키 파일(없으면 만든다, 0600)로 엔드포인트를 띄운다. 이미 떠 있으면 아무것도 안 한다. 0 성공, -1 실패.
///
/// # Safety
/// `key_path` 는 NUL 로 끝나는 C 문자열이어야 한다.
#[no_mangle]
pub unsafe extern "C" fn kasanet_start(key_path: *const c_char) -> i32 {
    let Some(path) = arg(key_path) else {
        return fail("kasanet: 키 경로가 없다");
    };
    let path = path.to_owned();
    guarded(-1, move || start(Path::new(&path)))
}

fn start(path: &Path) -> i32 {
    let mut slot = NODE.lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_some() {
        return 0;
    }
    let key = match identity::load_or_create(path) {
        Ok(k) => k,
        Err(e) => return fail(format!("kasanet: 키를 못 읽었다: {e}")),
    };
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("kasanet")
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return fail(format!("kasanet: 런타임: {e}")),
    };
    // 데스크톱과 같은 틀 — 주소 찾기(pkarr) 없이 n0 중계는 구멍 뚫기 신호에만. 받는 프로토콜이 없고 허용 목록이
    // 비어 있어 들어오는 연결은 핸드셰이크 직후 끊긴다.
    let mut builder = kasa_net::builder(presets::Minimal, key, &AllowList::default())
        .relay_mode(RelayMode::Default);
    // 검증 리그(시뮬레이터)용 — 데스크톱의 같은 이름 스위치와 같다. 맥에서 0.0.0.0 UDP 를 열면 방화벽이 사람 화면에
    // 묻기 창을 띄운다.
    if let Some(at) = std::env::var(BIND_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<std::net::SocketAddr>().ok())
    {
        builder = match builder.clear_ip_transports().bind_addr(at) {
            Ok(b) => b,
            Err(e) => return fail(format!("kasanet: {BIND_ENV}={at}: {e}")),
        };
    }
    let bound = rt.block_on(builder.bind());
    let endpoint = match bound {
        Ok(ep) => ep,
        Err(e) => return fail(format!("kasanet: 엔드포인트: {e}")),
    };
    rt.spawn(kasa_net::portmap::keep_swept(endpoint.clone(), |_| {}));
    *slot = Some(Node {
        rt,
        endpoint,
        links: HashMap::new(),
        entrances: HashMap::new(),
    });
    0
}

/// 이 폰의 EndpointId. 떠 있지 않으면 NULL.
#[no_mangle]
pub extern "C" fn kasanet_id() -> *mut c_char {
    guarded(std::ptr::null_mut(), || {
        let slot = NODE.lock().unwrap_or_else(|e| e.into_inner());
        slot.as_ref()
            .map_or(std::ptr::null_mut(), |n| out(n.endpoint.id().to_string()))
    })
}

/// 데스크톱 `/version` 의 `kasanet` 칸(JSON)으로 입구를 연다. 입구의 로컬 포트, 실패면 -1.
/// 같은 데스크톱이면 같은 입구를 돌려주고 주소만 새로 고친다.
///
/// # Safety
/// `peer_json` 은 NUL 로 끝나는 C 문자열이어야 한다.
#[no_mangle]
pub unsafe extern "C" fn kasanet_open(peer_json: *const c_char) -> i32 {
    let Some(text) = arg(peer_json) else {
        return fail("kasanet: 상대 주소가 없다");
    };
    let text = text.to_owned();
    guarded(-1, move || open(&text))
}

fn open(text: &str) -> i32 {
    let Some((addr, port)) = serde_json::from_str(text)
        .ok()
        .and_then(|v| peer::from_json(&v))
    else {
        return fail("kasanet: 상대 주소를 못 읽었다");
    };
    let mut slot = NODE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(n) = slot.as_mut() else {
        return fail("kasanet: 아직 안 떴다");
    };
    if addr.id == n.endpoint.id() {
        return fail("kasanet: 제 자신이다");
    }
    let _rt = n.rt.enter();
    let link = match n.links.get(&addr.id) {
        Some(link) => {
            link.set_addr(addr);
            link.clone()
        }
        None => {
            let link = Link::start(n.endpoint.clone(), addr, Default::default());
            n.links.insert(link.id(), link.clone());
            link
        }
    };
    // 데스크톱 앱이 다른 포트로 다시 떴으면 같은 입구가 새 포트로 잇는다.
    if let Some((local, e)) = n.entrances.iter().find(|(_, e)| e.link.id() == link.id()) {
        e.route.set_link(link, port);
        return i32::from(*local);
    }
    let route = match Route::direct_only() {
        Ok(r) => r,
        Err(e) => return fail(format!("kasanet: 입구: {e}")),
    };
    route.set_link(link.clone(), port);
    let local = route.local_addr().port();
    n.entrances.insert(local, Entrance { route, link });
    i32::from(local)
}

/// 입구의 지금 길. `{"path":"direct"|"relay"|"down","rtt_ms":N|null,"carried":N,"error":"…"|null}`.
/// 입구가 없으면 NULL. `direct` 일 때만 입구로 보낸다.
#[no_mangle]
pub extern "C" fn kasanet_state(local_port: u16) -> *mut c_char {
    guarded(std::ptr::null_mut(), || {
        let slot = NODE.lock().unwrap_or_else(|e| e.into_inner());
        let Some(e) = slot.as_ref().and_then(|n| n.entrances.get(&local_port)) else {
            return std::ptr::null_mut();
        };
        let (path, rtt) = match e.link.state() {
            LinkState::Direct { rtt } | LinkState::TrustedRelay { rtt } => {
                ("direct", Some(rtt.as_millis() as u64))
            }
            LinkState::Relay => ("relay", None),
            LinkState::Down => ("down", None),
        };
        out(serde_json::json!({
            "path": path,
            "rtt_ms": rtt,
            "carried": e.route.carried().0,
            "error": e.link.last_error(),
        })
        .to_string())
    })
}

/// 입구를 닫는다. 그 데스크톱으로 가는 다른 입구가 없으면 연결도 놓는다.
#[no_mangle]
pub extern "C" fn kasanet_close(local_port: u16) {
    guarded((), || {
        let mut slot = NODE.lock().unwrap_or_else(|e| e.into_inner());
        let Some(n) = slot.as_mut() else { return };
        let _rt = n.rt.enter();
        if let Some(e) = n.entrances.remove(&local_port) {
            let id = e.link.id();
            drop(e);
            if !n.entrances.values().any(|x| x.link.id() == id) {
                n.links.remove(&id);
            }
        }
    })
}

/// 망이 바뀌었다(와이파이↔셀룰러, 앱이 깨어남). 경로를 다시 찾게 한다.
#[no_mangle]
pub extern "C" fn kasanet_network_changed() {
    guarded((), || {
        let slot = NODE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = slot.as_ref() {
            let ep = n.endpoint.clone();
            n.rt.spawn(async move { ep.network_change().await });
        }
    })
}

/// 모두 닫는다. 상대가 유휴 시간 초과를 기다리지 않게 닫는다고 알린다(최대 0.5초).
#[no_mangle]
pub extern "C" fn kasanet_stop() {
    guarded((), || {
        let node = NODE.lock().unwrap_or_else(|e| e.into_inner()).take();
        let Some(n) = node else { return };
        let Node {
            rt,
            endpoint,
            links,
            entrances,
        } = n;
        {
            let _rt = rt.enter();
            drop(entrances);
            drop(links);
        }
        rt.block_on(async {
            let _ = tokio::time::timeout(Duration::from_millis(500), endpoint.close()).await;
        });
        rt.shutdown_timeout(Duration::from_millis(200));
    })
}

/// 마지막 실패 까닭. 없으면 NULL.
#[no_mangle]
pub extern "C" fn kasanet_last_error() -> *mut c_char {
    guarded(std::ptr::null_mut(), || {
        LAST_ERROR
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .map_or(std::ptr::null_mut(), out)
    })
}

/// 이 라이브러리가 돌려준 문자열을 놓는다.
///
/// # Safety
/// `s` 는 이 라이브러리가 돌려준 포인터이거나 NULL 이어야 하고, 한 번만 놓는다.
#[no_mangle]
pub unsafe extern "C" fn kasanet_free_string(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}
