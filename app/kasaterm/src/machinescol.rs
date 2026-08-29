//! 이사 칼럼 — 우측 패널 `SideTab::Machines` 본문.
//!
//! 처음엔 Persona 식 자식 웹뷰(`/arona-ui/machines.html`)였는데 같은 날 네이티브로
//! 뒤집었다(2026-08-29 지적 「타우리 말고 wgpu로 해야되지않나」). 우측 패널의 다른
//! 탭이 전부 셀 렌더인데 이 탭만 OS 뷰라 테마·z순서·캡처가 다 갈렸고, 무엇보다
//! 데이터(기계 캐시·pane 상태)가 **같은 프로세스**에 있어 HTTP 왕복이 낭비였다.
//!
//! 데이터 조립(`refresh_machines_col`)은 페인트 루프 밖(handler 프레임 끝)에서 한다
//! — 페인트는 gpu 를 빌린 상태라 `&self` 메서드를 못 부른다(사이드바 스냅샷과 같은
//! 이유). 그리기(`draw_machines_col`)는 sesscol/mcpcol 과 같은 자유함수 관례다.

use super::*;

/// ago 초를 사람 말로 — 아로나 판(MachinesTab)의 agoLabel 과 같은 문구.
fn ago_label(secs: Option<u64>) -> String {
    match secs {
        None => "한 번도 못 닿았어요".to_string(),
        Some(s) if s < 60 => format!("{s}초 전까지 닿았어요"),
        Some(s) if s < 3600 => format!("{}분 전까지 닿았어요", s / 60),
        Some(s) => format!("{}시간 전까지 닿았어요", s / 3600),
    }
}

/// 상태점 색 — 앱 상태 언어 그대로: 기다림=attention, 도는 중=accent, 그 외=success.
fn status_color(status: &str) -> [u8; 4] {
    match status {
        "waiting" | "blocked" => theme::attention(),
        "working" | "thinking" | "building" => theme::accent(),
        _ => theme::success(),
    }
}

impl App {
    /// 이사 칼럼 데이터를 다시 조립한다. 탭이 보일 때만, 1초 스로틀 —
    /// 기계 쪽은 폴링 캐시(`machines::snapshot`)라 읽기 자체는 공짜다.
    pub(crate) fn refresh_machines_col(&mut self) {
        if !self.machines_tab_active() {
            return;
        }
        if self
            .info
            .machines_col
            .last_refresh
            .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(1))
        {
            return;
        }
        self.info.machines_col.last_refresh = Some(std::time::Instant::now());

        // 로컬 pane 들 — claude 가 붙어 학생이 앉은 자리만(사이드바와 같은 기준).
        let mut pane_ids: Vec<String> = self.pty.keys().cloned().collect();
        pane_ids.sort_by_key(|s| s.trim_start_matches('%').parse::<u64>().unwrap_or(u64::MAX));
        let mut locals: Vec<state::MachinesColRow> = Vec::new();
        // 라벨 → (원격 surface id 집합, 이사 간 학생 행들). 원격 목록에서 미러와
        // 같은 pane 을 두 번 세우지 않기 위한 대조표다.
        let mut mirrored: std::collections::HashMap<String, (std::collections::HashSet<String>, Vec<state::MachinesColRow>)> =
            std::collections::HashMap::new();
        for id in &pane_ids {
            if !self.pane_claude_ready(id) {
                continue;
            }
            let Some(name) = self.pane_character_if_known(id) else { continue };
            let row = state::MachinesColRow {
                pane: id.clone(),
                name,
                title: self.pane_row_label(id),
                status: self
                    .pane_activity
                    .get(id)
                    .map(|v| v.status.clone())
                    .unwrap_or_default(),
            };
            match kasa_mcp::remote::remote_info(id) {
                Some(info) => {
                    let label = if info.label.is_empty() { info.base.clone() } else { info.label.clone() };
                    let slot = mirrored.entry(label).or_default();
                    slot.0.insert(info.remote_id.clone());
                    slot.1.push(row);
                }
                None => locals.push(row),
            }
        }

        // 기계 섹션 — 폴링 캐시 스냅샷을 그대로 편다.
        let machines = kasa_mcp::machines::snapshot()
            .into_iter()
            .filter_map(|m| {
                let label = m.get("label")?.as_str()?.to_string();
                let (mirror_ids, mirror_rows) = mirrored.remove(&label).unwrap_or_default();
                let remote = m
                    .get("panes")
                    .and_then(|p| p.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|p| {
                                let rid = p.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                if mirror_ids.contains(rid) {
                                    return None; // 이사 간 학생의 원격 반쪽 — 미러 행이 대표한다.
                                }
                                let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
                                Some(state::MachinesColRow {
                                    pane: String::new(), // 로컬 자리가 없다 — 데려오기 대상이 못 된다.
                                    name: if name.is_empty() { "이름 없는 학생".to_string() } else { name.to_string() },
                                    title: p.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                    status: p.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Some(state::MachinesColMachine {
                    label,
                    online: m.get("online").and_then(|v| v.as_bool()).unwrap_or(false),
                    ago_secs: m.get("ago_secs").and_then(|v| v.as_u64()),
                    mirrored: mirror_rows,
                    remote,
                })
            })
            .collect();
        self.info.machines_col.locals = locals;
        self.info.machines_col.machines = machines;
    }

    /// 이사 칼럼 클릭 — 버튼에 맞으면 이사를 실행하고 true.
    ///
    /// 이사는 이 자리(GUI 스레드)에서 동기로 돈다 — CLI·아로나 경로도 결국 GUI
    /// 이벤트로 위임돼 같은 스레드에서 돌므로, 어는 정도는 기존과 같다. 대신
    /// 시작 전에 「이사 중」 상태를 한 프레임 그려 둔다.
    pub(crate) fn machines_col_click(&mut self, cx: f32, cy: f32) -> bool {
        let hit = self
            .info
            .machines_col
            .btn_rects
            .iter()
            .find(|(_, (x, y, w, h))| cx >= *x && cx <= *x + *w && cy >= *y && cy <= *y + *h)
            .map(|(b, _)| b.clone());
        let Some(btn) = hit else { return false };
        if self.info.machines_col.busy.is_some() {
            return true; // 한 번에 하나 — 이사 중 클릭은 삼킨다.
        }
        let (pane, going) = match &btn {
            state::MachinesColBtn::Send { pane, label } => (pane.clone(), format!("{label}(으)로 보내는 중…")),
            state::MachinesColBtn::Bring { pane } => (pane.clone(), "데려오는 중…".to_string()),
        };
        self.info.machines_col.busy = Some((pane.clone(), going.clone()));
        self.info.machines_col.note = None;
        self.set_toast(format!("이사 — {going}"));
        self.chrome_dirty = true;
        self.render_frame();
        let outcome = match btn {
            state::MachinesColBtn::Send { pane, label } => (|| -> anyhow::Result<String> {
                let m = kasa_mcp::machines::find(&label)
                    .ok_or_else(|| anyhow::anyhow!("기계 {label} 를 명부에서 못 찾았다 — machines.json 확인"))?;
                // cwd 는 명부 roots 로 매핑 — 규칙이 없으면 막고 이유를 말한다
                // (HTTP 판 pane_migrate_handler 와 같은 정책).
                let local = self
                    .pty
                    .get(&pane)
                    .and_then(|s| s.shell_pid())
                    .and_then(socket::pid_cwd);
                let remote_cwd = match &local {
                    Some(l) => Some(
                        kasa_mcp::machines::map_local_to_remote(&m, &l.to_string_lossy())
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "{} 를 {label} 경로로 못 옮겼다 — machines.json roots 에 규칙을",
                                    l.display()
                                )
                            })?,
                    ),
                    None => None,
                };
                self.migrate_pane(&pane, &m.base, remote_cwd.as_deref(), false)
            })(),
            state::MachinesColBtn::Bring { pane } => self.migrate_pane_back(&pane, None, false),
        };
        self.info.machines_col.busy = None;
        match outcome {
            Ok(msg) => {
                self.info.machines_col.note = Some((pane, true, msg.clone()));
                self.set_toast(msg);
            }
            Err(e) => {
                let why = format!("{e:#}");
                self.info.machines_col.note = Some((pane, false, why.clone()));
                self.set_toast(format!("이사 실패 — {why}"));
            }
        }
        self.info.machines_col.last_refresh = None; // 다음 틱에 바로 새 배치를 읽게.
        true
    }
}

/// `max` 폭에 들어가게 뒤를 …로 자른다. 렌더러에 폭 제한 그리기가 없어서
/// 문자열 쪽에서 자르는 것 — 행 텍스트가 버튼 밑으로 흐르지 않게.
fn truncate_to(g: &mut gpu::GpuRenderer, s: &str, size: f32, bold: bool, max: f32) -> String {
    if g.measure_chrome_text(s, size, bold) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    for ch in s.chars() {
        out.push(ch);
        if g.measure_chrome_text(&format!("{out}…"), size, bold) > max {
            out.pop();
            out.push('…');
            return out;
        }
    }
    out
}

/// 섹션 라벨 한 줄("이 맥북" / 기계 이름). y 를 소비한 높이만큼 돌려준다.
fn section_label(g: &mut gpu::GpuRenderer, x: f32, y: f32, text: &str) -> f32 {
    g.draw_text(
        x,
        y,
        text,
        gpu::DrawOpts { font_size: 10.0, color: theme::text_mute(), bold: true, italic: false },
    );
    y + 20.0
}

/// 학생 한 행. 버튼 rect 는 `mc.btn_rects` 에 쌓는다.
#[allow(clippy::too_many_arguments)]
fn student_row(
    g: &mut gpu::GpuRenderer,
    cursor: (f32, f32),
    mc: &mut state::MachinesColState,
    row: &state::MachinesColRow,
    buttons: &[(state::MachinesColBtn, String, bool)], // (동작, 라벨, 눌림 가능)
    x: f32,
    right: f32,
    y: f32,
) -> f32 {
    let busy_here = mc.busy.as_ref().is_some_and(|(p, _)| !row.pane.is_empty() && *p == row.pane);
    // 상태점.
    let dot = 7.0;
    round_rect(g, x, y + 5.0, dot, dot, dot / 2.0, status_color(&row.status));
    // 오른쪽 끝에서 버튼을 먼저 앉히고 남는 폭이 글 자리다.
    let mut bx = right;
    if busy_here {
        let t = mc.busy.as_ref().map(|(_, g)| g.clone()).unwrap_or_default();
        let tw = g.measure_chrome_text(&t, 10.0, true);
        bx -= tw;
        g.draw_text(
            bx,
            y + 2.0,
            &t,
            gpu::DrawOpts { font_size: 10.0, color: theme::accent(), bold: true, italic: false },
        );
    } else {
        for (btn, label, enabled) in buttons.iter().rev() {
            let bw = g.measure_chrome_text(label, 10.0, true) + 12.0;
            let bh = 17.0;
            bx -= bw;
            let hov = *enabled
                && cursor.0 >= bx
                && cursor.0 <= bx + bw
                && cursor.1 >= y
                && cursor.1 <= y + bh;
            g.hover_pointer |= hov;
            let fill = theme::raised_on(theme::surface(), hov);
            round_rect(g, bx, y, bw, bh, theme::radius_sm(), fill);
            g.draw_text(
                bx + 6.0,
                y + 2.0,
                label,
                gpu::DrawOpts {
                    font_size: 10.0,
                    color: if *enabled { theme::text() } else { theme::text_mute() },
                    bold: true,
                    italic: false,
                },
            );
            if *enabled {
                mc.btn_rects.push((btn.clone(), (bx, y, bw, bh)));
            }
            bx -= 4.0;
        }
    }
    let text_x = x + dot + 7.0;
    let text_max = (bx - 8.0 - text_x).max(0.0);
    let name = truncate_to(g, &row.name, 12.0, true, text_max);
    g.draw_text(
        text_x,
        y,
        &name,
        gpu::DrawOpts { font_size: 12.0, color: theme::text(), bold: true, italic: false },
    );
    let mut yy = y + 15.0;
    if !row.title.is_empty() {
        // 제목 줄은 버튼과 세로로 살짝 겹치는 자리라 같은 폭으로 자른다.
        let title = truncate_to(g, &row.title, 10.5, false, text_max);
        g.draw_text(
            text_x,
            yy,
            &title,
            gpu::DrawOpts { font_size: 10.5, color: theme::text_mute(), bold: false, italic: false },
        );
    }
    yy += 15.0;
    // 이 행의 최근 이사 결과 한 줄.
    if let Some((p, ok, note)) = &mc.note {
        if !row.pane.is_empty() && p == &row.pane {
            g.draw_text(
                text_x,
                yy,
                note,
                gpu::DrawOpts {
                    font_size: 10.0,
                    color: if *ok { theme::success() } else { theme::danger() },
                    bold: false,
                    italic: false,
                },
            );
            yy += 14.0;
        }
    }
    yy + 6.0
}

/// 이사 칼럼 본문. sesscol/mcpcol 과 같은 호출 관례(자유함수, 상태만 받아 그림).
pub(crate) fn draw_machines_col(
    g: &mut gpu::GpuRenderer,
    cursor: (f32, f32),
    mc: &mut state::MachinesColState,
    x: f32,
    w: f32,
    top: f32,
    bottom: f32,
) {
    let x0 = x + 14.0;
    let right = x + w - 12.0;
    mc.btn_rects.clear();
    let vis_h = (bottom - top).max(0.0);
    g.push_clip(x, top, w, vis_h);
    let mut y = top + 8.0 - mc.scroll;

    if mc.machines.is_empty() {
        g.pop_clip();
        let msg = ["등록된 기계가 없어요 —", "~/.config/kasaterm/machines.json 에 적으면 여기 떠요"];
        let mut my = top + vis_h * 0.4;
        for line in msg {
            let tw = g.measure_chrome_text(line, 11.0, false);
            g.draw_text(
                x + (w - tw) / 2.0,
                my,
                line,
                gpu::DrawOpts { font_size: 11.0, color: theme::text_mute(), bold: false, italic: false },
            );
            my += 17.0;
        }
        return;
    }

    // ── 이 맥북 ───────────────────────────────────────────────────────────
    y = section_label(g, x0, y, "이 맥북");
    if mc.locals.is_empty() {
        g.draw_text(
            x0,
            y,
            "학생 없음",
            gpu::DrawOpts { font_size: 11.0, color: theme::text_mute(), bold: false, italic: false },
        );
        y += 22.0;
    } else {
        let machines_meta: Vec<(String, bool)> =
            mc.machines.iter().map(|m| (m.label.clone(), m.online)).collect();
        let locals = mc.locals.clone();
        for row in &locals {
            let buttons: Vec<(state::MachinesColBtn, String, bool)> = machines_meta
                .iter()
                .map(|(label, online)| {
                    (
                        state::MachinesColBtn::Send { pane: row.pane.clone(), label: label.clone() },
                        format!("→ {label}"),
                        *online,
                    )
                })
                .collect();
            y = student_row(g, cursor, mc, row, &buttons, x0, right, y);
        }
    }

    // ── 기계별 섹션 ───────────────────────────────────────────────────────
    let machines = mc.machines.clone();
    for m in &machines {
        y += 10.0;
        let label_w = g.measure_chrome_text(&m.label, 10.0, true);
        y = section_label(g, x0, y, &m.label);
        // 연결 칩 — 라벨 오른쪽에.
        let chip_y = y - 20.0;
        let chip_text = if m.online { "연결됨" } else { "연결 안 닿아요" };
        let cw = g.measure_chrome_text(chip_text, 9.0, true) + 10.0;
        let chip_x = x0 + label_w + 8.0;
        round_rect(
            g,
            chip_x,
            chip_y - 2.0,
            cw,
            14.0,
            7.0,
            if m.online { theme::success() } else { theme::surface() },
        );
        g.draw_text(
            chip_x + 5.0,
            chip_y,
            chip_text,
            gpu::DrawOpts {
                font_size: 9.0,
                color: if m.online { [255, 255, 255, 255] } else { theme::text_mute() },
                bold: true,
                italic: false,
            },
        );
        if !m.online {
            g.draw_text(
                chip_x + cw + 8.0,
                chip_y,
                &ago_label(m.ago_secs),
                gpu::DrawOpts { font_size: 9.0, color: theme::text_mute(), bold: false, italic: false },
            );
        }
        if m.mirrored.is_empty() && m.remote.is_empty() {
            g.draw_text(
                x0,
                y,
                "학생 없음",
                gpu::DrawOpts { font_size: 11.0, color: theme::text_mute(), bold: false, italic: false },
            );
            y += 22.0;
            continue;
        }
        for row in &m.mirrored {
            let buttons = vec![(
                state::MachinesColBtn::Bring { pane: row.pane.clone() },
                "← 데려오기".to_string(),
                true,
            )];
            y = student_row(g, cursor, mc, row, &buttons, x0, right, y);
        }
        for row in &m.remote {
            y = student_row(g, cursor, mc, row, &[], x0, right, y);
        }
    }

    // 스크롤 상한 — 다른 칼럼과 같은 규칙(상한은 렌더가 잡는다).
    let content_h = (y + mc.scroll - top).max(0.0);
    let max_scroll = (content_h - vis_h).max(0.0);
    if mc.scroll > max_scroll {
        mc.scroll = max_scroll;
    }
    g.pop_clip();
}
