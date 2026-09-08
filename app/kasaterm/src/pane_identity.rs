use super::*;

pub(super) struct PaneIdentity {
    pub shown: String,
    pub title: String,
    pub machine: MachineIdentity,
}

pub(super) struct MachineIdentity {
    pub label: String,
    pub detail: String,
    pub remote: bool,
    tint: [u8; 4],
}

impl MachineIdentity {
    pub fn for_pane(pane_id: Option<&str>, local_name: Option<&str>) -> Self {
        let remote = pane_id.and_then(kasa_mcp::remote::remote_info);
        let (label, detail, is_remote) = match remote {
            Some(info) => {
                let label = if info.label.trim().is_empty() {
                    info.base
                        .trim_start_matches("https://")
                        .trim_start_matches("http://")
                        .trim_end_matches('/')
                        .to_string()
                } else {
                    info.label
                };
                let kind = if info.view { "미러" } else { "원격" };
                (label, format!("{kind} · {}", info.remote_id), true)
            }
            None => (
                local_name.unwrap_or("이 기기").to_string(),
                "로컬".to_string(),
                false,
            ),
        };
        let tint = machine_tint(&label);
        Self {
            label,
            detail,
            remote: is_remote,
            tint,
        }
    }

    pub fn icon(&self) -> &'static str {
        let name = self.label.to_lowercase();
        if name.contains("macbook") || name.contains("맥북") || name.contains("laptop") {
            "laptop"
        } else if name.contains("mini") || name.contains("미니") || name.contains("server") {
            "server"
        } else {
            "monitor"
        }
    }

    pub fn background(&self, base: [u8; 4]) -> [u8; 4] {
        theme::lerp(base, self.tint, 0.12)
    }

    pub fn foreground(&self, background: [u8; 4]) -> [u8; 4] {
        theme::enforce_contrast_at(self.tint, background, 4.5)
    }
}

fn machine_tint(label: &str) -> [u8; 4] {
    // 작업 상태의 빨강·주황·초록을 빌리지 않는다. 같은 이름은 앱을 다시
    // 열거나 로컬/원격 위치가 바뀌어도 같은 차분한 기기색을 갖는다.
    const COLORS: [[u8; 4]; 5] = [
        [111, 143, 170, 255],
        [148, 130, 175, 255],
        [104, 153, 167, 255],
        [132, 144, 185, 255],
        [151, 138, 161, 255],
    ];
    let hash = label
        .trim()
        .to_lowercase()
        .bytes()
        .fold(2166136261_u32, |hash, byte| {
            (hash ^ byte as u32).wrapping_mul(16777619)
        });
    COLORS[hash as usize % COLORS.len()]
}

pub(super) fn draw_card(
    g: &mut gpu::GpuRenderer,
    identity: &PaneIdentity,
    rect: (f32, f32, f32, f32),
) {
    let (rx, ry, rw, rh) = rect;
    if rw < 40.0 || rh < 32.0 {
        return;
    }
    let compact = rh < 148.0 || rw < 200.0;
    let padding = if rh < 64.0 || rw < 96.0 {
        6.0
    } else if compact {
        12.0
    } else {
        18.0
    };
    let width = (rw - 16.0).min(if compact { 248.0 } else { 304.0 });
    let content_w = (width - padding * 2.0).max(0.0);
    let number_font = (rh - padding * 2.0 - 8.0).min(if compact { 24.0 } else { 32.0 });
    let number_width = g.measure_chrome_text(&identity.shown, number_font, true);
    let number_font = (number_font * (content_w / number_width.max(1.0)).min(1.0)).max(9.0);
    let title_font = 13.0;
    let machine_font = 12.0;
    let show_machine = rh >= 90.0 && content_w >= 72.0;
    let show_title = rh >= 142.0 && !identity.title.trim().is_empty();
    let show_detail = rh >= 186.0 && content_w >= 120.0;
    let height = (padding * 2.0
        + number_font
        + if show_title { 24.0 } else { 0.0 }
        + if show_machine { 36.0 } else { 0.0 }
        + if show_detail { 20.0 } else { 0.0 })
    .min(rh - 8.0);
    let x = rx + (rw - width) / 2.0;
    let y = ry + (rh - height) / 2.0;
    let surface = theme::panel_bg();
    let fg = theme::enforce_contrast_at(theme::text(), surface, 4.5);
    let dim = theme::enforce_contrast_at(theme::text_dim(), surface, 4.5);
    g.push_clip(rx, ry, rw, rh);
    round_rect(g, x, y, width, height, 12.0, theme::border());
    round_rect(
        g,
        x + 1.0,
        y + 1.0,
        width - 2.0,
        height - 2.0,
        11.0,
        surface,
    );
    let number = crate::info::fit_text(g, &identity.shown, content_w, number_font, true);
    let number_w = g.measure_chrome_text(&number, number_font, true);
    g.draw_text(
        x + (width - number_w) / 2.0,
        y + padding,
        &number,
        gpu::DrawOpts {
            font_size: number_font,
            color: fg,
            bold: true,
            italic: false,
        },
    );
    let mut next_y = y + padding + number_font;
    if show_title {
        let title = crate::info::fit_text(g, &identity.title, content_w, title_font, false);
        let title_w = g.measure_chrome_text(&title, title_font, false);
        g.draw_text(
            x + (width - title_w) / 2.0,
            next_y + 6.0,
            &title,
            gpu::DrawOpts {
                font_size: title_font,
                color: fg,
                bold: false,
                italic: false,
            },
        );
        next_y += 24.0;
    }
    if show_machine {
        let machine = &identity.machine;
        let label = crate::info::fit_text(g, &machine.label, content_w - 36.0, machine_font, false);
        let label_w = g.measure_chrome_text(&label, machine_font, false);
        let chip_w = label_w + 36.0;
        let chip_x = x + (width - chip_w) / 2.0;
        let chip_y = next_y + 12.0;
        let chip_bg = machine.background(surface);
        let chip_fg = machine.foreground(chip_bg);
        round_rect(g, chip_x, chip_y, chip_w, 24.0, 6.0, chip_bg);
        g.queue_icon(machine.icon(), chip_x + 8.0, chip_y + 5.0, 14.0, chip_fg);
        g.draw_text(
            chip_x + 28.0,
            chip_y + 6.0,
            &label,
            gpu::DrawOpts {
                font_size: machine_font,
                color: chip_fg,
                bold: false,
                italic: false,
            },
        );
        next_y += 36.0;
    }
    if show_detail {
        let detail = crate::info::fit_text(g, &identity.machine.detail, content_w, 11.0, false);
        let detail_w = g.measure_chrome_text(&detail, 11.0, false);
        g.draw_text(
            x + (width - detail_w) / 2.0,
            next_y + 8.0,
            &detail,
            gpu::DrawOpts {
                font_size: 11.0,
                color: dim,
                bold: false,
                italic: false,
            },
        );
    }
    g.pop_clip();
}
