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
            WebHost { webview, window, last_frame: None, visible: false },
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
        let scale = self.effective_scale();
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

        for (host_id, pane_id, (x, y, w, h)) in visible {
            let header = self.pane_header_px(&pane_id);
            let lx = pad + x as f32 * self.cell.w + WEB_INSET;
            let ly = TITLE_HEIGHT + y as f32 * self.cell.h + header + WEB_INSET;
            let lw = (w as f32 * self.cell.w - 2.0 * WEB_INSET).max(1.0);
            let lh = (h as f32 * self.cell.h - header - 2.0 * WEB_INSET).max(1.0);
            let frame = (
                origin.x + (lx * scale).round() as i32,
                origin.y + (ly * scale).round() as i32,
                (lw * scale).round().max(1.0) as u32,
                (lh * scale).round().max(1.0) as u32,
            );
            let Some(host) = self.web_hosts.get_mut(&host_id) else { continue };
            if host.last_frame.is_none() {
                // 첫 배치 한 번만 — 헤드리스 검증이 자식 창 좌표를 읽을 유일한
                // 창구다(오토캡처는 본창 wgpu 만 찍어 자식 창이 안 보인다).
                eprintln!(
                    "[webpane] place host={host_id} pane={pane_id} frame={},{} {}x{}",
                    frame.0, frame.1, frame.2, frame.3
                );
            }
            if host.last_frame != Some(frame) {
                host.window
                    .set_outer_position(winit::dpi::PhysicalPosition::new(frame.0, frame.1));
                let _ = host
                    .window
                    .request_inner_size(winit::dpi::PhysicalSize::new(frame.2, frame.3));
                let _ = host.webview.set_bounds(wry::Rect {
                    position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
                    size: wry::dpi::LogicalSize::new(lw as f64, lh as f64).into(),
                });
                host.last_frame = Some(frame);
            }
            if !host.visible {
                host.window.set_visible(true);
                attach_child(&main, &host.window);
                host.visible = true;
            }
        }
    }
}

impl App {
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
