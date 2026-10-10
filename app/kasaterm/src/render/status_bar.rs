use super::*;

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
    let (status_h, claude_observations) = (f.status_h, f.claude_observations);
    let (win_w, win_h) = f.viewport;
    let sy = win_h - status_h;
    g.rect(0.0, sy, win_w, status_h, theme::panel_bg());
    g.rect(0.0, sy, win_w, 1.0, theme::border());

    let status_prefs = f.prefs;
    let badge = claude_observations.get(f.accounts.claude).and_then(|(badge, _)| badge.clone());
    let acct_name = claude_account_label(
        f.accounts.claude,
        f.accounts.claude_list,
    );

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
    let widget_count = ["ports", "schedules", "pet", "clipboard", "resources", "version", "tunnel", "link"]
        .iter().filter(|id| status_prefs.visible(id))
        .map(|id| if *id == "tunnel" { 2 } else { 1 }).sum::<usize>().max(1);
    let compact_tools = (win_w * 0.68 - 24.0) / (widget_count as f32) < 28.0;
    let account_right = if status_prefs.visible("claude") || status_prefs.visible("codex") {
        if compact_tools { seg_x0 + 16.0 } else { (win_w * 0.32).max(seg_x0).min(win_w - 12.0) }
    } else { seg_x0 };
    g.push_clip(seg_x0 - 6.0, sy, (account_right - seg_x0 + 6.0).max(0.0), status_h);
    let mut account_drawn = false;
    macro_rules! draw_claude_status {
        () => {{
    if status_prefs.visible("claude") && !compact_tools {
        account_drawn = true;
    // 클로드 로고 — 이 숫자가 「클로드 한도」라는 것을 그림이 먼저
    // 말한다(2026-08-16 「클로드사용량 로고도 넣어주고」). 계정 이름은
    // 좁아지면 빠지는 값이라 로고가 유일한 정체 표식이 되는 폭이 있다.
    if win_w >= 500.0 {
        g.queue_icon(
            "claude",
            x,
            sy + (status_h - 12.0) / 2.0,
            12.0,
            status_prefs.color("claude", theme::text_dim()),
        );
        x += 17.0;
    }

    let account_state = claude_observations.get(f.accounts.claude).map_or("unselected", |(_, state)| *state);
    if account_state != "ready" {
        let note = match account_state {
            "unselected" => "계정 선택 필요",
            "logged_out" => "로그인 필요",
            "failed" => "확인 못 함",
            _ => "확인 중…",
        };
        let label = if account_state == "unselected" {
            note.to_string()
        } else {
            format!("{acct_name} · {note}")
        };
        let state = match account_state {
            "logged_out" => Some(ChipState::Bad),
            "unselected" | "failed" => Some(ChipState::Warn),
            _ => None,
        };
        let dot = if state.is_some() { STATUS_DOT_GAP + STATUS_DOT } else { 0.0 };
        let label = crate::info::fit_text(g, &label, (account_right - x - dot).max(0.0), fs, false);
        g.draw_text(x, ty, &label, gpu::DrawOpts {
            font_size: fs, color: status_prefs.color("claude", theme::text_dim()),
            bold: false, italic: false,
        });
        x += g.measure_chrome_text(&label, fs, false);
        if let Some(state) = state {
            x += status_dot(g, x, sy, status_h, state);
        }
        x += 10.0;
    } else {

    // 게이지 — Orca 처럼 **항상 중립색**이다. 하단바에서까지 빨갛게 하면
    // 시야 끝에서 늘 깜빡이는 경고가 되어 오히려 안 보게 된다. 위험은
    // 숫자 색으로만 말한다(드롭다운·Info pill 과 같은 임계값).
    // 한도 — **5시간이 먼저고 주간이 그 옆**이다(2026-08-15 지시
    // 「5시간 한도 먼저 보여주고 7일 한도는 눌렀을 때만」).
    //
    // 「눌렀을 때만」을 곧이곧대로 주간을 **숨기는** 것으로 읽으면 2026-08-05
    // 사고가 되돌아온다: 그때 5시간이 0%, 주간이 95% 였는데 하단바가 0% 를
    // 띄워 「3계정 다 소진이야? info엔 다 0퍼로뜨는데」가 됐다. 그래서 둘을
    // 나란히 두고, 폭이 모자랄 때만 급한 쪽을 남긴다 — 요청도 지켜지고
    // 그 사고도 안 돌아온다(사용자 확정: 「둘 다 나란히」).
    //
    // 게이지는 Orca 처럼 **항상 중립색**이다. 하단바에서까지 빨갛게 하면
    // 시야 끝에서 늘 깜빡이는 경고가 되어 오히려 안 보게 된다. 위험은
    // 숫자 색으로만 말한다(드롭다운·Info pill 과 같은 임계값).
    // Orca 하단바와 **같은 규격**이다(`h-[5px] w-7 rounded-full` — 앱
    // 번들에서 실측, 2026-09-06 「Orca랑 똑같이 해보라니까」). 전에는
    // 40x6 각진 막대였는데, 그 차이가 이 줄에서 제일 먼저 눈에 띈다.
    const GW: f32 = 28.0;
    const GH: f32 = 5.0;
    let gy = sy + (status_h - GH) / 2.0;
    let pct_col = |p: f32| {
        if p >= 90.0 {
            theme::danger()
        } else if p >= 70.0 {
            theme::syn_number()
        } else {
            theme::text()
        }
    };
    // **떠나온 계정의 숫자를 이어 그리지 않는다.** 계정을 바꾸면 이름은
    // 그 자리에서 바뀌는데 새 사용량은 1.1~2.0초(평균 1.6초) 뒤에 온다
    // (토키 실측 2026-08-15). 그 사이 「새 계정 이름 + 옛 계정 %」가
    // 그려지는데, 한도를 보고 계정을 고르는 기능이라 이 조합은 그냥
    // 거짓말이다. 배지가 어느 계정에서 나온 값인지 들고 다니므로
    // (`account_dir`) 활성 슬롯과 대조해 다르면 읽는 중으로 둔다.
    // 폴러가 조회한 자리와 **같은 규칙**으로 계산해야 한다 — 활성 계정은
    // 작업대라, 여기서 금고 경로를 쓰면 매번 「읽는 중」으로 보인다.
    // ⚠️`runtime_dir_for` 를 그대로 부르면 안 된다 — 활성 계정을 물으면
    // 자격증명을 읽느라 `security` 를 자식 프로세스로 띄우고(14ms),
    // 이 자리는 상태줄이라 **프레임마다** 돈다. pane 여럿이 동시에
    // 출력해 프레임이 쉼 없이 뜨는 동안 메인 스레드의 88%가 그
    // 대기였다(2026-08-18 실측). 캐시판은 답이 같고 전환 때 무효화된다.
    let active_dir = crate::claude_auth::runtime_dir_for_cached(
        f.accounts.claude,
        f.accounts.claude,
    )
    .map_or(String::new(), |p| p.to_string_lossy().into_owned());
    let switching = badge.as_ref().is_some_and(|b| b.account_dir != active_dir);

    // `windows` 는 5시간이 앞이고, `pct`/`label` 은 **가장 급한** 창이다.
    // 좁을 때 후자로 떨어지는 것이 요점 — 자리가 하나뿐이면 급한 쪽을
    // 보여야 한다. `None` 은 「읽는 중」 — 자리는 잡되 숫자는 안 말한다.
    // **평소엔 5시간 창 하나만**(사용자 2026-09-05 「평소에는 5시간 세션만
    // 보여주고 눌러야 보이게」). 창을 셋 다 세우면 줄 절반이 숫자가 되고,
    // 그중 지금 판단에 쓰는 것은 대개 5시간 하나다. 나머지는 이 세그먼트를
    // 누르면 열리는 계정 드롭다운에 이미 전부 있다.
    //
    // 다만 **접힌 창이 위험하면 그것도 세운다.** 접기만 하면 주간 95% 를
    // 놓치는데, 그건 예전에 실제로 당한 사고다(2026-08-05: 화면이
    // five_hour 만 봐서 weekly 95% 를 「0%」로 표시). 위험한 창을 숨기는
    // 것은 자리를 아끼는 게 아니라 틀린 답을 주는 것이다.
    let wins = selected_status_usage_windows(
        badge.as_ref(),
        &status_prefs,
        "claude",
        switching,
    );
    if wins.is_empty() && status_prefs.wants_usage("claude") {
        // 값이 없으면 `—`. 0% 로 그리면 「여유 있음」이라는 거짓말이 되고,
        // 그게 옮길지 말지를 정확히 반대로 만든다(드롭다운과 같은 규칙).
        g.draw_text(
            x,
            ty,
            "—",
            gpu::DrawOpts {
                font_size: fs,
                color: theme::text_dim(),
                bold: false,
                italic: false,
            },
        );
        x += g.measure_chrome_text("—", fs, true) + 10.0;
    }
    let stale = badge.as_ref().is_some_and(|b| b.stale);
    for (i, (label, pct)) in wins.iter().enumerate() {
        if i > 0 {
            x += 12.0;
        }
        // 창 이름은 둘을 나란히 둘 때 **반드시** 있어야 한다 — 없으면
        // 12% 와 95% 중 어느 쪽이 5시간인지 알 길이 없다. 하나만 그릴
        // 때는 좁은 창이라 접는다(그때는 급한 쪽이라는 것만 알면 된다).
        if wins.len() > 1 || win_w >= 900.0 {
            g.draw_text(
                x,
                ty,
                label,
                gpu::DrawOpts {
                    font_size: fs,
                    color: theme::text_dim(),
                    bold: false,
                    italic: false,
                },
            );
            x += g.measure_chrome_text(label, fs, true) + 5.0;
        }
        if win_w >= 500.0 {
            // 트랙이 보여야 «얼마나 남았나»가 읽힌다 — 채움만 그리면 15%
            // 짜리 짧은 막대가 어디까지 갈 수 있는 것인지 알 수가 없어서
            // 그냥 얼룩이 된다(첫 캡처에서 실제로 그랬다).
            round_rect(
                g,
                x,
                gy,
                GW,
                GH,
                GH / 2.0,
                theme::with_alpha(theme::text_dim(), 90),
            );
            // 읽는 중이면 **트랙만**. 빈 트랙은 0% 처럼 보일 수 있지만
            // 옆의 숫자가 `…` 라 「모른다」로 읽힌다 — 채움을 그리면
            // 그 순간 옛 숫자가 되살아난다.
            if let Some(p) = pct {
                let fw = (GW * (p / 100.0).clamp(0.0, 1.0)).max(GH);
                round_rect(
                    g,
                    x,
                    gy,
                    fw,
                    GH,
                    GH / 2.0,
                    theme::with_alpha(
                        status_prefs.color("claude", theme::text()),
                        210,
                    ),
                );
            }
            x += GW + 6.0;
        }
        let (s, col) = match pct {
            Some(p) if stale => (format!("~{p:.0}%"), pct_col(*p)),
            Some(p) => (format!("{p:.0}%"), pct_col(*p)),
            None => ("…".to_string(), theme::text_dim()),
        };
        g.draw_text(
            x,
            ty,
            &s,
            gpu::DrawOpts {
                font_size: fs,
                color: col,
                bold: false,
                italic: false,
            },
        );
        x += g.measure_chrome_text(&s, fs, true);
    }
    // 언제 풀리는지는 5시간 창에 대해서만, 그것도 아주 넓을 때만. 퍼센트가
    // 같아도 12분 뒤면 기다리면 되고 3시간 뒤면 지금 옮겨야 한다 — 다만
    // 두 창을 나란히 두고 나면 자리가 없어서, 좁아지면 팝오버로 물러난다.
    // 전환 중엔 이것도 빼야 한다 — 초기화 시각은 떠나온 계정 것이라
    // 게이지만 가리고 여기를 남기면 거짓말이 옆칸으로 옮겨갈 뿐이다.
    if let (Some(b), true) = (badge.as_ref(), win_w >= 1100.0 && !switching) {
        if let Some(l) = crate::resets_in_label(b.resets_at) {
            let s = format!("· {l}");
            g.draw_text(
                x + 8.0,
                ty,
                &s,
                gpu::DrawOpts {
                    font_size: fs,
                    color: theme::text_dim(),
                    bold: false,
                    italic: false,
                },
            );
            x += 8.0 + g.measure_chrome_text(&s, fs, true);
        }
    }
    x += 10.0;

    // 계정 이름은 가장 먼저 버린다 — 한도 숫자가 이 줄의 존재 이유고,
    // 이름은 드롭다운을 열면 어차피 맨 위에 있다.
    //
    // 라벨을 안 지은 슬롯은 이름이 이메일로 폴백되는데, 그걸 통째로 적으면
    // 한 줄의 절반을 주소가 먹는다. **@ 앞만** 남긴다 — 계정을 가리는 데는
    // 그걸로 충분하고(오늘 넷이 같은 계정인 걸 못 알아본 게 문제였지 주소
    // 뒷부분을 몰라서가 아니다), 전체는 드롭다운에 그대로 있다.
    //
    // 다만 **겹치면 안 줄인다.** 슬롯 둘이 같은 아이디에 다른 도메인이면
    // (`sampleuser@maila` · `sampleuser@mailb`) 화면에서 통째로 같은 글자가
    // 되어, 지금 어느 계정인지 이 자리로는 알 수가 없다(토키 실측
    // 2026-08-15). 겹칠 때만 도메인 앞머리를 붙여 가른다 — 안 겹치면
    // 예전대로 짧게.
    // 슬롯 표시명 전체. 이름을 줄일 때 겹침을 판정하는 근거이자,
    // 아래 나머지 계정 줄이 그대로 쓰는 목록이다.
    let all_names: Vec<String> =
            f.accounts.claude_list.iter().enumerate().map(|(i, a)| {
                crate::settings::account_display(
                    &a.id,
                    &a.label,
                    &format!("계정 {}", i + 1),
                )
            })
            .collect();
    if win_w >= 720.0
        && (status_prefs.has_usage_field("claude", "account")
            || status_prefs.has_usage_field("claude", "email"))
    {
        // 별명과 이메일을 **둘 다** 적는다(2026-09-07 지시). 별명만으로는
        // 「지메일」이 어느 주소인지 확인이 안 되고(계정 넷 중 둘이 같은
        // 아이디를 쓴다), 이메일만 남기면 사람이 붙인 이름이 사라져 어느
        // 슬롯인지 목록과 대조가 안 된다.
        let nick = statusbar_account_short(&acct_name, &all_names);
        let email = status_prefs
            .has_usage_field("claude", "email")
            .then(|| crate::settings::auth_probe(f.accounts.claude))
            .flatten()
            .map(|p| p.email)
            // 별명이 없는 슬롯은 이름 자리에 이미 이메일(또는 그 @ 앞)이
            // 들어가 있다 — 그때 이메일을 또 붙이면 같은 글자가 두 번 선다.
            .filter(|e| {
                !e.is_empty()
                    && !nick.contains(e.as_str())
                    && !e.starts_with(&format!("{nick}@"))
            });
        let short = match (
            status_prefs.has_usage_field("claude", "account"),
            email,
        ) {
            (true, Some(e)) => format!("{nick} {e}"),
            (true, None) => nick,
            (false, Some(e)) => e,
            (false, None) => String::new(),
        };
        let short = crate::info::fit_text(g, &short, (account_right - x).max(0.0), fs, false);
        let short = short.as_str();
        if !short.is_empty() {
            g.draw_text(
                x,
                ty,
                short,
                gpu::DrawOpts {
                    font_size: fs,
                    color: status_prefs.color("claude", theme::text_dim()),
                    bold: false,
                    italic: false,
                },
            );
            x += g.measure_chrome_text(short, fs, true);
        }
    }
    }
    }
        }};
    }

    // 나머지 계정은 **여기 안 세운다.** 눌러서 여는 목록에 이름·이메일·
    // 한도가 다 있고, 이 줄에 늘어놓으면 정작 이 줄의 존재 이유인 활성
    // 계정의 막대가 이름밭에 묻힌다(2026-09-07 「하단바 다른계정 누르면
    // 보이니까 이메일만 나오게하라니까 현재계정하나랑」).


    // 코덱스는 **사용량 조회에 성공했을 때만** 세우면 안 된다. 인증이
    // 풀렸거나 막 계정을 바꾼 순간이 오히려 계정 이름을 봐야 할 때인데,
    // 옛 조건은 그때 세그먼트 전체를 감췄다(2026-09-07 「왜 안 나오지」).
    // 등록했거나 로그인한 사람에게는 현재 계정과 상태를 늘 남긴다.
    macro_rules! draw_codex_status {
        () => {{
    if status_prefs.visible("codex") && !compact_tools {
        let codex_id = f.accounts.codex.as_str();
        let codex_logged_in = crate::settings::codex_logged_in(codex_id);
        let codex_configured =
            !codex_id.is_empty() || !f.accounts.codex_list.is_empty();
        let codex_snapshot = crate::codexlimits::snapshot_for(codex_id);
        let codex_limits = codex_windows_for(codex_id);
        let codex_stale = codex_snapshot.as_ref().is_some_and(|limits| limits.stale);
        if codex_statusbar_visible(win_w, codex_logged_in, codex_configured) {
            account_drawn = true;
            if x > seg_x0 {
                x += 16.0;
            }
            let icon = theme::ICON_SIZE - 3.0;
            g.queue_icon(
                AccountProvider::Codex.icon(),
                x,
                ty + (fs - icon) / 2.0,
                icon,
                status_prefs.color("codex", theme::text_dim()),
            );
            x += icon + 6.0;

            let all_names: Vec<String> = std::iter::once(codex_account_name(
                "",
                f.accounts.codex_list,
            ))
            .chain(
                f.accounts.codex_list
                    .iter()
                    .map(|account| {
                        codex_account_name(&account.id, f.accounts.codex_list)
                    }),
            )
            .collect();
            let full_name = codex_account_label(codex_id, f.accounts.codex_list);
            let short_name = statusbar_account_short(&full_name, &all_names);
            let show_account = status_prefs.has_usage_field("codex", "account");
            let show_email = status_prefs.has_usage_field("codex", "email");
            let email = show_email
                .then(|| crate::settings::codex_identity(codex_id))
                .flatten()
                .filter(|value| !short_name.contains(value));
            let identity = match (show_account, email) {
                (true, Some(email)) => format!("{short_name} {email}"),
                (true, None) => short_name,
                (false, Some(email)) => email,
                (false, None) => String::new(),
            };
            let name_w = g.measure_chrome_text(&identity, fs, false);

            let raw_wins = codex_limits.clone().unwrap_or_default();
            let wins = selected_codex_windows(&raw_wins, &status_prefs);
            let missing = codex_limits
                .as_ref()
                .map(|raw| missing_codex_windows(raw, &status_prefs))
                .unwrap_or_default();

            if !codex_logged_in
                || (status_prefs.wants_usage("codex") && codex_limits.is_none())
            {
                let status = if codex_logged_in { "—" } else { "로그인 필요" };
                g.draw_text(
                    x,
                    ty,
                    status,
                    gpu::DrawOpts {
                        font_size: fs,
                        color: status_prefs.color("codex", theme::text_dim()),
                        bold: false,
                        italic: false,
                    },
                );
                x += g.measure_chrome_text(status, fs, false);
                if !codex_logged_in {
                    x += status_dot(g, x, sy, status_h, ChipState::Bad);
                }
                x += 8.0;
            } else if status_prefs.wants_usage("codex") {
                for label in missing {
                    let text = format!("{label} 미제공");
                    g.draw_text(
                        x,
                        ty,
                        &text,
                        gpu::DrawOpts {
                            font_size: fs,
                            color: theme::text_mute(),
                            bold: false,
                            italic: false,
                        },
                    );
                    x += g.measure_chrome_text(&text, fs, false) + 10.0;
                }
                // 오른쪽 상태 칩과 계정 이름 자리를 먼저 남긴다. 폭이
                // 모자라면 draw_window_gauges가 긴 창부터 자연스럽게 접는다.
                let gauge_x = x;
                let right = (win_w - 280.0 - name_w).max(gauge_x);
                x = draw_window_gauges(
                    g,
                    gauge_x,
                    ty,
                    right,
                    fs,
                    &wins,
                    codex_stale,
                );
                if x == gauge_x && !wins.is_empty() {
                    let pct = wins[0].1;
                    let text = format!("{}{pct:.0}%", if codex_stale { "~" } else { "" });
                    g.draw_text(
                        x,
                        ty,
                        &text,
                        gpu::DrawOpts {
                            font_size: fs,
                            color: usage_pct_color(pct),
                            bold: true,
                            italic: false,
                        },
                    );
                    x += g.measure_chrome_text(&text, fs, true) + 8.0;
                }
            }

            if !identity.is_empty() {
                g.draw_text(
                    x,
                    ty,
                    &identity,
                    gpu::DrawOpts {
                        font_size: fs,
                        color: status_prefs.color("codex", theme::text_dim()),
                        bold: false,
                        italic: false,
                    },
                );
                x += name_w;
            }
        }
    }
        }};
    }

    // 두 제공자의 렌더러는 같지 않지만, 호출 순서는 개인 설정을 따른다.
    // 매크로로 감싼 것은 `g`와 `self`의 빌림을 두 클로저가 동시에 붙들지
    // 않게 하면서 기존 그리기를 그대로 보존하기 위해서다.
    for id in status_prefs
        .order
        .iter()
        .filter(|id| matches!(id.as_str(), "claude" | "codex"))
    {
        match id.as_str() {
            "claude" => draw_claude_status!(),
            "codex" => draw_codex_status!(),
            _ => {}
        }
    }

    if compact_tools && (status_prefs.visible("claude") || status_prefs.visible("codex")) {
        g.queue_icon("users", seg_x0, sy + (status_h - 12.0) / 2.0, 12.0, theme::text_dim());
        account_drawn = true;
        x = seg_x0 + 12.0;
    }
    // 세그먼트 전체가 손잡이다 — 게이지든 숫자든 이름이든 판 번호든
    // 누르면 열린다. 자세한 것은 전부 그 안에 있다.
    x = x.min(account_right - 6.0);
    let acct_r = (seg_x0 - 6.0, sy, (x - seg_x0 + 12.0).max(0.0), status_h);
    // 세그먼트가 곧 계정 스위처 손잡이다 — 손모양이 없으면 눌러 볼
    // 생각조차 안 든다(사용자 2026-08-12). 채움은 주지 않는다: 세그먼트
    // 폭은 텍스트를 다 그린 뒤에야 확정되고, 이 렌더는 나중에 그린 것이
    // 위로 오므로 여기서 사각형을 깔면 방금 쓴 글자를 덮는다.
    {
        let (hx, hy) = f.cursor;
        g.hover_pointer |= hx >= acct_r.0
            && hx <= acct_r.0 + acct_r.2
            && hy >= acct_r.1
            && hy <= acct_r.1 + acct_r.3;
    }
    *account_rect = account_drawn.then_some(acct_r);
    // 계정이 바뀐 직후 잠깐 반짝인다 — 우상단 토스트만으로는 정작 이
    // 칩이 그대로라 「바뀐 줄 모르겠다」가 된다(사용자 2026-08-25).
    // 칩을 그리는 이 자리에서 함께 그려야 층이 안 어긋난다.
    if account_drawn {
        if let Some(k) = crate::chrome::account_flash_k(f.account_flash) {
            paint_account_flash(g, acct_r, k);
        }
    }
    if account_drawn
        && ["ports", "pet", "clipboard", "resources", "tunnel", "version"]
            .iter()
            .any(|id| status_prefs.visible(id))
    {
        if status_prefs.separators {
            g.rect(
                x + 9.0,
                sy + 6.0,
                1.0,
                (status_h - 12.0).max(6.0),
                theme::with_alpha(theme::border(), 140),
            );
        }
    }

    g.pop_clip();
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
        if status_prefs.visible("link") && !links.is_empty() {
            // 한 칸에 요약만 — 이름은 팝오버가 말한다.
            let text = if compact_tools { String::new() } else { crate::machinescol::status_links_summary(&links) };
            let worst = links.iter().map(|l| l.tone()).min().unwrap_or(2);
            let open = matches!(bar.popover, Some((state::StatusbarPopover::Link, _)));
            let col = chip_ink(&status_prefs, "link", open);
            let dot = if compact_tools { 0.0 } else { STATUS_DOT_GAP + STATUS_DOT };
            let text = crate::info::fit_text(g, &text, (slot_w - 24.0 - dot).max(0.0), fs, false);
            let w = g.measure_chrome_text(&text, fs, false);
            rx -= w + icon + gap + dot + chip;
            g.queue_icon("monitor", rx, sy + (status_h - icon) / 2.0, icon, col);
            g.draw_text(rx + icon + gap, ty, &text,
                gpu::DrawOpts { font_size: fs, color: col, bold: false, italic: false });
            if !compact_tools {
                status_dot(g, rx + icon + gap + w, sy, status_h, link_state(worst));
            }
            let r = (rx - chip / 2.0, sy, w + icon + gap + dot + chip, status_h);
            let (hx, hy) = f.cursor;
            g.hover_pointer |= hx >= r.0 && hx <= r.0 + r.2 && hy >= r.1 && hy <= r.1 + r.3;
            bar.link_rect = Some(r);
        }
        }};
    }
    macro_rules! draw_version_widget {
        () => {{
        // 판 번호 — 이 줄의 **맨 오른쪽**(2026-09-06 지시: 「하단바
        // 버전표시를 맨 오른쪽으로 하자」). 09-05 에 계정 세그먼트
        // 꼬리에서 이 그룹으로 옮겼는데, 그때는 그룹 맨 왼쪽이라 왼쪽
        // 이웃(포트·클립보드)이 늘고 줄 때마다 판 번호도 따라 움직였다.
        // 그룹은 끝에서부터 자라므로 **가장 먼저 그리는 것**이 가장
        // 오른쪽이고, 그 자리만이 이웃과 무관하게 고정이다.
        //
        // 새로 구운 것이 설치를 기다리면 화살표가 붙는다 — 「껐다 켜면
        // 반영됩니다」를 사람이 말로 전하던 자리다. 판정은 종료 때 실제로
        // 설치를 움직이는 것과 **같은 함수**를 쓴다(갈리면 표시는 떴는데
        // 안 바뀌거나 그 반대가 된다).
        if status_prefs.visible("version") && compact_tools {
            rx -= tool_icon + chip;
            g.queue_icon("info", rx, sy + (status_h - tool_icon) / 2.0, tool_icon, theme::text_dim());
            *version_rect = Some((rx - chip / 2.0, sy, tool_icon + chip, status_h));
        } else if status_prefs.visible("version") && win_w >= 720.0 {
            let mismatched = crate::statusbar_config::mismatched_machines();
            let waiting = crate::install_pending()
                || matches!(crate::version::state(), crate::version::Check::Newer(_));
            // 손수 구운 판은 번호 뒤에 `+` 하나. 릴리스와 번호가 같아서
            // 그냥 두면 둘을 구별할 자리가 화면 어디에도 없다.
            let mark = if crate::version::is_local_build() { "+" } else { "" };
            let mut s_ver = if waiting {
                format!("v{}{mark} ↑", crate::version::CURRENT)
            } else {
                format!("v{}{mark}", crate::version::CURRENT)
            };
            if let Some(machine) = mismatched.first() {
                if mismatched.len() == 1 {
                    s_ver.push_str(&format!(" · {machine} 빌드 다름"));
                } else {
                    s_ver.push_str(&format!(" · 기기 {}대 빌드 다름", mismatched.len()));
                }
            }
            let state = if !mismatched.is_empty() {
                Some(ChipState::Bad)
            } else if waiting {
                Some(ChipState::Todo)
            } else {
                None
            };
            let dot = if state.is_some() { STATUS_DOT_GAP + STATUS_DOT } else { 0.0 };
            let s_ver = crate::info::fit_text(g, &s_ver, (slot_w - 21.0 - dot).max(0.0), fs, false);
            let w = g.measure_chrome_text(&s_ver, fs, true);
            rx -= w + dot + chip;
            let open = matches!(bar.popover, Some((state::StatusbarPopover::Build, _)));
            g.draw_text(
                rx,
                ty,
                &s_ver,
                gpu::DrawOpts {
                    font_size: fs,
                    color: chip_ink(&status_prefs, "version", open),
                    bold: false,
                    italic: false,
                },
            );
            if let Some(state) = state {
                status_dot(g, rx + w, sy, status_h, state);
            }
            // 눌러서 여는 곳은 그대로 계정 드롭다운이다 — 몇 커밋 앞인지는
            // 거기 있고, 자리를 옮겼다고 그 동선까지 잃으면 판 번호는
            // 읽을 수만 있고 캐물을 수 없는 글자가 된다.
            let vr = (rx - chip / 2.0, sy, w + dot + chip, status_h);
            {
                let (hx, hy) = f.cursor;
                g.hover_pointer |=
                    hx >= vr.0 && hx <= vr.0 + vr.2 && hy >= vr.1 && hy <= vr.1 + vr.3;
            }
            *version_rect = Some(vr);
        } else {
            *version_rect = None;
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
            .filter(|id| matches!(id.as_str(), "resources" | "link" | "tunnel" | "version"))
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
                "link" => draw_link_widget!(),
                "tunnel" => draw_tunnel_widget!(),
                "version" => draw_version_widget!(),
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
