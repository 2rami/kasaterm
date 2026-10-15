use crate::*;

impl App {
    pub(crate) fn run_cloud_control_probe(&mut self) {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            OnceLock,
        };
        static START: OnceLock<std::time::Instant> = OnceLock::new();
        static DONE: AtomicBool = AtomicBool::new(false);
        if !crate::verification_run()
            || std::env::var("KASATERM_CLOUD_METRICS").as_deref() != Ok("1")
        {
            return;
        }
        if DONE.load(Ordering::Relaxed)
            || START
                .get_or_init(std::time::Instant::now)
                .elapsed()
                .as_millis()
                < 1500
        {
            return;
        }
        let temporary = |key: &str| {
            std::env::var_os(key)
                .and_then(|p| std::fs::canonicalize(p).ok())
                .is_some_and(|p| p.starts_with("/private/tmp") || p.starts_with("/tmp"))
        };
        if !["KASATERM_SETTINGS_FILE", "KASATERM_SESSION_FILE", "TMPDIR"]
            .iter()
            .all(|key| temporary(key))
        {
            return;
        }
        let Some(g) = self.gpu.as_mut() else {
            return;
        };
        DONE.store(true, Ordering::Relaxed);
        let original_scale = g.scale();
        let original_font = theme::ui_font();
        let mut rows = Vec::new();
        let mut failures = 0;
        for font in ["system", "terminal"] {
            theme::set_ui_font(font);
            for factor in [1.0, 1.25, 1.5] {
                let scale = original_scale * factor;
                g.set_scale(scale);
                for height in [26.0, 32.0] {
                    for bold in [false, true] {
                        for label in ["확인", "공유 폴더", "Apply", "gyp", "Continue workspace"]
                        {
                            g.clear_chrome();
                            let width = super::button_width(g, label, height, bold);
                            let r = (10.25, 20.5, width, height);
                            let metrics =
                                g.chrome_text_metrics(label, super::font_size(height), bold);
                            let start = g.chrome.len();
                            g.centered_chrome_text(
                                r,
                                label,
                                gpu::DrawOpts {
                                    font_size: super::font_size(height),
                                    color: theme::text(),
                                    bold,
                                    italic: false,
                                },
                            );
                            let mut ink: Option<(f32, f32, f32, f32)> = None;
                            for quad in &g.chrome[start..] {
                                let [x, y, w, h] = quad.cell_px;
                                ink = Some(match ink {
                                    Some((l, t, rr, b)) => {
                                        (l.min(x), t.min(y), rr.max(x + w), b.max(y + h))
                                    }
                                    None => (x, y, x + w, y + h),
                                });
                            }
                            let ok = ink.is_some_and(|(l, t, rr, b)| {
                                let centered = ((l + rr) * 0.5 - (r.0 + r.2 * 0.5) * scale).abs()
                                    <= 0.51
                                    && ((t + b) * 0.5 - (r.1 + r.3 * 0.5) * scale).abs() <= 0.51;
                                centered
                                    && l >= r.0 * scale - 0.51
                                    && rr <= (r.0 + r.2) * scale + 0.51
                                    && t >= r.1 * scale - 0.51
                                    && b <= (r.1 + r.3) * scale + 0.51
                                    && width - metrics.width() <= 25.01
                            });
                            if !ok {
                                failures += 1;
                            }
                            rows.push(serde_json::json!({"font":font,"scale":scale,"label":label,"bold":bold,"height":height,"width":width,"ink_px":ink,"ok":ok}));
                        }
                    }
                }
            }
        }
        g.set_scale(original_scale);
        theme::set_ui_font(&original_font);
        g.clear_chrome();
        if let Some(path) = std::env::var_os("TMPDIR").map(std::path::PathBuf::from) {
            let report = serde_json::json!({"checks":rows.len(),"failures":failures,"cases":rows});
            let _ = std::fs::write(path.join("cloud-controls.json"), report.to_string());
        }
        eprintln!("[cloud-controls] checks={} failures={failures}", rows.len());
        self.chrome_dirty = true;
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}
