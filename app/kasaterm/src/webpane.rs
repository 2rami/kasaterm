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
}

/// pane 상자 가장자리에서 webview 를 들이는 논리 px — 포커스 테두리(렌더가
/// pane 상자 모서리에 그린다)가 자식 창에 가려 안 보이는 것을 막는다.
const WEB_INSET: f32 = 2.0;

/// 스킴 없는 입력(`localhost:5173`)을 브라우저가 여는 주소로 만든다.
/// 개발 서버 확인이 주 용도라 기본 스킴은 http 다(로컬은 https 가 없다).
pub(crate) fn normalize_web_url(raw: &str) -> Option<String> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    if t.contains("://") {
        Some(t.to_string())
    } else {
        Some(format!("http://{t}"))
    }
}

/// 탭/창 라벨용 짧은 이름 — 스킴을 떼고 첫 경로 구분자까지(`localhost:5173`).
fn short_label(url: &str) -> String {
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

/// 자식 창을 본창에 접착한다. 이후 본창 드래그에 AppKit 이 자식을 함께 옮긴다.
fn attach_child(parent: &winit::window::Window, child: &winit::window::Window) {
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
fn detach_child(parent: &winit::window::Window, child: &winit::window::Window) {
    #[cfg(target_os = "macos")]
    if let (Some(p), Some(c)) = (ns_window_of(parent), ns_window_of(child)) {
        p.removeChildWindow(&c);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (parent, child);
}

impl App {
    /// URL 을 웹 pane 으로 연다 — `target`(요청자 pid) pane 옆 split.
    /// 같은 주소가 이미 열려 있으면 그 pane 으로 포커스만 옮긴다(open_file 의
    /// 중복 방지와 같은 규칙).
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

        // 어느 pane 옆에 붙일지: 요청자 pane → 없으면 active.
        let anchor = {
            let ws = self.ws.lock().unwrap();
            target
                .and_then(|t| ws.outer_for_pty(t))
                .filter(|o| ws.panes.contains_key(o))
                .or_else(|| ws.active_pane.clone())
        };
        let Some(anchor) = anchor else {
            return;
        };

        // 자식 창 + webview. 크기는 자리표시자 — 첫 sync 가 pane 프레임으로
        // 맞춘다. 보이지 않게 만들어 두고 sync 가 위치를 잡은 뒤에 띄운다
        // (안 그러면 화면 가운데서 한 프레임 번쩍하고 이동하는 게 보인다).
        let attrs = winit::window::WindowAttributes::default()
            .with_title(short_label(&url))
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
                return;
            }
        };
        // build_as_child — 같은 wry 를 쓰는 chrome.rs 패널들과 같은 이유(build()
        // 는 content view 를 갈아치워 use-after-free).
        let webview = match wry::WebViewBuilder::new()
            .with_url(url.clone())
            .with_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
                size: wry::dpi::LogicalSize::new(480.0, 360.0).into(),
            })
            .build_as_child(window.as_ref())
        {
            Ok(wv) => wv,
            Err(e) => {
                eprintln!("[webpane] webview build failed: {e}");
                return;
            }
        };
        let host_id = self.web_host_seq;
        self.web_host_seq += 1;
        self.web_hosts.insert(
            host_id,
            WebHost {
                webview,
                window,
                last_frame: None,
                visible: false,
                last_url_poll: std::time::Instant::now(),
            },
        );

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
            for (id, x, y, w, h) in &rects {
                let Some(p) = ws.panes.get(id) else { continue };
                let Some(tab) = p.tabs.get(p.active_tab) else { continue };
                if let Some(web) = tab.web() {
                    vis.push((web.host_id, id.clone(), (*x, *y, *w, *h)));
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
            let lx = origin.x + (pad + x as f32 * self.cell.w + WEB_INSET) as f64 * zoom;
            let ly = origin.y
                + (TITLE_HEIGHT + y as f32 * self.cell.h + header + WEB_INSET) as f64 * zoom;
            let lw = ((w as f32 * self.cell.w - 2.0 * WEB_INSET).max(1.0)) as f64 * zoom;
            let lh =
                ((h as f32 * self.cell.h - header - 2.0 * WEB_INSET).max(1.0)) as f64 * zoom;
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
                if let Ok(cur) = host.webview.url() {
                    if !cur.is_empty() {
                        url_updates.push((pane_id.clone(), host_id, cur));
                    }
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
            let mut changed = false;
            {
                let mut ws = self.ws.lock().unwrap();
                for (pid, host_id, cur) in url_updates {
                    let Some(p) = ws.panes.get_mut(&pid) else { continue };
                    for t in &mut p.tabs {
                        let PaneContent::Web(w) = &mut t.content else { continue };
                        if w.host_id == host_id && w.url != cur {
                            w.url = cur.clone();
                            t.title = Some(short_label(&cur));
                            p.dirty = true;
                            changed = true;
                        }
                    }
                }
            }
            if changed {
                self.chrome_dirty = true;
                main.request_redraw();
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
            "reload" => host.webview.reload().map_err(|e| e.to_string()),
            "external" => {
                // 저장된 WebPane.url 이 아니라 웹뷰의 지금 주소 — 이동했으면
                // 사용자가 보고 있는 그 페이지를 열어야 한다.
                match host.webview.url() {
                    Ok(u) if !u.is_empty() => open_external(&u),
                    Ok(_) => Err("주소가 비어 있다".to_string()),
                    Err(e) => Err(e.to_string()),
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
                let _ = reply.send(host.webview.url().map_err(|e| e.to_string()));
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
        if let winit::event::WindowEvent::CloseRequested = event {
            let pane = {
                let ws = self.ws.lock().unwrap();
                ws.panes.iter().find_map(|(pid, p)| {
                    p.tabs
                        .iter()
                        .any(|t| t.web().map(|w| w.host_id) == Some(host_id))
                        .then(|| pid.clone())
                })
            };
            if let Some(pid) = pane {
                // pane 이 사라지면 다음 sync 가 host(창째)를 거둔다.
                self.close_pane(&pid);
            }
        }
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
        assert_eq!(normalize_web_url("   "), None);
    }

    #[test]
    fn web_label_is_host_only() {
        assert_eq!(short_label("http://localhost:5173/app/x"), "localhost:5173");
        assert_eq!(short_label("example.com"), "example.com");
    }
}
