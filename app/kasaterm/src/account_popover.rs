use super::*;

type Rect = (f32, f32, f32, f32);

pub(super) struct PopoverLayout {
    pub frame: Rect,
    pub body_height: f32,
    pub scroll_max: f32,
    pub above: bool,
}

pub(super) struct FlyoutLayout {
    pub frame: Rect,
    pub corridor: Option<Rect>,
    pub body_height: f32,
    pub scroll_max: f32,
}

pub(super) fn flyout_layout(
    viewport: (f32, f32),
    parent: Rect,
    provider: Rect,
    width: f32,
    fixed_height: f32,
    content_height: f32,
) -> FlyoutLayout {
    let width = width.min((viewport.0 - 16.0).max(0.0));
    let height = (fixed_height + content_height).min((viewport.1 - 16.0).max(0.0));
    let right = parent.0 + parent.2 + 4.0;
    let left = parent.0 - width - 4.0;
    let (x, corridor) = if right + width <= viewport.0 - 8.0 {
        (right, Some((right - 4.0, provider.1, 4.0, provider.3)))
    } else if left >= 8.0 {
        (left, Some((parent.0 - 4.0, provider.1, 4.0, provider.3)))
    } else {
        ((viewport.0 - width - 8.0).max(8.0), None)
    };
    let y = provider.1.clamp(8.0, (viewport.1 - height - 8.0).max(8.0));
    let body_height = (height - fixed_height).max(0.0);
    FlyoutLayout {
        frame: (x, y, width, height),
        corridor,
        body_height,
        scroll_max: (content_height - body_height).max(0.0),
    }
}

pub(super) fn layout(
    viewport: (f32, f32),
    anchor: Rect,
    width: f32,
    fixed_height: f32,
    content_height: f32,
) -> PopoverLayout {
    let (ax, ay, aw, ah) = anchor;
    let width = width.min((viewport.0 - 16.0).max(0.0));
    let above_room = (ay - 8.0).max(0.0);
    let below_room = (viewport.1 - ay - ah - 8.0).max(0.0);
    let above = above_room >= below_room;
    let available = if above { above_room } else { below_room };
    let height = (fixed_height + content_height).min(available);
    let body_height = (height - fixed_height).max(0.0);
    let x = (ax + aw - width).clamp(8.0, (viewport.0 - width - 8.0).max(8.0));
    let y = if above { ay - height } else { ay + ah };
    PopoverLayout {
        frame: (x, y, width, height),
        body_height,
        scroll_max: (content_height - body_height).max(0.0),
        above,
    }
}

/// 계정 메뉴가 이 프레임에 읽는 값. `g` 가 `self.gpu` 를 잡은 안쪽에서 부르므로
/// `&self` 로 구해야 하는 것은 부르는 쪽이 미리 풀어 넘긴다.
pub(super) struct Frame<'a> {
    pub open: bool,
    pub anchor: Option<Rect>,
    pub viewport: (f32, f32),
    pub status_h: f32,
    pub cursor: (f32, f32),
    pub compact: bool,
    pub accounts: AccountSettings<'a>,
    pub provider: Option<AccountProvider>,
    pub claude_observations: &'a ClaudeObservations,
    pub codex_rollout: Option<&'a crate::transcript::CodexRolloutSnapshot>,
}

/// `App` 에 흩어진 `account_menu_*` 필드들이라 참조로 묶어 넘긴다.
pub(super) struct MenuState<'a> {
    pub hits: &'a mut Vec<(AccountMenuItem, Rect)>,
    pub rect: &'a mut Option<Rect>,
    pub body_rect: &'a mut Option<Rect>,
    pub scroll: &'a mut f32,
    pub scroll_max: &'a mut f32,
    pub submenu_rect: &'a mut Option<Rect>,
    pub submenu_body_rect: &'a mut Option<Rect>,
    pub corridor_rect: &'a mut Option<Rect>,
    pub submenu_scroll: &'a mut f32,
    pub submenu_scroll_max: &'a mut f32,
    pub submenu_hit_start: &'a mut usize,
}

/// 계정 드롭다운. 닫혀 있어도 지난 프레임의 히트렉트·틀은 비운다.
pub(super) fn paint(g: &mut gpu::GpuRenderer, st: MenuState<'_>, frame: &Frame<'_>) {
    st.hits.clear();
    *st.rect = None;
    *st.body_rect = None;
    *st.scroll_max = 0.0;
    *st.submenu_rect = None;
    *st.submenu_body_rect = None;
    *st.corridor_rect = None;
    *st.submenu_scroll_max = 0.0;
    *st.submenu_hit_start = 0;
    let Some((ax, ay, aw, ah)) = frame.anchor.filter(|_| frame.open) else {
        return;
    };

    g.hover_pointer = false;
    let (hmx, hmy) = frame.cursor;
    let f = 13.0_f32;
    let pad = 4.0_f32;
    let pad_x = 10.0_f32;
    let icon = theme::ICON_SIZE;
    let compact = frame.compact;
    let win_h = frame.viewport.1;
    let win_w = frame.viewport.0;

    // ── 값 읽기 ──────────────────────────────────────────────────
    // 슬롯별 한도표. 폴러가 계정 디렉터리를 키로 채운다.
    let usage_of = |id: &str| -> Option<crate::UsageBadge> {
        frame.claude_observations
            .get(id)
            .and_then(|(badge, _)| badge.clone())
    };
    // 로스터 행은 **활성 계정의** 한도를 말한다. 표에 아직 없으면 상태줄이
    // 쓰는 값으로 떨어진다 — 둘 다 지금 계정을 가리키므로 숫자가 갈리지 않는다.
    let claude_badge = usage_of(frame.accounts.claude);
    let claude_reset = crate::limit_reset::current(frame.accounts.claude);
    let codex_limits = crate::codexlimits::snapshot();

    // `62% 씀 · 5h` — 퍼센트가 먼저다. 창 이름이 앞에 오면 눈이 «어느 창인가»
    // 를 먼저 읽는데, 정작 판단을 가르는 건 숫자다.
    let usage_text = |b: &crate::UsageBadge| -> String {
        let head = if b.stale {
            format!("이전 {:.0}% 씀", b.pct)
        } else {
            format!("{:.0}% 씀", b.pct)
        };
        format!("{head} {}", b.label)
    };
    // 임계는 60/80. 그 아래는 초록이 아니라 **중립**이다 — 초록은 "좋다"는
    // 신호라 늘 켜져 있으면 아무 말도 안 하는 색이 된다.
    let pct_col = |pct: f32| {
        if pct >= 80.0 {
            theme::danger()
        } else if pct >= 60.0 {
            theme::syn_number()
        } else {
            theme::text()
        }
    };

    // 제공자 두 줄. **사용률 높은 순** — 옮길 곳을 고르려고 여는 목록이라
    // 급한 쪽이 위로 와야 한다. Codex도 rollout이 아니라 계정별 direct
    // snapshot을 쓴다. 최근 대화가 없어도 한도는 계정에 그대로 있기 때문이다.
    let codex_signed_in = crate::settings::codex_logged_in(frame.accounts.codex)
        || crate::codexlimits::seeded_for_probe(frame.accounts.codex);
    let codex_current_limits =
        codex_signed_in.then_some(codex_limits.as_ref()).flatten();
    let mut provs: Vec<(AccountProvider, f32)> = vec![
        (
            AccountProvider::Claude,
            claude_badge.as_ref().map_or(-1.0, |b| b.pct),
        ),
        (
            AccountProvider::Codex,
            codex_current_limits
                .and_then(|limits| {
                    limits
                        .windows
                        .iter()
                        .map(|(_, pct, _)| *pct)
                        .max_by(f32::total_cmp)
                })
                .unwrap_or(-1.0),
        ),
    ];
    provs.sort_by(|a, b| b.1.total_cmp(&a.1));

    // ── 치수 ────────────────────────────────────────────────────
    // 플랫 정리(2026-09-14 승인 목업): 계정마다 블록 하나, 그 안에 창(5h·7d)
    // 마다 한 줄 — `[창][막대][퍼센트][풀리는 때]` 가 같은 자리에 선다.
    // 모델별 창은 들여 쓴 작은 줄. 토글은 채운 알약이 아니라 테두리다.
    let mw = 380.0_f32.min((win_w - 16.0).max(0.0));
    let head_h = 26.0_f32;
    let row_h = 28.0_f32;
    let ph_h = 22.0_f32;
    let win_h_row = 22.0_f32;
    let blk_top = 6.0_f32;
    let blk_bot = 4.0_f32;
    struct WinRow {
        lab: String,
        pct: f32,
        rs: String,
        sub: bool,
    }
    let reset_word = |t: Option<String>| -> String {
        t.map(|t| {
            if t == "곧" {
                "곧 풀림".to_string()
            } else {
                format!("{t} 뒤 풀림")
            }
        })
        .unwrap_or_default()
    };
    // 창 줄 목록. 값이 없으면 빈 목록이고, 그 자리엔 한 줄짜리 안내가 선다.
    let rows_of = |p: AccountProvider| -> Vec<WinRow> {
        match p {
            AccountProvider::Claude => match claude_badge.as_ref() {
                Some(b) if b.windows.is_empty() => vec![WinRow {
                    lab: b.label.clone(),
                    pct: b.pct,
                    rs: reset_word(crate::resets_in_label(b.resets_at)),
                    sub: false,
                }],
                Some(b) => b
                    .windows
                    .iter()
                    .map(|w| {
                        // `7d <모델>` — 모델 스코프 창은 들여 쓴 줄로, 모델명은
                        // 풀리는 때 자리에 적는다(그 창의 시각은 따로 없다).
                        match w.label.split_once(' ') {
                            Some((lab, model)) => WinRow {
                                lab: lab.to_string(),
                                pct: w.pct,
                                rs: model.to_string(),
                                sub: true,
                            },
                            None => WinRow {
                                lab: w.label.clone(),
                                pct: w.pct,
                                rs: reset_word(crate::resets_in_label(w.resets_at)),
                                sub: false,
                            },
                        }
                    })
                    .collect(),
                None => Vec::new(),
            },
            AccountProvider::Codex => match codex_current_limits {
                Some(limits) => limits
                    .windows
                    .iter()
                    .map(|(minutes, pct, at)| WinRow {
                        lab: codex_rate_window_label(Some(*minutes)),
                        pct: *pct,
                        rs: reset_word(
                            at.filter(|at| *at > 0)
                                .and_then(|at| crate::resets_in_label(Some(at as u64))),
                        ),
                        sub: false,
                    })
                    .chain(limits.named_windows.iter().map(|w| WinRow {
                        lab: codex_rate_window_label(Some(w.minutes)),
                        pct: w.pct,
                        rs: w.name.clone(),
                        sub: true,
                    }))
                    .collect(),
                None => Vec::new(),
            },
        }
    };
    // 코덱스가 5h·7d 를 안 준 경우의 안내 줄. 빈칸은 「여유」로 읽히므로 적는다.
    let codex_missing = || -> Option<String> {
        let limits = codex_current_limits?;
        let has = |wanted: &str| {
            limits
                .windows
                .iter()
                .any(|(m, _, _)| codex_rate_window_label(Some(*m)) == wanted)
        };
        let missing: Vec<&str> = ["5h", "7d"].into_iter().filter(|w| !has(w)).collect();
        (!missing.is_empty()).then(|| format!("{} 미제공", missing.join("·")))
    };
    let provider_h = |p: AccountProvider| -> f32 {
        if compact {
            return blk_top + ph_h + blk_bot;
        }
        let mut lines = rows_of(p).len();
        if p == AccountProvider::Codex && codex_missing().is_some() {
            lines += 1;
        }
        blk_top + ph_h + win_h_row * lines.max(1) as f32 + blk_bot
    };
    let rule = 5.0_f32;
    // 판 줄. 액션 행보다 낮다 — 누르는 자리가 아니라 읽는 자리다.
    let ver_h = 22.0_f32;
    let expanded_accounts = frame.provider.map(|p| {
        let rows: Vec<(String, String, bool)> = match p {
            AccountProvider::Claude => frame
                .accounts
                .claude_list
                .iter()
                .filter(|account| !account.id.is_empty())
                .enumerate()
                .map(|(i, a)| {
                    (
                        a.id.clone(),
                        crate::settings::account_display(
                            &a.id,
                            &a.label,
                            &format!("계정 {}", i + 1),
                        ),
                        *frame.accounts.claude == a.id,
                    )
                })
                .collect(),
            AccountProvider::Codex => {
                let mut v = vec![(
                    String::new(),
                    crate::settings::codex_account_display("", "", "기본"),
                    frame.accounts.codex.is_empty(),
                )];
                v.extend(frame.accounts.codex_list.iter().enumerate().map(|(i, a)| {
                    (
                        a.id.clone(),
                        crate::settings::codex_account_display(
                            &a.id,
                            &a.label,
                            &format!("계정 {}", i + 2),
                        ),
                        *frame.accounts.codex == a.id,
                    )
                }));
                v
            }
        };
        let codex_note = (p == AccountProvider::Codex)
            .then(|| {
                let snapshot = frame.codex_rollout?;
                let summary = codex_run_summary(snapshot);
                (!summary.is_empty()).then(|| format!("최근 실행 · {summary}"))
            })
            .flatten();
        let lab_h = if codex_note.is_some() { 42.0 } else { 24.0 };
        // **고르기 전에** 각 계정의 5시간·7일이 둘 다 보여야 한다(사용자
        // 2026-08-15 「계정전환전에 5시간 7일 한도 보이게」). 누르면 그 자리서
        // 전환되므로 눌러 보고 판단할 수가 없다. 막대 두 벌은 이름과 한 줄에
        // 못 들어가니 행을 두 줄로 키운다 — 「간단히」 밀도에서는 예전처럼
        // 한 줄에 글자로만.
        let row_heights: Vec<f32> = rows
            .iter()
            .map(|(id, _, _)| {
                if compact && p == AccountProvider::Codex {
                    return 28.0 + 16.0 + 30.0;
                }
                let lines = match p {
                    AccountProvider::Claude => usage_of(id)
                        .map(|badge| {
                            let n = if badge.windows.is_empty() {
                                1
                            } else {
                                badge.windows.len()
                            };
                            n.div_ceil(2)
                        })
                        .unwrap_or(1),
                    AccountProvider::Codex => {
                        if !crate::settings::codex_logged_in(id)
                            && !crate::codexlimits::seeded_for_probe(id)
                        {
                            1
                        } else {
                            codex_windows_for(id)
                                .map(|wins| {
                                    let missing = ["5h", "7d"].iter().any(|wanted| {
                                        !wins.iter().any(|(label, _)| label == wanted)
                                    });
                                    wins.len().div_ceil(2) + usize::from(missing)
                                })
                                .unwrap_or(1)
                        }
                    }
                };
                28.0 + 16.0 * lines.max(1) as f32 + 30.0
            })
            .collect();

        let empty_h = if rows.is_empty() { 32.0 } else { 0.0 };
        (p, rows, row_heights, codex_note, lab_h, empty_h)
    });
    let content_h = provs.iter().map(|(p, _)| provider_h(*p)).sum::<f32>();
    // 초기화권은 머리 바로 아래 고정 줄이다 — 제공자 블록은 사용률 순으로 자리를
    // 바꾸고 목록은 굴러가므로, 그 안에 두면 누를 단추가 매번 다른 곳에 있다.
    // 「간단히」에서도 세운다: 드물게 생기는 것이고, 접힌 쪽에 숨으면 가진 줄을 모른다.
    let reset_row_h = if claude_reset.is_some() { 38.0_f32 } else { 0.0 };
    let fixed_h = pad * 2.0
        + head_h
        + rule * 3.0
        + row_h * 2.0
        + ver_h
        + if claude_reset.is_some() { reset_row_h + rule } else { 0.0 };
    let layout = account_popover::layout(
        (win_w, win_h),
        (ax, ay, aw, ah),
        mw,
        fixed_h,
        content_h,
    );
    let (mx, my, mw, mh) = layout.frame;
    let above = layout.above;
    let body_h = layout.body_height;
    *st.rect = Some((mx, my, mw, mh));
    *st.scroll_max = layout.scroll_max;
    *st.scroll = st
        .scroll
        .clamp(0.0, *st.scroll_max);
    // 패널 배경과 팝업 배경은 6단계밖에 안 벌어져서, 색만으로는 이게 떠 있는
    // 메뉴인지 패널의 한 구역인지 읽히지 않았다(사용자: 뒤가 비쳐 보인다).
    // 층 선언은 색이 아니라 그림자·테두리가 하는 일이다.
    panel_rect_outlined(
        g,
        mx,
        my,
        mw,
        mh,
        theme::radius_sm(),
        theme::surface_hover(),
    );
    // Joining the trigger edge makes the popover read as part of the status bar.
    let join_x = ax.max(mx + theme::radius_sm());
    let join_right = (ax + aw).min(mx + mw - theme::radius_sm());
    if join_right > join_x {
        g.rect(
            join_x,
            if above { ay - 1.0 } else { ay + ah - 1.0 },
            join_right - join_x,
            2.0,
            theme::surface_hover(),
        );
    }
    g.push_clip(mx, my, mw, mh);
    let mut ry = my + pad;

    // ── 머리: Usage · all agents ────────────────────────────────
    g.draw_text(
        mx + pad_x,
        ry + (head_h - f) / 2.0 - 1.0,
        "사용량",
        gpu::DrawOpts {
            font_size: f,
            color: theme::text(),
            bold: true,
            italic: false,
        },
    );
    {
        // 「이 앱에서 도는 학생 전부의 합」이라는 뜻 — 계정 하나를 여러
        // pane 이 나눠 쓰므로, 이 숫자가 내 pane 것이 아님을 밝혀야 한다.
        let sub = "캐릭터 전체";
        let sf = f - 2.5;
        let hw = g.measure_chrome_text("사용량", f, true);
        g.draw_text(
            mx + pad_x + hw + 8.0,
            ry + (head_h - sf) / 2.0 - 1.0,
            sub,
            gpu::DrawOpts {
                font_size: sf,
                color: theme::text_mute(),
                bold: false,
                italic: false,
            },
        );
    }
    ry += head_h;

    g.rect(mx + pad_x, ry + 2.0, mw - pad_x * 2.0, 1.0, theme::border());
    ry += rule;

    // ── 초기화권 ────────────────────────────────────────────────
    if let Some(grant) = claude_reset.as_ref() {
        let pi = 14.0_f32;
        let cy = ry + reset_row_h / 2.0;
        g.queue_icon(
            AccountProvider::Claude.icon(),
            mx + pad_x,
            cy - pi / 2.0,
            pi,
            theme::text_dim(),
        );
        let tx = mx + pad_x + pi + 7.0;
        // 설정 화면의 주 버튼과 한 벌이다(`native_settings::button`): 채움 없이
        // 강조색 테두리와 글자, 몸통 높이는 CTL_H.
        let bf = 12.0_f32;
        let action = "웹에서 쓰기";
        let bw = if grant.usable_now {
            g.measure_chrome_text(action, bf, true) + 20.0 + 12.0 + 4.0
        } else {
            0.0
        };
        let tf = f - 1.0;
        let text = crate::info::fit_text(
            g,
            &crate::limit_reset::label(grant),
            (mx + mw - pad_x - bw - 10.0 - tx).max(0.0),
            tf,
            false,
        );
        g.draw_text(
            tx,
            cy - tf / 2.0 - 1.0,
            &text,
            gpu::DrawOpts {
                font_size: tf,
                color: theme::text(),
                bold: false,
                italic: false,
            },
        );
        if grant.usable_now {
            let h = crate::native_settings::CTL_H;
            let r = (mx + mw - pad_x - bw, cy - h / 2.0, bw, h);
            let hot = hmx >= r.0 && hmx <= r.0 + r.2 && hmy >= r.1 && hmy <= r.1 + r.3;
            g.hover_pointer |= hot;
            // 주 버튼은 호버에 꺼지는 대신 한 톤 밝아진다 — 강조색은 그대로 두고
            // 바탕에만 옅게 깐다.
            if hot {
                round_rect(
                    g,
                    r.0,
                    r.1,
                    r.2,
                    r.3,
                    crate::native_settings::ctrl_radius(),
                    theme::with_alpha(theme::accent(), 28),
                );
            }
            g.round_rect_stroke(
                r.0,
                r.1,
                r.2,
                r.3,
                crate::native_settings::ctrl_radius(),
                theme::border_w().max(1.0),
                theme::accent(),
            );
            let lw = g.measure_chrome_text(action, bf, true);
            let lx = r.0 + (r.2 - (lw + 4.0 + 12.0)) / 2.0;
            // 한글은 아래 삐침이 없어 글자 상자 가운데가 눈의 가운데보다 높다 —
            // 같은 공식이면 옆 아이콘보다 1pt 남짓 떠 보였다(px 실측).
            g.draw_text(
                lx,
                cy - bf / 2.0 + 0.5,
                action,
                gpu::DrawOpts {
                    font_size: bf,
                    color: theme::accent(),
                    bold: true,
                    italic: false,
                },
            );
            g.queue_icon("external-link", lx + lw + 4.0, cy - 6.0, 12.0, theme::accent());
            st.hits.push((AccountMenuItem::UseLimitReset, r));
        }
        ry += reset_row_h;
        g.rect(mx + pad_x, ry + 2.0, mw - pad_x * 2.0, 1.0, theme::border());
        ry += rule;
    }
    let body_top = ry;
    let body_rect = (mx + pad, body_top, mw - pad * 2.0, body_h);
    *st.body_rect = Some(body_rect);
    let body_hit_start = st.hits.len();
    g.push_clip(body_rect.0, body_rect.1, body_rect.2, body_rect.3);
    ry -= *st.scroll;
    let body_hover = hmx >= body_rect.0
        && hmx <= body_rect.0 + body_rect.2
        && hmy >= body_rect.1
        && hmy <= body_rect.1 + body_rect.3;

    // ── 제공자 블록 ─────────────────────────────────────────────
    let right = mx + mw - pad_x - icon - 8.0;
    let ix = mx + pad_x + 21.0;
    let mut submenu_anchor = None;
    for (p, _) in provs.iter().copied() {
        let prow_h = provider_h(p);
        let open = frame.provider == Some(p);
        let on = body_hover && hmy >= ry && hmy <= ry + prow_h;
        g.hover_pointer |= on;
        let hy = ry + blk_top;
        g.queue_icon(
            "chevron-right",
            mx + mw - pad_x - icon,
            hy + (ph_h - icon) / 2.0,
            icon,
            theme::text_dim(),
        );
        if on || open {
            round_rect(
                g,
                mx + pad,
                ry,
                mw - pad * 2.0,
                prow_h,
                theme::radius_sm(),
                theme::surface_active(),
            );
        }
        // 머리: 아이콘·이름·계정 별명 왼쪽, 플랜(또는 간단히 모드의 숫자) 오른쪽.
        let pi = 14.0_f32;
        g.queue_icon(
            p.icon(),
            mx + pad_x,
            hy + (ph_h - pi) / 2.0,
            pi,
            theme::text_dim(),
        );
        let nf = f - 1.0;
        let name_x = mx + pad_x + pi + 7.0;
        g.draw_text(
            name_x,
            hy + (ph_h - nf) / 2.0 - 1.0,
            p.label(),
            gpu::DrawOpts {
                font_size: nf,
                color: theme::text(),
                bold: true,
                italic: false,
            },
        );
        let acct = match p {
            AccountProvider::Claude => claude_account_label(
                frame.accounts.claude,
                frame.accounts.claude_list,
            ),
            AccountProvider::Codex => {
                codex_account_label(frame.accounts.codex, frame.accounts.codex_list)
            }
        };
        let af = f - 2.5;
        let nw = g.measure_chrome_text(p.label(), nf, true);
        let acct = crate::info::fit_text(g, &acct, (mw * 0.30).max(0.0), af, false);
        g.draw_text(
            name_x + nw + 7.0,
            hy + (ph_h - af) / 2.0 - 1.0,
            &acct,
            gpu::DrawOpts {
                font_size: af,
                color: theme::text_mute(),
                bold: false,
                italic: false,
            },
        );
        // 오른쪽 글자: 간단히 모드면 압박 숫자, 아니면 플랜.
        let rows = rows_of(p);
        let (rt, rcol, rbold) = if compact {
            match p {
                AccountProvider::Claude => match claude_badge.as_ref() {
                    Some(b) => (usage_text(b), pct_col(b.pct), true),
                    None => ("기록 없음".to_string(), theme::text_mute(), false),
                },
                AccountProvider::Codex => match codex_current_limits {
                    Some(limits) => {
                        let pressure =
                            limits.windows.iter().max_by(|a, b| a.1.total_cmp(&b.1));
                        let usage = pressure.map(|(minutes, pct, _)| {
                            format!(
                                "{}{pct:.0}% 씀 {}",
                                if limits.stale { "~" } else { "" },
                                codex_rate_window_label(Some(*minutes))
                            )
                        });
                        match (codex_missing(), usage) {
                            (None, Some(u)) => (
                                u,
                                pressure.map_or(theme::text_mute(), |(_, pct, _)| {
                                    pct_col(*pct)
                                }),
                                true,
                            ),
                            (Some(m), Some(u)) => (
                                format!("{m} · {u}"),
                                pressure.map_or(theme::text_mute(), |(_, pct, _)| {
                                    pct_col(*pct)
                                }),
                                true,
                            ),
                            (_, None) => {
                                ("한도 미제공".to_string(), theme::text_mute(), false)
                            }
                        }
                    }
                    None if codex_signed_in => {
                        ("한도 확인 중…".to_string(), theme::text_mute(), false)
                    }
                    None => ("로그인 안 됨".to_string(), theme::danger(), false),
                },
            }
        } else {
            let plan = match p {
                AccountProvider::Codex => codex_current_limits
                    .and_then(|l| l.plan.clone())
                    .map(|plan| {
                        let mut c = plan.chars();
                        match c.next() {
                            Some(h) => {
                                h.to_uppercase().collect::<String>() + c.as_str()
                            }
                            None => String::new(),
                        }
                    })
                    .unwrap_or_default(),
                AccountProvider::Claude => String::new(),
            };
            (plan, theme::text_mute(), false)
        };
        if !rt.is_empty() {
            let tf = if rbold { f - 1.0 } else { f - 2.5 };
            let avail = right
                - (name_x + nw + 7.0 + g.measure_chrome_text(&acct, af, false) + 8.0);
            let rt = crate::info::fit_text(g, &rt, avail.max(0.0), tf, rbold);
            let tw = g.measure_chrome_text(&rt, tf, rbold);
            g.draw_text(
                right - tw,
                hy + (ph_h - tf) / 2.0 - 1.0,
                &rt,
                gpu::DrawOpts {
                    font_size: tf,
                    color: rcol,
                    bold: rbold,
                    italic: false,
                },
            );
        }
        // 창 줄. 간단히 모드는 머리 한 줄로 끝난다.
        if !compact {
            let mut wy = hy + ph_h;
            let stale = match p {
                AccountProvider::Claude => {
                    claude_badge.as_ref().is_some_and(|b| b.stale)
                }
                AccountProvider::Codex => codex_current_limits.is_some_and(|l| l.stale),
            };
            if let (AccountProvider::Codex, Some(t)) = (p, codex_missing()) {
                draw_usage_note(g, ix, wy, win_h_row, f - 2.0, &t, theme::text_mute());
                wy += win_h_row;
            }
            if rows.is_empty() {
                let (t, col) = match p {
                    AccountProvider::Claude => ("기록 없음", theme::text_mute()),
                    AccountProvider::Codex if codex_current_limits.is_some() => {
                        ("한도 미제공", theme::text_mute())
                    }
                    AccountProvider::Codex if codex_signed_in => {
                        ("한도 확인 중…", theme::text_mute())
                    }
                    AccountProvider::Codex => ("로그인 안 됨", theme::danger()),
                };
                if !(p == AccountProvider::Codex && codex_missing().is_some()) {
                    draw_usage_note(g, ix, wy, win_h_row, f - 2.0, t, col);
                }
            }
            for row in &rows {
                draw_usage_win_row(
                    g, ix, wy, right, win_h_row, f, &row.lab, row.pct, &row.rs,
                    row.sub, stale,
                );
                wy += win_h_row;
            }
        }
        st.hits
            .push((AccountMenuItem::Provider(p), (mx, ry, mw, prow_h)));
        ry += prow_h;
        if open {
            submenu_anchor = g.clip_hit((mx, ry - prow_h, mw, prow_h));
        }
    }

    for (_, rect) in &mut st.hits[body_hit_start..] {
        *rect = g.clip_hit(*rect).unwrap_or((0.0, 0.0, 0.0, 0.0));
    }
    st.hits
        .retain(|(_, rect)| rect.2 > 0.0 && rect.3 > 0.0);
    g.pop_clip();
    if *st.scroll_max > 0.0 && body_h > 0.0 {
        let thumb_h = (body_h * body_h / content_h).max(20.0).min(body_h);
        let thumb_y = body_top
            + (body_h - thumb_h) * *st.scroll
                / *st.scroll_max;
        round_rect(
            g,
            mx + mw - 5.0,
            thumb_y,
            2.0,
            thumb_h,
            1.0,
            theme::text_mute(),
        );
    }
    ry = body_top + body_h;

    // ── 하단 액션 ───────────────────────────────────────────────
    g.rect(mx + pad, ry + 2.0, mw - pad * 2.0, 1.0, theme::border());
    ry += rule;
    for (item, label) in [
        (
            AccountMenuItem::UsageDetails,
            if compact {
                "사용량 자세히"
            } else {
                "사용량 간단히"
            },
        ),
        (AccountMenuItem::ManageAccounts, "계정 관리"),
    ] {
        let on = hmx >= mx && hmx <= mx + mw && hmy >= ry && hmy <= ry + row_h;
        g.hover_pointer |= on;
        if on {
            round_rect(
                g,
                mx + pad,
                ry,
                mw - pad * 2.0,
                row_h,
                theme::radius_sm(),
                theme::surface_active(),
            );
        }
        g.draw_text(
            mx + pad_x,
            ry + (row_h - f) / 2.0 - 1.0,
            label,
            gpu::DrawOpts {
                font_size: f,
                color: theme::text_dim(),
                bold: false,
                italic: false,
            },
        );
        g.queue_icon(
            if matches!(item, AccountMenuItem::ManageAccounts) {
                "external-link"
            } else if compact {
                "chevron-down"
            } else {
                "chevron-up"
            },
            mx + mw - pad_x - icon,
            ry + (row_h - icon) / 2.0,
            icon,
            theme::text_dim(),
        );
        st.hits.push((item, (mx, ry, mw, row_h)));
        ry += row_h;
    }

    // ── 판 번호 ─────────────────────────────────────────────────
    // 「나 카사텀 버전몇이지」(2026-08-29)가 바로 답이 나오는 자리.
    // 상태줄엔 번호만 두고 사정은 여기서 말한다 — 늘 보이는 줄에
    // 넣기엔 셋 중 둘이 평소엔 아무 일 없다는 말이라서다.
    //
    // 오른쪽 문구의 우선순위는 **사용자가 지금 할 수 있는 일** 순이다.
    // 구워 둔 것이 있으면 껐다 켜기만 하면 되니 그게 먼저고, 그다음이
    // 아직 받지 않은 새 판이다.
    g.rect(mx + pad, ry + 2.0, mw - pad * 2.0, 1.0, theme::border());
    ry += rule;
    {
        // 누르면 이 기기의 업데이터가 확인·받기·설치를 묻는다 — 설정을 뒤지지 않고
        // 번호를 본 자리에서 바로 받게. 설치·재실행은 업데이터 창에서 사람이 고른다.
        let ver_hover = hmx >= mx && hmx <= mx + mw && hmy >= ry && hmy <= ry + ver_h;
        g.hover_pointer |= ver_hover;
        if ver_hover {
            round_rect(g, mx + pad, ry, mw - pad * 2.0, ver_h, theme::radius_sm(), theme::surface_active());
        }
        st.hits
            .push((AccountMenuItem::CheckUpdates, (mx, ry, mw, ver_h)));
        let vf = f - 2.0;
        let left = format!("카사텀 {}", crate::version::label());
        g.draw_text(
            mx + pad_x,
            ry + (ver_h - vf) / 2.0 - 1.0,
            &left,
            gpu::DrawOpts {
                font_size: vf,
                color: theme::text_mute(),
                bold: false,
                italic: false,
            },
        );
        let (note, col) = if crate::install_pending() {
            ("새 판 준비됨 · 껐다 켜기".to_string(), theme::accent())
        } else if crate::version::is_local_build() {
            // 피드와 견주지 않았으므로 견준 척도 하지 않는다. 대신 그
            // 판이 언제 것인지를 말한다 — 「아까 구운 게 이건가」가
            // 손수 구운 판에 오는 유일한 질문이다.
            let built = crate::version::BUILT;
            let t = if built.is_empty() {
                "내 빌드".to_string()
            } else {
                format!("내 빌드 · {built}")
            };
            (t, theme::text_mute())
        } else {
            match crate::version::state() {
                crate::version::Check::Newer(v) => {
                    (format!("v{v} 나왔음"), theme::accent())
                }
                crate::version::Check::Latest => {
                    ("최신".to_string(), theme::text_mute())
                }
                crate::version::Check::Busy => {
                    ("확인 중…".to_string(), theme::text_dim())
                }
                crate::version::Check::Failed => {
                    ("확인 못 함".to_string(), theme::text_dim())
                }
                crate::version::Check::Idle => (String::new(), theme::text_dim()),
            }
        };
        if !note.is_empty() {
            let left_w = g.measure_chrome_text(&left, vf, false);
            let note = crate::info::fit_text(
                g,
                &note,
                (mw - pad_x * 2.0 - left_w - 12.0).max(0.0),
                vf,
                false,
            );
            let nw = g.measure_chrome_text(&note, vf, false);
            g.draw_text(
                mx + mw - pad_x - nw,
                ry + (ver_h - vf) / 2.0 - 1.0,
                &note,
                gpu::DrawOpts {
                    font_size: vf,
                    color: col,
                    bold: false,
                    italic: false,
                },
            );
        }
    }
    for (_, rect) in st.hits.iter_mut() {
        *rect = g.clip_hit(*rect).unwrap_or((0.0, 0.0, 0.0, 0.0));
    }
    st.hits
        .retain(|(_, rect)| rect.2 > 0.0 && rect.3 > 0.0);
    g.pop_clip();
    if let (
        Some(provider_rect),
        Some((p, rows, row_heights, codex_note, lab_h, empty_h)),
    ) = (submenu_anchor, expanded_accounts.as_ref())
    {
        let p = *p;
        let child_content = empty_h + row_heights.iter().sum::<f32>();
        let child_fixed = pad * 2.0
            + lab_h
            + if p == AccountProvider::Codex {
                24.0
            } else {
                0.0
            };
        let child_layout = account_popover::flyout_layout(
            (win_w, win_h - frame.status_h),
            (mx, my, mw, mh),
            provider_rect,
            380.0,
            child_fixed,
            child_content,
        );
        let (sx, sy, sw, sh) = child_layout.frame;
        *st.submenu_rect = Some(child_layout.frame);
        *st.corridor_rect = child_layout.corridor;
        *st.submenu_scroll_max = child_layout.scroll_max;
        *st.submenu_scroll = st
            .submenu_scroll
            .clamp(0.0, child_layout.scroll_max);
        *st.submenu_hit_start = st.hits.len();
        if hmx >= sx && hmx <= sx + sw && hmy >= sy && hmy <= sy + sh {
            g.hover_pointer = false;
        }
        panel_rect_outlined(
            g,
            sx,
            sy,
            sw,
            sh,
            theme::radius_sm(),
            theme::surface_hover(),
        );
        g.push_clip(sx, sy, sw, sh);
        let rows = rows.clone();
        let (lab_h, empty_h) = (*lab_h, *empty_h);
        let mut sry = sy + pad;
        {
            let t = format!("{} 계정", p.label());
            let lf = f - 2.0;
            let header = (sx + pad, sry, sw - pad * 2.0, lab_h);
            let header_hover = hmx >= header.0
                && hmx <= header.0 + header.2
                && hmy >= header.1
                && hmy <= header.1 + header.3;
            g.hover_pointer |= header_hover;
            if header_hover {
                round_rect(
                    g,
                    header.0,
                    header.1,
                    header.2,
                    header.3,
                    theme::radius_sm(),
                    theme::surface_active(),
                );
            }
            g.queue_icon(
                "chevron-left",
                sx + pad_x,
                sry + 2.0,
                icon,
                theme::text_dim(),
            );
            if let Some(hit) = g.clip_hit(header) {
                st.hits
                    .push((AccountMenuItem::Provider(p), hit));
            }
            g.draw_text(
                sx + pad_x + icon + 6.0,
                sry + 3.0,
                &t,
                gpu::DrawOpts {
                    font_size: lf,
                    color: theme::text_mute(),
                    bold: true,
                    italic: false,
                },
            );
            if let Some(note) = codex_note.as_deref() {
                let nf = f - 4.0;
                let note = crate::info::fit_text(g, note, sw - pad_x * 2.0, nf, false);
                g.draw_text(
                    sx + pad_x,
                    sry + 21.0,
                    &note,
                    gpu::DrawOpts {
                        font_size: nf,
                        color: theme::text_mute(),
                        bold: false,
                        italic: false,
                    },
                );
            }
            sry += lab_h;
        }
        let child_body_top = sry;
        let child_body_hit_start = st.hits.len();
        let child_body = (sx + pad, sry, sw - pad * 2.0, child_layout.body_height);
        *st.submenu_body_rect = Some(child_body);
        g.push_clip(child_body.0, child_body.1, child_body.2, child_body.3);
        let body_hover = hmx >= child_body.0
            && hmx <= child_body.0 + child_body.2
            && hmy >= child_body.1
            && hmy <= child_body.1 + child_body.3;
        sry -= *st.submenu_scroll;
        if rows.is_empty() {
            let note = crate::info::fit_text(
                g,
                "등록한 계정이 없어요",
                sw - pad_x * 2.0,
                f - 1.0,
                false,
            );
            g.draw_text(
                sx + pad_x,
                sry + 6.0,
                &note,
                gpu::DrawOpts {
                    font_size: f - 1.0,
                    color: theme::text_dim(),
                    bold: false,
                    italic: false,
                },
            );
            sry += empty_h;
        }
        for (row_index, (id, label, active)) in rows.into_iter().enumerate() {
            let arow_h = row_heights[row_index];
            let two_line = !compact || p == AccountProvider::Claude;
            let on = body_hover
                && hmx >= sx
                && hmx <= sx + sw
                && hmy >= sry
                && hmy <= sry + arow_h;
            // 활성 행은 갈 곳이 없다 — hover 도 히트박스도 손모양도 없다.
            g.hover_pointer |= on && !active;
            if on && !active {
                round_rect(
                    g,
                    sx + pad,
                    sry,
                    sw - pad * 2.0,
                    arow_h,
                    theme::radius_sm(),
                    theme::surface_active(),
                );
            }
            let line1 = sry + 7.0;
            let label =
                crate::info::fit_text(g, &label, sw - pad_x * 2.0 - 56.0, f, active);
            g.draw_text(
                sx + pad_x,
                line1,
                &label,
                gpu::DrawOpts {
                    font_size: f,
                    color: if active {
                        theme::text()
                    } else {
                        theme::text_dim()
                    },
                    bold: active,
                    italic: false,
                },
            );
            let tf = f - 3.0;
            let right = sx + sw - pad_x;
            // 라벨 옆에 **누구인지**. 라벨은 사람이 붙인 별명이라
            // (「네이버」·「지메일」) 그것만으로는 어느 계정인지 확인이
            // 안 되는데, 설정 화면 카드에는 있고 이 목록에만 없었다
            // (2026-09-07 「하단바에서도 계정뭔지 나오게해줘」).
            // 별명이 곧 이메일인 슬롯에서는 같은 말을 두 번 하지 않는다.
            let who = match p {
                AccountProvider::Claude => {
                    crate::settings::auth_probe(&id).map(|probe| probe.email)
                }
                AccountProvider::Codex => crate::settings::codex_identity(&id),
            };
            if let Some(who) =
                who.filter(|who| !who.is_empty() && !label.contains(who.as_str()))
            {
                let lw = g.measure_chrome_text(&label, f, active);
                let wx = sx + pad_x + lw + 8.0;
                let room = right - 52.0 - wx;
                if room > 30.0 {
                    let who = crate::info::fit_text(g, &who, room, tf, false);
                    g.draw_text(
                        wx,
                        line1 + (f - tf) / 2.0,
                        &who,
                        gpu::DrawOpts {
                            font_size: tf,
                            color: theme::with_alpha(theme::text_dim(), 170),
                            bold: false,
                            italic: false,
                        },
                    );
                }
            }
            // 활성 표시는 오른쪽 배지. 체크 아이콘이나 왼쪽 막대와 달리,
            // 그 자리에 다른 계정이 쓰는 한도 숫자와 같은 층으로 읽힌다.
            if active {
                let t = if p == AccountProvider::Claude {
                    "선택됨"
                } else {
                    "사용 중"
                };
                let tw = g.measure_chrome_text(t, tf, true);
                g.draw_text(
                    right - tw,
                    line1 + if two_line { 0.0 } else { (f - tf) / 2.0 },
                    t,
                    gpu::DrawOpts {
                        font_size: tf,
                        color: theme::text_mute(),
                        bold: true,
                        italic: false,
                    },
                );
            } else if p == AccountProvider::Codex
                && !crate::settings::codex_logged_in(&id)
                && !crate::codexlimits::seeded_for_probe(&id)
            {
                let t = "로그인";
                let tw = g.measure_chrome_text(t, tf, true);
                g.draw_text(
                    right - tw,
                    line1 + if two_line { 0.0 } else { (f - tf) / 2.0 },
                    t,
                    gpu::DrawOpts {
                        font_size: tf,
                        color: theme::danger(),
                        bold: true,
                        italic: false,
                    },
                );
            }
            // 두 제공자 모두 계정별 direct snapshot을 쓴다. 상세 모드는
            // 한 줄에 게이지 둘만 놓고 세 번째 모델 창은 다음 줄로 보낸다.
            match p {
                AccountProvider::Claude => match (usage_of(&id), two_line) {
                    (Some(b), true) => {
                        let note = if b.stale {
                            "이전 조회".to_string()
                        } else {
                            crate::resets_in_label(b.resets_at)
                                .map(|s| format!("{s} 뒤 초기화"))
                                .unwrap_or_default()
                        };
                        let note = crate::info::fit_text(
                            g,
                            &note,
                            (sw - 172.0).max(0.0),
                            tf,
                            false,
                        );
                        g.draw_text(
                            sx + pad_x,
                            sry + arow_h - 22.0,
                            &note,
                            gpu::DrawOpts {
                                font_size: tf,
                                color: theme::text_dim(),
                                bold: false,
                                italic: false,
                            },
                        );
                        let wins: Vec<(String, f32)> = if b.windows.is_empty() {
                            vec![(b.label.clone(), b.pct)]
                        } else {
                            b.windows
                                .iter()
                                .map(|window| (window.label.clone(), window.pct))
                                .collect()
                        };
                        for (line, chunk) in wins.chunks(2).enumerate() {
                            draw_window_gauges(
                                g,
                                sx + pad_x,
                                sry + 28.0 + line as f32 * 16.0,
                                right,
                                tf,
                                chunk,
                                b.stale,
                            );
                        }
                    }
                    (Some(b), false) => {
                        let t = usage_text(&b);
                        let tw = g.measure_chrome_text(t.as_str(), tf, true);
                        // 「사용 중」 배지와 겹치지 않게 그 왼쪽으로 물린다.
                        let bx = if active {
                            right - g.measure_chrome_text("사용 중", tf, true) - 8.0
                        } else {
                            right
                        };
                        g.draw_text(
                            bx - tw,
                            sry + (arow_h - tf) / 2.0 - 1.0,
                            &t,
                            gpu::DrawOpts {
                                font_size: tf,
                                color: pct_col(b.pct),
                                bold: true,
                                italic: false,
                            },
                        );
                    }
                    (None, _) => {
                        let status = frame.claude_observations
                            .get(&id)
                            .map_or("unselected", |(_, state)| *state);
                        let signed_out = status == "logged_out";
                        let t = match status {
                            "logged_out" => "로그인 필요",
                            "failed" => "확인 못 함",
                            _ => "확인 중…",
                        };
                        let ty2 = if two_line {
                            sry + 28.0
                        } else {
                            sry + (arow_h - tf) / 2.0 - 1.0
                        };
                        let tx = if two_line {
                            sx + pad_x
                        } else {
                            right - g.measure_chrome_text(t, tf, false)
                        };
                        g.draw_text(
                            tx,
                            ty2,
                            t,
                            gpu::DrawOpts {
                                font_size: tf,
                                color: if signed_out {
                                    theme::danger()
                                } else {
                                    theme::text_mute()
                                },
                                bold: false,
                                italic: false,
                            },
                        );
                    }
                },
                AccountProvider::Codex => {
                    let signed_out = !crate::settings::codex_logged_in(&id)
                        && !crate::codexlimits::seeded_for_probe(&id);
                    let snapshot = (!signed_out)
                        .then(|| crate::codexlimits::snapshot_for(&id))
                        .flatten();
                    let wins = if signed_out {
                        Vec::new()
                    } else {
                        codex_windows_for(&id).unwrap_or_default()
                    };
                    match (snapshot.as_ref(), two_line) {
                        (Some(limits), true) => {
                            let pressure = limits
                                .windows
                                .iter()
                                .max_by(|a, b| a.1.total_cmp(&b.1));
                            if let Some(t) = pressure
                                .and_then(|(_, _, at)| *at)
                                .filter(|at| *at > 0)
                                .and_then(|at| crate::resets_in_label(Some(at as u64)))
                            {
                                let t = crate::info::fit_text(
                                    g,
                                    &format!("{t} 뒤 초기화"),
                                    (sw - 172.0).max(0.0),
                                    tf,
                                    false,
                                );
                                g.draw_text(
                                    sx + pad_x,
                                    sry + arow_h - 22.0,
                                    &t,
                                    gpu::DrawOpts {
                                        font_size: tf,
                                        color: theme::text_mute(),
                                        bold: false,
                                        italic: false,
                                    },
                                );
                            }
                            let missing: Vec<&str> = ["5h", "7d"]
                                .into_iter()
                                .filter(|wanted| {
                                    !wins.iter().any(|(label, _)| label == wanted)
                                })
                                .collect();
                            let mut line = 0usize;
                            if !missing.is_empty() {
                                let t = format!("{} 미제공", missing.join("·"));
                                g.draw_text(
                                    sx + pad_x,
                                    sry + 28.0,
                                    &t,
                                    gpu::DrawOpts {
                                        font_size: tf,
                                        color: theme::text_mute(),
                                        bold: false,
                                        italic: false,
                                    },
                                );
                                line += 1;
                            }
                            for chunk in wins.chunks(2) {
                                draw_window_gauges(
                                    g,
                                    sx + pad_x,
                                    sry + 28.0 + line as f32 * 16.0,
                                    right,
                                    tf,
                                    chunk,
                                    limits.stale,
                                );
                                line += 1;
                            }
                        }
                        (Some(limits), false) => {
                            let pressure = limits
                                .windows
                                .iter()
                                .max_by(|a, b| a.1.total_cmp(&b.1));
                            let missing: Vec<&str> = ["5h", "7d"]
                                .into_iter()
                                .filter(|wanted| {
                                    !wins.iter().any(|(label, _)| label == wanted)
                                })
                                .collect();
                            let usage = pressure.map(|(minutes, pct, _)| {
                                format!(
                                    "{}{pct:.0}% 씀 · {}",
                                    if limits.stale { "~" } else { "" },
                                    codex_rate_window_label(Some(*minutes))
                                )
                            });
                            let t = match (missing.is_empty(), usage) {
                                (true, Some(usage)) => usage,
                                (false, Some(usage)) => {
                                    format!("{} 미제공 · {usage}", missing.join("·"))
                                }
                                (_, None) => "한도 미제공".to_string(),
                            };
                            let t = crate::info::fit_text(
                                g,
                                &t,
                                sw - pad_x * 2.0,
                                tf,
                                true,
                            );
                            g.draw_text(
                                sx + pad_x,
                                sry + 28.0,
                                &t,
                                gpu::DrawOpts {
                                    font_size: tf,
                                    color: pressure
                                        .map_or(theme::text_mute(), |(_, pct, _)| {
                                            pct_col(*pct)
                                        }),
                                    bold: true,
                                    italic: false,
                                },
                            );
                        }
                        (None, _) => {
                            let t = if signed_out {
                                "로그인 필요"
                            } else {
                                "한도 확인 중…"
                            };
                            g.draw_text(
                                sx + pad_x,
                                sry + 28.0,
                                t,
                                gpu::DrawOpts {
                                    font_size: tf,
                                    color: if signed_out {
                                        theme::danger()
                                    } else {
                                        theme::text_mute()
                                    },
                                    bold: false,
                                    italic: false,
                                },
                            );
                        }
                    }
                }
            }
            if !active {
                st.hits.push((
                    AccountMenuItem::Select(p, id.clone()),
                    (sx, sry, sw, arow_h),
                ));
            }
            // 곁 단추 — 다시 로그인·목록에서 빼기. 설정 화면에만 있던
            // 것을 여기에도 둔다(2026-09-07 「하단바랑 설정이랑 완전
            // 똑같이 떠야해」): 로그인이 풀린 것을 **여기서** 보게 됐으니
            // 고치는 것도 여기여야 한다.
            //
            // Action hits follow selection so the topmost painted control wins.
            {
                let bf = tf - 0.5;
                let by = sry + arow_h - 22.0;
                let mut bx = right;
                for (label, item, skip) in [
                    (
                        "빼기",
                        AccountMenuItem::Forget(p, id.clone()),
                        // 기본 로그인은 뺄 수 있는 것이 아니다 — 목록에
                        // 없는 암묵적 첫 줄이라 지울 대상이 없다.
                        id.is_empty(),
                    ),
                    ("다시 로그인", AccountMenuItem::Reauth(p, id.clone()), false),
                ] {
                    if skip {
                        continue;
                    }
                    let tw = g.measure_chrome_text(label, bf, false);
                    bx -= tw + 14.0;
                    let r = (bx - 5.0, by - 3.0, tw + 10.0, 18.0);
                    let hot = body_hover
                        && hmx >= r.0
                        && hmx <= r.0 + r.2
                        && hmy >= r.1
                        && hmy <= r.1 + r.3;
                    g.hover_pointer |= hot;
                    if hot {
                        round_rect(
                            g,
                            r.0,
                            r.1,
                            r.2,
                            r.3,
                            theme::radius_sm(),
                            theme::surface_active(),
                        );
                    }
                    g.draw_text(
                        bx,
                        by,
                        label,
                        gpu::DrawOpts {
                            font_size: bf,
                            color: if hot {
                                theme::text()
                            } else {
                                theme::text_mute()
                            },
                            bold: false,
                            italic: false,
                        },
                    );
                    st.hits.push((item, r));
                }
            }
            sry += arow_h;
        }

        for (_, rect) in &mut st.hits[child_body_hit_start..] {
            *rect = g.clip_hit(*rect).unwrap_or((0.0, 0.0, 0.0, 0.0));
        }
        st.hits
            .retain(|(_, rect)| rect.2 > 0.0 && rect.3 > 0.0);
        g.pop_clip();
        if child_layout.scroll_max > 0.0 && child_layout.body_height > 0.0 {
            let body_h = child_layout.body_height;
            let thumb_h = (body_h * body_h / child_content).max(20.0).min(body_h);
            let thumb_y = child_body_top
                + (body_h - thumb_h) * *st.submenu_scroll
                    / child_layout.scroll_max;
            round_rect(
                g,
                sx + sw - 5.0,
                thumb_y,
                2.0,
                thumb_h,
                1.0,
                theme::text_mute(),
            );
        }
        if p == AccountProvider::Codex {
            draw_usage_note(
                g,
                sx + pad_x,
                child_body_top + child_layout.body_height,
                24.0,
                f - 2.0,
                "선택은 다음 Codex 실행부터 적용",
                theme::text_dim(),
            );
        }

        g.pop_clip();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flyout_prefers_right_and_connects_only_provider_row() {
        let p = flyout_layout(
            (1200.0, 800.0),
            (8.0, 300.0, 380.0, 300.0),
            (8.0, 350.0, 380.0, 40.0),
            380.0,
            40.0,
            200.0,
        );
        assert_eq!(p.frame, (392.0, 350.0, 380.0, 240.0));
        assert_eq!(p.corridor, Some((388.0, 350.0, 4.0, 40.0)));
    }

    #[test]
    fn flyout_flips_left_when_right_is_full() {
        let p = flyout_layout(
            (1200.0, 800.0),
            (800.0, 300.0, 380.0, 300.0),
            (800.0, 350.0, 380.0, 40.0),
            380.0,
            40.0,
            200.0,
        );
        assert_eq!(p.frame.0, 416.0);
        assert_eq!(p.corridor, Some((796.0, 350.0, 4.0, 40.0)));
    }

    #[test]
    fn narrow_flyout_overlaps_with_bounded_scroll_body() {
        let p = flyout_layout(
            (300.0, 400.0),
            (8.0, 100.0, 284.0, 272.0),
            (8.0, 150.0, 284.0, 40.0),
            380.0,
            40.0,
            2000.0,
        );
        assert_eq!(p.frame, (8.0, 8.0, 284.0, 384.0));
        assert_eq!(p.corridor, None);
        assert_eq!(p.body_height, 344.0);
        assert_eq!(p.scroll_max, 1656.0);
    }

    #[test]
    fn bottom_trigger_and_panel_share_an_edge() {
        let p = layout(
            (1200.0, 800.0),
            (980.0, 772.0, 140.0, 28.0),
            380.0,
            127.0,
            240.0,
        );
        assert!(p.above);
        assert_eq!(p.frame.1 + p.frame.3, 772.0);
        assert_eq!(p.frame.0 + p.frame.2, 1120.0);
        assert_eq!(p.scroll_max, 0.0);
    }

    #[test]
    fn narrow_window_keeps_the_full_panel_inside() {
        let p = layout(
            (300.0, 500.0),
            (250.0, 472.0, 42.0, 28.0),
            380.0,
            127.0,
            200.0,
        );
        assert_eq!(p.frame.0, 8.0);
        assert_eq!(p.frame.2, 284.0);
        assert!(p.frame.0 + p.frame.2 <= 292.0);
    }

    #[test]
    fn long_accounts_scroll_without_moving_fixed_chrome() {
        let p = layout(
            (800.0, 400.0),
            (620.0, 372.0, 100.0, 28.0),
            380.0,
            127.0,
            2000.0,
        );
        assert_eq!(p.frame.1, 8.0);
        assert_eq!(p.frame.3 - p.body_height, 127.0);
        assert_eq!(p.scroll_max + p.body_height, 2000.0);
    }

    #[test]
    fn upper_trigger_opens_below_without_a_gap() {
        let p = layout(
            (800.0, 600.0),
            (10.0, 50.0, 120.0, 28.0),
            380.0,
            127.0,
            200.0,
        );
        assert!(!p.above);
        assert_eq!(p.frame.1, 78.0);
        assert_eq!(p.frame.0, 8.0);
    }
}
