use super::*;
mod accounts;

/// 상태줄이 이 프레임에 읽는 값. `g` 가 `self.gpu` 를 잡은 안쪽에서 부르므로
/// `&self` 로 구해야 하는 것은 부르는 쪽이 미리 풀어 넘긴다.
pub(super) struct Frame<'a> {
    pub viewport: (f32, f32),
    pub status_h: f32,
    pub cursor: (f32, f32),
    pub prefs: &'a crate::statusbar_config::Prefs,
    pub accounts: AccountSettings<'a>,
    pub claude_observations: &'a ClaudeObservations,
    pub account_flash: Option<std::time::Instant>,
    pub info_view: &'a crate::info::InfoSnap,
}

pub(super) struct Chips<'a> {
    pub bar: &'a mut state::StatusbarState,
    pub account_rect: &'a mut Option<(f32, f32, f32, f32)>,
    pub version_rect: &'a mut Option<(f32, f32, f32, f32)>,
}

pub(super) fn paint(g: &mut gpu::GpuRenderer, chips: Chips<'_>, f: &Frame<'_>) {
    let Chips { bar, account_rect, version_rect } = chips;
    let status_h = f.status_h;
    let (win_w, win_h) = f.viewport;
    let sy = win_h - status_h;
    g.rect(0.0, sy, win_w, status_h, theme::panel_bg());
    g.rect(0.0, sy, win_w, 1.0, theme::border());

    let status_prefs = f.prefs;
    let fs = 11.0_f32;
    let ty = sy + (status_h - fs) / 2.0 - 1.0;
    let mut x = 12.0_f32;
    // 커서가 얹히면 배경이 깔린다 — Orca 세그먼트가 그렇다
    // (`hover:bg-accent/70`, 앱 번들 실측 2026-09-06). 손모양만으로는
    // 「눌리는 것」이라는 표시가 약했다.
    //
    // **직전 프레임의 자리**로 그린다. 세그먼트 폭은 글자를 다 그린
    // 뒤에야 확정되는데 배경은 글자보다 먼저 깔려야 해서(나중에 그린
    // 것이 위로 온다) 이번 프레임 폭을 기다릴 수가 없다. 한 프레임 늦지만
    // 호버는 이어지는 동작이라 눈에 안 띈다.
    //
    // 눌리는 칩 전부가 같은 판을 받는다 — 설정 목록·사이드바와 같은
    // 옅은 판 하나가 「여기가 눌린다」의 유일한 표시다(플랫 정리 2026-09-14).
    {
        let (hx, hy) = f.cursor;
        for r in [
            *account_rect,
            *version_rect,
            bar.tunnel_rect,
            bar.link_rect,
            bar.chrome_rect,
            bar.res_rect,
            bar.clip_rect,
            bar.schedule_rect,
            bar.pet_rect,
        ]
        .into_iter()
        .flatten()
        {
            if hx >= r.0 && hx <= r.0 + r.2 && hy >= r.1 && hy <= r.1 + r.3 {
                round_rect(
                    g,
                    r.0,
                    r.1 + 3.0,
                    r.2,
                    (r.3 - 6.0).max(1.0),
                    theme::radius_sm(),
                    theme::surface_hover(),
                );
            }
        }
    }
    let seg_x0 = x;
    let widget_count = status_prefs.order.iter().filter(|id| status_prefs.visible(id))
        .map(|id| statusbar_tool_weight(id) as usize).sum::<usize>().max(1);
    let badges = accounts::badges(f);
    let account_width = accounts::budget(win_w, widget_count, badges.len());
    let account_right = seg_x0 + account_width;
    let compact_tools = (win_w - account_right - 24.0) / (widget_count as f32) < 28.0;
    let used = accounts::paint(g, f, &badges, (seg_x0, sy, account_width, status_h));
    x = seg_x0 + used;
    let acct_r = (seg_x0 - 6.0, sy, used + 12.0, status_h);
    *account_rect = (used > 0.0).then_some(acct_r);
    if used > 0.0 {
        g.hover_pointer |= f.cursor.0 >= acct_r.0 && f.cursor.0 <= acct_r.0 + acct_r.2 && f.cursor.1 >= sy;
        if let Some(k) = crate::chrome::account_flash_k(f.account_flash) { paint_account_flash(g, acct_r, k); }
        if status_prefs.separators { g.rect(x + 8.0, sy + 6.0, 1.0, (status_h - 12.0).max(1.0), theme::border()); }
    }

    // 양끝 그룹이 같은 폭을 서로 예약하지 않도록 각 도구의 공간을 먼저 나눈다.
    let tool_left = account_right + 12.0;
    let fair_w = ((win_w - 12.0 - tool_left) / widget_count as f32).max(1.0);
    // 칩 하나가 이번에 쓸 수 있는 폭. 아래 두 루프가 칩마다 다시 정한다.
    let mut slot_w: f32;
    // 아직 안 그린 칩들 몫의 합. 칩마다 직전 프레임에 실제로 쓴 폭만 잡는다.
    let mut later_reserve: f32 = status_prefs
        .order
        .iter()
        .filter(|id| statusbar_tool_weight(id) > 0.0)
        .filter(|id| status_prefs.visible(id))
        .map(|id| {
            statusbar_tool_reserve(
                bar.tool_used.get(id.as_str()).copied(),
                fair_w * statusbar_tool_weight(id),
            )
        })
        .sum();
    // 오른쪽 끝에서 왼쪽으로 자라는 자들의 공통 기준선. 판 번호가
    // 이 끝을 먼저 먹고, 터널 스위치와 나머지 칩이 그 왼쪽으로 선다.
    let right_edge = win_w - 12.0;
    // 칩 사이 간격. 12 로는 아이콘·글자가 서로 붙어 어디까지가 한 칩인지
    // 눈으로 안 갈렸다(2026-09-07 지적 「간격이 너무 없어서」).
    let chip = 12.0_f32.min(fair_w * 0.2);
    let tool_icon = 12.0_f32.min((fair_w - chip - 2.0).max(1.0));
    // 칩은 저마다 오른쪽에 `chip` 간격을 달고 선다. 맨 오른쪽 칩의 그 간격을
    // 바깥 여백 자리에 겹쳐야 오른쪽 끝이 왼쪽 계정 줄과 같은 12 에 선다.
    let mut rx = right_edge + chip;
    *version_rect = None;
    bar.tunnel_rect = None;
    bar.link_rect = None;
    bar.chrome_rect = None;
    bar.res_rect = None;
    bar.port_rect = None;
    bar.schedule_rect = None;
    bar.pet_rect = None;
    bar.clip_rect = None;
    // 다른 기기와 어떻게·얼마나 가깝게 붙어 있나. 직통(그 기계에 바로)과
    // 중계(공용 관문 경유)는 체감이 딴판이라, 숫자와 함께 한눈에 보여야 한다.
    macro_rules! draw_link_widget {
        () => {{
        let links = crate::machinescol::status_links();
        let icon = tool_icon;
        let gap = if compact_tools { 0.0 } else { 3.0_f32 };
        if status_prefs.visible("devices") {
            // 한 칸에 요약만 — 이름은 팝오버가 말한다.
            let waiting = crate::install_pending() || matches!(crate::version::state(), crate::version::Check::Newer(_));
            let offline = links.iter().filter(|link| !link.online()).count();
            let mismatch = !crate::statusbar_config::mismatched_machines().is_empty();
            let text = if compact_tools { String::new() } else if waiting { "업데이트".into() }
                else if offline > 0 { format!("기기 {offline}대 끊김") }
                else if mismatch { "기기 · 업데이트".into() }
                else if links.is_empty() { "기기".into() } else { format!("기기 {}대", links.len()) };
            // 점 색은 붙어 있는 길의 질 — 늘 꺼져 있는 기기 하나가 점을 늘 빨갛게 두지 않게. 다 끊겼을 때만 나쁨.
            let worst = links.iter().filter(|l| l.online()).map(|l| l.tone()).min().unwrap_or(0);
            let open = matches!(bar.popover, Some((state::StatusbarPopover::Link, _)));
            let col = chip_ink(&status_prefs, "devices", open);
            let dot = if compact_tools { 0.0 } else { STATUS_DOT_GAP + STATUS_DOT };
            let text = crate::info::fit_text(g, &text, (slot_w - 24.0 - dot).max(0.0), fs, false);
            let w = g.measure_chrome_text(&text, fs, false);
            rx -= w + icon + gap + dot + chip;
            g.queue_icon("monitor", rx, sy + (status_h - icon) / 2.0, icon, col);
            g.draw_text(rx + icon + gap, ty, &text,
                gpu::DrawOpts { font_size: fs, color: col, bold: false, italic: false });
            if !compact_tools {
                status_dot(g, rx + icon + gap + w, sy, status_h, if waiting { ChipState::Todo } else if offline > 0 || mismatch { ChipState::Warn } else if links.is_empty() { ChipState::Off } else { link_state(worst) });
            }
            let r = (rx - chip / 2.0, sy, w + icon + gap + dot + chip, status_h);
            let (hx, hy) = f.cursor;
            g.hover_pointer |= hx >= r.0 && hx <= r.0 + r.2 && hy >= r.1 && hy <= r.1 + r.3;
            bar.link_rect = Some(r);
        }
        }};
    }
    // 바깥주소(터널) 스위치 — 이 줄의 **오른쪽 끝**(2026-08-15 지시
    // 「하단우측」). 폰 하단바는 좁고, 문이 닫히면 폰은 접속 자체가
    // 안 돼 스위치를 폰에 둘 이유가 없다 — 여닫는 손은 맥이다.
    // 점이 상태다: 초록=열림, 흐림=닫힘. 누르면 handler 가 토글한다.
    macro_rules! draw_tunnel_widget {
        () => {{
        // 「바깥」→「원격」이었다가 「모바일」— 원격 접속과 카사크롬 다리를 한
        // 칩으로 합치며(2026-09-08 지시 「크롬다리랑 원격을 통합」) 이 칩이
        // 말하는 것은 「이 맥 밖의 기기」가 됐다. 폰 아이콘이 뜻을 지고,
        // 나머지 설명(QR·주소·다리)은 팝오버가 한다.
        let label = if compact_tools { String::new() } else { crate::info::fit_text(g, "모바일", (slot_w - chip - 36.0).max(0.0), fs, false) };
        let icon = tool_icon;
        let dot = if compact_tools { 0.0 } else { STATUS_DOT_GAP + STATUS_DOT };
        let gap = if compact_tools { 0.0 } else { 3.0_f32 };
        let on = bar.tunnel_on == Some(true);
        // Browser connectivity now has its own chip and status dot.
        let tw = g.measure_chrome_text(&label, fs, false);
        let seg_w = icon + gap + tw + dot;
        // 판 번호가 이미 오른쪽 끝을 먹었다 — 그 왼쪽에 선다
        // (2026-09-06 지시: 버전 표시를 맨 오른쪽으로).
        let tunnel_visible = status_prefs.visible("tunnel");
        let tx = if tunnel_visible {
            rx - seg_w - chip
        } else {
            rx
        };
        let col = chip_ink(
            &status_prefs,
            "tunnel",
            matches!(bar.popover, Some((state::StatusbarPopover::Tunnel, _))),
        );
        if tunnel_visible {
            g.queue_icon("smartphone", tx, sy + (status_h - icon) / 2.0, icon, col);
            g.draw_text(
                tx + icon + gap,
                ty,
                &label,
                gpu::DrawOpts {
                    font_size: fs,
                    color: col,
                    bold: false,
                    italic: false,
                },
            );
        }
        // 점은 이름 뒤로 옮겼다 — 상태(열림/닫힘)는 이름을 읽은 **다음에**
        // 궁금해지는 것이고, 앞에 두면 지구본과 나란히 서서 둘 다 뜻이 흐려진다.
        if tunnel_visible {
            if !compact_tools {
                status_dot(g, tx + icon + gap + tw, sy, status_h,
                    if on { ChipState::Ok } else { ChipState::Off });
            }
            let r = (tx - chip / 2.0, sy, seg_w + chip, status_h);
            {
                let (hx, hy) = f.cursor;
                g.hover_pointer |= hx >= r.0
                    && hx <= r.0 + r.2
                    && hy >= r.1
                    && hy <= r.1 + r.3;
            }
            bar.tunnel_rect = Some(r);
        } else {
            bar.tunnel_rect = None;
        }
        if tunnel_visible {
            rx = tx;
        }

        // Browser choice is independent of mobile access. Keep the
        // actual selected computer visible and open its own menu.
        if tunnel_visible {
            let machine = if bar.chrome_machine.is_empty() {
                pane_identity::local_device_name().unwrap_or_else(|| "이 기기".to_string())
            } else { pane_identity::device_name(&bar.chrome_machine) };
            let name = if compact_tools { String::new() } else { crate::info::fit_text(g, &machine, (slot_w - chip - 36.0).max(0.0), fs, false) };
            let name_w = g.measure_chrome_text(&name, fs, false);
            let browser_w = icon + gap + name_w + dot;
            let bx = rx - browser_w - chip;
            let col = chip_ink(
                &status_prefs,
                "tunnel",
                matches!(bar.popover, Some((state::StatusbarPopover::Chrome, _))),
            );
            g.queue_icon("globe", bx, sy + (status_h - icon) / 2.0, icon, col);
            g.draw_text(bx + icon + gap, ty, &name, gpu::DrawOpts {
                font_size: fs, color: col, bold: false, italic: false,
            });
            if !compact_tools {
                status_dot(g, bx + icon + gap + name_w, sy, status_h,
                    match bar.chrome_reach {
                        Some(true) => ChipState::Ok,
                        Some(false) => ChipState::Warn,
                        None => ChipState::Off,
                    });
            }
            let r = (bx - chip / 2.0, sy, browser_w + chip, status_h);
            let (hx, hy) = f.cursor;
            g.hover_pointer |= hx >= r.0 && hx <= r.0 + r.2 && hy >= r.1 && hy <= r.1 + r.3;
            bar.chrome_rect = Some(r);
            rx = bx;
        }
        }};
    }
        // 리소스 — 앱 + 학생 트리 합. 폭이 좁으면 먼저 버린다:
        // 이 줄의 존재 이유는 한도(왼쪽)와 조작(바깥·포트)이다.
        macro_rules! draw_resources_widget {
            () => {{
        bar.res_rect = None;
        if let (Some((cpu, rss)), true, true) = (
            bar.res,
            win_w >= 640.0 || compact_tools,
            status_prefs.visible("resources"),
        ) {
            let gb = rss as f32 / (1024.0 * 1024.0 * 1024.0);
            let label = if gb >= 1.0 {
                format!("{cpu:.0}% · {gb:.1}G")
            } else {
                format!("{cpu:.0}% · {:.0}M", gb * 1024.0)
            };
            let label = if compact_tools { String::new() } else { crate::info::fit_text(g, &label, (slot_w - chip - 34.0).max(0.0), fs, false) };
            let lw = if label.is_empty() { tool_icon } else { g.measure_chrome_text(&label, fs, false) };
            rx -= lw + chip;
            if label.is_empty() {
                g.queue_icon("monitor", rx, sy + (status_h - tool_icon) / 2.0, tool_icon, theme::text_dim());
            }
            let open = matches!(
                bar.popover,
                Some((state::StatusbarPopover::Usage, _))
            );
            g.draw_text(
                rx,
                ty,
                &label,
                gpu::DrawOpts {
                    font_size: fs,
                    color: chip_ink(&status_prefs, "resources", open),
                    bold: false,
                    italic: false,
                },
            );
            // ── 재시작 권장 ─────────────────────────────────
            // 물리 메모리에서 **안 돌아오는 몫**(wired)이 쌓이면
            // 재부팅 말고는 회수 경로가 없다. 왼쪽 옆의 `12% · 3.1G`
            // 는 우리 트리가 쓰는 양이라 그게 아무리 커도 이 말을
            // 대신하지 못한다(2026-08-27 지시).
            //
            // 계정 게이지는 **늘 있는 값**이라 중립색으로 두지만
            // (위 주석), 이건 반대다 — 평소엔 아예 없다가 뜨는
            // 신호라 흐리게 하면 뜬 줄을 모른다. 늘 깜빡여서 무뎌질
            // 걱정이 없는 자리에서만 색을 쓴다.
            // 라벨 오른쪽 끝. 아래에서 `rx` 를 경고 자리로 옮기므로
            // 손잡이 폭을 재려면 여기서 먼저 잡아 둬야 한다.
            let label_right = rx + lw;
            let mut seg_x = rx;
            // 경고는 **하나만** 세운다. 하단바는 좁아서 둘을 나란히
            // 두면 라벨이 다 잘리고 삼각형 두 개만 남는다. 급한 것부터
            // 고른다: 메모리 위험 → 계속 태우는 앱 → 메모리 주의.
            //
            // CPU 를 함께 보는 것은 메모리만으로는 「지금 왜 시끄러운가」에
            // 답을 못 하기 때문이다 — 실측 2026-08-27 에 팬이 도는데
            // 메모리는 정상(wired 17%)이었고, 범인은 코어를 계속 태우던
            // 바깥 앱들이었다(「안조용한데 위젯좀 잘만들어봐」).
            let mem_adv = bar
                .mem
                .map_or(crate::sysmem::Advice::Ok, |m| m.advice());
            let gb = |b: u64| b as f32 / (1024.0 * 1024.0 * 1024.0);
            // 목록 순서에 기대지 않고 **잣대별로 골라 온다** —
            // `usage_outside` 는 두 잣대의 후보를 합쳐 둔 자루라
            // 앞자리가 무엇인지 정해져 있지 않다(2026-08-29).
            let heaviest = bar.usage_outside.iter().max_by_key(|a| a.rss);
            let hottest = bar
                .usage_outside
                .iter()
                .filter(|a| a.is_hog())
                .max_by(|a, b| a.cpu.total_cmp(&b.cpu));
            let warn: Option<(bool, String)> = if mem_adv.is_danger() {
                let w = match (mem_adv, heaviest) {
                    // 「메모리 부족」에는 **범인 이름을 붙인다**. 그게
                    // 재시작과 갈리는 지점이라서다 — 재시작은 할 일이
                    // 하나뿐이라 이름이 필요 없지만, 비우는 쪽은 무엇을
                    // 닫아야 하는지를 모르면 조언이 아니다(2026-08-27
                    // 지시: 「위험! 종료할까요?(뭔지)」).
                    (crate::sysmem::Advice::FreeUp, Some(a)) => format!(
                        "메모리 부족 · {} {:.0}G",
                        crate::input::short_app_name(&a.name),
                        gb(a.rss)
                    ),
                    (crate::sysmem::Advice::FreeUp, None) => "메모리 부족".to_string(),
                    // 하단바용으로 「맥북」을 뗀다 — 이 줄에 뜨는 것은
                    // 전부 이 기계 얘기라 그 두 글자가 자리만 먹는다.
                    _ => "재시작 권장".to_string(),
                };
                Some((true, w))
            } else if let Some(a) = hottest {
                Some((
                    true,
                    format!("{} {:.0}%", crate::input::short_app_name(&a.name), a.cpu),
                ))
            // 우리 자신은 바깥 앱 **뒤**다. 저쪽은 눌러서 닫을 수
            // 있지만 이건 손 쓸 데가 없어서, 둘 다 걸렸을 때 답이
            // 있는 쪽을 먼저 말한다.
            } else if crate::input::is_hot(bar.usage_self.1) {
                Some((true, format!("카사텀 {:.0}%", bar.usage_self.0)))
            } else if mem_adv != crate::sysmem::Advice::Ok {
                Some((false, "메모리 주의".to_string()))
            } else {
                None
            };
            if let Some((danger, words)) = warn.filter(|_| !compact_tools) {
                let col = if danger {
                    theme::danger()
                } else {
                    theme::syn_number()
                };
                let icon = 12.0_f32;
                // 좁으면 글자를 버리고 아이콘만 — 누르면 팝오버가
                // 무슨 일인지 다 적어 준다.
                let words = crate::info::fit_text(g, &words, (slot_w - chip - lw - 34.0).max(0.0), fs, false);
                let words = (!words.is_empty()).then_some(words);
                let ww = words
                    .as_ref()
                    .map_or(0.0, |t| 4.0 + g.measure_chrome_text(t, fs, false));
                seg_x -= icon + ww + 10.0;
                g.queue_icon(
                    "triangle-alert",
                    seg_x,
                    sy + (status_h - icon) / 2.0,
                    icon,
                    col,
                );
                // 뜻 색은 삼각형만 진다 — 글까지 칠하면 이 칸만 글자색으로
                // 상태를 말하게 된다. 평소엔 없던 말이라 밝은 글자로 충분히 뜬다.
                if let Some(t) = words {
                    g.draw_text(
                        seg_x + icon + 4.0,
                        ty,
                        &t,
                        gpu::DrawOpts {
                            font_size: fs,
                            color: theme::text(),
                            bold: false,
                            italic: false,
                        },
                    );
                }
                // 왼쪽 이웃(포트)이 이 자리를 침범하지 않도록.
                rx = seg_x;
            }
            // 손잡이는 경고까지 통째로 — 경고를 보고 누르는 것이
            // 자연스러운 동작인데 아이콘만 죽은 픽셀이면 헛손질이 된다.
            let rr = (seg_x - 6.0, sy, label_right - seg_x + chip, status_h);
            {
                let (hx, hy) = f.cursor;
                g.hover_pointer |=
                    hx >= rr.0 && hx <= rr.0 + rr.2 && hy >= rr.1 && hy <= rr.1 + rr.3;
            }
            bar.res_rect = Some(rr);
        }
            }};
        }
        let device_right = rx;
        for id in status_prefs
            .order
            .iter()
            .rev()
            .filter(|id| matches!(id.as_str(), "resources" | "devices" | "tunnel"))
            .filter(|id| status_prefs.visible(id))
        {
            let slot_right = rx;
            let weight = statusbar_tool_weight(id);
            let share = fair_w * weight;
            later_reserve -= statusbar_tool_reserve(
                bar.tool_used.get(id.as_str()).copied(),
                share,
            );
            let allocated =
                statusbar_slot_cap(rx - tool_left, share, later_reserve.max(0.0));
            // 터널은 모바일·브라우저 두 칩이 칸을 반씩 나눠 쓴다.
            slot_w = allocated / weight;
            g.push_clip(slot_right - allocated, sy, allocated, status_h);
            match id.as_str() {
                "resources" => draw_resources_widget!(),
                "devices" => draw_link_widget!(),
                "tunnel" => draw_tunnel_widget!(),
                _ => {}
            }
            *version_rect = version_rect.and_then(|r| if id == "version" { g.clip_hit(r) } else { Some(r) });
            bar.tunnel_rect = bar.tunnel_rect.and_then(|r| if id == "tunnel" { g.clip_hit(r) } else { Some(r) });
            bar.chrome_rect = bar.chrome_rect.and_then(|r| if id == "tunnel" { g.clip_hit(r) } else { Some(r) });
            bar.res_rect = bar.res_rect.and_then(|r| if id == "resources" { g.clip_hit(r) } else { Some(r) });
            g.pop_clip();
            rx = statusbar_next_right(slot_right, rx, allocated);
            bar.tool_used.insert(id.clone(), slot_right - rx);
        }
        // 기기 칩이 실제로 섰는지는 켜 둔 설정이 아니라 `rx` 가 움직였는지로
        // 본다 — 값이 아직 없는 칩은 이제 자리를 안 먹으므로 설정만 보면 빈
        // 자리 옆에 선만 남는다.
        let has_device = rx < device_right;
        let has_work = ["ports", "pet", "clipboard"]
            .iter()
            .any(|id| status_prefs.visible(id));
        if status_prefs.separators && has_device && has_work {
            // 선 오른쪽에도 칩 사이와 같은 간격을 둔다. 왼쪽 간격은 다음 칩이
            // 스스로 달고 온다.
            rx -= chip;
            g.rect(
                rx,
                sy + 6.0,
                1.0,
                (status_h - 12.0).max(6.0),
                theme::with_alpha(theme::border(), 140),
            );
        }
        // 클립보드 — 지금 담긴 것의 앞머리. 클립보드는 보이지 않는
        // 그릇이라, 붙여넣기 전까지 무엇이 들었는지 알 수가 없다. 칩이
        // 그걸 늘 보이게 하고, 누르면 지나간 것들이 펼쳐진다(2026-09-06
        // 지시: 「하단바에 만들자 클립보드기능 — 최근 복사한 것들 목록」).
        //
        // 목록이 비었으면 칩도 없다 — 아무것도 복사한 적 없는 창에서
        // 빈 아이콘이 자리만 먹는다.
        macro_rules! draw_clipboard_widget {
            () => {{
        bar.clip_rect = None;
        if status_prefs.visible("clipboard") {
            let head = crate::clipboard::history()
                .first()
                .map(|i| if i.secret { "비밀".to_string() } else { crate::clipboard::preview(&i.text, 8) })
                .unwrap_or_default();
            if !head.is_empty() {
                let icon = tool_icon;
                let gap = if compact_tools { 0.0 } else { 4.0_f32 };
                let head = if compact_tools { String::new() } else { crate::info::fit_text(g, &head, (slot_w - chip - 24.0).max(0.0), fs, false) };
                let lw = g.measure_chrome_text(&head, fs, false);
                let seg = icon + gap + lw;
                rx -= seg + chip;
                let open = matches!(
                    bar.popover,
                    Some((state::StatusbarPopover::Clipboard, _))
                );
                let col = status_prefs.color(
                    "clipboard",
                    if open { theme::text() } else { theme::text_dim() },
                );
                g.queue_icon("clipboard", rx, sy + (status_h - icon) / 2.0, icon, col);
                g.draw_text(
                    rx + icon + gap,
                    ty,
                    &head,
                    gpu::DrawOpts {
                        font_size: fs,
                        color: col,
                        bold: false,
                        italic: false,
                    },
                );
                let cr = (rx - chip / 2.0, sy, seg + chip, status_h);
                {
                    let (hx, hy) = f.cursor;
                    g.hover_pointer |= hx >= cr.0
                        && hx <= cr.0 + cr.2
                        && hy >= cr.1
                        && hy <= cr.1 + cr.3;
                }
                bar.clip_rect = Some(cr);
            }
        }
            }};
        }
        // 펫 — 바탕화면 캐릭터를 켜고 끈다. 펫은 앱과 프로세스가 달라
        // 앱을 껐다 켜도 살아 있고, 그래서 이 칩의 상태도 앱 메모리가 아니라
        // 그쪽 프로세스가 살아 있는지로 정한다(2026-09-07 지시: 「하단에
        // 온오프만」).
        // 예약 칩 — 보드 방 「등록됨」의 켜진 것 수. 팝오버에서 멈춤·켜기·지우기.
        macro_rules! draw_schedules_widget {
            () => {{
        bar.schedule_rect = None;
        if status_prefs.visible("schedules") {
            let n = f.info_view.schedules.iter().filter(|s| s.enabled).count();
            let label = n.to_string();
            let label = if compact_tools { String::new() } else { crate::info::fit_text(g, &label, (slot_w - chip - 24.0).max(0.0), fs, false) };
            let icon = tool_icon;
            let gap = if compact_tools { 0.0 } else { 4.0_f32 };
            let lw = g.measure_chrome_text(&label, fs, false);
            let seg = icon + gap + lw;
            rx -= seg + chip;
            let open = matches!(
                bar.popover,
                Some((state::StatusbarPopover::Schedules, _))
            );
            let col = chip_ink(&status_prefs, "schedules", open);
            g.queue_icon("rotate-cw", rx, sy + (status_h - icon) / 2.0, icon, col);
            g.draw_text(
                rx + icon + gap,
                ty,
                &label,
                gpu::DrawOpts {
                    font_size: fs,
                    color: col,
                    bold: false,
                    italic: false,
                },
            );
            let pr = (rx - chip / 2.0, sy, seg + chip, status_h);
            {
                let (hx, hy) = f.cursor;
                g.hover_pointer |=
                    hx >= pr.0 && hx <= pr.0 + pr.2 && hy >= pr.1 && hy <= pr.1 + pr.3;
            }
            bar.schedule_rect = Some(pr);
        }
            }};
        }
        macro_rules! draw_pet_widget {
            () => {{
        bar.pet_rect = None;
        if status_prefs.visible("pet") {
            let shown = crate::chrome::pet_shown_cached();
            let on = shown.is_some();
            // 누가 나와 있는지도 적는다 — 아홉 중 하나라 아이콘만으로는
            // 지금 누가 서 있는지 알 길이 없다(2026-09-07 지시).
            let name = shown.unwrap_or_default();
            let name = if compact_tools { String::new() } else { crate::info::fit_text(g, &name, (slot_w - chip - 24.0).max(0.0), fs, false) };
            let icon = tool_icon;
            let gap = if compact_tools { 0.0 } else { 4.0_f32 };
            let lw = if name.is_empty() {
                0.0
            } else {
                gap + g.measure_chrome_text(&name, fs, false)
            };
            let dot = if compact_tools { 0.0 } else { STATUS_DOT_GAP + STATUS_DOT };
            let seg = icon + lw + dot;
            rx -= seg + chip;
            let col = chip_ink(&status_prefs, "pet", false);
            g.queue_icon("sparkles", rx, sy + (status_h - icon) / 2.0, icon, col);
            if !name.is_empty() {
                g.draw_text(
                    rx + icon + gap,
                    ty,
                    &name,
                    gpu::DrawOpts {
                        font_size: fs,
                        color: col,
                        bold: false,
                        italic: false,
                    },
                );
            }
            if !compact_tools {
                status_dot(g, rx + icon + lw, sy, status_h,
                    if on { ChipState::Ok } else { ChipState::Off });
            }
            let pr = (rx - chip / 2.0, sy, seg + chip, status_h);
            {
                let (hx, hy) = f.cursor;
                g.hover_pointer |=
                    hx >= pr.0 && hx <= pr.0 + pr.2 && hy >= pr.1 && hy <= pr.1 + pr.3;
            }
            bar.pet_rect = Some(pr);
        }
            }};
        }
        // 포트 — 열려 있는 워크스페이스 포트 **개수**다. 예전엔 이 앱의
        // `:8765` 만 적었는데, 그건 이미 알고 있는 값이라 자리를 쓰면서
        // 아무것도 안 알렸다. 개수는 "지금 뭔가 떠 있나" 에 답하고, 눌러
        // 펼치면 그 목록이 나온다(2026-08-15 지시 「포트 하단바로」).
        macro_rules! draw_ports_widget {
            () => {{
        bar.port_rect = None;
        if status_prefs.visible("ports") {
            let n = f.info_view.ports.len();
            let label = n.to_string();
            let label = if compact_tools { String::new() } else { crate::info::fit_text(g, &label, (slot_w - chip - 24.0).max(0.0), fs, false) };
            let icon = tool_icon;
            let gap = if compact_tools { 0.0 } else { 4.0_f32 };
            let lw = g.measure_chrome_text(&label, fs, false);
            let seg = icon + gap + lw;
            rx -= seg + chip;
            let open = matches!(
                bar.popover,
                Some((state::StatusbarPopover::Ports, _))
            );
            let col = chip_ink(&status_prefs, "ports", open);
            g.queue_icon("plug", rx, sy + (status_h - icon) / 2.0, icon, col);
            g.draw_text(
                rx + icon + gap,
                ty,
                &label,
                gpu::DrawOpts {
                    font_size: fs,
                    color: col,
                    bold: false,
                    italic: false,
                },
            );
            let pr = (rx - chip / 2.0, sy, seg + chip, status_h);
            {
                let (hx, hy) = f.cursor;
                g.hover_pointer |=
                    hx >= pr.0 && hx <= pr.0 + pr.2 && hy >= pr.1 && hy <= pr.1 + pr.3;
            }
            bar.port_rect = Some(pr);
        }
            }};
        }

        // `rx`가 오른쪽에서 왼쪽으로 자라므로 원하는 시각 순서의 역순으로
        // 호출한다. 미리보기와 실물이 같은 `statusbar_order`를 읽는다.
        for id in status_prefs
            .order
            .iter()
            .rev()
            .filter(|id| {
                matches!(id.as_str(), "ports" | "schedules" | "pet" | "clipboard")
            })
            .filter(|id| status_prefs.visible(id))
        {
            let slot_right = rx;
            later_reserve -= statusbar_tool_reserve(
                bar.tool_used.get(id.as_str()).copied(),
                fair_w,
            );
            slot_w =
                statusbar_slot_cap(rx - tool_left, fair_w, later_reserve.max(0.0));
            g.push_clip(slot_right - slot_w, sy, slot_w, status_h);
            match id.as_str() {
                "ports" => draw_ports_widget!(),
                "schedules" => draw_schedules_widget!(),
                "pet" => draw_pet_widget!(),
                "clipboard" => draw_clipboard_widget!(),
                _ => {}
            }
            bar.port_rect = bar.port_rect.and_then(|r| if id == "ports" { g.clip_hit(r) } else { Some(r) });
            bar.schedule_rect = bar.schedule_rect.and_then(|r| if id == "schedules" { g.clip_hit(r) } else { Some(r) });
            bar.pet_rect = bar.pet_rect.and_then(|r| if id == "pet" { g.clip_hit(r) } else { Some(r) });
            bar.clip_rect = bar.clip_rect.and_then(|r| if id == "clipboard" { g.clip_hit(r) } else { Some(r) });
            g.pop_clip();
            rx = statusbar_next_right(slot_right, rx, slot_w);
            bar.tool_used.insert(id.clone(), slot_right - rx);
        }

    // 팝오버는 상태줄 **뒤**다 — 같은 자리 위로 떠야 하고, 칩을 그린
    // 뒤라야 앵커 사각형이 이번 프레임 값으로 서 있다.
    crate::statusbar::paint_popover(
        g,
        bar,
        f.info_view,
        f.cursor,
        win_w,
        win_h,
    );
}
