//! 하단 상태줄에서 **펼쳐지는 것들**. 상태줄 본체(계정 게이지·칩)는 `render.rs`
//! 의 프레임 끝에서 그리고, 여기 있는 건 그 위로 떠오르는 팝오버다.
//!
//! 분리한 이유는 순서다 — 팝오버는 pane·독·상태줄이 다 그려진 **뒤에** 얹혀야
//! 하고, 클릭도 그 역순으로 가장 먼저 잡아야 한다. 그리는 자리와 잡는 자리가
//! 둘 다 본체와 떨어져 있으니 코드도 떼어 둔다.

use crate::gpu;
use crate::state;
use crate::theme;
use crate::{hover_rect, panel_rect_outlined, round_rect};

/// 팝오버 한 행의 높이 — `info::PORT_H` 와 같은 값을 쓴다(같은 행 그림을 재사용).
const ROW_H: f32 = 32.0;
const HEAD_H: f32 = 28.0;
const GROUP_H: f32 = 22.0;
const PAD: f32 = 6.0;
const POP_W: f32 = 320.0;

fn hit(cursor: (f32, f32), r: &(f32, f32, f32, f32)) -> bool {
    cursor.0 >= r.0 && cursor.0 <= r.0 + r.2 && cursor.1 >= r.1 && cursor.1 <= r.1 + r.3
}

/// 상태줄 팝오버. 상태줄을 다 그린 **뒤에** 부른다.
///
/// `App` 메서드가 아니라 자유 함수인 건 borrow 때문이다 — 렌더는 `self.gpu` 를
/// 이미 빌린 안쪽에서 돌아서, 거기서 `&mut self` 를 다시 잡으면 컴파일이 막힌다.
/// 필드를 따로 받으면 서로 겹치지 않는 빌림이라 통과한다.
pub(crate) fn paint_popover(
    g: &mut gpu::GpuRenderer,
    sb: &mut state::StatusbarState,
    view: &crate::info::InfoSnap,
    cursor: (f32, f32),
    win_w: f32,
    win_h: f32,
) {
    sb.popover_hits.clear();
    sb.popover_rect = None;
    let Some((kind, anchor)) = sb.popover else {
        return;
    };
    match kind {
        state::StatusbarPopover::Ports => {
            paint_ports_popover(g, sb, view, cursor, anchor, win_w, win_h)
        }
        state::StatusbarPopover::Tunnel => paint_tunnel_popover(g, sb, cursor, anchor, win_w),
    }
}

/// 원격 접속 팝오버 — 여닫는 스위치 · 주소 · 복사.
///
/// 예전엔 칩을 누르는 즉시 토글이었다. 되돌릴 수 있는 조작이긴 해도 **밖으로 문을
/// 여는 일**이라 손이 스치면 열려 버렸고, 정작 열고 나면 어디로 접속하는지가 토스트
/// 한 번 뜨고 사라져 다시 확인할 길이 없었다(2026-08-15 지시 「누르면 좌측 사용량처럼
/// 펼쳐져서 거기서 조작하게 하자」). 조작과 주소를 같은 자리에 둔다.
fn paint_tunnel_popover(
    g: &mut gpu::GpuRenderer,
    sb: &mut state::StatusbarState,
    cursor: (f32, f32),
    anchor: (f32, f32, f32, f32),
    win_w: f32,
) {
    let on = sb.tunnel_on == Some(true);
    let host = on.then(|| sb.tunnel_host.clone()).flatten();
    let w = 288.0_f32.min(win_w - 16.0);
    // 닫혀 있으면 주소 줄이 통째로 빠진다 — 높이를 안 줄이면 그만큼이 빈 여백으로
    // 남아 팝오버가 이유 없이 커 보인다.
    let h = if on { 118.0 } else { 86.0 };
    let x = (anchor.0 + anchor.2 - w).clamp(8.0, (win_w - w - 8.0).max(8.0));
    let y = (anchor.1 - h - 6.0).max(8.0);
    sb.popover_rect = Some((x, y, w, h));
    panel_rect_outlined(g, x, y, w, h, theme::radius_md(), theme::surface());

    // 제목이 이름을 대신 설명한다 — 칩에는 아이콘과 두 글자밖에 안 들어가서,
    // 그것만으로 "무엇이 밖으로 열리나" 를 알 수는 없다.
    g.draw_text(
        x + 12.0,
        y + 10.0,
        "원격 접속",
        gpu::DrawOpts { font_size: 12.0, color: theme::text(), bold: true, italic: false },
    );
    g.draw_text(
        x + 12.0,
        y + 28.0,
        "폰·다른 기기에서 이 kasaterm 에 붙는다",
        gpu::DrawOpts { font_size: 10.0, color: theme::text_mute(), bold: false, italic: false },
    );
    let sw = (x + w - 12.0 - 36.0, y + 10.0, 36.0, 20.0);
    crate::settings::toggle(g, sw, on, cursor);
    sb.popover_hits.push((state::StatusbarHit::ToggleTunnel, sw));

    let line = y + 50.0;
    g.rect(x + 12.0, line, w - 24.0, 1.0, theme::with_alpha(theme::border(), 0x88));
    match host {
        Some(host) => {
            let addr = format!("https://{host}");
            let cr = (x + w - 12.0 - 22.0, line + 12.0, 22.0, 22.0);
            let ch = hit(cursor, &cr);
            g.hover_pointer |= ch;
            if ch {
                round_rect(g, cr.0, cr.1, cr.2, cr.3, theme::radius_sm(), theme::surface_hover());
            }
            g.queue_icon(
                "copy",
                cr.0 + 5.0,
                cr.1 + 5.0,
                12.0,
                if ch { theme::text() } else { theme::text_mute() },
            );
            sb.popover_hits.push((state::StatusbarHit::CopyTunnelHost, cr));
            let s = crate::info::fit_text(g, &addr, (cr.0 - x - 24.0).max(0.0), 11.0, false);
            g.draw_text(
                x + 12.0,
                line + 17.0,
                &s,
                gpu::DrawOpts { font_size: 11.0, color: theme::text(), bold: false, italic: false },
            );
            g.draw_text(
                x + 12.0,
                line + 36.0,
                "이 주소를 아는 사람은 누구나 붙을 수 있다",
                gpu::DrawOpts {
                    font_size: 10.0,
                    color: theme::with_alpha(theme::text_mute(), 0xB0),
                    bold: false,
                    italic: false,
                },
            );
        }
        None => {
            // 켜져 있는데 주소가 없으면 아직 무는 중이다 — "닫힘" 으로 적으면
            // 스위치와 어긋나 보인다.
            let msg = if on { "주소를 받는 중…" } else { "닫혀 있다 — 이 기계에서만 열린다" };
            g.draw_text(
                x + 12.0,
                line + 14.0,
                msg,
                gpu::DrawOpts { font_size: 11.0, color: theme::text_dim(), bold: false, italic: false },
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_ports_popover(
    g: &mut gpu::GpuRenderer,
    sb: &mut state::StatusbarState,
    view: &crate::info::InfoSnap,
    cursor: (f32, f32),
    anchor: (f32, f32, f32, f32),
    win_w: f32,
    win_h: f32,
) {
    // 레포별로 묶는다. 목록이 열 몇 줄이 되면 포트 번호만으로는 어느 프로젝트
    // 것인지 못 가르는데, 그 판단이 곧 "꺼도 되나" 라서 묶음이 필요하다.
    // 레포를 모르는 것(pane 이 레포 밖)은 맨 아래 이름 없는 묶음으로 모인다.
    let mut groups: Vec<(String, Vec<&crate::info::PortRow>)> = Vec::new();
    for p in &view.ports {
        match groups.iter_mut().find(|(k, _)| *k == p.repo) {
            Some((_, v)) => v.push(p),
            None => groups.push((p.repo.clone(), vec![p])),
        }
    }
    groups.sort_by_key(|(k, _)| (k.is_empty(), k.clone()));

    // 웹터미널 행은 목록이 아니라 **이 앱 자신**이라 묶음 밖 맨 위에 둔다.
    // 예전 상태줄의 `:8765` 라벨이 하던 일을 이 행이 이어받는다 — 그게 사라지면
    // 폰에서 접속할 주소를 확인할 길이 없어진다.
    let web = sb.port.clone();
    let rows = view.ports.len()
        + usize::from(web.is_some())
        + groups.iter().filter(|(k, _)| !k.is_empty()).count();
    // `+ 2.0` 은 첫 행 위 숨틈(아래 `ry` 의 시작 오프셋)이다. 이걸 빠뜨리면 그만큼
    // 마지막 행이 아래로 밀려 나가 두 줄 중 아랫줄이 잘린다.
    let body = 2.0
        + rows as f32 * ROW_H
        + groups.iter().filter(|(k, _)| !k.is_empty()).count() as f32 * (GROUP_H - ROW_H)
        + if view.ports.is_empty() { ROW_H } else { 0.0 };
    // 화면 절반을 넘기지 않는다 — 넘치면 스크롤이 있고, 팝오버가 창을 덮으면
    // 뒤의 pane 을 못 보면서 판단하게 된다.
    let max_h = (win_h * 0.5).max(160.0);
    let inner = body.min(max_h);
    let h = HEAD_H + inner + PAD;
    let w = POP_W.min(win_w - 16.0);
    // 앵커(칩)의 오른쪽 끝에 맞춰 위로. 창 왼쪽으로 넘치면 밀어 넣는다.
    let x = (anchor.0 + anchor.2 - w).clamp(8.0, (win_w - w - 8.0).max(8.0));
    let y = (anchor.1 - h - 6.0).max(8.0);
    sb.popover_rect = Some((x, y, w, h));
    panel_rect_outlined(g, x, y, w, h, theme::radius_md(), theme::surface());

    // ── 제목줄 ──
    g.draw_text(
        x + 12.0,
        y + 8.0,
        "포트",
        gpu::DrawOpts {
            font_size: 12.0,
            color: theme::text(),
            bold: true,
            italic: false,
        },
    );
    let sub = format!("{} 워크스페이스 · {} 외부", view.ports.len(), view.outside);
    let sw = g.measure_chrome_text(&sub, 10.0, false);
    g.draw_text(
        x + w - 12.0 - sw,
        y + 9.0,
        &sub,
        gpu::DrawOpts {
            font_size: 10.0,
            color: theme::text_mute(),
            bold: false,
            italic: false,
        },
    );
    let top = y + HEAD_H;
    g.rect(
        x + 1.0,
        top,
        w - 2.0,
        1.0,
        theme::with_alpha(theme::border(), 0x88),
    );

    let bottom = y + h - PAD;
    sb.popover_scroll = sb.popover_scroll.clamp(0.0, (body - inner).max(0.0));
    g.push_clip(x, top, w, (bottom - top).max(0.0));
    let x0 = x + 10.0;
    let right = x + w - 10.0;
    let mut ry = top + 2.0 - sb.popover_scroll;

    if let Some(port) = web {
        let r = (x, ry, w, ROW_H);
        if hit(cursor, &r) {
            hover_rect(g, r.0, r.1, r.2, r.3, 0.0);
            g.hover_pointer = true;
        }
        round_rect(g, x0, ry + 7.0, 6.0, 6.0, 3.0, theme::accent());
        g.draw_text(
            x0 + 12.0,
            ry + 2.0,
            &port,
            gpu::DrawOpts {
                font_size: 12.0,
                color: theme::text(),
                bold: true,
                italic: false,
            },
        );
        let pw = g.measure_chrome_text(&port, 12.0, true);
        g.draw_text(
            x0 + 12.0 + pw + 8.0,
            ry + 4.0,
            "이 kasaterm",
            gpu::DrawOpts {
                font_size: 10.0,
                color: theme::text_mute(),
                bold: false,
                italic: false,
            },
        );
        g.draw_text(
            x0 + 12.0,
            ry + 16.0,
            "웹터미널",
            gpu::DrawOpts {
                font_size: 11.0,
                color: theme::text_dim(),
                bold: false,
                italic: false,
            },
        );
        sb.popover_hits.push((state::StatusbarHit::OpenWebTerm, r));
        ry += ROW_H;
    }

    if view.ports.is_empty() {
        g.draw_text(
            x0 + 12.0,
            ry + 9.0,
            "이 워크스페이스에서 listen 중인 포트 없음",
            gpu::DrawOpts {
                font_size: 11.0,
                color: theme::text_dim(),
                bold: false,
                italic: false,
            },
        );
    }
    for (repo, list) in &groups {
        if !repo.is_empty() {
            g.draw_text(
                x0 + 2.0,
                ry + 6.0,
                repo,
                gpu::DrawOpts {
                    font_size: 10.0,
                    color: theme::text_mute(),
                    bold: true,
                    italic: false,
                },
            );
            ry += GROUP_H;
        }
        for p in list {
            if ry + ROW_H > top && ry < bottom {
                crate::info::draw_port_row(g, cursor, &mut sb.popover_hits, p, x, w, x0, right, ry);
            }
            sb.popover_hits.push((state::StatusbarHit::OpenPort(p.port), (x, ry, w, ROW_H)));
            ry += ROW_H;
        }
    }
    // 시저는 픽셀만 자르지 클릭은 안 자른다 — 스크롤 밖으로 나간 행이 그대로
    // 눌리면 안 보이는 포트가 열린다. 클립이 아직 서 있는 여기서 걸러 둔다.
    sb.popover_hits.retain_mut(|(_, r)| match g.clip_hit(*r) {
        Some(h) => {
            *r = h;
            true
        }
        None => false,
    });
    g.pop_clip();
}

impl crate::App {
    /// 팝오버 안 클릭. 상태줄 칩보다 **먼저** 봐야 한다 — 팝오버가 위에 떠 있는데
    /// 아래 칩이 먼저 잡으면 열자마자 닫히거나 엉뚱한 게 눌린다.
    ///
    /// 반환값은 "이 클릭을 삼켰나". 팝오버가 열려 있으면 바깥 클릭도 삼키고 닫는다
    /// — 안 그러면 닫으려던 클릭이 그 밑의 pane 에 들어간다.
    pub(crate) fn statusbar_popover_click(&mut self, cx: f32, cy: f32) -> bool {
        if self.statusbar.popover.is_none() {
            return false;
        }
        let inside =
            |r: &(f32, f32, f32, f32)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3;
        // 죽이기(×)를 열기(행)보다 먼저 — 둘이 겹쳐 있어 순서가 뒤집히면 ×가
        // 행에 삼켜져 영영 안 눌린다.
        let hit = self
            .statusbar
            .popover_hits
            .iter()
            .find(|(h, r)| matches!(h, state::StatusbarHit::KillPort(_)) && inside(r))
            .or_else(|| self.statusbar.popover_hits.iter().find(|(_, r)| inside(r)))
            .map(|(h, _)| h.clone());
        match hit {
            Some(state::StatusbarHit::KillPort(pid)) => {
                self.kill_process(pid, false);
                return true;
            }
            Some(state::StatusbarHit::OpenPort(port)) => {
                self.open_localhost(port);
                return true;
            }
            Some(state::StatusbarHit::OpenWebTerm) => {
                if let Some(port) = self.statusbar.port.clone() {
                    self.open_url(&format!("http://127.0.0.1:{port}/term"));
                }
                return true;
            }
            Some(state::StatusbarHit::ToggleTunnel) => {
                // 결과는 낙관 반영하고 5초 뒤 폴이 확정한다 — 끄기(TERM)는 소멸이
                // 한 박자 늦어 즉시 되물으면 아직 살아 보인다.
                let want = !self.statusbar.tunnel_on.unwrap_or(false);
                let msg = match kasa_mcp::tunnel::set(want) {
                    Ok(on) => {
                        self.statusbar.tunnel_on = Some(on);
                        if on { "원격 접속 열림" } else { "원격 접속 닫힘" }.to_string()
                    }
                    Err(e) => e,
                };
                self.statusbar.tunnel_checked = Some(std::time::Instant::now());
                self.collab.toast = Some((msg, std::time::Instant::now()));
                self.chrome_dirty = true;
                return true;
            }
            Some(state::StatusbarHit::CopyTunnelHost) => {
                if let Some(h) = self.statusbar.tunnel_host.clone() {
                    self.copy_to_clipboard(format!("https://{h}"), "원격 주소 복사됨");
                }
                return true;
            }
            None => {}
        }
        if self.statusbar.popover_rect.is_some_and(|r| inside(&r)) {
            // 팝오버 여백 — 아무 일도 안 하지만 뒤로 새지도 않는다.
            return true;
        }
        self.statusbar.popover = None;
        self.chrome_dirty = true;
        true
    }

    /// 칩 토글. 같은 것을 다시 누르면 닫고, 다른 것을 누르면 갈아탄다 — 칩들이
    /// 8px 간격으로 붙어 있어 둘이 동시에 열리면 서로를 덮는다.
    pub(crate) fn toggle_statusbar_popover(
        &mut self,
        kind: state::StatusbarPopover,
        anchor: (f32, f32, f32, f32),
    ) {
        let same = matches!(self.statusbar.popover, Some((k, _)) if k == kind);
        self.statusbar.popover = (!same).then_some((kind, anchor));
        self.statusbar.popover_scroll = 0.0;
        self.chrome_dirty = true;
    }
}
