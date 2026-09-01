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

/// 두 줄짜리 행. 예전 32 는 두 줄을 밀어 넣기만 했지 사이를 두지 못해, 정체와
/// 주소가 한 덩어리로 뭉쳐 보였다(2026-08-15 「깔끔하게 뭐가 뭔지」).
const ROW_H: f32 = 44.0;
const HEAD_H: f32 = 38.0;
const GROUP_H: f32 = 30.0;
const PAD: f32 = 6.0;
const POP_W: f32 = 364.0;
/// 좌우 여백. 제목·머리·행이 같은 값을 써야 세로선이 하나로 선다.
const PADX: f32 = 14.0;
/// 포트 번호 칼럼의 **고정** 폭. 네 자리와 다섯 자리가 섞이므로 폭을 재서 이어
/// 붙이면 다음 칼럼이 행마다 들쭉날쭉해진다.
const COL_PORT: f32 = 54.0;

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
        state::StatusbarPopover::Usage => {
            paint_usage_popover(g, sb, view, cursor, anchor, win_w, win_h)
        }
    }
}

/// 자원 팝오버 — 무엇이 잡아먹는지(2026-08-15 지시 「사용량도 펼쳐져서 보이게 뭐가
/// 잡아먹는지」). 합계만으로는 "많이 쓴다" 까지만 알고 손 쓸 데를 모른다.
///
/// 목록과 제목의 합계는 **같은 표본**이다(`sample_process_tree_usage` 가 둘 다
/// 낸다). 두 소스를 쓰면 합계 74% 옆에 12% 짜리 목록이 서서 둘 다 못 믿게 된다.
fn paint_usage_popover(
    g: &mut gpu::GpuRenderer,
    sb: &mut state::StatusbarState,
    view: &crate::info::InfoSnap,
    cursor: (f32, f32),
    anchor: (f32, f32, f32, f32),
    win_w: f32,
    win_h: f32,
) {
    const UROW: f32 = 30.0;
    let tab = sb.usage_tab;
    let cpu_tab = tab == state::UsageTab::Cpu;
    // 수집한 만큼 전부 담는다 — 넘치는 몫은 스크롤이 맡는다. 예전엔 여기서 8개로
    // 자르고 나머지를 「그 외 N개」 한 줄로 접었는데, 그 여덟이 전체의 39% 뿐이라
    // (실측 2026-08-27: 231개 중 8개 = 3.3G / 8.4G) 무엇이 잡아먹는지 묻는 이
    // 목록의 목적 자체가 안 서고, 굴릴 수도 없어 나머지를 볼 길이 없었다.
    //
    // 정렬은 **고른 탭의 잣대로만** 한다. 예전엔 메모리 순 하나로 고정이라, CPU
    // 를 통째로 태우는 프로세스가 목록 한복판에 파묻혔다.
    let mut list: Vec<_> = sb.usage_top.clone();
    if cpu_tab {
        list.sort_by(|a, b| b.1.total_cmp(&a.1));
    } else {
        list.sort_by(|a, b| b.2.cmp(&a.2));
    }
    // 목록 밖에 남은 것들 — 합계는 트리 전체 합이라, 이 줄이 없으면 「다 더해도
    // 합계가 안 나온다」가 된다(2026-08-16 「3.1G가 다 더하면 아니지않나」).
    let rest_n = sb.usage_rows.saturating_sub(list.len());
    let rest = (rest_n > 0)
        .then(|| {
            let (tc, tr) = sb.res?;
            let lc: f32 = list.iter().map(|(_, c, _, _)| *c).sum();
            let lr: u64 = list.iter().map(|(_, _, r, _)| *r * 1024).sum();
            Some((rest_n, (tc - lc).max(0.0), tr.saturating_sub(lr)))
        })
        .flatten();
    let w = 300.0_f32.min(win_w - 16.0);
    let rows = list.len().max(1);
    let body = PAD + rows as f32 * UROW;
    // 화면 절반을 넘기지 않는다(포트 팝오버와 같은 규칙) — 팝오버가 창을 덮으면
    // 뒤의 pane 을 못 보면서 판단하게 된다.
    let inner = body.min((win_h * 0.5).max(200.0));
    // 기계 전체의 물리 메모리. 아래 목록·합계와 **다른 것을 잰다** — 그쪽은
    // 우리 트리가 쓰는 양이라 앱을 닫으면 돌아오고, 이쪽엔 재부팅 말고는
    // 회수 경로가 없는 몫(wired)이 들어 있다. 상태줄의 「재시작 권장」이 어디서
    // 나온 말인지 여기서만 확인할 수 있으니 임계 아래에서도 늘 적는다.
    //
    // 메모리 탭에서만 편다 — CPU 를 보러 온 사람에게 wired 비율은 답이 아니고,
    // 팝오버 높이만 한 줄 더 먹는다.
    let mem = sb.mem.filter(|_| !cpu_tab);
    let mem_h = if mem.is_some() { UROW + PAD } else { 0.0 };
    // 「그 외 N개」는 **굴리지 않아도 보여야 한다**. 목록 끝에 두면 스크롤 맨
    // 아래라 굴리기 전에는 안 보이는데, 거기 접힌 몫이 40% 나 된다(실측
    // 2026-08-27: 트리 233개 중 목록 밖 203개 = 5.4G / 13.6G). 그러면 합계
    // 13.6G 옆에 8.2G 짜리 목록이 서서 「13기간데 안에는 그정도는없던데」가
    // 된다 — 상위 몇 개를 늘려도 꼬리가 길면 같은 일이 되풀이되므로, 접힌
    // 몫을 늘 보이는 자리에 못박는 쪽이 답이다.
    let rest_h = if rest.is_some() { UROW + PAD } else { 0.0 };
    // 바깥 앱 — **무엇을 닫아야 하는지**. 경고 구간에서만 편다. 평소엔 이 앱이
    // 낳은 것이 이 팝오버의 주인공이고, 남의 앱 목록은 물었을 때만 답하면 된다.
    // 재시작 구간에서도 적는다 — wired 가 임계를 넘었어도 무거운 앱은 무겁고,
    // 재부팅 전에 당장 숨통을 틔우는 손이 여기 있다.
    // 계속 코어를 태우는 앱이 있으면 메모리가 정상이어도 편다. 그게 없으면
    // 「지금 왜 시끄럽냐」에 이 팝오버가 답을 못 한다(2026-08-27 실측: 팬이
    // 도는데 wired 는 17% 로 멀쩡했고 범인은 CPU 쪽이었다).
    let hog = sb.usage_outside.iter().any(|a| a.is_hog());
    let warm = sb.mem.is_some_and(|m| m.advice() != crate::sysmem::Advice::Ok) || hog;
    let outside: Vec<crate::input::AppUsage> = if warm {
        let mut v = sb.usage_outside.clone();
        if cpu_tab {
            v.sort_by(|a, b| b.cpu.total_cmp(&a.cpu));
        } else {
            v.sort_by(|a, b| b.rss.cmp(&a.rss));
        }
        v.truncate(3);
        v
    } else {
        Vec::new()
    };
    // 「끌까요?」 대기는 몇 초 뒤 저절로 풀린다. 눌러 놓고 잊은 줄이 빨갛게 남아
    // 있으면, 한참 뒤 무심코 그 자리를 눌렀을 때 앱이 닫힌다.
    if sb.usage_kill_armed.is_some_and(|(_, t)| t.elapsed().as_secs() >= 5) {
        sb.usage_kill_armed = None;
    }
    const OUT_HEAD: f32 = 15.0;
    let out_h =
        if outside.is_empty() { 0.0 } else { PAD + OUT_HEAD + outside.len() as f32 * UROW };
    let h = HEAD_H + mem_h + out_h + inner + rest_h + PAD;
    let x = (anchor.0 + anchor.2 - w).clamp(8.0, (win_w - w - 8.0).max(8.0));
    let y = (anchor.1 - h - 6.0).max(8.0);
    sb.popover_rect = Some((x, y, w, h));
    panel_rect_outlined(g, x, y, w, h, theme::radius_md(), theme::surface());
    g.draw_text(
        x + 12.0,
        y + 8.0,
        "사용량",
        gpu::DrawOpts { font_size: 12.0, color: theme::text(), bold: true, italic: false },
    );
    // 합계 두 숫자가 곧 탭이다. 따로 탭 줄을 두면 같은 값을 두 번 적게 되고
    // 머리가 한 줄 더 두꺼워진다 — 이미 「이 잣대로 지금 얼마」를 말하고 있는
    // 자리라, 그걸 누르게 하는 편이 짧다.
    //
    // 고르지 않은 쪽도 늘 적는다. 그래야 지금 보고 있지 않은 잣대에 문제가
    // 생겼을 때(빨갛게 서면) 넘어가 볼 생각이 든다 — 탭을 나눈 대가로 반대편이
    // 안 보이게 되면 나눈 만큼 잃는다.
    if let Some((cpu, rss)) = sb.res {
        let gb = rss as f32 / (1024.0 * 1024.0 * 1024.0);
        let mem_s =
            if gb >= 1.0 { format!("{gb:.1}G") } else { format!("{:.0}M", gb * 1024.0) };
        let cpu_s = format!("{cpu:.0}%");
        // 우리 자신이 코어를 계속 태우는 중이면 이 숫자가 그 근거다. 아래 바깥
        // 앱 목록에는 우리가 안 들어가므로, 빨갛게 하지 않으면 하단바의 「카사텀
        // 130%」가 어디서 나온 말인지 확인할 자리가 없다.
        let cpu_hot = crate::input::is_hot(sb.usage_self.1) || hog;
        let adv = sb.mem.map_or(crate::sysmem::Advice::Ok, |m| m.advice());
        let mem_col = if adv.is_danger() {
            Some(theme::danger())
        } else if adv == crate::sysmem::Advice::Watch {
            Some(theme::syn_number())
        } else {
            None
        };
        let pills = [
            (state::UsageTab::Mem, mem_s, mem_col),
            (state::UsageTab::Cpu, cpu_s, cpu_hot.then(theme::danger)),
        ];
        let mut px = x + w - 8.0;
        for (t, label, warn) in pills {
            let on = t == tab;
            let lw = g.measure_chrome_text(&label, 11.0, on);
            px -= lw + 12.0;
            let r = (px, y + 5.0, lw + 12.0, 22.0);
            if on {
                round_rect(
                    g,
                    r.0,
                    r.1,
                    r.2,
                    r.3,
                    theme::radius_sm(),
                    theme::with_alpha(theme::text(), 0x1c),
                );
            } else if hit(cursor, &r) {
                round_rect(
                    g,
                    r.0,
                    r.1,
                    r.2,
                    r.3,
                    theme::radius_sm(),
                    theme::with_alpha(theme::text(), 0x0e),
                );
            }
            g.draw_text(
                px + 6.0,
                y + 9.0,
                &label,
                gpu::DrawOpts {
                    font_size: 11.0,
                    // 안 고른 쪽은 흐리게 — 다만 경고가 걸린 잣대는 고르지
                    // 않았어도 제 색을 유지한다. 그게 넘어가 보라는 신호다.
                    color: warn.unwrap_or(if on { theme::text() } else { theme::text_mute() }),
                    bold: on || warn.is_some(),
                    italic: false,
                },
            );
            sb.popover_hits.push((state::StatusbarHit::UsageTab(t), r));
        }
    }
    let mut top = y + HEAD_H;
    g.rect(x + 1.0, top, w - 2.0, 1.0, theme::with_alpha(theme::border(), 0x88));
    if let Some(m) = mem {
        let gb = |b: u64| b as f32 / (1024.0 * 1024.0 * 1024.0);
        let adv = m.advice();
        let col = if adv.is_danger() {
            theme::danger()
        } else if adv == crate::sysmem::Advice::Watch {
            theme::syn_number()
        } else {
            theme::text()
        };
        let my = top + PAD;
        // 제목은 **판정만** 말한다. 이유를 뒤에 이어 붙였더니 300px 안에서
        // `맥북 재시작 권장 · 메모리 압박 경…` 으로 잘렸다(실측 2026-08-27) —
        // 이유는 아랫줄로 내린다.
        let head = adv.headline();
        let pct = format!("wired {:.0}%", m.wired_pct());
        let size = format!("{:.1}G / {:.0}G", gb(m.wired), gb(m.total));
        let pw = g.measure_chrome_text(&pct, 11.0, true);
        let sw = g.measure_chrome_text(&size, 10.0, false);
        let right = x + w - 12.0;
        g.draw_text(
            right - pw,
            my + 2.0,
            &pct,
            gpu::DrawOpts { font_size: 11.0, color: col, bold: true, italic: false },
        );
        g.draw_text(
            right - sw,
            my + 16.0,
            &size,
            gpu::DrawOpts { font_size: 10.0, color: theme::text_mute(), bold: false, italic: false },
        );
        let avail = (right - pw.max(sw) - 10.0 - (x + 12.0)).max(0.0);
        let h1 = crate::info::fit_text(g, head, avail, 11.0, false);
        g.draw_text(
            x + 12.0,
            my + 2.0,
            &h1,
            gpu::DrawOpts { font_size: 11.0, color: col, bold: false, italic: false },
        );
        // 아랫줄은 **왜 그렇게 판정했는지**가 먼저다 — 옆 칸의 wired 만 보고는
        // 커널 압박이나 스왑 때문이라는 걸 알 수가 없고, 그러면 재시작 말고
        // 다른 손을 쓸 수 있는지 판단할 근거가 없다. 원인이 wired 면 그 숫자가
        // 이미 옆에 있으므로, 그때만 압축·스왑을 대신 적는다(둘 다 0 이 아닌데
        // wired 만 낮으면 「왜 무겁지」의 답이 거기 있을 수 있다).
        let sub = m.extra_reason().unwrap_or_else(|| {
            format!("압축 {:.1}G · 스왑 {:.1}G", gb(m.compressed), gb(m.swap_used))
        });
        let s2 = crate::info::fit_text(g, &sub, avail, 10.0, false);
        g.draw_text(
            x + 12.0,
            my + 17.0,
            &s2,
            gpu::DrawOpts { font_size: 10.0, color: theme::text_dim(), bold: false, italic: false },
        );
        top += mem_h;
        g.rect(x + 1.0, top, w - 2.0, 1.0, theme::with_alpha(theme::border(), 0x88));
    }
    if !outside.is_empty() {
        // 아래 목록과 **다른 것을 잰다** — 저긴 이 앱이 낳은 것, 여긴 남의 앱이다.
        // 제목으로 그 경계를 못 박지 않으면 두 목록이 한 표본으로 읽혀, 합계가
        // 안 맞는다는 오해가 그대로 되풀이된다.
        let head = if cpu_tab && outside.iter().any(|a| a.is_hog()) {
            // 빨간 숫자가 무슨 뜻인지는 여기서만 말할 수 있다. 줄에는 `98%` 만
            // 남는데, 그것만 보고 잠깐 바쁜 것인지 계속 태우는 것인지 못 가린다.
            "이 앱 밖 — 빨간 쪽이 코어를 계속 태운다"
        } else {
            "이 앱 밖에서 쥔 것"
        };
        let head = crate::info::fit_text(g, head, w - 24.0, 9.5, false);
        g.draw_text(
            x + 12.0,
            top + 3.0,
            &head,
            gpu::DrawOpts { font_size: 9.5, color: theme::text_dim(), bold: false, italic: false },
        );
        let mut oy = top + OUT_HEAD;
        for a in &outside {
            let (pid, name, n) = (&a.pid, &a.name, &a.procs);
            let r = (x + 1.0, oy, w - 2.0, UROW);
            let hov = hit(cursor, &r);
            let armed = sb.usage_kill_armed.is_some_and(|(p, _)| p == *pid);
            if hov || armed {
                round_rect(
                    g,
                    r.0,
                    r.1,
                    r.2,
                    r.3,
                    theme::radius_sm(),
                    theme::with_alpha(
                        if armed { theme::danger() } else { theme::text() },
                        if armed { 0x2a } else { 0x14 },
                    ),
                );
            }
            // 고른 잣대의 값 **하나만**. 둘을 겹쳐 쌓았더니 오른쪽이 숫자 두
            // 기둥이 되어, 정작 무엇으로 줄이 세워졌는지가 안 읽혔다.
            let gb = a.rss as f32 / (1024.0 * 1024.0 * 1024.0);
            let val = if cpu_tab {
                format!("{:.0}%", a.cpu)
            } else if gb >= 1.0 {
                format!("{gb:.1}G")
            } else {
                format!("{:.0}M", gb * 1024.0)
            };
            let vw = g.measure_chrome_text(&val, 12.0, true);
            // 겨눈 줄에서만 오른쪽 한 칸을 ×에 내준다 — 값은 늘 보여야 하는
            // 것이라 자리를 뺏기는 대신 왼쪽으로 물러난다.
            let tail = if hov || armed { 24.0 } else { 0.0 };
            let right = x + w - 12.0 - tail;
            g.draw_text(
                right - vw,
                oy + 8.0,
                &val,
                gpu::DrawOpts {
                    font_size: 12.0,
                    color: if cpu_tab && a.is_hog() { theme::danger() } else { theme::text() },
                    bold: true,
                    italic: false,
                },
            );
            let label = if armed {
                "한 번 더 누르면 종료".to_string()
            } else if *n > 1 {
                // 프로세스 수를 밝히는 것은 이 줄이 **합**이라는 표시다. Chrome
                // 12G 는 Helper 마흔 개를 접은 값이고, 그걸 모르면 활성 모니터의
                // 프로세스 목록과 대조했을 때 어디에도 없는 숫자로 보인다.
                format!("{name} · {n}개")
            } else {
                name.clone()
            };
            let avail = (right - vw - 10.0 - (x + 12.0)).max(0.0);
            let lt = crate::info::fit_text(g, &label, avail, 11.0, false);
            g.draw_text(
                x + 12.0,
                oy + 9.0,
                &lt,
                gpu::DrawOpts {
                    font_size: 11.0,
                    color: if armed { theme::danger() } else { theme::text() },
                    bold: false,
                    italic: false,
                },
            );
            if hov || armed {
                let br = (x + w - 12.0 - 18.0, oy + (UROW - 18.0) / 2.0, 18.0, 18.0);
                let bhov = hit(cursor, &br);
                if bhov {
                    round_rect(
                        g,
                        br.0,
                        br.1,
                        br.2,
                        br.3,
                        theme::radius_sm(),
                        theme::with_alpha(theme::danger(), 0x33),
                    );
                }
                g.queue_icon(
                    "x",
                    br.0 + 4.0,
                    br.1 + 4.0,
                    10.0,
                    if bhov || armed { theme::danger() } else { theme::text_mute() },
                );
                sb.popover_hits.push((state::StatusbarHit::KillApp(*pid), br));
            }
            oy += UROW;
        }
        top += out_h;
        g.rect(x + 1.0, top, w - 2.0, 1.0, theme::with_alpha(theme::border(), 0x88));
    }
    if list.is_empty() {
        g.draw_text(
            x + 22.0,
            top + PAD + 8.0,
            "아직 재는 중…",
            gpu::DrawOpts { font_size: 11.0, color: theme::text_dim(), bold: false, italic: false },
        );
        return;
    }
    // pid 로 pane 목록을 되짚어 **누구 것인지**를 붙인다. `ps` 의 comm 은 죄다
    // `node`·`python3` 라 그것만으로는 여덟 줄이 서로 구별되지 않는다.
    let owner_of = |pid: u32| -> Option<(&str, &str)> {
        view.panes.iter().find_map(|gp| {
            gp.rows.iter().find(|r| r.pid == pid).map(|r| {
                (if r.name.is_empty() { "" } else { r.name.as_str() }, gp.label.as_str())
            })
        })
    };
    let me = std::process::id();
    // 목록만 굴린다 — 머리(합계)와 맥북 메모리 줄은 굴려도 늘 보여야 하는 값이라
    // 잘라내는 창 밖에 둔다. 클릭 대상이 없는 팝오버라 시저가 픽셀만 자르고
    // 클릭은 못 자르는 함정(렌더 카탈로그)에는 걸리지 않는다.
    let list_bottom = top + inner;
    sb.popover_scroll = sb.popover_scroll.clamp(0.0, (body - inner).max(0.0));
    g.push_clip(x, top, w, inner.max(0.0));
    let mut ry = top + PAD - sb.popover_scroll;
    for (pid, cpu, rss_kb, comm) in &list {
        // 이 앱 자신은 이름을 밝혀 준다 — 목록 맨 위에 `kasaterm` 이 떠 있는데
        // 그게 나인 줄 모르면 "이게 뭐지" 로 남는다.
        let (name, owner) = match owner_of(*pid) {
            Some((n, o)) if !n.is_empty() => (n.to_string(), o.to_string()),
            Some((_, o)) => (comm.clone(), o.to_string()),
            None if *pid == me => (comm.clone(), "이 앱".to_string()),
            None => (comm.clone(), String::new()),
        };
        let val = if cpu_tab {
            format!("{cpu:.1}%")
        } else {
            match rss_kb {
                0..=1023 => format!("{rss_kb} KB"),
                1024..=1_048_575 => format!("{} MB", rss_kb / 1024),
                _ => format!("{:.1} GB", *rss_kb as f32 / (1024.0 * 1024.0)),
            }
        };
        let vw = g.measure_chrome_text(&val, 12.0, true);
        let right = x + w - 12.0;
        g.draw_text(
            right - vw,
            ry + 8.0,
            &val,
            gpu::DrawOpts { font_size: 12.0, color: theme::text(), bold: true, italic: false },
        );
        let avail = (right - vw - 10.0 - (x + 12.0)).max(0.0);
        let n = crate::info::fit_text(g, &name, avail, 11.0, false);
        // 주인이 없으면 아랫줄도 없다 — 그때까지 이름을 윗줄에 붙여 두면 옆의
        // 값만 가운데 서서 두 글자가 어긋난 줄로 읽힌다.
        g.draw_text(
            x + 12.0,
            if owner.is_empty() { ry + 8.0 } else { ry + 2.0 },
            &n,
            gpu::DrawOpts { font_size: 11.0, color: theme::text(), bold: false, italic: false },
        );
        if !owner.is_empty() {
            let mut ox = x + 12.0;
            if crate::render::draw_student_face(g, &owner, ox, ry + 15.0, 12.0) {
                ox += 15.0;
            }
            let o = crate::info::fit_text(g, &owner, (avail - (ox - x - 12.0)).max(0.0), 10.0, false);
            g.draw_text(
                ox,
                ry + 17.0,
                &o,
                gpu::DrawOpts {
                    font_size: 10.0,
                    color: theme::text_dim(),
                    bold: false,
                    italic: false,
                },
            );
        }
        ry += UROW;
    }
    g.pop_clip();
    if let Some((n, c, r)) = rest {
        // 목록과 가르는 선 — 없으면 굴러 올라온 마지막 행과 맞닿아, 목록의 한
        // 줄인지 나머지 합계인지 구별되지 않는다.
        g.rect(x + 1.0, list_bottom, w - 2.0, 1.0, theme::with_alpha(theme::border(), 0x88));
        let ry = list_bottom + PAD;
        let gb = r as f32 / (1024.0 * 1024.0 * 1024.0);
        let val = if cpu_tab {
            format!("{c:.1}%")
        } else if gb >= 1.0 {
            format!("{gb:.1} GB")
        } else {
            format!("{:.0} MB", gb * 1024.0)
        };
        let vw = g.measure_chrome_text(&val, 12.0, true);
        let right = x + w - 12.0;
        g.draw_text(
            right - vw,
            ry + 8.0,
            &val,
            gpu::DrawOpts { font_size: 12.0, color: theme::text_mute(), bold: true, italic: false },
        );
        g.draw_text(
            x + 12.0,
            ry + 8.0,
            &format!("그 외 {n}개"),
            gpu::DrawOpts { font_size: 11.0, color: theme::text_dim(), bold: false, italic: false },
        );
    }
}

/// 터널 주소 한 벌. 표시·열기·복사가 각자 문자열을 조립하면 한 곳만 고쳤을 때
/// 「복사한 것과 열리는 것이 다른」 상태가 된다 — 실제로 `/term` → `/term/grid` 로
/// 옮길 때 세 자리를 따로 고쳐야 했다(2026-08-25).
///
/// 주소는 **주인의 유저 주소**(`https://<host>/u/<slug>/…`)다 — 주소 자체가 자격이라
/// `?t=` 토큰이 필요 없고, 슬랙 링크·홈화면 앱에서 열어도 그대로 붙는다(2026-09-02,
/// `kasa_mcp::mobile` 머리말). 주인 주소를 못 만드는 경우(홈 폴더 없음)에만 옛 꼴.
fn tunnel_url(host: &str, path: &str) -> String {
    kasa_mcp::mobile::owner_address(host, path).unwrap_or_else(|| match kasa_mcp::remote_token() {
        Some(t) => format!("https://{host}{path}?t={t}"),
        None => format!("https://{host}{path}"),
    })
}

/// 팝오버에 **보일** 짧은 주소. 스킴과 토큰을 떼고, 넘치면 호스트를 줄인다.
///
/// 기본 우측 잘림에 맡기면 안 된다 — 두 줄을 가르는 것은 경로(`/term/grid` ·
/// `/arona-ui/`)뿐인데 그게 맨 뒤라, 288px 안에서 두 줄이 `https://kasaterm-…`
/// 으로 글자 하나까지 똑같아진다(실측). 호스트는 두 줄이 어차피 같으니 그쪽을 깎는다.
///
/// 여는 것·복사하는 것은 위 `tunnel_url` 의 완성 주소 그대로다. 표시만 짧다.
fn fit_addr(g: &mut gpu::GpuRenderer, host: &str, path: &str, avail: f32) -> String {
    const SIZE: f32 = 11.0;
    let full = format!("{host}{path}");
    if g.measure_chrome_text(&full, SIZE, false) <= avail {
        return full;
    }
    let path_w = g.measure_chrome_text(path, SIZE, false);
    let head = crate::info::fit_text(g, host, (avail - path_w).max(0.0), SIZE, false);
    format!("{head}{path}")
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
    // 남아 팝오버가 이유 없이 커 보인다. 열려 있으면 라벨+주소 두 벌이 들어간다.
    let h = if on { 150.0 } else { 86.0 };
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
            // **완성 주소**를 준다 — 호스트만 주면 그 주소는 문전에서 막힌다. 사용자가
            // 폰으로 열어 보고 정확히 그 화면을 만났다(2026-08-16 「원격주소 제대로
            // 나오게」). 자격은 이제 토큰 쿼리가 아니라 주소의 `/u/<slug>/` 부분이고
            // (`tunnel_url`), 복사·표시 둘 다 같은 주소여야 「복사한 것이 열리는 것」이
            // 성립한다. 표시에서는 slug 를 `…` 로 접는다 — 남이 어깨너머로 볼 자리다.
            //
            // 두 줄인 이유: 폰에서 하는 일이 갈린다 — 학생 고르기와 타자는 허브→터미널,
            // 대화 읽기와 학생 전환은 아로나다. 라벨 없이 주소만 두 개면 어느 게 무엇인지 모른다.
            let rows: [(&str, &str, state::StatusbarHit, state::StatusbarHit); 2] = [
                (
                    "폰 허브 — 학생 목록·터미널",
                    "/u/…/",
                    state::StatusbarHit::OpenTunnelUrl,
                    state::StatusbarHit::CopyTunnelHost,
                ),
                (
                    "아로나 — 대화 읽기",
                    "/u/…/arona-ui/",
                    state::StatusbarHit::OpenAronaUrl,
                    state::StatusbarHit::CopyAronaUrl,
                ),
            ];
            for (i, (label, path, open_hit, copy_hit)) in rows.into_iter().enumerate() {
                let top = line + i as f32 * 42.0;
                g.draw_text(
                    x + 12.0,
                    top + 4.0,
                    label,
                    gpu::DrawOpts {
                        font_size: 10.0,
                        color: theme::text_mute(),
                        bold: false,
                        italic: false,
                    },
                );
                let cr = (x + w - 12.0 - 22.0, top + 20.0, 22.0, 22.0);
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
                sb.popover_hits.push((copy_hit, cr));
                let s = fit_addr(g, &host, path, (cr.0 - x - 24.0).max(0.0));
                let ar = (x + 8.0, top + 18.0, cr.0 - x - 16.0, 20.0);
                let ah = hit(cursor, &ar);
                g.hover_pointer |= ah;
                if ah {
                    hover_rect(g, ar.0, ar.1, ar.2, ar.3, theme::radius_sm());
                }
                g.draw_text(
                    x + 12.0,
                    top + 25.0,
                    &s,
                    gpu::DrawOpts { font_size: 11.0, color: theme::text(), bold: false, italic: false },
                );
                // 주소를 누르면 바로 연다 — 폰으로 보내기 전에 맥에서 먼저 열어 확인하는
                // 손이 복사보다 잦다. 복사 버튼은 그대로 옆에 있다(둘 다).
                sb.popover_hits.push((open_hit, ar));
            }
            g.draw_text(
                x + 12.0,
                line + 88.0,
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
    let named = groups.iter().filter(|(k, _)| !k.is_empty()).count() as f32;
    // `+ 2.0` 은 첫 행 위 숨틈(아래 `ry` 의 시작 오프셋)이다. 이걸 빠뜨리면 그만큼
    // 마지막 행이 아래로 밀려 나가 두 줄 중 아랫줄이 잘린다.
    let body = 2.0
        + (view.ports.len() + usize::from(web.is_some())) as f32 * ROW_H
        + named * GROUP_H
        + if view.ports.is_empty() { ROW_H } else { 0.0 };
    // 화면 절반을 넘기지 않는다 — 넘치면 스크롤이 있고, 팝오버가 창을 덮으면
    // 뒤의 pane 을 못 보면서 판단하게 된다.
    let max_h = (win_h * 0.5).max(200.0);
    let inner = body.min(max_h);
    let h = HEAD_H + inner + PAD;
    let w = POP_W.min(win_w - 16.0);
    // 앵커(칩)의 오른쪽 끝에 맞춰 위로. 창 왼쪽으로 넘치면 밀어 넣는다.
    let x = (anchor.0 + anchor.2 - w).clamp(8.0, (win_w - w - 8.0).max(8.0));
    let y = (anchor.1 - h - 6.0).max(8.0);
    sb.popover_rect = Some((x, y, w, h));
    panel_rect_outlined(g, x, y, w, h, theme::radius_md(), theme::surface());

    // ── 제목줄 ── 칩과 같은 그림(콘센트)을 이고 있어야 "방금 누른 그것" 으로 읽힌다.
    g.queue_icon("plug", x + PADX, y + (HEAD_H - 13.0) / 2.0, 13.0, theme::text());
    g.draw_text(
        x + PADX + 19.0,
        y + (HEAD_H - 13.0) / 2.0 - 1.0,
        "포트",
        gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: true, italic: false },
    );
    let sub = format!("{} 워크스페이스 · {} 외부", view.ports.len(), view.outside);
    let sw = g.measure_chrome_text(&sub, 11.0, false);
    g.draw_text(
        x + w - PADX - sw,
        y + (HEAD_H - 11.0) / 2.0 - 1.0,
        &sub,
        gpu::DrawOpts { font_size: 11.0, color: theme::text_mute(), bold: false, italic: false },
    );
    let top = y + HEAD_H;
    g.rect(x + 1.0, top, w - 2.0, 1.0, theme::with_alpha(theme::border(), 0x88));

    let bottom = y + h - PAD;
    sb.popover_scroll = sb.popover_scroll.clamp(0.0, (body - inner).max(0.0));
    g.push_clip(x, top, w, (bottom - top).max(0.0));
    let mut ry = top + 2.0 - sb.popover_scroll;

    if let Some(port) = web {
        let r = (x, ry, w, ROW_H);
        if hit(cursor, &r) {
            hover_rect(g, r.0, r.1, r.2, r.3, 0.0);
            g.hover_pointer = true;
        }
        port_row(g, x, w, ry, theme::accent(), "globe", &port, "웹터미널", "이 kasaterm", "", 0.0);
        sb.popover_hits.push((state::StatusbarHit::OpenWebTerm, r));
        ry += ROW_H;
    }

    if view.ports.is_empty() {
        g.draw_text(
            x + PADX,
            ry + 10.0,
            "이 워크스페이스에서 listen 중인 포트 없음",
            gpu::DrawOpts { font_size: 11.0, color: theme::text_dim(), bold: false, italic: false },
        );
    }
    for (repo, list) in &groups {
        if !repo.is_empty() {
            // 머리를 **띠 배경**으로 깐다. 글자만 키우면 목록이 길어질수록 어디서
            // 프로젝트가 갈리는지 눈이 매번 다시 찾는다 — 배경 한 단이 그 일을
            // 대신한다(2026-08-15 지시 「이렇게 깔끔하게 뭐가 뭔지 알 수 있으면」).
            g.rect(x + 1.0, ry, w - 2.0, GROUP_H, theme::surface_active());
            g.draw_text(
                x + PADX,
                ry + (GROUP_H - 13.0) / 2.0 - 1.0,
                repo,
                gpu::DrawOpts { font_size: 13.0, color: theme::text(), bold: true, italic: false },
            );
            let n = list.len().to_string();
            let nw = g.measure_chrome_text(&n, 11.0, false);
            g.draw_text(
                x + w - PADX - nw,
                ry + (GROUP_H - 11.0) / 2.0 - 1.0,
                &n,
                gpu::DrawOpts {
                    font_size: 11.0,
                    color: theme::text_mute(),
                    bold: false,
                    italic: false,
                },
            );
            g.queue_icon(
                "folder",
                x + w - PADX - nw - 18.0,
                ry + (GROUP_H - 12.0) / 2.0,
                12.0,
                theme::text_mute(),
            );
            ry += GROUP_H;
        }
        for (i, p) in list.iter().enumerate() {
            let r = (x, ry, w, ROW_H);
            let hov = hit(cursor, &r);
            if ry + ROW_H > top && ry < bottom {
                // 같은 묶음 안 행끼리는 아주 옅게만 가른다 — 묶음을 가르는 일은
                // 위의 띠가 이미 하므로, 여기서 진하게 그으면 선이 둘이 된다.
                if i > 0 {
                    g.rect(
                        x + PADX,
                        ry,
                        w - PADX * 2.0,
                        1.0,
                        theme::with_alpha(theme::border(), 0x40),
                    );
                }
                if hov {
                    hover_rect(g, r.0, r.1, r.2, r.3, 0.0);
                    g.hover_pointer = true;
                }
                // 점 색이 곧 "이걸 꺼도 되나" 에 대한 답이다. 세 갈래인 것이 요점 —
                //   파랑  = 지금 이 pane 이 돌리고 있다(셸 자손). 끄려면 그 pane 을 보라.
                //   흐림  = 띄운 셸이 죽어 launchd 로 넘어갔지만 **주인 pane 은 살아 있다**.
                //           pane 을 닫아도 안 죽으니 여기서 꺼야 한다.
                //   빨강  = 띄운 pane 자체가 없다. 아무도 안 쓰는 것이므로 꺼도 된다.
                // 예전엔 뒤의 둘이 같은 흐림이라, 주인이 사라진 서버와 학생이 방금 띄운
                // 서버가 구별되지 않았다(거노: "죽은 학생이 생성해서 꺼도 되는지 모르겠다").
                let dot = if p.owner_dead {
                    theme::danger()
                } else if p.orphan {
                    theme::text_dim()
                } else {
                    theme::accent()
                };
                let site = if p.site.is_empty() { p.name.as_str() } else { p.site.as_str() };
                let owner = if p.label.is_empty() {
                    p.pane.clone().unwrap_or_default()
                } else {
                    p.label.clone()
                };
                let port_s = p.port.to_string();
                // 호버 중엔 오른쪽 두 칸을 아이콘에 내준다 — 주인 이름은 늘 보이는
                // 값이지만 끄기·열기는 지금 이 행을 겨눴을 때만 필요하다.
                let tail = if hov { 44.0 } else { 0.0 };
                port_row(g, x, w, ry, dot, p.kind, &port_s, site, &owner, &p.name, tail);
                if hov {
                    let br = (x + w - PADX - 18.0, ry + (ROW_H - 18.0) / 2.0, 18.0, 18.0);
                    let bhov = hit(cursor, &br);
                    if bhov {
                        round_rect(
                            g,
                            br.0,
                            br.1,
                            br.2,
                            br.3,
                            theme::radius_sm(),
                            theme::with_alpha(theme::danger(), 0x33),
                        );
                    }
                    g.queue_icon(
                        "x",
                        br.0 + 4.0,
                        br.1 + 4.0,
                        10.0,
                        if bhov { theme::danger() } else { theme::text_mute() },
                    );
                    sb.popover_hits.push((state::StatusbarHit::KillPort(p.pid), br));
                    g.queue_icon(
                        "external-link",
                        br.0 - 20.0,
                        br.1 + 4.0,
                        11.0,
                        theme::text_dim(),
                    );
                }
            }
            sb.popover_hits.push((state::StatusbarHit::OpenPort(p.port), r));
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

/// 포트 한 줄. 칼럼 셋이다 — 점+번호 / 정체+주소 / 주인+프로세스.
///
/// 번호 칼럼을 **고정폭**으로 잡는 것이 이 그림의 핵심이다. 번호는 네 자리와 다섯
/// 자리가 섞이는데(3000 · 62292), 폭을 재서 이어 붙이면 다음 칼럼이 행마다 들쭉날쭉
/// 해져 눈이 세로로 못 훑는다. 자릿수가 칸을 넘으면 그건 포트가 아니므로 걱정이 없다.
#[allow(clippy::too_many_arguments)]
fn port_row(
    g: &mut gpu::GpuRenderer,
    x: f32,
    w: f32,
    y: f32,
    dot: [u8; 4],
    kind: &str,
    port: &str,
    site: &str,
    owner: &str,
    proc: &str,
    tail: f32,
) {
    // 점은 행이 아니라 **번호**의 중심에 건다. 두 줄 행의 한가운데에 두면 정작
    // 그것이 수식하는 번호보다 아래로 내려가, 무엇에 붙은 표시인지가 흐려진다.
    crate::circle_rect(g, x + PADX, y + 12.0, 6.0, dot);
    g.draw_text(
        x + PADX + 14.0,
        y + 8.0,
        port,
        gpu::DrawOpts { font_size: 14.0, color: theme::text(), bold: true, italic: false },
    );
    // 오른쪽 칼럼(주인·프로세스)은 폭이 남을 때만 — 좁은 창에서는 "무엇이 떠 있나"
    // 가 "누가 띄웠나" 보다 먼저다.
    let right = x + w - PADX - tail;
    let rw = if w >= 300.0 { 96.0_f32.min(w * 0.3) } else { 0.0 };
    let mut cx = x + PADX + 14.0 + COL_PORT;
    // 정체 아이콘 — 번호 다음, 이름 앞. 「웹이면 열어 본다 / DB 면 안 건드린다」
    // 판단이 이름을 읽기 전에 서게 한다(2026-08-16 「웹인지 백엔드뭐시긴지
    // 아이콘으로」).
    if !kind.is_empty() {
        g.queue_icon(kind, cx, y + 8.0, 13.0, theme::text_dim());
        cx += 19.0;
    }
    let mid = (right - rw - 8.0 - cx).max(0.0);
    let s = crate::info::fit_text(g, site, mid, 12.0, false);
    g.draw_text(
        cx,
        y + 9.0,
        &s,
        gpu::DrawOpts { font_size: 12.0, color: theme::text_mute(), bold: false, italic: false },
    );
    let a = crate::info::fit_text(g, &format!("127.0.0.1:{port}"), mid, 11.0, false);
    g.draw_text(
        cx,
        y + 25.0,
        &a,
        gpu::DrawOpts {
            font_size: 11.0,
            color: theme::with_alpha(theme::text_dim(), 0xB0),
            bold: false,
            italic: false,
        },
    );
    if rw <= 0.0 {
        return;
    }
    if !owner.is_empty() && tail <= 0.0 {
        let mut ox = right - rw;
        if crate::render::draw_student_face(g, owner, ox, y + 8.0, 13.0) {
            ox += 16.0;
        }
        let o = crate::info::fit_text(g, owner, (right - ox).max(0.0), 11.0, false);
        let ow = g.measure_chrome_text(&o, 11.0, false);
        // 얼굴이 붙으면 왼쪽 정렬(얼굴-이름이 한 덩어리라야 읽힌다), 아니면 오른쪽
        // 정렬로 다른 행의 꼬리와 세로선을 맞춘다.
        let tx = if ox > right - rw { ox } else { right - ow };
        g.draw_text(
            tx,
            y + 9.0,
            &o,
            gpu::DrawOpts { font_size: 11.0, color: theme::text_dim(), bold: false, italic: false },
        );
    }
    if !proc.is_empty() {
        let p = crate::info::fit_text(g, proc, rw, 11.0, false);
        let pw = g.measure_chrome_text(&p, 11.0, false);
        g.draw_text(
            right - pw,
            y + 25.0,
            &p,
            gpu::DrawOpts {
                font_size: 11.0,
                color: theme::with_alpha(theme::text_mute(), 0x90),
                bold: false,
                italic: false,
            },
        );
    }
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
        // 아직 한 번도 안 그려졌으면 **삼키고 기다린다.** 히트박스도 바깥 판정용
        // 사각형도 render 가 채우는데, 여는 클릭과 다음 프레임 사이에 클릭이 하나
        // 더 오면 「아무것도 안 맞음 = 밖」이 되어 열리자마자 닫힌다(계정 메뉴에서
        // 실제로 관측됐다 — 토키 2026-08-15). 칩 자체를 다시 누른 경우는 부르는
        // 쪽에서 이미 걸러 온다.
        if self.statusbar.popover_rect.is_none() {
            return true;
        }
        let inside =
            |r: &(f32, f32, f32, f32)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3;
        // 죽이기(×)를 열기(행)보다 먼저 — 둘이 겹쳐 있어 순서가 뒤집히면 ×가
        // 행에 삼켜져 영영 안 눌린다.
        let hit = self
            .statusbar
            .popover_hits
            .iter()
            .find(|(h, r)| {
                matches!(
                    h,
                    state::StatusbarHit::KillPort(_) | state::StatusbarHit::KillApp(_)
                ) && inside(r)
            })
            .or_else(|| self.statusbar.popover_hits.iter().find(|(_, r)| inside(r)))
            .map(|(h, _)| h.clone());
        match hit {
            Some(state::StatusbarHit::KillPort(pid)) => {
                self.kill_process(pid, false);
                return true;
            }
            Some(state::StatusbarHit::KillApp(pid)) => {
                // 첫 클릭은 겨누기만 한다. 포트 팝오버의 ×는 자기가 띄운 dev
                // 서버라 잘못 눌러도 다시 띄우면 그만이지만, 여기 뜨는 건 사람이
                // 쓰던 앱(브라우저·편집기)이라 열어 둔 것을 잃는다.
                match self.statusbar.usage_kill_armed {
                    Some((p, _)) if p == pid => {
                        self.statusbar.usage_kill_armed = None;
                        let name = self
                            .statusbar
                            .usage_outside
                            .iter()
                            .find(|a| a.pid == pid)
                            .map(|a| a.name.clone());
                        // SIGTERM 이다 — 앱이 저장 여부를 물을 틈이 있다.
                        self.kill_process(pid, false);
                        if let Some(n) = name {
                            // `kill_process` 의 「종료 신호 4821」을 덮는다. 이
                            // 목록에서 사람이 고른 것은 pid 가 아니라 앱이다.
                            self.set_toast(format!("{n} 종료 신호"));
                        }
                    }
                    _ => {
                        self.statusbar.usage_kill_armed =
                            Some((pid, std::time::Instant::now()));
                        self.chrome_dirty = true;
                    }
                }
                return true;
            }
            Some(state::StatusbarHit::UsageTab(t)) => {
                // 잣대가 바뀌면 정렬도 바뀌므로 굴린 자리는 뜻을 잃는다 — 다른
                // 순서의 목록 한복판에 서 있게 되어, 맨 위가 무엇인지 보려면
                // 매번 되감아야 한다.
                if self.statusbar.usage_tab != t {
                    self.statusbar.popover_scroll = 0.0;
                }
                self.statusbar.usage_tab = t;
                self.statusbar.usage_kill_armed = None;
                self.chrome_dirty = true;
                return true;
            }
            Some(state::StatusbarHit::OpenPort(port)) => {
                self.open_localhost(port);
                return true;
            }
            Some(state::StatusbarHit::OpenWebTerm) => {
                if let Some(port) = self.statusbar.port.clone() {
                    // grid — 옛 `/term`(xterm.js)은 폰 IME 에서 한글이 자모로 쪼개진다.
                    self.open_url(&format!("http://127.0.0.1:{port}/term/grid"));
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
            Some(state::StatusbarHit::OpenTunnelUrl) => {
                if let Some(h) = self.statusbar.tunnel_host.clone() {
                    self.open_url(&tunnel_url(&h, ""));
                }
                return true;
            }
            Some(state::StatusbarHit::OpenAronaUrl) => {
                if let Some(h) = self.statusbar.tunnel_host.clone() {
                    self.open_url(&tunnel_url(&h, "/arona-ui/"));
                }
                return true;
            }
            Some(state::StatusbarHit::CopyTunnelHost) => {
                if let Some(h) = self.statusbar.tunnel_host.clone() {
                    // 표시와 같은 **완성 주소**(유저 slug 포함) — 호스트만 복사하면 붙는
                    // 순간 관문에 막힌다.
                    self.copy_to_clipboard(tunnel_url(&h, ""), "폰 주소 복사됨");
                }
                return true;
            }
            Some(state::StatusbarHit::CopyAronaUrl) => {
                if let Some(h) = self.statusbar.tunnel_host.clone() {
                    self.copy_to_clipboard(tunnel_url(&h, "/arona-ui/"), "아로나 주소 복사됨");
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
        // 사용량은 **경고를 보고 누르는** 자리다. 하단바가 「재시작 권장」이라
        // 말했는데 CPU 탭이 펴지면, 방금 읽은 그 말의 근거를 보려고 탭을 한 번
        // 더 눌러야 한다. 지금 급한 잣대로 열어 준다 — 경고가 없을 때만 마지막에
        // 보던 탭이 남는다.
        if !same && kind == state::StatusbarPopover::Usage {
            let mem_bad =
                self.statusbar.mem.is_some_and(|m| m.advice() != crate::sysmem::Advice::Ok);
            let cpu_bad = self.statusbar.usage_outside.iter().any(|a| a.is_hog())
                || crate::input::is_hot(self.statusbar.usage_self.1);
            // 메모리가 먼저다 — 하단바의 경고 우선순위와 같은 순서라야, 거기서
            // 읽은 말과 펴진 탭이 어긋나지 않는다.
            if mem_bad {
                self.statusbar.usage_tab = state::UsageTab::Mem;
            } else if cpu_bad {
                self.statusbar.usage_tab = state::UsageTab::Cpu;
            }
        }
        self.chrome_dirty = true;
    }
}
