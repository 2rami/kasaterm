//! 우측 칼럼의 「세션 기록」 탭 — 과거 대화를 골라 그 자리에서 잇는다.
//!
//! 살아 있는 tmux 세션을 보여주는 `/sessions` 패널과 다르다. 이쪽은 각 하네스가
//! 디스크에 남긴 기록(claude jsonl · codex · agy)이고, 목록을 만들려면 그 저장소를
//! 통째로 stat 해야 한다 — 그래서 수집은 `App::pump_sessions_col` 이 워커 스레드로
//! 뺀다(Info 탭이 `ps`/`lsof` 를 GUI 스레드 밖으로 뺀 것과 같은 이유).
//!
//! 별도 OS 창이 아니라 칼럼의 세 번째 탭인 건, 창을 옮기면 흩어지는 게 참고본
//! (Orca 의 상주 레일)과 정반대여서다. 칼럼은 폭 예약(`effective_right_chrome_w`
//! → `window_cells`)·드래그 리사이즈·창 리사이즈 추종을 이미 갖고 있어, 탭을 하나
//! 더 다는 것만으로 그 전부가 따라온다. wry 를 메인 창에 자식으로 붙이는 길은
//! Metal layer 충돌로 이미 접힌 판정이다(커밋 7893e95).
use super::*;
use std::sync::atomic::Ordering::Relaxed;

/// 한 행의 높이. 라벨·마지막 대화·메타 세 줄이 들어간다.
const ROW_H: f32 = 58.0;
/// 스코프 칩 줄의 높이(본문 위 고정).
const HEAD_H: f32 = 30.0;
/// 목록이 비었을 때 안내가 차지하는 높이.
const EMPTY_H: f32 = 44.0;
/// 재수집 간격. 기록은 대화가 끝나야 바뀌므로 Info 탭(1.5초)보다 훨씬 성기다 —
/// 저장소 전체를 stat 하는 비용이라 자주 돌 이유가 없다.
const REFRESH_MS: u64 = 8000;

impl App {
    /// 탭이 보일 때만 워커를 깨워 세션 목록을 새로 고친다. 닫혀 있으면 아무 일도
    /// 안 한다 — 안 보이는 목록 때문에 디스크를 훑을 이유가 없다.
    pub(crate) fn pump_sessions_col(&mut self) {
        if self.info.tab != state::SideTab::Sessions || !self.git.col_visible {
            return;
        }
        // 워커가 새 목록을 올렸을 때만 렌더용 사본으로 옮긴다. Info 탭과 달리
        // 커서 동결을 두지 않는 건 이 목록이 8초에 한 번, 그것도 대화가 끝났을
        // 때만 바뀌어서다 — 누르려는 찰나에 행이 밀릴 일이 사실상 없다.
        let rev = self.sessions_col.rev.load(Relaxed);
        if rev != self.sessions_col.seen_rev {
            if let Ok(g) = self.sessions_col.snap.lock() {
                self.sessions_col.view = g.clone();
            }
            self.sessions_col.seen_rev = rev;
        }
        if self.sessions_col.busy.load(Relaxed) {
            return;
        }
        // 「이 방」범위는 활성 pane 의 cwd 를 기준으로 삼는다. pane 을 옮겨 cwd 가
        // 달라지면 목록의 대상 자체가 바뀌므로 주기를 기다리지 않고 즉시 다시 모은다.
        let cwd = (!self.sessions_col.scope_all)
            .then(|| self.active_pane_cwd())
            .flatten();
        let cwd_changed = !self.sessions_col.scope_all && cwd != self.sessions_col.cwd;
        if cwd_changed {
            self.sessions_col.cwd = cwd.clone();
            self.sessions_col.scroll = 0.0;
        }
        let fresh = self
            .sessions_col
            .last_refresh
            .is_some_and(|t: Instant| t.elapsed() < std::time::Duration::from_millis(REFRESH_MS));
        if fresh && !cwd_changed {
            return;
        }
        self.sessions_col.last_refresh = Some(Instant::now());
        self.sessions_col.busy.store(true, Relaxed);
        let snap = self.sessions_col.snap.clone();
        let busy = self.sessions_col.busy.clone();
        let rev = self.sessions_col.rev.clone();
        let proxy = self.proxy.clone();
        let scope_all = self.sessions_col.scope_all;
        std::thread::spawn(move || {
            let mut next = match (scope_all, cwd.as_deref()) {
                (true, _) | (false, None) => kasa_socket::sessions::recent_all_sessions(40),
                (false, Some(p)) => kasa_socket::sessions::recent_sessions_here(p, 40),
            };
            // 누가 맡았던 대화인가. 스폰이 이미 남기고 있는 세션→캐릭터 표를 그대로
            // 읽는다 — claude 를 띄울 때 쓰는 `--session-id` 가 곧 jsonl 파일 이름이라
            // 목록의 `id` 로 바로 조회된다(실측: 이 레포 222개 중 182개가 이미 매핑돼
            // 있다). 그래서 기록을 새로 심지 않았다: 새 저장소를 만들면 오늘 이후
            // 세션만 얼굴이 붙고, 이미 있는 표는 그대로 놀게 된다.
            //
            // 목록을 만드는 `kasa-socket` 이 아니라 여기서 채우는 건 순환 때문이다 —
            // 표를 쥔 `kasa-mcp` 가 `kasa-socket` 을 의존한다.
            for s in next.iter_mut() {
                if let Some(n) = kasa_mcp::character::session_character(&s.id) {
                    s.student = n;
                }
            }
            // RecentSession 에 PartialEq 가 없어 (하네스, id, mtime, 학생) 으로 비교한다 —
            // 목록이 그대로면 깨우지 않아야 앱이 idle 에 잠든 채로 있는다. 학생까지 보는
            // 건 방금 시작한 세션의 바인딩이 목록보다 늦게 자리잡는 경우가 있어서다.
            let key = |v: &[kasa_socket::backend::RecentSession]| {
                v.iter()
                    .map(|s| (s.harness.clone(), s.id.clone(), s.mtime, s.student.clone()))
                    .collect::<Vec<_>>()
            };
            let changed = match snap.lock() {
                Ok(mut g) => {
                    let differs = key(&g) != key(&next);
                    if differs {
                        *g = next;
                    }
                    differs
                }
                Err(_) => false,
            };
            busy.store(false, Relaxed);
            if changed {
                rev.fetch_add(1, Relaxed);
                let _ = proxy.send_event(UserEvent::Redraw);
            }
        });
    }

    /// 활성 pane 의 cwd. 「이 방」범위가 어느 프로젝트를 뜻하는지의 기준이다.
    /// MCP 탭도 같은 기준을 쓴다 — claude 쪽 꺼짐이 폴더마다 다르다.
    pub(crate) fn active_pane_cwd(&self) -> Option<std::path::PathBuf> {
        let active = self.ws.lock().ok().and_then(|w| w.active_pane.clone())?;
        self.pane_cwd_cache.get(&active).cloned()
    }

    /// 세션 기록 탭의 클릭. 처리했으면 `true` — 호출부가 거기서 멈춘다.
    pub(crate) fn sessions_col_click(&mut self, x: f32, y: f32) -> bool {
        let inside = |r: &(f32, f32, f32, f32)| {
            x >= r.0 && x <= r.0 + r.2 && y >= r.1 && y <= r.1 + r.3
        };
        if let Some(r) = self.sessions_col.refresh_rect {
            if inside(&r) {
                // 손으로 새로 고침 — 주기 게이트를 풀어 다음 pump 가 바로 돈다.
                self.sessions_col.last_refresh = None;
                return true;
            }
        }
        for (all, r) in self.sessions_col.scope_rects.clone() {
            if inside(&r) {
                if self.sessions_col.scope_all != all {
                    self.sessions_col.scope_all = all;
                    self.sessions_col.scroll = 0.0;
                    self.sessions_col.last_refresh = None;
                }
                return true;
            }
        }
        for (idx, r) in self.sessions_col.row_rects.clone() {
            if inside(&r) {
                if let Some(s) = self.sessions_col.view.get(idx) {
                    // 이어가기는 소켓 백엔드가 쓰는 것과 같은 이벤트로 보낸다 —
                    // pane 을 띄우고 프롬프트가 뜰 즈음 명령을 주입하는 절차가
                    // handler.rs 한 곳에 있고, 그 절차를 여기서 베끼면 한쪽만
                    // 고쳐지는 쌍이 된다.
                    let _ = self.proxy.send_event(UserEvent::ResumeSession {
                        id: s.id.clone(),
                        cwd: (!s.cwd.is_empty()).then(|| s.cwd.clone()),
                        newroom: false,
                        attach: false,
                        harness: s.harness.clone(),
                    });
                }
                return true;
            }
        }
        false
    }
}

/// 하네스별 색. 테마 강조색에서 파생해 라이트/다크·강조색 변경을 따라간다 —
/// 고정 RGB 로 박으면 테마를 바꿨을 때 이 세 줄만 남의 색이 된다.
fn harness_color(harness: &str) -> [u8; 4] {
    match harness {
        "codex" => theme::accent_variant(theme::accent(), 1),
        "agy" => theme::accent_variant(theme::accent(), 2),
        _ => theme::accent(),
    }
}

/// 하네스 로고.
fn harness_icon(harness: &str) -> &'static str {
    match harness {
        "codex" => "codex",
        "agy" => "antigravity",
        _ => "claude",
    }
}

/// `mtime`(unix 초) → "12분 전". 목록을 훑는 눈이 찾는 건 절대 시각이 아니라
/// "얼마나 최근인가"다.
fn ago(mtime: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let d = now.saturating_sub(mtime);
    match d {
        0..=59 => "방금".to_string(),
        60..=3599 => format!("{}분 전", d / 60),
        3600..=86399 => format!("{}시간 전", d / 3600),
        _ => format!("{}일 전", d / 86400),
    }
}

/// 경로의 마지막 한 칸(프로젝트 이름). 전체 경로는 칼럼 폭에서 어차피 잘려
/// 앞부분만 남는데, 그 앞부분(`/Users/…`)은 어느 행이나 똑같아 아무것도 구분해
/// 주지 않는다.
fn tail(path: &str) -> &str {
    path.rsplit('/').find(|s| !s.is_empty()).unwrap_or(path)
}

/// 세션 기록 본문. `App` 메서드가 아니라 자유 함수인 건 `draw_info_col` 과 같은
/// 빌림 문제 때문이다 — 호출부가 이미 `self.gpu.as_mut()` 를 쥐고 있어 `self` 를
/// 통째로 다시 빌릴 수 없다.
pub(crate) fn draw_sessions_col(
    g: &mut gpu::GpuRenderer,
    cursor: (f32, f32),
    sc: &mut state::SessionsColState,
    x: f32,
    w: f32,
    top: f32,
    bottom: f32,
) {
    let x0 = x + 14.0;
    let right = x + w - 12.0;
    // 행 왼쪽 거터에 학생 얼굴이 앉고 텍스트 세 줄은 그만큼 들여쓴다. 머리(칩
    // 줄)와 빈 목록 안내는 거터가 없어 `x0` 을 그대로 쓴다.
    let icon_x = x + 9.0;
    let text_x = x + 35.0;
    let avail = (right - text_x).max(0.0);
    sc.row_rects.clear();
    sc.scope_rects.clear();
    sc.refresh_rect = None;
    let hit = |r: &(f32, f32, f32, f32)| {
        cursor.0 >= r.0 && cursor.0 <= r.0 + r.2 && cursor.1 >= r.1 && cursor.1 <= r.1 + r.3
    };

    // ── 범위 칩 + 새로고침 (스크롤 밖 고정) ──
    // 목록의 대상이 무엇인지는 늘 보여야 한다. 스크롤 안에 두면 기록이 몇 개만
    // 쌓여도 "지금 보는 게 이 방인가 전부인가"가 화면 밖으로 밀린다.
    let mut cx = x0;
    let chip_y = top + 6.0;
    for (all, label) in [(false, "이 방"), (true, "전체")] {
        let tw = g.measure_chrome_text(label, 11.0, false);
        let r = (cx, chip_y, tw + 16.0, 20.0);
        let on = sc.scope_all == all;
        let hov = hit(&r);
        g.hover_pointer |= hov;
        round_rect(
            g,
            r.0,
            r.1,
            r.2,
            r.3,
            theme::radius_sm(),
            match (on, hov) {
                (true, _) => theme::with_alpha(theme::accent(), 0x33),
                (false, true) => theme::surface_hover(),
                (false, false) => theme::surface(),
            },
        );
        g.draw_text(
            r.0 + 8.0,
            r.1 + 4.0,
            label,
            gpu::DrawOpts {
                font_size: 11.0,
                color: if on { theme::text() } else { theme::text_mute() },
                bold: on,
                italic: false,
            },
        );
        sc.scope_rects.push((all, r));
        cx += r.2 + 6.0;
    }
    {
        let r = (right - 18.0, chip_y + 1.0, 18.0, 18.0);
        let hov = hit(&r);
        g.hover_pointer |= hov;
        g.queue_icon(
            "refresh-cw",
            r.0,
            r.1,
            15.0,
            if hov { theme::text() } else { theme::text_mute() },
        );
        sc.refresh_rect = Some(r);
    }

    // ── 목록 ──
    let body_top = top + HEAD_H;
    let vis_h = (bottom - body_top).max(0.0);
    sc.body_rect = (x, body_top, w, vis_h);
    let rows = sc.view.len();
    sc.content_h = if rows == 0 { EMPTY_H } else { rows as f32 * ROW_H };
    // 스크롤은 그리기 **전에** clamp 한다 — 나중에 하면 목록이 줄어든 프레임에서
    // 한 번 빈 공간이 보였다가 다음 프레임에 튄다.
    let max_scroll = (sc.content_h - vis_h).max(0.0);
    sc.scroll = sc.scroll.clamp(0.0, max_scroll);

    if rows == 0 {
        g.draw_text(
            x0,
            body_top + 12.0,
            if sc.scope_all { "기록이 없다" } else { "이 방에 기록이 없다" },
            gpu::DrawOpts {
                font_size: 12.0,
                color: theme::text_mute(),
                bold: false,
                italic: false,
            },
        );
        return;
    }

    // 본문은 여기서부터 시저로 가둔다. 스크롤로 위에 반쯤 걸친 행은 예전엔 통째로
    // 그려져 범위 칩을 덮었다 — 「화면 밖이면 건너뛴다」로는 **반쯤** 걸친 것을 막을
    // 수가 없기 때문이다. 루프 **밖**에서 한 번만 세운다: 안에서 세우면 행마다
    // 세그먼트가 둘씩 쌓여 draw call 이 행 수만큼 는다.
    g.push_clip(x, body_top, w, vis_h);
    let mut y = body_top - sc.scroll;
    for (i, s) in sc.view.iter().enumerate() {
        let row_bottom = y + ROW_H;
        // 완전히 밖인 행만 건너뛴다(컬링). 반쯤 걸친 행은 그리고, 삐져나온 픽셀은
        // 시저가 자른다 — 컬링은 클리핑이 아니다. 경계를 손으로 다시 쓰지 않고
        // 클립에게 묻는 건, 그래야 클립을 옮겼을 때 컬링이 따라오기 때문이다.
        if !g.clip_visible(x, y, w, ROW_H) {
            y = row_bottom;
            continue;
        }
        let r = (x, y, w, ROW_H);
        // 시저는 픽셀만 자르지 클릭은 안 자른다. 잘려 안 보이는 부분이 눌리면
        // 엉뚱한 세션이 열리므로, 눌리는 자리는 클립과의 교집합이어야 한다.
        let Some(hitr) = g.clip_hit(r) else {
            y = row_bottom;
            continue;
        };
        let hov = hit(&hitr);
        g.hover_pointer |= hov;
        if hov {
            g.rect(r.0, r.1, r.2, r.3, theme::surface_hover());
        }
        // 거터는 **누가 맡았던 대화인가**를 먼저 말한다(2026-08-15 지시 「세션탭에서도
        // 학생프사 나오게」). 목록을 훑는 눈이 찾는 게 그거라서다 — 라벨은 무엇을
        // 시켰는지만 말하고, 같은 일을 여럿이 나눠 한 날에는 그것만으로 안 갈린다.
        //
        // 하네스 로고는 없애지 않고 얼굴 오른쪽 아래에 겹친다(2026-08-11 지적으로
        // 들어온 것이라 뺄 수 없다). 나란히 두면 거터가 두 배가 되고, 이 칼럼은
        // 좁아서 그 폭이 곧 잘리는 라벨이다.
        let face = !s.student.is_empty()
            && sprites::draw_student_face(g, &s.student, icon_x, y + (ROW_H - 22.0) / 2.0, 22.0);
        if face {
            // 얼굴 위에 그대로 얹으면 그림과 로고가 섞여 둘 다 안 읽힌다 — 밑에
            // 판을 깔아 로고만 남긴다.
            // 얼굴 상자 **안쪽**에 붙인다 — 밖으로 내밀면 그만큼 라벨과의 사이가
            // 좁아지고, 이 칼럼에서 2px 는 글자 한 칸이다.
            let bs = 13.0;
            let bx = icon_x + 22.0 - bs;
            let by = y + (ROW_H - 22.0) / 2.0 + 22.0 - bs;
            round_rect(g, bx, by, bs, bs, bs / 2.0, theme::panel_bg());
            g.queue_icon(
                harness_icon(&s.harness),
                bx + 1.5,
                by + 1.5,
                bs - 3.0,
                harness_color(&s.harness),
            );
        } else {
            // 매핑이 없는 옛 세션. 짐작해 아무 얼굴이나 붙이면 남의 대화가 남의
            // 얼굴로 보이므로 그 자리는 비우고, 하네스 로고만 원래 크기로 둔다.
            g.queue_icon(
                harness_icon(&s.harness),
                icon_x + 3.0,
                y + (ROW_H - 16.0) / 2.0,
                16.0,
                harness_color(&s.harness),
            );
        }

        // 세 줄 다 `fit_text` 로 미리 자른다. `draw_text_clipped` 는 픽셀에서
        // 끊어 마지막 글자가 반쪽으로 남는데, 목록은 어차피 훑는 화면이라
        // 말줄임이 붙은 온전한 글자가 읽힌다.
        let label = if s.label.is_empty() { s.id.as_str() } else { s.label.as_str() };
        let label = info::fit_text(g, label, avail, 13.0, false);
        g.draw_text(
            text_x,
            y + 8.0,
            &label,
            gpu::DrawOpts {
                font_size: 13.0,
                color: theme::text(),
                bold: false,
                italic: false,
            },
        );
        if !s.preview.is_empty() {
            let prev = info::fit_text(g, &s.preview, avail, 11.0, false);
            g.draw_text(
                text_x,
                y + 25.0,
                &prev,
                gpu::DrawOpts {
                    font_size: 11.0,
                    color: theme::text_dim(),
                    bold: false,
                    italic: false,
                },
            );
        }
        // 메타는 한 줄로 합쳐 그린다 — 조각마다 그리면 폭이 좁을 때 어느 조각이
        // 잘렸는지 알 수 없어 "claude · pro" 같은 꼬리가 남는다. 하네스 이름은
        // 빠졌다: 왼쪽 로고가 같은 말을 하고, 좁은 칼럼에선 그 자리만큼 프로젝트
        // 이름이 잘린다.
        let meta = format!("{} · {}", ago(s.mtime), tail(&s.cwd));
        let meta = info::fit_text(g, &meta, avail, 10.0, false);
        g.draw_text(
            text_x,
            y + 41.0,
            &meta,
            gpu::DrawOpts {
                font_size: 10.0,
                color: theme::text_mute(),
                bold: false,
                italic: false,
            },
        );
        sc.row_rects.push((i, hitr));
        y = row_bottom;
    }
    g.pop_clip();
}

#[cfg(test)]
mod harness_icon_tests {
    use super::harness_icon;
    use crate::gpu::GpuRenderer;

    /// 이름이 어긋나면 화면을 봐야만 안다 — 등록되지 않은 이름은 `icon_svg` 가
    /// `None` 을 주고 `queue_icon` 이 아무것도 그리지 않은 채 조용히 돌아간다.
    /// 행에서 로고만 빠지고 나머지는 멀쩡해 회귀로 보이지도 않는다.
    #[test]
    fn every_harness_maps_to_a_registered_icon() {
        for h in ["claude", "codex", "agy", "처음 보는 하네스"] {
            let name = harness_icon(h);
            assert!(
                GpuRenderer::icon_svg(name).is_some(),
                "{h} → \"{name}\" 이 icon_svg 목록에 없다"
            );
        }
    }

    /// antigravity 는 원본 워드마크에서 심볼만 떼어내느라 viewBox 원점이 0 이
    /// 아니다(`14 14 84 84`). 래스터라이저가 그 원점을 접어 넣지 못하면 잉크가
    /// 캔버스 밖으로 밀려 **빈 이미지**가 되는데, 그래도 예외는 안 나고 아이콘만
    /// 사라진다. 그려지는 것과 잘리지 않은 것을 함께 본다.
    #[test]
    fn an_offset_viewbox_still_lands_inside_the_canvas() {
        const PX: u32 = 32;
        for name in ["antigravity", "claude", "codex"] {
            let svg = GpuRenderer::icon_svg(name).expect("등록된 아이콘");
            let buf = GpuRenderer::rasterize_icon(svg, PX).expect("래스터");
            let alpha = |x: u32, y: u32| buf[((y * PX + x) * 4 + 3) as usize];
            let inked = (0..PX * PX).filter(|i| buf[(*i as usize) * 4 + 3] > 8).count();
            let ratio = inked as f32 / (PX * PX) as f32;
            assert!(
                (0.05..0.9).contains(&ratio),
                "{name}: 잉크 비율 {ratio:.3} — 빈 이미지이거나 캔버스를 다 덮었다"
            );
            // 가장자리 한 줄이 통째로 차 있으면 그 방향으로 잘려 나간 것이다.
            let full_row = |y: u32| (0..PX).all(|x| alpha(x, y) > 8);
            let full_col = |x: u32| (0..PX).all(|y| alpha(x, y) > 8);
            assert!(
                !full_row(0) && !full_row(PX - 1) && !full_col(0) && !full_col(PX - 1),
                "{name}: 잉크가 캔버스 가장자리에 꽉 닿았다 — viewBox 밖으로 넘쳤을 수 있다"
            );
        }
    }
}
