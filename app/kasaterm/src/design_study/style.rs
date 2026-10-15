use super::{Kind, Rect};
use crate::{gpu, theme};

pub(super) type Color = [u8; 4];
pub(super) const fn rgb(v: u32) -> Color {
    [(v >> 16) as u8, (v >> 8) as u8, v as u8, 255]
}

#[derive(Clone, Copy)]
pub(super) struct Skin {
    pub kind: Kind,
    pub bg: Color,
    pub panel: Color,
    pub field: Color,
    pub ink: Color,
    pub dim: Color,
    pub edge: Color,
    pub accent: Color,
    pub radius: f32,
    pub button: f32,
    pub row: f32,
    pub gap: f32,
    pub text: f32,
}

impl Skin {
    pub fn new(kind: Kind, dark: bool, accent: Color) -> Self {
        let (bg, panel, field, ink, dim, edge) = match (kind, dark) {
            (Kind::Cloud, false) => (0xeaf0f8, 0xffffff, 0xf1f5fb, 0x24344d, 0x667790, 0xd9e2ef),
            (Kind::Cloud, true) => (0x182131, 0x222e42, 0x1b2638, 0xeef4ff, 0xa5b8d2, 0x384b64),
            (Kind::Precision, false) => {
                (0xe9ecef, 0xfafcfd, 0xf0f3f5, 0x202a34, 0x596875, 0xcbd3da)
            }
            (Kind::Precision, true) => (0x161a20, 0x20262e, 0x181e25, 0xe8eef5, 0x9baab8, 0x38434d),
            (Kind::Playroom, false) => (0xf6eff8, 0xfffcff, 0xf0e6f5, 0x493850, 0x806989, 0xe2d3e8),
            (Kind::Playroom, true) => (0x281f30, 0x382a42, 0x44334e, 0xf9edf9, 0xc7add0, 0x644f70),
        };
        let (radius, button, row, gap, text) = match kind {
            Kind::Cloud => (20.0, 36.0, 58.0, 24.0, 13.0),
            Kind::Precision => (6.0, 28.0, 42.0, 14.0, 12.0),
            Kind::Playroom => (26.0, 40.0, 64.0, 24.0, 14.0),
        };
        Self {
            kind,
            bg: rgb(bg),
            panel: rgb(panel),
            field: rgb(field),
            ink: rgb(ink),
            dim: rgb(dim),
            edge: rgb(edge),
            accent,
            radius,
            button,
            row,
            gap,
            text,
        }
    }
    pub fn box_radius(self) -> f32 {
        match self.kind {
            Kind::Cloud => 12.0,
            Kind::Precision => 4.0,
            Kind::Playroom => 18.0,
        }
    }
    pub fn panel(self, g: &mut gpu::GpuRenderer, r: Rect, raised: bool) {
        if raised && self.kind != Kind::Precision {
            let shadow = if self.kind == Kind::Playroom {
                theme::with_alpha(self.accent, 24)
            } else {
                [35, 63, 100, 18]
            };
            g.round_rect_fill(r.0, r.1 + 5.0, r.2, r.3, self.radius, shadow);
            g.round_rect_fill(
                r.0 - 1.0,
                r.1 + 2.0,
                r.2 + 2.0,
                r.3,
                self.radius,
                theme::with_alpha(shadow, 9),
            );
        }
        g.round_rect_fill(r.0, r.1, r.2, r.3, self.radius, self.panel);
        g.round_rect_stroke(r.0, r.1, r.2, r.3, self.radius, 1.0, self.edge);
    }
    pub fn button(
        self,
        g: &mut gpu::GpuRenderer,
        r: Rect,
        label: &str,
        primary: bool,
        active: bool,
        hover: bool,
    ) {
        let base = if primary {
            self.accent
        } else if active {
            theme::lerp(self.field, self.accent, 0.16)
        } else {
            self.field
        };
        let fill = if hover {
            theme::lerp(base, self.ink, 0.06)
        } else {
            base
        };
        let ink = if primary {
            theme::foreground_on(fill)
        } else if active {
            theme::enforce_contrast_at(self.accent, fill, 4.5)
        } else {
            self.ink
        };
        let radius = self.box_radius();
        if self.kind != Kind::Precision {
            g.round_rect_fill(
                r.0,
                r.1 + 2.0,
                r.2,
                r.3,
                radius,
                theme::with_alpha(if primary { self.accent } else { self.edge }, 70),
            );
        }
        g.round_rect_fill(r.0, r.1, r.2, r.3, radius, fill);
        {
            g.round_rect_stroke(
                r.0,
                r.1,
                r.2,
                r.3,
                radius,
                1.0,
                if primary {
                    theme::with_alpha(theme::foreground_on(fill), 45)
                } else if active {
                    theme::with_alpha(self.accent, 140)
                } else {
                    self.edge
                },
            );
        }
        let size = if r.3 < 30.0 { 12.0 } else { self.text };
        let shown = crate::info::fit_text(
            g,
            label,
            (r.2 - if r.2 < 48.0 { 8.0 } else { 16.0 }).max(0.0),
            size,
            primary || active,
        );
        let width = g.measure_chrome_text(&shown, size, primary || active);
        text(
            g,
            (
                r.0 + (r.2 - width) / 2.0,
                r.1 + (r.3 - size) / 2.0 - 1.0,
                r.2 - 8.0,
                size + 2.0,
            ),
            &shown,
            size,
            ink,
            primary || active,
        );
    }
    pub fn field(self, g: &mut gpu::GpuRenderer, r: Rect, active: bool) {
        g.round_rect_fill(r.0, r.1, r.2, r.3, self.box_radius(), self.field);
        g.round_rect_stroke(
            r.0,
            r.1,
            r.2,
            r.3,
            self.box_radius(),
            if active { 2.0 } else { 1.0 },
            if active {
                theme::enforce_contrast_at(self.accent, self.field, 3.0)
            } else {
                self.edge
            },
        );
    }
}

pub(super) fn text(
    g: &mut gpu::GpuRenderer,
    r: Rect,
    value: &str,
    size: f32,
    color: Color,
    bold: bool,
) {
    let shown = crate::info::fit_text(g, value, r.2.max(0.0), size, bold);
    g.draw_text(
        r.0,
        r.1,
        &shown,
        gpu::DrawOpts {
            font_size: size,
            color,
            bold,
            italic: false,
        },
    );
}

pub(super) fn paragraph(
    g: &mut gpu::GpuRenderer,
    r: Rect,
    value: &str,
    size: f32,
    color: Color,
) -> f32 {
    let mut line = String::new();
    let mut y = r.1;
    for word in value.split_whitespace() {
        let next = if line.is_empty() {
            word.into()
        } else {
            format!("{line} {word}")
        };
        if !line.is_empty() && g.measure_chrome_text(&next, size, false) > r.2 {
            text(g, (r.0, y, r.2, size + 3.0), &line, size, color, false);
            y += size + 6.0;
            line = word.into();
        } else {
            line = next;
        }
    }
    if !line.is_empty() {
        text(g, (r.0, y, r.2, size + 3.0), &line, size, color, false);
        y += size + 6.0;
    }
    y - r.1
}
