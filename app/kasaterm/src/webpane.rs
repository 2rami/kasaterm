//! 웹(브라우저) pane — pane 자리에 붙어 다니는 자식 OS 창.
//!
//! wgpu 본창 **안**에는 WKWebView 를 겹칠 수 없다(CAMetalLayer 가 자식 뷰를
//! 전부 덮는다 — 2026-05-25 실측, 그때 포기했던 이유). 그래서 반대로 간다:
//! 장식 없는 자식 창을 pane 사각형 위에 얹고 NSWindow `addChildWindow:` 로
//! 본창에 접착한다. 창을 끌면 AppKit 이 자식을 같이 옮기고(프레임 간 위임이
//! 없어 드래그 중에도 안 떨어진다), pane 이 갈라지거나 커지거나 탭이 바뀌면
//! `sync_web_hosts` 가 다음 루프 턴에 프레임을 따라잡는다.
//!
//! 그리드 쪽 자리는 `PaneContent::Web`(주소 + host_id 뿐)이 잡는다 — 셀
//! 렌더는 그 pane 을 빈 바탕으로 두고, 이 모듈의 자식 창이 그 위를 덮는다.
//! `Workspace` 는 PTY 스레드와 공유라 !Send 인 webview 실물은 거기 못 산다.

use super::*;
use std::collections::HashSet;
use winit::event_loop::ActiveEventLoop;

/// 웹 pane 하나의 실물. `App.web_hosts` 에 `WebPane.host_id` 로 산다.
pub(crate) struct WebHost {
    // webview 가 window 를 빌린다 — 필드는 선언 순서로 drop 되므로 webview 가
    // 먼저 와야 한다(chrome.rs 패널들이 수동으로 지키는 순서와 같은 이유).
    webview: wry::WebView,
    window: Arc<winit::window::Window>,
    /// 마지막으로 적용한 프레임(물리 px: x, y, w, h) — 같으면 안 건드린다.
    /// 매 루프 턴 호출되는 sync 가 OS 창 이동/리사이즈를 반복 호출하지 않게.
    last_frame: Option<(i32, i32, u32, u32)>,
    /// 지금 화면에 붙어(orderFront + child 접착) 있는가.
    visible: bool,
    /// 마지막 주소 폴링 시각 — 매 루프 턴 도는 sync 가 ObjC 왕복을 반복하지
    /// 않게 500ms 로 죈다. 페이지 이동을 따라 라벨·WebPane.url 을 갱신하는 용.
    last_url_poll: std::time::Instant,
    /// 페이지 로딩 중(`PageLoadEvent::Started`~`Finished`) — 헤더 작업 바와
    /// 리로드↔정지 버튼 토글이 읽는다.
    pub(crate) loading: bool,
    /// 문서 제목(`document_title_changed`). 있으면 탭 라벨이 host 대신 이걸
    /// 쓴다(Orca 의 탭 제목과 같은 규칙). 페이지 이동 시작 시 비운다 — 제목
    /// 없는 새 페이지에 옛 제목이 남으면 안 된다.
    page_title: Option<String>,
    /// 페이지 줌 배율(Cmd+= / - / 0). 1.0 = 100%.
    zoom_level: f64,
}

impl WebHost {
    /// 웹뷰가 지금 보고 있는 주소. **없을 수 있다** — 첫 네비게이션이 commit
    /// 되기 전, 그리고 서버가 죽어 로드가 실패한 뒤엔 `WKWebView.URL` 이 계속
    /// nil 이다.
    ///
    /// wry 의 `WebView::url()` 을 쓰지 않는 이유: macOS 구현(0.55.1
    /// wkwebview/mod.rs `url_from_webview`)이 그 nil 을 `unwrap` 해서 Result 가
    /// 아니라 **앱째 패닉**한다. 2026-08-27 복원 때 죽은 로컬 서버(8731)를
    /// 가리키던 웹 pane 하나가 0.5초 주소 폴링에 걸려 창 전체가 1초 만에
    /// 꺼졌다(`kasaterm-panic.log`). 못 여는 주소 하나가 앱을 죽이면 안 되므로
    /// WKWebView 에 직접 물어 Option 으로 받는다. 다른 플랫폼의 wry 구현은
    /// 빈 주소를 Err/빈 문자열로 돌려주니 그대로 쓴다.
    fn current_url(&self) -> Option<String> {
        #[cfg(target_os = "macos")]
        {
            use wry::WebViewExtMacOS;
            let wk = self.webview.webview();
            let url = unsafe { wk.URL() }?;
            let s = url.absoluteString()?.to_string();
            (!s.is_empty()).then_some(s)
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.webview.url().ok().filter(|s| !s.is_empty())
        }
    }
}

/// pane 상자 가장자리에서 webview 를 들이는 논리 px — 포커스 테두리(렌더가
/// pane 상자 모서리에 그린다)가 자식 창에 가려 안 보이는 것을 막는다.
const WEB_INSET: f32 = 2.0;

/// **시임(분할선)이 닿는 변**의 인셋 — `divider_at_px` 의 잡기 톨러런스(±6px)와
/// 같아야 한다. 자식 창이 그 띠까지 덮으면 마우스가 자식 창에 먹혀 시임을 웹
/// pane 쪽에서 잡을 수 없다(웹 pane 크기조절이 안 되던 원인). 이 띠는 본창
/// 소유로 남겨 hover 커서 전환·드래그가 일반 pane 과 똑같이 동작하게 한다.
/// 위변은 헤더(34px)가 이미 본창 띠라 따로 안 들인다.
const WEB_SEAM_INSET: f32 = 6.0;

/// 웹뷰마다 심는 keydown 훅. 자식 창이 key 인 동안 앱은 키 이벤트를 못 받으므로
/// (WKWebView 가 first responder — winit 뷰까지 안 온다), 페이지 캡처 단계에서
/// kasaterm 의 pane 단축키만 골라 IPC 로 넘긴다. Cmd+C/V/A/Z 같은 편집 계열은
/// 건드리지 않는다 — 그건 페이지 몫이다. Cmd+R 은 앱까지 갈 것 없이 그 자리에서
/// 리로드한다(브라우저 관례).
const WEB_CHORD_JS: &str = r#"(() => {
  if (window.__kasatermChords) return; window.__kasatermChords = 1;
  // 본창 host chord 와 같은 판정 — mac=Cmd, 그 외=Ctrl+Shift(chrome.rs host_mod).
  // meta 만 보면 Windows/Linux 에선 chord 가 통째로 죽는다(리뷰 지적).
  const mac = (navigator.platform || '').indexOf('Mac') >= 0;
  const post = (m) => { try { window.ipc.postMessage('chord:' + m); } catch (_) {} };
  window.addEventListener('keydown', (e) => {
    // 자동반복 무시 — 본창 chord 경로와 같은 이유(Cmd+D 를 살짝 길게 누르는
    // 것만으로 split 이 우르르 나간다).
    if (e.repeat) return;
    const host = mac ? (e.metaKey && !e.ctrlKey) : (e.ctrlKey && e.shiftKey && !e.metaKey);
    if (!host) return;
    // e.key 대신 e.code — 본창도 물리 키(KeyCode)로 판정하고, Alt(mac)와
    // Shift(win 은 host 에 포함)가 e.key 를 딴 문자로 바꾼다.
    const c = e.code || '';
    let cmd = null;
    if (e.altKey) {
      const dirs = { ArrowLeft: 'left', ArrowRight: 'right', ArrowUp: 'up', ArrowDown: 'down' };
      const d = dirs[c];
      if (d) cmd = (e.shiftKey ? 'swap-' : 'focus-') + d;
      else if (c === 'KeyI') cmd = 'devtools';
    } else if (c === 'KeyD') cmd = mac && e.shiftKey ? 'split-v' : 'split-h';
    else if (c === 'KeyE') cmd = 'split-v';
    else if (c === 'KeyW') cmd = 'close';
    else if (c === 'KeyL') cmd = 'addr';
    else if (c === 'KeyF') cmd = 'find';
    else if (c === 'KeyR') {
      // Cmd+R = 페이지 리로드(브라우저 관례). Cmd+Shift+R 은 본창과 같은 렌더러
      // 리프레시 — 화면 복구 단축키가 웹뷰 안에서만 죽으면 「고장」으로 보인다.
      if (!mac || e.shiftKey) cmd = 'refresh';
      else { e.preventDefault(); e.stopImmediatePropagation(); location.reload(); return; }
    }
    else if (c === 'BracketLeft') cmd = 'cycle-prev';
    else if (c === 'BracketRight') cmd = 'cycle-next';
    else if (c === 'KeyT') cmd = mac && e.shiftKey ? 'reopen' : 'new-window';
    else if (c === 'Equal') cmd = 'zoom-in';
    else if (c === 'Minus') cmd = 'zoom-out';
    else if (c === 'Digit0') cmd = 'zoom-reset';
    else if (/^Digit[1-9]$/.test(c)) cmd = 'win-' + c.slice(5);
    if (cmd) { e.preventDefault(); e.stopImmediatePropagation(); post(cmd); }
  }, true);
})();"#;

/// 스킴 없는 입력(`localhost:5173`)을 브라우저가 여는 주소로 만든다.
/// 개발 서버 확인이 주 용도라 기본 스킴은 http 다(로컬은 https 가 없다).
/// **주소 꼴이 아니면 검색으로 폴백한다**(Orca 주소창 규칙) — 공백이 있거나
/// 점 없는 한 단어는 도메인일 수 없으니 검색어다.
pub(crate) fn normalize_web_url(raw: &str) -> Option<String> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    if t.contains("://") {
        return Some(t.to_string());
    }
    let host = t.split('/').next().unwrap_or(t);
    let hostname = host.split(':').next().unwrap_or(host);
    let looks_host =
        !t.contains(' ') && (hostname == "localhost" || hostname.contains('.'));
    if looks_host {
        Some(format!("http://{t}"))
    } else {
        Some(format!("https://www.google.com/search?q={}", url_query_encode(t)))
    }
}

/// 검색 폴백용 최소 percent-encode — 비예약 문자만 그대로 둔다.
fn url_query_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 탭/창 라벨용 짧은 이름 — 스킴을 떼고 첫 경로 구분자까지(`localhost:5173`).
/// 세션 복원(session.rs)이 웹 leaf 라벨을 같은 규칙으로 지어야 해서 crate 공개.
pub(crate) fn short_label(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    rest.split('/').next().unwrap_or(rest).to_string()
}

/// winit 창의 NSWindow. auxwin.rs 와 같은 경로 — ns_view 는 호출 동안 살아
/// 있는 NSView* 다(창 Arc 를 우리가 쥐고 있다).
#[cfg(target_os = "macos")]
fn ns_window_of(
    w: &winit::window::Window,
) -> Option<objc2::rc::Retained<objc2_app_kit::NSWindow>> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let h = w.window_handle().ok()?;
    let RawWindowHandle::AppKit(h) = h.as_raw() else {
        return None;
    };
    unsafe {
        let view: &objc2_app_kit::NSView = h.ns_view.cast().as_ref();
        view.window()
    }
}

/// OS 기본 브라우저로 연다 — 내장 웹뷰는 쿠키 창고가 따로라 로그인·확장이
/// 필요해지면 여기로 탈출한다.
fn open_external(url: &str) -> std::result::Result<(), String> {
    #[cfg(target_os = "macos")]
    let spawned = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let spawned = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let spawned = std::process::Command::new("xdg-open").arg(url).spawn();
    spawned.map(|_| ()).map_err(|e| e.to_string())
}

/// 다운로드 목적지 — `~/Downloads/<URL 마지막 조각>`. 같은 이름이 있으면
/// ` (1)` 식으로 비킨다(브라우저 관례). 조각이 비면 `download`.
fn download_dest(url: &str) -> std::path::PathBuf {
    let name = url
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("download")
        .to_string();
    let dir = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|h| std::path::PathBuf::from(h).join("Downloads"))
        .filter(|d| d.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    let cand = dir.join(&name);
    if !cand.exists() {
        return cand;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.clone(), String::new()),
    };
    for n in 1..1000 {
        let c = dir.join(format!("{stem} ({n}){ext}"));
        if !c.exists() {
            return c;
        }
    }
    dir.join(name)
}

/// 자식 창을 본창에 접착한다. 이후 본창 드래그에 AppKit 이 자식을 함께 옮긴다.
pub(crate) fn attach_child(parent: &winit::window::Window, child: &winit::window::Window) {
    #[cfg(target_os = "macos")]
    if let (Some(p), Some(c)) = (ns_window_of(parent), ns_window_of(child)) {
        unsafe { p.addChildWindow_ordered(&c, objc2_app_kit::NSWindowOrderingMode::Above) };
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (parent, child);
}

/// 접착 해제. **orderOut(숨김) 전에 반드시 떼야 한다** — AppKit 은 child 인
/// 채로 orderOut 하면 관계가 어정쩡하게 남아 다음 orderFront 가 엉뚱한 z 로
/// 돌아온다.
pub(crate) fn detach_child(parent: &winit::window::Window, child: &winit::window::Window) {
    #[cfg(target_os = "macos")]
    if let (Some(p), Some(c)) = (ns_window_of(parent), ns_window_of(child)) {
        p.removeChildWindow(&c);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (parent, child);
}

impl App {
    /// `WebPane.host_id` 발급 — spawn 과 분리한 이유: 세션 복원은 그리드 쪽
    /// `WebPane` 을 먼저 앉히고(id 필요) 자식 창은 event_loop 가 잡히는
    /// `about_to_wait` 에서 뒤늦게 만든다(`pending_web_hosts`).
    pub(crate) fn alloc_web_host_id(&mut self) -> u64 {
        let id = self.web_host_seq;
        self.web_host_seq += 1;
        id
    }

    /// 자식 창 + webview 실물을 만들어 `web_hosts[host_id]` 에 앉힌다.
    /// 실패하면 false(창/웹뷰 생성 실패 — 호출자가 그리드 자리를 되감는다).
    ///
    /// 크기는 자리표시자 — 첫 sync 가 pane 프레임으로 맞춘다. 보이지 않게
    /// 만들어 두고 sync 가 위치를 잡은 뒤에 띄운다(안 그러면 화면 가운데서
    /// 한 프레임 번쩍하고 이동하는 게 보인다).
    pub(crate) fn spawn_web_host(
        &mut self,
        event_loop: &ActiveEventLoop,
        url: &str,
        host_id: u64,
    ) -> bool {
        let attrs = winit::window::WindowAttributes::default()
            .with_title(short_label(url))
            .with_decorations(false)
            .with_resizable(false)
            .with_visible(false)
            .with_inner_size(winit::dpi::LogicalSize::new(480.0, 360.0));
        #[cfg(target_os = "macos")]
        let attrs = {
            use winit::platform::macos::WindowAttributesExtMacOS;
            // 그림자가 있으면 pane 경계 안쪽에 창 그림자 띠가 진다 — 내장처럼
            // 보여야 하므로 끈다.
            attrs.with_has_shadow(false)
        };
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[webpane] window create failed: {e}");
                return false;
            }
        };
        // 아래 핸들러 전부 같은 패턴 — wry 콜백은 메인 스레드지만 &mut App 이
        // 없어, 소켓 명령과 같은 proxy 경로로 이벤트 루프에 넘긴다.
        let chord_proxy = self.proxy.clone();
        let title_proxy = self.proxy.clone();
        let load_proxy = self.proxy.clone();
        let popup_proxy = self.proxy.clone();
        let dl_proxy = self.proxy.clone();
        // 다운로드 목적지 기억 — macOS 는 완료 콜백의 path 가 항상 빈다(wry
        // 문서 명시). 시작 때 우리가 정한 경로를 완료 토스트가 쓴다. 두 콜백은
        // 같은 웹뷰의 메인 스레드 호출이라 Rc<RefCell> 로 충분하다.
        let dl_dests: std::rc::Rc<std::cell::RefCell<HashMap<String, std::path::PathBuf>>> =
            std::rc::Rc::new(std::cell::RefCell::new(HashMap::new()));
        let dl_dests_done = dl_dests.clone();
        // build_as_child — 같은 wry 를 쓰는 chrome.rs 패널들과 같은 이유(build()
        // 는 content view 를 갈아치워 use-after-free).
        let webview = match wry::WebViewBuilder::new()
            .with_url(url.to_string())
            .with_devtools(true)
            .with_initialization_script(WEB_CHORD_JS)
            .with_ipc_handler(move |req: wry::http::Request<String>| {
                let body = req.into_body();
                if let Some(cmd) = body.strip_prefix("chord:") {
                    let _ = chord_proxy.send_event(crate::UserEvent::WebPaneCmd {
                        host_id,
                        cmd: cmd.to_string(),
                    });
                }
            })
            .with_document_title_changed_handler(move |title| {
                let _ = title_proxy
                    .send_event(crate::UserEvent::WebTitleChanged { host_id, title });
            })
            .with_on_page_load_handler(move |ev, _url| {
                let loading = matches!(ev, wry::PageLoadEvent::Started);
                let _ =
                    load_proxy.send_event(crate::UserEvent::WebLoadState { host_id, loading });
            })
            .with_new_window_req_handler(move |url, _features| {
                // target=_blank·window.open — 전엔 소리 없이 무동작이었다.
                // Orca 처럼 앱 안에서 받되, 우리 단위는 탭이 아니라 pane split.
                let _ = popup_proxy.send_event(crate::UserEvent::WebPopup {
                    host_id,
                    url: url.clone(),
                });
                wry::NewWindowResponse::Deny
            })
            .with_download_started_handler(move |dl_url, dest| {
                // 목적지를 ~/Downloads/<파일명> 으로 — 안 정하면 진행 자체가
                // 플랫폼 기본에 맡겨져 어디로 갔는지 알 길이 없다.
                let d = download_dest(&dl_url);
                dl_dests.borrow_mut().insert(dl_url, d.clone());
                *dest = d;
                true
            })
            .with_download_completed_handler(move |dl_url, _path, ok| {
                let dest = dl_dests_done.borrow_mut().remove(&dl_url);
                let path = dest.map(|p| p.display().to_string()).unwrap_or_default();
                let _ = dl_proxy
                    .send_event(crate::UserEvent::WebDownloadDone { path, ok });
            })
            .with_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
                size: wry::dpi::LogicalSize::new(480.0, 360.0).into(),
            })
            .build_as_child(window.as_ref())
        {
            Ok(wv) => wv,
            Err(e) => {
                eprintln!("[webpane] webview build failed: {e}");
                return false;
            }
        };
        self.web_hosts.insert(
            host_id,
            WebHost {
                webview,
                window,
                last_frame: None,
                visible: false,
                last_url_poll: std::time::Instant::now(),
                loading: false,
                page_title: None,
                zoom_level: 1.0,
            },
        );
        true
    }

    /// URL 을 웹 pane 으로 연다 — `target`(요청자 pid) pane 옆 split.
    /// 같은 주소가 이미 열려 있으면 그 pane 으로 포커스만 옮긴다(open_file 의
    /// 중복 방지와 같은 규칙).
    /// URL 을 **사람이 보는 브라우저**로. 요청 pane 을 거울로 보는 기계가 있으면
    /// 그 거울에 `open-url` 을 밀어 그쪽(맥북 크롬)에서 열고, 아무도 안 보면 이
    /// 기계의 기본 브라우저. 본진(맥미니) 학생이 연 페이지를 화면공유로 보러 가지
    /// 않게 하는 길이다(2026-09-02 지시). 요청자가 탭 pid 면 outer pane 으로
    /// 접는다 — 거울 등록부는 pane id 로 산다.
    pub(crate) fn open_url_for_pane(&mut self, raw_url: &str, target: Option<&str>) {
        let Some(url) = normalize_web_url(raw_url) else {
            return;
        };
        let outer = target.map(|t| {
            self.ws
                .lock()
                .unwrap()
                .outer_for_pty(t)
                .unwrap_or_else(|| t.to_string())
        });
        let msg = serde_json::json!({ "t": "open-url", "url": url }).to_string();
        let sent = outer
            .as_deref()
            .map_or(0, |p| kasa_mcp::push_viewer_control(p, &msg));
        if sent > 0 {
            self.collab.toast = Some((
                "보고 있는 기계의 브라우저로 열었어요".to_string(),
                std::time::Instant::now(),
            ));
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
            return;
        }
        self.open_url_here(&url, None);
    }

    /// 이 기계의 기본 브라우저로 연다. `from` 은 원격 호스트가 되돌려 보낸 경우 그
    /// 로컬 거울 pane — 토스트로 어디서 온 페이지인지 말한다. 검증 리그는
    /// `KASATERM_OPEN_URL_SINK=<파일>` 로 실제 창 대신 한 줄을 남기게 한다(testkit 관례).
    pub(crate) fn open_url_here(&mut self, url: &str, from: Option<&str>) {
        match std::env::var("KASATERM_OPEN_URL_SINK") {
            Ok(path) if !path.is_empty() => {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                    let _ = writeln!(f, "{}\t{url}", from.unwrap_or("-"));
                }
            }
            _ => crate::chrome::open_url_in_browser(url),
        }
        if let Some(pane) = from {
            let who = self
                .ws
                .lock()
                .unwrap()
                .pane_character
                .get(pane)
                .cloned()
                .unwrap_or_else(|| pane.to_string());
            self.collab.toast = Some((
                format!("{who} 이 연 페이지를 이 기계 브라우저로 열었어요"),
                std::time::Instant::now(),
            ));
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
    }

    pub(crate) fn open_web_pane(
        &mut self,
        event_loop: &ActiveEventLoop,
        raw_url: &str,
        target: Option<&str>,
    ) {
        if self.tmux.is_some() {
            return;
        }
        let Some(url) = normalize_web_url(raw_url) else {
            return;
        };
        // 이미 열려 있으면 포커스만.
        let matches: Vec<(String, usize)> = {
            let ws = self.ws.lock().unwrap();
            ws.panes
                .iter()
                .filter_map(|(id, p)| {
                    p.tabs
                        .iter()
                        .position(|t| t.web().map(|w| w.url.as_str()) == Some(url.as_str()))
                        .map(|tab_idx| (id.clone(), tab_idx))
                })
                .collect()
        };
        // 트리에 실재하는(=화면에 살릴 수 있는) pane 만 재사용한다. 닫힌 웹
        // pane 은 스태시로 물러나도 ws.panes 에 남아 있어서, 그걸 잡아 포커스만
        // 옮기면 사용자 눈엔 아무 일도 안 일어난다 — 그땐 새로 연다(리그 실측).
        // HashMap 순회는 순서가 임의라, 스태시가 먼저 걸려 산 pane 을 영영
        // 못 보는 일이 없게 창에 실재하는 매치를 골라 잡는다. 딴 창에 있으면
        // 그 창으로 전환해 보여 준다(open_markdown_window 선례).
        let existing = matches
            .into_iter()
            .find_map(|(id, ti)| self.window_of_pane(&id).map(|wi| (id, ti, wi)));
        if let Some((id, tab_idx, wi)) = existing {
            self.switch_window(wi);
            {
                let mut ws = self.ws.lock().unwrap();
                if let Some(p) = ws.panes.get_mut(&id) {
                    p.active_tab = tab_idx.min(p.tabs.len().saturating_sub(1));
                    p.dirty = true;
                }
                ws.active_pane = Some(id);
            }
            self.chrome_dirty = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            return;
        }

        // 어느 pane 옆에 붙일지: 요청자 pane → 없으면 active. target 은 PTY
        // pid 일 수도(CLI 의 $KASATERM_PANE_ID), outer pane id 일 수도(팝업 —
        // 웹 pane 은 PTY 가 없어 outer_for_pty 로는 못 찾는다) 있다.
        let anchor = {
            let ws = self.ws.lock().unwrap();
            target
                .and_then(|t| {
                    if ws.panes.contains_key(t) {
                        Some(t.to_string())
                    } else {
                        ws.outer_for_pty(t)
                    }
                })
                .filter(|o| ws.panes.contains_key(o))
                .or_else(|| ws.active_pane.clone())
        };
        let Some(anchor) = anchor else {
            return;
        };

        let host_id = self.alloc_web_host_id();
        if !self.spawn_web_host(event_loop, &url, host_id) {
            return;
        }

        // 그리드 쪽 자리: PTY 없는 pane(이미지 split 과 같은 선례 — pid None,
        // resize_backend/키 입력은 PTY miss 로 자동 skip).
        let new_id = self.alloc_pane_id();
        let mut tab = PaneTab::default();
        tab.content = PaneContent::Web(WebPane { url: url.clone(), host_id });
        tab.title = Some(short_label(&url));
        tab.title_pinned = true;
        let ps = PaneState { tabs: vec![tab], dirty: true, ..Default::default() };
        self.ws.lock().unwrap().panes.insert(new_id.clone(), ps);
        let Some(layout) = self.pty_layout.as_mut() else {
            self.ws.lock().unwrap().panes.remove(&new_id);
            self.web_hosts.remove(&host_id);
            return;
        };
        if !layout.split_leaf(&anchor, kasa_pty::SplitDir::Horizontal, new_id.clone()) {
            self.ws.lock().unwrap().panes.remove(&new_id);
            self.web_hosts.remove(&host_id);
            return;
        }
        self.ws.lock().unwrap().active_pane = Some(new_id);
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.publish_pty_layout();
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        eprintln!("[webpane] open {url} (host {host_id})");
    }

    /// 자식 창들을 pane 프레임에 맞춘다. 매 루프 턴(`about_to_wait`) 호출 —
    /// 호스트가 없으면 즉시 반환이라 평소 비용은 0 이다. pane 이 사라졌으면
    /// 창을 거두고, 다른 창/뒤 탭/딴 워크스페이스로 밀려나 안 보이면 숨긴다.
    pub(crate) fn sync_web_hosts(&mut self) {
        if self.web_hosts.is_empty() {
            return;
        }
        let Some(main) = self.window.clone() else {
            return;
        };
        let Ok(origin) = main.inner_position() else {
            return;
        };
        // 전부 **OS 논리 포인트**로 계산해 Logical* 타입으로 넘긴다. 물리 px 로
        // 넘기면 winit 이 "그 창의" 배율로 환산하는데, 숨겨 둔 자식 창은 아직
        // 어느 모니터에 있는지 몰라 본창과 다른 배율을 믿을 수 있다 — 실측:
        // 본창이 1x 모니터인데 자식이 2x 를 믿어 창 꼭대기에 반쯤 잘려 붙었다.
        // 레이아웃 좌표(pad·cell·TITLE_HEIGHT)는 줌 이전 공간이라 ui_zoom 만
        // 곱하면 포인트가 된다(effective_scale 은 dpi 까지 곱해 물리 px 용).
        let origin = origin.to_logical::<f64>(main.scale_factor());
        let zoom = (self.ui_zoom as f64).max(0.1);
        let (cols, rows) = self.window_cells();
        let pad = WINDOW_PADDING + self.effective_sidebar_w();
        let rects = self.effective_leaf_rects(cols, rows);

        // ws 잠금 아래에서 「어느 host 가 어느 셀 상자에 보이나」와 「어느 host 가
        // 아직 어딘가에 존재하나」만 뽑는다 — pane_header_px 가 다시 잠그므로
        // 프레임 계산은 잠금 밖에서.
        let inline_open = self.inline_web.is_some();
        let (visible, alive): (Vec<(u64, String, (u16, u16, u16, u16))>, HashSet<u64>) = {
            let ws = self.ws.lock().unwrap();
            let mut vis = Vec::new();
            let mut alive = HashSet::new();
            for p in ws.panes.values() {
                for t in &p.tabs {
                    if let Some(w) = t.web() {
                        alive.insert(w.host_id);
                    }
                }
            }
            if !inline_open {
                for (id, x, y, w, h) in &rects {
                    let Some(p) = ws.panes.get(id) else { continue };
                    let Some(tab) = p.tabs.get(p.active_tab) else { continue };
                    if let Some(web) = tab.web() {
                        vis.push((web.host_id, id.clone(), (*x, *y, *w, *h)));
                    }
                }
            }
            (vis, alive)
        };

        // 사라진 pane 의 host 는 거둔다(창째 닫힘). 떼고 지워야 z-order 찌꺼기가
        // 안 남는다.
        let dead: Vec<u64> =
            self.web_hosts.keys().copied().filter(|k| !alive.contains(k)).collect();
        for k in dead {
            if let Some(h) = self.web_hosts.remove(&k) {
                if h.visible {
                    detach_child(&main, &h.window);
                }
            }
        }

        let shown: HashSet<u64> = visible.iter().map(|(k, _, _)| *k).collect();
        // 화면에 없는(뒤 탭·딴 워크스페이스·줌에 밀린) host 는 숨긴다.
        let to_hide: Vec<u64> = self
            .web_hosts
            .iter()
            .filter(|(k, h)| h.visible && !shown.contains(k))
            .map(|(k, _)| *k)
            .collect();
        for k in to_hide {
            if let Some(h) = self.web_hosts.get_mut(&k) {
                detach_child(&main, &h.window);
                h.window.set_visible(false);
                h.visible = false;
            }
        }

        // 이동한 host 의 새 주소 — 루프 안에서 모아 ws 잠금은 아래서 한 번만.
        let mut url_updates: Vec<(String, u64, String)> = Vec::new();
        for (host_id, pane_id, (x, y, w, h)) in visible {
            let header = self.pane_header_px(&pane_id);
            // 시임이 닿는 변만 WEB_SEAM_INSET 으로 들인다 — divider 잡기 띠를
            // 본창에 남기기 위해서다(상수 주석 참조). 창 바깥변은 시임이 없으니
            // 포커스 테두리 몫(WEB_INSET)만.
            let inset_l = if x > 0 { WEB_SEAM_INSET } else { WEB_INSET };
            let inset_r = if x + w < cols { WEB_SEAM_INSET } else { WEB_INSET };
            let inset_b = if y + h < rows { WEB_SEAM_INSET } else { WEB_INSET };
            // 위변은 헤더(34px)가 본창 잡기 띠 노릇을 하지만, ⋮ 로 헤더를 접은
            // pane 은 그 띠가 없어 좌우아래와 같은 조건이 필요하다(리뷰 지적).
            let inset_t = if header <= 0.0 && y > 0 { WEB_SEAM_INSET } else { WEB_INSET };
            let lx = origin.x + (pad + x as f32 * self.cell.w + inset_l) as f64 * zoom;
            let ly = origin.y
                + (TITLE_HEIGHT + y as f32 * self.cell.h + header + inset_t) as f64 * zoom;
            let lw = ((w as f32 * self.cell.w - inset_l - inset_r).max(1.0)) as f64 * zoom;
            let lh = ((h as f32 * self.cell.h - header - inset_t - inset_b).max(1.0)) as f64
                * zoom;
            // 비교용 정수 스냅(포인트) — 매 턴 같은 값이면 OS 호출을 안 한다.
            let frame = (
                lx.round() as i32,
                ly.round() as i32,
                lw.round().max(1.0) as u32,
                lh.round().max(1.0) as u32,
            );
            let Some(host) = self.web_hosts.get_mut(&host_id) else { continue };
            // 페이지 이동(링크·리다이렉트)을 따라간다 — WebPane.url 을 열 때
            // 주소로 박제해 두면 탭 라벨과 중복-포커스 판정이 거짓말한다.
            if host.last_url_poll.elapsed() >= std::time::Duration::from_millis(500) {
                host.last_url_poll = std::time::Instant::now();
                if let Some(cur) = host.current_url() {
                    url_updates.push((pane_id.clone(), host_id, cur));
                }
            }
            if host.last_frame.is_none() {
                // 첫 배치 한 번만 — 헤드리스 검증이 자식 창 좌표를 읽을 유일한
                // 창구다(오토캡처는 본창 wgpu 만 찍어 자식 창이 안 보인다).
                eprintln!(
                    "[webpane] place host={host_id} pane={pane_id} pt={},{} {}x{}",
                    frame.0, frame.1, frame.2, frame.3
                );
            }
            if host.last_frame != Some(frame) {
                // **크기 먼저, 위치 나중.** AppKit 리사이즈는 창의 아래변을
                // 고정하고 위로 자란다 — 위치를 먼저 잡으면 그 뒤 리사이즈가
                // 위변을 화면 밖으로 밀고, constrain 이 창을 화면 꼭대기로
                // 끌어올린다(실측: 의도 y=-1320 이 화면 상단 -1570 에 박혔다).
                let _ = host
                    .window
                    .request_inner_size(winit::dpi::LogicalSize::new(lw, lh));
                host.window
                    .set_outer_position(winit::dpi::LogicalPosition::new(lx, ly));
                let _ = host.webview.set_bounds(wry::Rect {
                    position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
                    size: wry::dpi::LogicalSize::new(lw, lh).into(),
                });
                host.last_frame = Some(frame);
            }
            if !host.visible {
                host.window.set_visible(true);
                attach_child(&main, &host.window);
                host.visible = true;
            }
        }
        if !url_updates.is_empty() {
            let mut moved: Vec<u64> = Vec::new();
            {
                let mut ws = self.ws.lock().unwrap();
                for (pid, host_id, cur) in url_updates {
                    let Some(p) = ws.panes.get_mut(&pid) else { continue };
                    for t in &mut p.tabs {
                        let PaneContent::Web(w) = &mut t.content else { continue };
                        if w.host_id == host_id && w.url != cur {
                            w.url = cur.clone();
                            p.dirty = true;
                            moved.push(host_id);
                        }
                    }
                }
            }
            // 라벨은 정본 규칙(페이지 제목 우선)으로 한 곳에서 — 여기서
            // short_label 을 직접 박으면 제목이 왔다가 주소 폴링에 덮인다.
            for host_id in moved {
                self.refresh_web_tab_label(host_id);
            }
        }
    }
}

impl App {
    /// 조종 대상 웹 pane 을 고른다. `surface` 를 주면 그 pane 의 웹 탭(활성 탭
    /// 우선), 안 주면 **살아 있는 웹 pane 이 하나일 때만** 그것 — 여럿이면
    /// 후보를 나열해 돌려준다(에이전트가 다음 호출에 %id 를 실을 수 있게).
    fn resolve_web_host(&self, surface: Option<&str>) -> std::result::Result<u64, String> {
        let ws = self.ws.lock().unwrap();
        if let Some(sid) = surface {
            let p = ws.panes.get(sid).ok_or_else(|| format!("{sid}: 그런 pane 이 없다"))?;
            return p
                .tabs
                .get(p.active_tab)
                .and_then(|t| t.web())
                .or_else(|| p.tabs.iter().find_map(|t| t.web()))
                .map(|w| w.host_id)
                .filter(|h| self.web_hosts.contains_key(h))
                .ok_or_else(|| format!("{sid} 는 웹 pane 이 아니다"));
        }
        let mut hosts: Vec<(u64, String, String)> = Vec::new();
        for (id, p) in ws.panes.iter() {
            for t in &p.tabs {
                if let Some(w) = t.web() {
                    if self.web_hosts.contains_key(&w.host_id) {
                        hosts.push((w.host_id, id.clone(), w.url.clone()));
                    }
                }
            }
        }
        match hosts.len() {
            0 => Err("열린 웹 pane 이 없다 — 먼저 `kasaterm-cli web <url>`".to_string()),
            1 => Ok(hosts[0].0),
            _ => Err(format!(
                "웹 pane 이 여럿이다 — %surface 로 골라라: {}",
                hosts
                    .iter()
                    .map(|(_, p, u)| format!("{p}({u})"))
                    .collect::<Vec<_>>()
                    .join(" · ")
            )),
        }
    }

    /// 헤더 브라우저 컨트롤(ActionKind::Web*) 실행. 뒤/앞은 wry 에 네이티브
    /// API 가 없어 history JS 로 간다 — 같은 웹뷰 안이라 권한 차이가 없다.
    pub(crate) fn web_nav(&mut self, pane_id: &str, op: &str) {
        let host_id = match self.resolve_web_host(Some(pane_id)) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("[webpane] nav {op}: {e}");
                return;
            }
        };
        let Some(host) = self.web_hosts.get(&host_id) else { return };
        let r = match op {
            "back" => host
                .webview
                .evaluate_script("history.back()")
                .map_err(|e| e.to_string()),
            "forward" => host
                .webview
                .evaluate_script("history.forward()")
                .map_err(|e| e.to_string()),
            // 로딩 중의 리로드 버튼은 정지다(브라우저 관례 — 렌더도 그때 아이콘을
            // ×로 바꾼다). wry 에 stop API 가 없어 JS 로 간다.
            "reload" if host.loading => host
                .webview
                .evaluate_script("window.stop()")
                .map_err(|e| e.to_string()),
            "reload" => host.webview.reload().map_err(|e| e.to_string()),
            "external" => {
                // 저장된 WebPane.url 이 아니라 웹뷰의 지금 주소 — 이동했으면
                // 사용자가 보고 있는 그 페이지를 열어야 한다.
                match host.current_url() {
                    Some(u) => open_external(&u),
                    None => Err("아직 주소가 없다 — 로드 전이거나 실패한 페이지".to_string()),
                }
            }
            other => Err(format!("{other}: 모르는 nav")),
        };
        if let Err(e) = r {
            eprintln!("[webpane] nav {op}: {e}");
        }
    }

    /// 웹 pane 조종 한 판. 답은 `reply` 로 — eval/text 는 wry 콜백, shot 은
    /// WebKit 스냅샷 완료 핸들러에서 늦게 온다(둘 다 메인 스레드 콜백이라
    /// GUI 를 세우지 않는다). 소켓 쪽 recv_timeout(10초)이 상한을 쥔다.
    pub(crate) fn web_drive(
        &mut self,
        op: &str,
        arg: &str,
        surface: Option<&str>,
        reply: std::sync::mpsc::Sender<std::result::Result<String, String>>,
    ) {
        let host_id = match self.resolve_web_host(surface) {
            Ok(h) => h,
            Err(e) => {
                let _ = reply.send(Err(e));
                return;
            }
        };
        let Some(host) = self.web_hosts.get(&host_id) else {
            let _ = reply.send(Err("웹 pane 창이 이미 닫혔다".to_string()));
            return;
        };
        match op {
            "url" => {
                let _ = reply.send(
                    host.current_url()
                        .ok_or_else(|| "아직 주소가 없다 — 로드 전이거나 실패한 페이지".to_string()),
                );
            }
            "eval" | "text" => {
                // text 는 eval 의 고정 스크립트일 뿐이다 — 본문 확인이 제일 잦은
                // 용도라 JS 없이 부를 수 있게 이름을 따로 냈다.
                let js = if op == "text" {
                    "document.body ? document.body.innerText : ''"
                } else {
                    arg
                };
                let cb = reply.clone();
                // 콜백 인자가 스크립트 결과의 JSON 직렬화다(직렬화 불가면 빈 값).
                if let Err(e) = host
                    .webview
                    .evaluate_script_with_callback(js, move |res| {
                        let _ = cb.send(Ok(res));
                    })
                {
                    let _ = reply.send(Err(format!("스크립트 실행 실패: {e}")));
                }
            }
            "shot" => self.web_shot(host_id, arg, reply),
            other => {
                let _ = reply.send(Err(format!("{other}: 모르는 op (eval|text|shot|url)")));
            }
        }
    }

    /// 웹뷰 화면을 PNG 로 `path` 에 저장한다. WKWebView 자체 스냅샷이라 화면
    /// 녹화 권한이 필요 없다(macOS `screencapture` 는 권한에 막힌다 — 이 앱의
    /// 오토캡처가 자체 렌더를 읽는 것과 같은 이유). 웹뷰 콘텐츠만 찍힌다.
    #[cfg(target_os = "macos")]
    fn web_shot(
        &self,
        host_id: u64,
        path: &str,
        reply: std::sync::mpsc::Sender<std::result::Result<String, String>>,
    ) {
        use wry::WebViewExtMacOS;
        if !std::path::Path::new(path).is_absolute() {
            let _ = reply.send(Err(format!("{path}: 절대 경로여야 한다")));
            return;
        }
        let Some(host) = self.web_hosts.get(&host_id) else {
            let _ = reply.send(Err("웹 pane 창이 이미 닫혔다".to_string()));
            return;
        };
        let path = path.to_string();
        let wk = host.webview.webview();
        let done = block2::RcBlock::new(
            move |img: *mut objc2_app_kit::NSImage, err: *mut objc2_foundation::NSError| {
                let out = (|| -> std::result::Result<String, String> {
                    if let Some(e) = unsafe { err.as_ref() } {
                        return Err(e.localizedDescription().to_string());
                    }
                    let img = unsafe { img.as_ref() }.ok_or("스냅샷이 비었다")?;
                    let tiff = img.TIFFRepresentation().ok_or("이미지 변환 실패")?;
                    let rep = objc2_app_kit::NSBitmapImageRep::imageRepWithData(&tiff)
                        .ok_or("비트맵 해석 실패")?;
                    let png = unsafe {
                        rep.representationUsingType_properties(
                            objc2_app_kit::NSBitmapImageFileType::PNG,
                            &objc2_foundation::NSDictionary::new(),
                        )
                    }
                    .ok_or("PNG 인코딩 실패")?;
                    if png.writeToFile_atomically(
                        &objc2_foundation::NSString::from_str(&path),
                        true,
                    ) {
                        Ok(path.clone())
                    } else {
                        Err(format!("{path}: 파일 쓰기 실패"))
                    }
                })();
                let _ = reply.send(out);
            },
        );
        unsafe { wk.takeSnapshotWithConfiguration_completionHandler(None, &done) };
    }

    #[cfg(not(target_os = "macos"))]
    fn web_shot(
        &self,
        _host_id: u64,
        _path: &str,
        reply: std::sync::mpsc::Sender<std::result::Result<String, String>>,
    ) {
        let _ = reply.send(Err("web-shot 은 아직 macOS 전용이다".to_string()));
    }

    /// 웹 자식 창의 winit 이벤트를 삼킨다(소비했으면 true). 패널 창들과 같은
    /// 이유의 가드다 — 자식 창의 Resized/ScaleFactorChanged 가 본창 로직으로
    /// 흘러들면 gpu.resize 가 자식 크기로 불려 화면이 확대되고, CloseRequested
    /// 는 앱 전체를 끝낸다. Cmd+W(자식이 key 일 때)는 그 웹 pane 을 닫는 것으로
    /// 해석한다.
    pub(crate) fn web_host_window_event(
        &mut self,
        id: winit::window::WindowId,
        event: &winit::event::WindowEvent,
    ) -> bool {
        let Some(host_id) =
            self.web_hosts.iter().find(|(_, h)| h.window.id() == id).map(|(k, _)| *k)
        else {
            return false;
        };
        match event {
            winit::event::WindowEvent::CloseRequested => {
                if let Some(pid) = self.pane_of_web_host(host_id) {
                    // pane 이 사라지면 다음 sync 가 host(창째)를 거둔다.
                    self.close_pane(&pid);
                }
            }
            winit::event::WindowEvent::Focused(true) => {
                // 웹뷰 클릭 = 그 pane 을 포커스한 것. 그 클릭은 자식 창이 삼켜
                // 본창의 셀 그리드 히트(active_pane 설정)가 못 보므로, 여기서라도
                // 따라가야 이후 분할·이동 chord 와 소켓 명령이 이 pane 을 잡는다
                // (웹 pane 스플릿이 안 되던 원인 절반 — 나머지 절반은 WEB_CHORD_JS).
                if let Some(pid) = self.pane_of_web_host(host_id) {
                    self.ws.lock().unwrap().active_pane = Some(pid);
                    // 주소창 편집 중 웹뷰를 클릭했으면 다른 곳 클릭과 같은 blur.
                    self.cancel_web_addr();
                    self.chrome_dirty = true;
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            _ => {}
        }
        true
    }

    /// host_id → 그 웹뷰가 붙은 pane(outer id). 어느 탭에 있든 찾는다.
    fn pane_of_web_host(&self, host_id: u64) -> Option<String> {
        let ws = self.ws.lock().unwrap();
        ws.panes.iter().find_map(|(pid, p)| {
            p.tabs
                .iter()
                .any(|t| t.web().map(|w| w.host_id) == Some(host_id))
                .then(|| pid.clone())
        })
    }

    /// 웹뷰 안에서 친 앱 단축키(WEB_CHORD_JS → IPC → `UserEvent::WebPaneCmd`).
    /// 대상 pane 을 활성으로 올린 뒤 본창 키보드 경로와 같은 동작을 부른다.
    /// 포커스가 터미널 pane 으로 옮겨가는 동작은 본창을 key 로 올린다 — 안
    /// 그러면 이후 타이핑이 여전히 웹뷰로 들어간다.
    pub(crate) fn web_pane_cmd(&mut self, host_id: u64, cmd: &str) {
        // 창이 닫히는 사이 도착한 IPC 잔류는 조용히 버린다.
        let Some(pid) = self.pane_of_web_host(host_id) else { return };
        // **IPC 는 신뢰 경계 밖이다** — WEB_CHORD_JS 만이 아니라 로드된 페이지의
        // 어떤 스크립트든 `window.ipc.postMessage('chord:…')` 를 부를 수 있다
        // (리뷰 실증). 사용자 제스처를 흉내낼 수는 있어도, 최소한 그 웹뷰 창이
        // key(사용자가 실제로 보고 치는 중)일 때만 받는다 — 배경 pane 의 페이지가
        // 몰래 pane 을 닫거나 쪼개는 것을 막는다.
        let focused = self
            .web_hosts
            .get(&host_id)
            .map(|h| h.window.has_focus())
            .unwrap_or(false);
        if !focused {
            eprintln!("[webpane] chord {cmd}: 웹뷰가 key 가 아니라 무시(페이지 주입 의심)");
            return;
        }
        self.ws.lock().unwrap().active_pane = Some(pid.clone());
        let mut refocus_main = true;
        match cmd {
            "split-h" | "split-v" => {
                let dir = if cmd == "split-v" {
                    kasa_pty::SplitDir::Vertical
                } else {
                    kasa_pty::SplitDir::Horizontal
                };
                if let Err(e) = self.split_active_pane(dir) {
                    eprintln!("[webpane] chord split: {e:#}");
                    refocus_main = false;
                }
            }
            "close" => {
                // 본창 Cmd+W(close_active_tab)와 같은 단위·같은 확인을 태운다 —
                // close_pane 직행은 멀티탭 pane 을 통째로 걷고 바쁨 확인 모달도
                // 우회했다(리뷰 지적). 웹뷰가 key 면 그 웹 탭이 활성 탭이다
                // (비활성 탭의 host 는 sync 가 숨겨 포커스가 갈 수 없다).
                self.close_active_tab();
            }
            "refresh" => {
                // 본창 Cmd+Shift+R 과 같은 렌더러 리프레시 — 모니터 이동 후 화면
                // 복구 단축키가 웹뷰 안에서만 안 먹으면 「고장」으로 보인다.
                self.refresh_renderer();
                refocus_main = false;
            }
            "addr" => {
                self.begin_web_addr_edit(&pid);
            }
            "find" => {
                self.begin_web_find(&pid);
            }
            "zoom-in" | "zoom-out" | "zoom-reset" => {
                self.web_zoom(host_id, cmd);
                refocus_main = false;
            }
            "devtools" => {
                self.web_devtools(host_id);
                refocus_main = false;
            }
            "cycle-prev" => self.cycle_focus(-1),
            "cycle-next" => self.cycle_focus(1),
            "new-window" => self.new_window(),
            "reopen" => self.reopen_closed_pane(),
            "focus-left" => self.focus_dir(crate::FocusDir::Left),
            "focus-right" => self.focus_dir(crate::FocusDir::Right),
            "focus-up" => self.focus_dir(crate::FocusDir::Up),
            "focus-down" => self.focus_dir(crate::FocusDir::Down),
            // swap 은 자리만 바뀌고 이 웹 pane 이 그대로 활성이다 — 키보드도
            // 웹뷰에 남는 것이 맞다.
            "swap-left" => {
                self.swap_dir(crate::FocusDir::Left);
                refocus_main = false;
            }
            "swap-right" => {
                self.swap_dir(crate::FocusDir::Right);
                refocus_main = false;
            }
            "swap-up" => {
                self.swap_dir(crate::FocusDir::Up);
                refocus_main = false;
            }
            "swap-down" => {
                self.swap_dir(crate::FocusDir::Down);
                refocus_main = false;
            }
            other => {
                // `n == 0` 가드 필수 — 페이지가 'chord:win-0' 을 직접 쏘면
                // `n - 1` 이 usize 언더플로로 debug 빌드를 통째로 죽인다(리뷰 실증).
                if let Some(n) = other
                    .strip_prefix("win-")
                    .and_then(|d| d.parse::<usize>().ok())
                    .and_then(|n| n.checked_sub(1))
                {
                    self.switch_window(n);
                } else {
                    eprintln!("[webpane] chord {other}: 모르는 동작");
                    refocus_main = false;
                }
            }
        }
        if refocus_main {
            if let Some(w) = &self.window {
                w.focus_window();
            }
        }
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}

impl App {
    /// 웹 pane 탭 라벨을 정본에서 다시 짓는다 — **페이지 제목 우선, 없으면
    /// 주소 host**(Orca 탭 라벨 규칙). 제목·로딩·주소가 바뀔 때마다 부른다.
    fn refresh_web_tab_label(&mut self, host_id: u64) {
        let label = self.web_hosts.get(&host_id).and_then(|h| h.page_title.clone());
        let mut changed = false;
        {
            let mut ws = self.ws.lock().unwrap();
            for p in ws.panes.values_mut() {
                for t in &mut p.tabs {
                    let PaneContent::Web(w) = &t.content else { continue };
                    if w.host_id != host_id {
                        continue;
                    }
                    let next = label.clone().unwrap_or_else(|| short_label(&w.url));
                    if t.title.as_deref() != Some(next.as_str()) {
                        t.title = Some(next);
                        p.dirty = true;
                        changed = true;
                    }
                }
            }
        }
        if changed {
            self.chrome_dirty = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }

    /// wry document_title_changed → 탭 라벨.
    pub(crate) fn web_title_changed(&mut self, host_id: u64, title: String) {
        let Some(host) = self.web_hosts.get_mut(&host_id) else { return };
        let t = title.trim();
        host.page_title = (!t.is_empty()).then(|| t.to_string());
        self.refresh_web_tab_label(host_id);
    }

    /// wry PageLoadEvent → 헤더 작업 바·리로드↔정지 토글. 이동 시작이면 옛
    /// 제목을 걷는다 — 제목 없는 새 페이지에 전 페이지 제목이 남으면 안 된다.
    pub(crate) fn web_load_state(&mut self, host_id: u64, loading: bool) {
        let Some(host) = self.web_hosts.get_mut(&host_id) else { return };
        host.loading = loading;
        if loading {
            host.page_title = None;
        }
        self.refresh_web_tab_label(host_id);
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// window.open/target=_blank — 요청한 웹 pane 옆에 새 웹 pane split.
    /// chord 와 같은 신뢰 경계: 그 웹뷰가 key(사용자가 방금 클릭한 창)일 때만
    /// 받는다 — 배경 pane 의 페이지가 사용자 몰래 pane 을 늘리면 안 된다.
    /// 정상 흐름(페이지 안 링크·OAuth 버튼 클릭)은 클릭 순간 그 창이 key 다.
    pub(crate) fn web_popup(
        &mut self,
        event_loop: &ActiveEventLoop,
        host_id: u64,
        url: &str,
    ) {
        let focused = self
            .web_hosts
            .get(&host_id)
            .map(|h| h.window.has_focus())
            .unwrap_or(false);
        if !focused {
            eprintln!("[webpane] popup {url}: 웹뷰가 key 가 아니라 무시(배경 팝업 차단)");
            return;
        }
        let anchor = self.pane_of_web_host(host_id);
        self.open_web_pane(event_loop, url, anchor.as_deref());
    }

    /// 페이지 줌(Cmd+= / - / 0). 배율은 host 에 남아 다음 조작의 기준이 된다.
    fn web_zoom(&mut self, host_id: u64, op: &str) {
        let Some(host) = self.web_hosts.get_mut(&host_id) else { return };
        let z = match op {
            "zoom-in" => (host.zoom_level * 1.1).min(5.0),
            "zoom-out" => (host.zoom_level / 1.1).max(0.25),
            _ => 1.0,
        };
        host.zoom_level = z;
        if let Err(e) = host.webview.zoom(z) {
            eprintln!("[webpane] zoom: {e}");
            return;
        }
        // Orca 는 우상단 배지 — 우리는 있는 토스트로 현재 배율을 알린다.
        self.set_toast(format!("웹 줌 {:.0}%", z * 100.0));
    }

    /// 인스펙터 토글(Cmd+Opt+I). WebKit 이 별도 창으로 띄운다.
    fn web_devtools(&mut self, host_id: u64) {
        let Some(host) = self.web_hosts.get(&host_id) else { return };
        if host.webview.is_devtools_open() {
            host.webview.close_devtools();
        } else {
            host.webview.open_devtools();
        }
    }

    /// 활성 pane 의 활성 탭이 웹이면 그 pane id — Cmd+L(본창) 게이트.
    pub(crate) fn active_web_pane(&self) -> Option<String> {
        let ws = self.ws.lock().unwrap();
        let id = ws.active_pane.clone()?;
        let p = ws.panes.get(&id)?;
        p.tabs.get(p.active_tab).and_then(|t| t.web()).map(|_| id)
    }

    /// 주소창 편집 시작(헤더 주소 pill 클릭·Cmd+L). 버퍼는 빈 채로 — 렌더가
    /// 현재 주소를 흐린 자리표시자로 깔고, 빈 채 Enter 는 그대로 두기다
    /// (`WebAddrEdit` 주석 참조).
    pub(crate) fn begin_web_addr_edit(&mut self, pane_id: &str) {
        // 같은 pane 을 이미 편집 중이면 no-op — pill 재클릭(커서를 옮기려는
        // 보편 제스처)이 반쯤 친 주소를 소리 없이 날리면 안 된다(리뷰 지적).
        if self.web_addr.as_ref().is_some_and(|e| e.pane == pane_id) {
            return;
        }
        // 찾기 칸과 같은 pill 자리를 쓴다 — 서로 배타.
        self.cancel_web_find();
        // 다른 pane 의 주소를 편집하다 넘어온 경우 — ImeFocus::WebAddr 는 pane 을
        // 구분하지 않아 ime_retarget 이 조기 반환하므로, 조합 중이던 음절이 새
        // pane 버퍼로 새기 전에 여기서 직접 걷는다(옛 버퍼는 어차피 버려진다).
        if self.web_addr.is_some() {
            let _ = self.hangul.flush();
            self.preedit.clear();
            self.in_preedit = false;
        }
        // 헤더가 접혀 있으면(⋮ ToggleHeader) 켠다 — pill 이 안 그려지는 채로
        // 편집만 시작되면 키보드가 보이지 않는 버퍼에 삼켜진다(리뷰 지적).
        if self.pane_header_px(pane_id) <= 0.0 {
            self.toggle_pane_header(pane_id);
        }
        self.web_addr = Some(crate::WebAddrEdit {
            pane: pane_id.to_string(),
            text: String::new(),
            cursor: 0,
        });
        // 터미널에서 조합 중이던 음절을 그쪽에 확정시키고 주소창이 조합기를 잡는다.
        self.ime_retarget(crate::ImeFocus::WebAddr);
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// 조합이 끝난 글자를 커서 자리에(`ime_retarget` 플러시도 여기로).
    pub(crate) fn web_addr_insert(&mut self, text: &str) {
        if let Some(e) = self.web_addr.as_mut() {
            crate::lineedit::insert(&mut e.text, &mut e.cursor, text);
            self.chrome_dirty = true;
        }
    }

    /// 편집 종료 공통 — `ime_focus` 를 비워야 다음 한글이 사라진 주소창으로
    /// 흘러가지 않는다(room rename 의 `end_room_rename_ime` 와 같은 이유).
    fn end_web_addr_ime(&mut self) {
        if matches!(self.ime_focus, Some(crate::ImeFocus::WebAddr)) {
            self.ime_focus = None;
        }
        self.preedit.clear();
        self.in_preedit = false;
    }

    /// 편집을 버린다(Esc·다른 곳 클릭·웹뷰 재클릭).
    pub(crate) fn cancel_web_addr(&mut self) {
        if self.web_addr.take().is_some() {
            let _ = self.hangul.flush();
            self.end_web_addr_ime();
            self.chrome_dirty = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }

    /// Enter — 친 주소로 이동. 빈 버퍼는 취소와 같다(자리표시자만 보고 Enter).
    fn commit_web_addr(&mut self) {
        let Some(mut e) = self.web_addr.take() else { return };
        if let Some(tail) = self.hangul.flush() {
            e.text.push_str(&tail);
        }
        self.end_web_addr_ime();
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        let Some(url) = normalize_web_url(&e.text) else { return };
        let host_id = {
            let ws = self.ws.lock().unwrap();
            ws.panes.get(&e.pane).and_then(|p| {
                p.tabs
                    .get(p.active_tab)
                    .and_then(|t| t.web())
                    .or_else(|| p.tabs.iter().find_map(|t| t.web()))
                    .map(|w| w.host_id)
            })
        };
        // 편집 사이 pane 이 닫혔으면 조용히 접는다.
        let Some(host_id) = host_id else { return };
        // host 가 없거나 load 가 실패했는데 WebPane.url 만 새 주소로 덮으면
        // pill·중복열기 판정이 화면과 다른 거짓말을 한다(리뷰 지적) — 실제로
        // 이동했을 때만 갱신한다.
        let Some(host) = self.web_hosts.get(&host_id) else {
            eprintln!("[webpane] load {url}: 웹뷰 창이 이미 닫혔다");
            return;
        };
        if let Err(err) = host.webview.load_url(&url) {
            eprintln!("[webpane] load {url}: {err}");
            return;
        }
        // WebPane.url·라벨 즉시 갱신 — 500ms 주소 폴링을 기다리면 옛 주소가
        // pill 에 잠깐 남아 「안 먹었나」로 보인다.
        {
            let mut ws = self.ws.lock().unwrap();
            if let Some(p) = ws.panes.get_mut(&e.pane) {
                for t in &mut p.tabs {
                    let PaneContent::Web(w) = &mut t.content else { continue };
                    if w.host_id == host_id {
                        w.url = url.clone();
                        t.title = Some(short_label(&url));
                        p.dirty = true;
                    }
                }
            }
        }
    }

    /// 페이지 내 찾기 시작(Cmd+F). 주소창과 같은 pill 자리를 쓰므로 서로 배타.
    pub(crate) fn begin_web_find(&mut self, pane_id: &str) {
        if self.web_find.as_ref().is_some_and(|e| e.pane == pane_id) {
            return;
        }
        self.cancel_web_addr();
        if self.web_find.is_some() {
            let _ = self.hangul.flush();
            self.preedit.clear();
            self.in_preedit = false;
        }
        if self.pane_header_px(pane_id) <= 0.0 {
            self.toggle_pane_header(pane_id);
        }
        self.web_find = Some(crate::WebAddrEdit {
            pane: pane_id.to_string(),
            text: String::new(),
            cursor: 0,
        });
        self.ime_retarget(crate::ImeFocus::WebFind);
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    pub(crate) fn web_find_insert(&mut self, text: &str) {
        if let Some(e) = self.web_find.as_mut() {
            crate::lineedit::insert(&mut e.text, &mut e.cursor, text);
            self.chrome_dirty = true;
        }
        self.web_find_live();
    }

    fn end_web_find_ime(&mut self) {
        if matches!(self.ime_focus, Some(crate::ImeFocus::WebFind)) {
            self.ime_focus = None;
        }
        self.preedit.clear();
        self.in_preedit = false;
    }

    /// 찾기를 접는다(Esc·다른 곳 클릭). 페이지의 선택(하이라이트)도 걷는다.
    pub(crate) fn cancel_web_find(&mut self) {
        let Some(e) = self.web_find.take() else { return };
        let _ = self.hangul.flush();
        self.end_web_find_ime();
        if let Some(host) = self.find_host_of_pane(&e.pane) {
            let _ = host.webview.evaluate_script("getSelection().removeAllRanges()");
        }
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// 찾기 대상 pane 의 host. 편집·찾기 공용 헬퍼.
    fn find_host_of_pane(&self, pane_id: &str) -> Option<&WebHost> {
        let host_id = {
            let ws = self.ws.lock().unwrap();
            ws.panes.get(pane_id).and_then(|p| {
                p.tabs
                    .get(p.active_tab)
                    .and_then(|t| t.web())
                    .or_else(|| p.tabs.iter().find_map(|t| t.web()))
                    .map(|w| w.host_id)
            })
        }?;
        self.web_hosts.get(&host_id)
    }

    /// 라이브 찾기 — 타이핑할 때마다 문서 처음부터 첫 일치로 점프한다.
    /// `window.find` 는 WebKit 이 아직 받쳐 주는 legacy API 로, 일치를
    /// 선택(하이라이트)하고 화면에 보이게 스크롤까지 해 준다.
    fn web_find_live(&mut self) {
        let Some(e) = self.web_find.as_ref() else { return };
        let Ok(q) = serde_json::to_string(&e.text) else { return };
        let Some(host) = self.find_host_of_pane(&e.pane.clone()) else { return };
        let js = format!(
            "(q=>{{getSelection().removeAllRanges(); if(q) window.find(q,false,false,true);}})({q})"
        );
        let _ = host.webview.evaluate_script(&js);
    }

    /// Enter = 다음 일치, Shift+Enter = 이전 일치(현재 선택에서 이어 간다).
    fn web_find_step(&mut self, backwards: bool) {
        let Some(e) = self.web_find.as_ref() else { return };
        if e.text.is_empty() {
            return;
        }
        let Ok(q) = serde_json::to_string(&e.text) else { return };
        let Some(host) = self.find_host_of_pane(&e.pane.clone()) else { return };
        let js = format!("window.find({q},false,{backwards},true)");
        let _ = host.webview.evaluate_script(&js);
    }

    /// 찾기 칸의 키 입력 — 주소창(web_addr_key)과 같은 얼개.
    pub(crate) fn web_find_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
        if self.web_find.is_none() {
            return false;
        }
        let alive = self
            .web_find
            .as_ref()
            .is_some_and(|e| self.window_of_pane(&e.pane).is_some());
        if !alive {
            self.cancel_web_find();
            return false;
        }
        if crate::input::is_modifier_key(event) {
            return true;
        }
        if self.modifiers.super_key() || self.modifiers.control_key() {
            if self.host_mod() && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyV))
            {
                if let Ok(text) = arboard::Clipboard::new().and_then(|mut cb| cb.get_text()) {
                    let text = text.trim().to_string();
                    if !text.is_empty() {
                        self.web_find_insert(&text);
                    }
                }
            }
            return true;
        }
        // Enter 는 lineedit 의 Submit(칸 닫기)이 아니라 「다음 일치」다 — 여기서
        // 먼저 가른다. Shift+Enter 는 이전 일치(브라우저 관례).
        if matches!(event.logical_key, Key::Named(NamedKey::Enter)) {
            if let Some(flushed) = self.hangul.flush() {
                self.web_find_insert(&flushed);
            }
            self.preedit.clear();
            self.in_preedit = false;
            self.web_find_step(self.modifiers.shift_key());
            return true;
        }
        self.ime_retarget(crate::ImeFocus::WebFind);
        #[cfg(target_os = "macos")]
        if let Some(t) = &event.text {
            if let Some(c) = t.chars().next().filter(|_| t.chars().count() == 1) {
                if (0x3130..=0x318F).contains(&(c as u32)) {
                    if let Some(done) = self.hangul.feed(c) {
                        self.web_find_insert(&done);
                    }
                    self.preedit = self.hangul.preedit().unwrap_or_default();
                    self.in_preedit = !self.preedit.is_empty();
                    self.chrome_dirty = true;
                    return true;
                }
            }
        }
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) && self.hangul.backspace()
        {
            self.preedit = self.hangul.preedit().unwrap_or_default();
            self.in_preedit = !self.preedit.is_empty();
            self.chrome_dirty = true;
            return true;
        }
        if let Some(flushed) = self.hangul.flush() {
            self.web_find_insert(&flushed);
        }
        self.preedit.clear();
        self.in_preedit = false;
        let act = match self.web_find.as_mut() {
            Some(e) => crate::lineedit::key(&mut e.text, &mut e.cursor, &event.logical_key),
            None => crate::lineedit::LineEditAction::Ignored,
        };
        match act {
            crate::lineedit::LineEditAction::Cancel => self.cancel_web_find(),
            crate::lineedit::LineEditAction::Edited => self.web_find_live(),
            _ => {}
        }
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
        true
    }

    /// 세션 복원이 미룬 웹 자식 창들을 만든다(about_to_wait — 여기가 복원 뒤
    /// 처음으로 ActiveEventLoop 를 쥐는 자리다).
    pub(crate) fn drain_pending_web_hosts(&mut self, event_loop: &ActiveEventLoop) {
        if self.pending_web_hosts.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.pending_web_hosts);
        for (host_id, url) in pending {
            if !self.spawn_web_host(event_loop, &url, host_id) {
                eprintln!("[webpane] 복원 host {host_id} 생성 실패 — {url}");
            }
        }
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// 주소창 편집 중의 키 입력(`forward_key` 최상단에서 가로챈다). 처리했으면
    /// true — 편집 중 타이핑이 셸·웹뷰로 새면 안 된다. 한글은 room rename 과
    /// 같은 자체 조합 경로(한글 도메인이 실재한다).
    pub(crate) fn web_addr_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
        if self.web_addr.is_none() {
            return false;
        }
        // 편집 중 pane 이 마우스 밖 경로(소켓 close·autobusyclose)로 닫히면
        // pill 은 안 그려지는데 이 가로채기만 남아 키보드 전체를 보이지 않는
        // 버퍼가 삼킨다(리뷰 지적) — 트리에 실존할 때만 편집이 유효하다.
        let alive = self
            .web_addr
            .as_ref()
            .is_some_and(|e| self.window_of_pane(&e.pane).is_some());
        if !alive {
            self.cancel_web_addr();
            return false;
        }
        if crate::input::is_modifier_key(event) {
            return true;
        }
        // 주소는 대개 붙여넣는다 — host+V 만 버퍼 삽입으로 통과시키고 나머지
        // Cmd/Ctrl 조합은 삼킨다(흘리면 편집 중인데 셸이 그 키를 먹는다).
        if self.modifiers.super_key() || self.modifiers.control_key() {
            if self.host_mod() && matches!(event.physical_key, PhysicalKey::Code(KeyCode::KeyV))
            {
                if let Ok(text) = arboard::Clipboard::new().and_then(|mut cb| cb.get_text()) {
                    let text = text.trim().to_string();
                    if !text.is_empty() {
                        self.web_addr_insert(&text);
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                }
            }
            return true;
        }
        self.ime_retarget(crate::ImeFocus::WebAddr);
        #[cfg(target_os = "macos")]
        if let Some(t) = &event.text {
            if let Some(c) = t.chars().next().filter(|_| t.chars().count() == 1) {
                if (0x3130..=0x318F).contains(&(c as u32)) {
                    if let Some(done) = self.hangul.feed(c) {
                        self.web_addr_insert(&done);
                    }
                    self.preedit = self.hangul.preedit().unwrap_or_default();
                    self.in_preedit = !self.preedit.is_empty();
                    self.chrome_dirty = true;
                    return true;
                }
            }
        }
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) && self.hangul.backspace()
        {
            self.preedit = self.hangul.preedit().unwrap_or_default();
            self.in_preedit = !self.preedit.is_empty();
            self.chrome_dirty = true;
            return true;
        }
        if let Some(flushed) = self.hangul.flush() {
            self.web_addr_insert(&flushed);
        }
        self.preedit.clear();
        self.in_preedit = false;
        let act = match self.web_addr.as_mut() {
            Some(e) => crate::lineedit::key(&mut e.text, &mut e.cursor, &event.logical_key),
            None => crate::lineedit::LineEditAction::Ignored,
        };
        match act {
            crate::lineedit::LineEditAction::Submit => self.commit_web_addr(),
            crate::lineedit::LineEditAction::Cancel => self.cancel_web_addr(),
            _ => {}
        }
        self.chrome_dirty = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_url_normalization_defaults_to_http() {
        assert_eq!(
            normalize_web_url("localhost:5173").as_deref(),
            Some("http://localhost:5173")
        );
        assert_eq!(
            normalize_web_url(" https://example.com/a ").as_deref(),
            Some("https://example.com/a")
        );
        assert_eq!(
            normalize_web_url("example.com/path").as_deref(),
            Some("http://example.com/path")
        );
        assert_eq!(
            normalize_web_url("192.168.0.7:8080").as_deref(),
            Some("http://192.168.0.7:8080")
        );
        assert_eq!(normalize_web_url("   "), None);
    }

    #[test]
    fn web_url_falls_back_to_search_for_non_hosts() {
        // 공백·점 없는 단어는 도메인일 수 없다 — 검색으로(Orca 주소창 규칙).
        assert_eq!(
            normalize_web_url("rust winit").as_deref(),
            Some("https://www.google.com/search?q=rust%20winit")
        );
        assert_eq!(
            normalize_web_url("검색어").as_deref(),
            Some("https://www.google.com/search?q=%EA%B2%80%EC%83%89%EC%96%B4")
        );
        // 점이 있으면 주소로 남는다 — 검색 폴백이 실주소를 삼키면 안 된다.
        assert_eq!(
            normalize_web_url("example.com").as_deref(),
            Some("http://example.com")
        );
    }

    #[test]
    fn web_label_is_host_only() {
        assert_eq!(short_label("http://localhost:5173/app/x"), "localhost:5173");
        assert_eq!(short_label("example.com"), "example.com");
    }
}
