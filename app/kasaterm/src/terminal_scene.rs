//! Shared terminal composition before the GPU or the web consumes a frame.
use super::*;
use std::hash::{Hash, Hasher};
use std::time::Duration;

#[allow(clippy::too_many_arguments)]
fn compose_student_banners(
    composed: &mut Vec<Vec<GridCell>>,
    name: &str,
    slug: &'static str,
    accent: Option<[u8; 4]>,
    body_left: f32,
    body_top: f32,
    scw: f32,
    sch: f32,
    banner_slots: &mut Vec<(&'static str, (f32, f32, f32, f32), (f32, f32))>,
) {
    let logos: Vec<(isize, usize, usize, usize, &[char])> = find_clawd_banners(&composed)
        .into_iter()
        .map(|(br, bc)| (br, bc, CLAWD_COLS, CLAWD_ROWS, CLAWD_TITLE))
        .chain(
            find_agy_banners(&composed)
                .into_iter()
                .map(|(br, bc)| (br, bc, AGY_COLS, AGY_ROWS, AGY_TITLE)),
        )
        .filter(|_| student_has_sprite(slug, "idle"))
        .collect();
    for (br, bc, lcols, lrows, title) in logos {
        // br 은 스크롤로 위가 잘리면 음수, 아래가 잘리면 박스가
        // 그리드 밖까지 이어진다 — 스프라이트는 pane 세로 범위로
        // 클립해 셀 스크롤과 함께 자연스럽게 잘려 나가게 한다.
        // 로고 칸이 Clawd 보다 크면 도트를 늘리지 말고 비율을 지켜
        // 안에 맞춘 뒤 바닥에 세운다(발이 로고 밑선에 닿는다).
        let (bw, bh) = fit_sprite_box(lcols, lrows, scw, sch);
        banner_slots.push((
            slug,
            (
                body_left + bc as f32 * scw + (lcols as f32 * scw - bw) * 0.5,
                body_top + br as f32 * sch + (lrows as f32 * sch - bh),
                bw,
                bh,
            ),
            (body_top, body_top + composed.len() as f32 * sch),
        ));
        let r0 = br.max(0) as usize;
        let r1 = (br + lrows as isize).clamp(0, composed.len() as isize) as usize;
        for row in composed[r0..r1].iter_mut() {
            for cell in row.iter_mut().skip(bc).take(lcols) {
                *cell = GridCell::blank();
            }
        }
        // 배너 타이틀("Claude Code"·"Antigravity CLI")도 학생 이름으로 —
        // 도트만 바뀌면 학생이 남의 이름표를 달고 서 있는 꼴(거노).
        replace_banner_title(composed, br, bc, lcols, lrows, title, name, accent);
        // 웰컴 배너("Welcome back <user>!")면 도트 위 인사말 행을
        // 배정 학생 페르소나 인사말로 — launcher 화면에선 no-op.
        replace_welcome_greeting(composed, br, name, accent);
        // 배너 박스 보더도 학생색 — 인사말 치환과 **분리**해서
        // 무조건 부른다. 인사말 함수 안에 뒀던 동안 인사말
        // 로스터에 없는 학생 pane 은 치환이 조기 반환하며
        // 테두리까지 파랑으로 남았다(2026-08-20 거노 스샷).
        // 박스 코너가 없는 launcher 화면에선 자연 no-op.
        if let Some(acc) = accent {
            let art_bottom = (br + lrows as isize).max(0) as usize;
            tint_welcome_box(composed, br.max(0) as usize, art_bottom, acc);
        }
    }
}
use kasa_mcp::visual::{
    self, PaneVisualFrame, VisualAsset, VisualMotion, VisualOverlay, VisualRect,
};

#[derive(Default)]
pub(crate) struct VisualPump {
    last_tick: Option<Instant>,
    fingerprints: std::collections::HashMap<String, (String, Vec<Vec<GridCell>>, Vec<u8>)>,
    sources: std::collections::HashMap<String, SourceStamp>,
    registered: std::collections::HashMap<(String, u64, usize), (Arc<[u8]>, String)>,
    assets: std::collections::HashMap<String, Option<(Arc<[u8]>, u32, u32)>>,
    animations: std::collections::HashMap<String, Option<Vec<Arc<[u8]>>>>,
    epoch_ms: Option<u64>,
    revision: u64,
}

struct SourceStamp {
    key: String,
    context: u64,
    checked_at: Instant,
    animated_cells: bool,
    revision: u64,
}

impl SourceStamp {
    fn reusable(&self, key: &str, context: u64, now: Instant, published: Option<u64>) -> bool {
        published == Some(self.revision)
            && self.key == key
            && self.context == context
            && !self.animated_cells
            && now.saturating_duration_since(self.checked_at) < Duration::from_secs(1)
    }
}

impl VisualPump {
    pub(crate) fn invalidate_assets(&mut self) {
        self.assets.clear();
        self.animations.clear();
        self.fingerprints.clear();
        self.sources.clear();
        self.registered.clear();
    }

    fn register(&mut self, pane: &str, image_id: u64, bytes: Arc<[u8]>) -> Option<String> {
        let key = (pane.to_string(), image_id, bytes.as_ptr() as usize);
        if let Some((_, id)) = self.registered.get(&key) {
            if visual::inline_asset_exists(pane, id) {
                return Some(id.clone());
            }
        }
        let id = visual::register_inline_asset(pane, image_id, bytes.clone())?;
        self.registered.insert(key, (bytes, id.clone()));
        Some(id)
    }

    fn image(
        &mut self,
        key: &str,
        load: impl FnOnce() -> Option<(Vec<u8>, u32, u32)>,
    ) -> Option<(Arc<[u8]>, u32, u32)> {
        self.assets
            .entry(key.to_string())
            .or_insert_with(|| load().map(|(b, w, h)| (b.into(), w, h)))
            .clone()
    }
}

fn png_asset((rgba, width, height): (Vec<u8>, u32, u32)) -> Option<(Vec<u8>, u32, u32)> {
    let image = image::RgbaImage::from_raw(width, height, rgba)?;
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .ok()?;
    Some((bytes.into_inner(), width, height))
}

fn cell_rect(rect: (f32, f32, f32, f32), cell: (f32, f32)) -> VisualRect {
    VisualRect {
        x: rect.0 / cell.0,
        y: rect.1 / cell.1,
        width: rect.2 / cell.0,
        height: rect.3 / cell.1,
    }
}

impl TerminalComposition {
    fn web_overlays(
        &self,
        pane: &str,
        cols: u16,
        rows: u16,
        cell: (f32, f32),
        epoch_ms: u64,
        cache: &mut VisualPump,
    ) -> Option<Vec<VisualOverlay>> {
        let mut out = Vec::new();
        let full_clip = (0.0, 0.0, cols as f32 * cell.0, rows as f32 * cell.1);
        let mut add = |rect, clip, z, asset, motion, fit: &str, anchor: &str| {
            out.push(VisualOverlay {
                id: format!("overlay-{}", out.len()),
                rect: cell_rect(rect, cell),
                clip: Some(cell_rect(clip, cell)),
                z,
                asset,
                motion,
                fit: fit.to_string(),
                anchor: anchor.to_string(),
            });
        };
        if self.agents_view {
            if let Some((bytes, _, _)) =
                cache.image("schale-classroom", || png_asset(schale_classroom_rgba()?))
            {
                if let Some(id) = cache.register(pane, 0, bytes) {
                    add(
                        full_clip,
                        full_clip,
                        -20,
                        VisualAsset::Inline { id },
                        None,
                        "cover",
                        "center",
                    );
                }
            }
        }
        for (index, (_, path, x, y, w, h, c0, c1, hug)) in self.inline_slots.iter().enumerate() {
            let asset = cache.image(path, || {
                let bytes = std::fs::read(path).ok()?;
                let decoded = image::load_from_memory(&bytes).ok()?;
                png_asset((
                    decoded.to_rgba8().into_raw(),
                    decoded.width(),
                    decoded.height(),
                ))
            });
            let Some((bytes, iw, ih)) = asset else {
                continue;
            };
            let width = if *hug {
                (h * iw as f32 / ih.max(1) as f32).min(iw as f32).min(*w)
            } else {
                *w
            };
            if let Some(id) = cache.register(pane, index as u64 + 1, bytes) {
                add(
                    (*x, *y, width, *h),
                    (*x, *c0, width, *c1 - *c0),
                    -10,
                    VisualAsset::Inline { id },
                    None,
                    "scale-down",
                    "center",
                );
            }
        }
        let mut sprite = |slug: &str, motion: &str, rect, clip, foreground| {
            let frames = cache
                .animations
                .entry(format!("{slug}:{motion}"))
                .or_insert_with(|| {
                    student_sprite_frames(slug, motion)?
                        .into_iter()
                        .map(|frame| png_asset(frame).map(|(bytes, _, _)| Arc::<[u8]>::from(bytes)))
                        .collect::<Option<Vec<_>>>()
                })
                .clone();
            let Some(frames) = frames else { return };
            let Some(ids) = frames
                .iter()
                .enumerate()
                .map(|(index, bytes)| cache.register(pane, 10_000 + index as u64, bytes.clone()))
                .collect::<Option<Vec<_>>>()
            else {
                return;
            };
            let count = ids.len() as u16;
            let frame_ms = if motion == "walk" {
                STUDENT_WALK_FRAME_MS as u32
            } else {
                STUDENT_ANIM_FRAME_MS as u32
            };
            add(
                rect,
                clip,
                if foreground { 10 } else { -5 },
                VisualAsset::Animation { frames: ids },
                Some(VisualMotion {
                    frames: count,
                    frame_ms,
                    started_at_ms: epoch_ms,
                    looping: true,
                }),
                if foreground { "contain" } else { "scale-down" },
                if foreground { "bottom" } else { "center" },
            );
        };
        for (slug, rect, (c0, c1)) in &self.banner_slots {
            sprite(slug, "idle", *rect, (rect.0, *c0, rect.2, *c1 - *c0), false);
        }
        for (slug, rect) in &self.spinner_slots {
            sprite(slug, "walk", *rect, full_clip, true);
        }
        for (slug, rect) in &self.waiting_slots {
            sprite(slug, "wave", *rect, full_clip, true);
        }
        for (slug, motion, rect) in &self.standing_slots {
            sprite(slug, motion, *rect, full_clip, true);
        }
        for (slug, rect) in &self.profile_slots {
            if let Some((bytes, _, _)) = cache.image(&format!("profile:{slug}"), || {
                png_asset(student_profile_rgba(slug)?)
            }) {
                if let Some(id) = cache.register(pane, 20_000, bytes) {
                    add(
                        *rect,
                        full_clip,
                        10,
                        VisualAsset::Inline { id },
                        None,
                        "contain",
                        "bottom",
                    );
                }
            }
        }
        for rect in &self.schale_logo_slots {
            if let Some((bytes, _, _)) =
                cache.image("schale-logo", || png_asset(schale_logo_rgba()?))
            {
                if let Some(id) = cache.register(pane, 30_000, bytes) {
                    add(
                        *rect,
                        full_clip,
                        10,
                        VisualAsset::Inline { id },
                        None,
                        "contain",
                        "bottom",
                    );
                }
            }
        }
        for icon in &self.status_model_icons {
            let name = icon.provider.icon_name();
            if let Some((bytes, _, _)) = cache.image(&format!("icon:{name}"), || {
                let svg = gpu::GpuRenderer::icon_svg(name)?;
                let mut rgba = gpu::GpuRenderer::rasterize_icon(svg, 96)?;
                for pixel in rgba.chunks_exact_mut(4) {
                    pixel[..3].copy_from_slice(&STATUS_MODEL_COLOR[..3]);
                }
                png_asset((rgba, 96, 96))
            }) {
                if let Some(id) = cache.register(pane, 40_000, bytes) {
                    add(
                        icon.image_rect(),
                        full_clip,
                        5,
                        VisualAsset::Inline { id },
                        None,
                        "contain",
                        "center",
                    );
                }
            }
        }
        for &(x, y, w, h, color) in &self.title_outline_slots {
            let t = 1.5;
            for rect in [
                (x, y, w, t),
                (x, y + h - t, w, t),
                (x, y, t, h),
                (x + w - t, y, t, h),
            ] {
                add(
                    rect,
                    full_clip,
                    1,
                    VisualAsset::Solid { color },
                    None,
                    "fill",
                    "center",
                );
            }
        }
        let expected = usize::from(self.agents_view)
            + self.inline_slots.len()
            + self.banner_slots.len()
            + self.spinner_slots.len()
            + self.waiting_slots.len()
            + self.standing_slots.len()
            + self.profile_slots.len()
            + self.schale_logo_slots.len()
            + self.status_model_icons.len()
            + self.title_outline_slots.len() * 4;
        // A failed asset registration must not ship cells whose original art was erased.
        (out.len() == expected).then_some(out)
    }
}

impl App {
    fn web_visual_context(&self, ws: &Workspace, pane: &PaneState, id: &str) -> u64 {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        self.cell.w.to_bits().hash(&mut hash);
        self.cell.h.to_bits().hash(&mut hash);
        theme::theme_name().hash(&mut hash);
        theme::shape_name().hash(&mut hash);
        theme::bg().hash(&mut hash);
        theme::text().hash(&mut hash);
        theme::accent_name().hash(&mut hash);
        // The full display gate may refresh argv with ps; idle invalidation only
        // needs the cached binding and agent state, not another process query.
        ws.pane_character.get(id).hash(&mut hash);
        theme::character_ordinal(&ws.pane_character, id).hash(&mut hash);
        pane.color.hash(&mut hash);
        pane.title.hash(&mut hash);
        self.pane_claude_sid.get(id).hash(&mut hash);
        self.pane_cwd_cache.get(id).hash(&mut hash);
        self.pane_view_cwd.get(id).hash(&mut hash);
        self.pane_activity
            .get(id)
            .map(|a| (a.status.as_str(), a.stalled.is_some()))
            .hash(&mut hash);
        self.pane_ultracode.contains(id).hash(&mut hash);
        self.turn_done_panes.contains(id).hash(&mut hash);
        self.notify_flash_factor(id).is_some().hash(&mut hash);
        self.spinner_probe
            .get(id)
            .map(|(_, _, confirmed, _)| *confirmed)
            .hash(&mut hash);
        if let Some(session) = self.pty.get(id) {
            format!("{:?}", session.active_agent()).hash(&mut hash);
            session
                .last_submit()
                .is_some_and(|at| at.elapsed() < Self::SUBMIT_TRUST)
                .hash(&mut hash);
            session.output_heartbeat_fresh().hash(&mut hash);
        }
        hash.finish()
    }

    /// Subscription wakes the event loop once; only subscribed panes keep this timer alive.
    pub(crate) fn publish_web_visual_scenes(&mut self) -> Option<Instant> {
        let panes = visual::subscribed_panes();
        if panes.is_empty() {
            self.web_visual = VisualPump::default();
            return None;
        }
        let now = Instant::now();
        let interval = Duration::from_millis(50);
        if let Some(last) = self.web_visual.last_tick {
            if now < last + interval {
                return Some(last + interval);
            }
        }
        let mut cache = std::mem::take(&mut self.web_visual);
        cache.last_tick = Some(now);
        cache.fingerprints.retain(|id, _| panes.contains(id));
        cache.sources.retain(|id, _| panes.contains(id));
        cache.registered.retain(|(id, _, _), _| panes.contains(id));
        if cache.assets.len() > 128 {
            cache.assets.clear();
            cache.registered.clear();
        }
        if cache.animations.len() > 128 {
            cache.animations.clear();
            cache.registered.clear();
        }
        let epoch = *cache.epoch_ms.get_or_insert_with(|| {
            (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64)
                .saturating_sub(self.version_anim_start.elapsed().as_millis() as u64)
        });
        let cell = (self.cell.w.max(0.01), self.cell.h.max(0.01));
        let mut ready = Vec::new();
        let ws = self.ws.lock().unwrap();
        for id in panes {
            // The requested PTY is a tab identity. Folding to the visible tab would
            // silently attach a hidden viewer to a different terminal.
            let Some(session) = self.pty.get(&id) else {
                continue;
            };
            let Some(outer) = ws.outer_for_pty(&id) else {
                continue;
            };
            let Some(pane) = ws.panes.get(&outer) else {
                continue;
            };
            let raw = session.live_screen();
            let Some(source_key) = visual::source_key(&raw, 0) else {
                continue;
            };
            let context = self.web_visual_context(&ws, pane, &id);
            let published = visual::cached_revision(&id);
            if cache
                .sources
                .get(&id)
                .is_some_and(|stamp| stamp.reusable(&source_key, context, now, published))
            {
                continue;
            }
            let mut term = TerminalPane {
                cols: raw.cols,
                rows: raw.rows,
                cells: vec![Vec::new(); raw.rows as usize],
                cursor_row: raw.cursor_row,
                cursor_col: raw.cursor_col,
                cursor_visible: raw.cursor_visible,
                alt_screen: raw.alt_screen,
                inline_images: raw.inline_images.clone(),
                ..Default::default()
            };
            for (row, cells) in &raw.dirty {
                term.cells[*row as usize] = cells.clone();
            }
            let composition = self.compose_terminal_pane(
                &ws,
                pane,
                Some(&term),
                &id,
                id.clone(),
                raw.cols as usize,
                raw.rows as usize,
                0.0,
                0.0,
                1.0,
                false,
                &Default::default(),
            );
            ready.push((id, raw, source_key, context, composition));
        }
        drop(ws);
        // Decoding a newly encountered image must not hold up PTY screen delivery.
        for (id, raw, source_key, context, composition) in ready {
            let Some(overlays) =
                composition.web_overlays(&id, raw.cols, raw.rows, cell, epoch, &mut cache)
            else {
                continue;
            };
            let overlay_key = serde_json::to_vec(&overlays).unwrap_or_default();
            let published = visual::cached_revision(&id);
            let same_publication = cache
                .sources
                .get(&id)
                .is_some_and(|stamp| published == Some(stamp.revision));
            if same_publication
                && cache.fingerprints.get(&id).is_some_and(
                    |(old_source, old_cells, old_overlays)| {
                        *old_source == source_key
                            && *old_cells == composition.rows
                            && *old_overlays == overlay_key
                    },
                )
            {
                if let Some(stamp) = cache.sources.get_mut(&id) {
                    stamp.checked_at = now;
                    stamp.context = context;
                    stamp.animated_cells = composition.animated_cells;
                }
                continue;
            }
            cache.revision += 1;
            let frame = PaneVisualFrame {
                pane_id: id.clone(),
                source_key: source_key.clone(),
                scene_revision: cache.revision,
                cols: raw.cols,
                rows: raw.rows,
                offset: 0,
                raw_snapshot: raw,
                composed_cells: composition.rows.clone(),
                overlays,
            };
            if visual::publish(frame) {
                cache.sources.insert(
                    id.clone(),
                    SourceStamp {
                        key: source_key.clone(),
                        context,
                        checked_at: now,
                        animated_cells: composition.animated_cells,
                        revision: cache.revision,
                    },
                );
                cache
                    .fingerprints
                    .insert(id, (source_key, composition.rows, overlay_key));
            }
        }
        self.web_visual = cache;
        Some(now + interval)
    }
}

pub(crate) type StickySlot = (
    f32,
    f32,
    f32,
    f32,
    String,
    String,
    Option<(f32, f32, f32, f32)>,
    Option<(f32, f32, f32, f32)>,
);
pub(crate) type TurnSlot = (
    String,
    (f32, f32, f32, f32),
    Option<(f32, f32, f32, f32)>,
    Option<(f32, f32, f32, f32)>,
    crate::turnjump::TurnHeader,
);

#[derive(Default)]
pub(crate) struct TerminalComposition {
    pub(crate) animated_cells: bool,
    pub(crate) rows: Vec<Vec<GridCell>>,
    pub(crate) banner_slots: Vec<(&'static str, (f32, f32, f32, f32), (f32, f32))>,
    pub(crate) spinner_slots: Vec<(&'static str, (f32, f32, f32, f32))>,
    pub(crate) waiting_slots: Vec<(&'static str, (f32, f32, f32, f32))>,
    pub(crate) standing_slots: Vec<(&'static str, &'static str, (f32, f32, f32, f32))>,
    pub(crate) profile_slots: Vec<(&'static str, (f32, f32, f32, f32))>,
    pub(crate) inline_slots: Vec<(String, String, f32, f32, f32, f32, f32, f32, bool)>,
    pub(crate) schale_logo_slots: Vec<(f32, f32, f32, f32)>,
    pub(crate) title_outline_slots: Vec<(f32, f32, f32, f32, [u8; 4])>,
    pub(crate) status_model_icons: Vec<StatusModelIconSlot>,
    pub(crate) sticky_pill_slots: Vec<StickySlot>,
    pub(crate) turn_header_slots: Vec<TurnSlot>,
    pub(crate) view_shifts: Vec<(String, crate::PaneViewShift)>,
    pub(crate) tip_hit: Option<(String, u32, (f32, f32, f32, f32))>,
    pub(crate) agents_view_panes: std::collections::HashSet<String>,
    pub(crate) mirror_claude_panes: std::collections::HashSet<String>,
    pub(crate) agents_view: bool,
    pub(crate) runs_claude: bool,
    pub(crate) true_char: Option<String>,
    pub(crate) tab_pid: String,
}

#[cfg(test)]
mod visual_scene_tests {
    use super::*;

    #[test]
    fn visual_scene_reconnect_rebuilds_even_when_source_fingerprint_is_unchanged() {
        let now = Instant::now();
        let stamp = SourceStamp {
            key: "raw".into(),
            context: 4,
            checked_at: now,
            animated_cells: false,
            revision: 9,
        };
        assert!(stamp.reusable("raw", 4, now, Some(9)));
        assert!(!stamp.reusable("raw", 4, now, None));
        assert!(!stamp.reusable("raw", 4, now, Some(10)));
    }

    #[test]
    fn visual_scene_idle_reuse_expires_on_metadata_changes_or_cell_animation() {
        let now = Instant::now();
        let mut stamp = SourceStamp {
            key: "raw".into(),
            context: 4,
            checked_at: now,
            animated_cells: false,
            revision: 9,
        };
        assert!(stamp.reusable("raw", 4, now + Duration::from_millis(50), Some(9)));
        assert!(!stamp.reusable("new-output", 4, now, Some(9)));
        assert!(!stamp.reusable("raw", 5, now, Some(9)));
        assert!(!stamp.reusable("raw", 4, now + Duration::from_secs(1), Some(9)));
        stamp.animated_cells = true;
        assert!(!stamp.reusable("raw", 4, now, Some(9)));
    }

    #[test]
    fn visual_scene_asset_registration_survives_fast_unsubscribe_and_resubscribe() {
        let pane = "%scene-asset-resubscribe";
        let first = visual::subscribe(pane);
        let mut cache = VisualPump::default();
        let bytes: Arc<[u8]> = png_asset((vec![255, 0, 0, 255], 1, 1)).unwrap().0.into();
        let id = cache.register(pane, 1, bytes.clone()).unwrap();
        assert_eq!(
            cache.register(pane, 1, bytes.clone()).as_deref(),
            Some(id.as_str())
        );
        drop(first);
        assert!(!visual::inline_asset_exists(pane, &id));
        let _second = visual::subscribe(pane);
        assert_eq!(cache.register(pane, 1, bytes).as_deref(), Some(id.as_str()));
        assert!(visual::inline_asset_exists(pane, &id));
        cache.invalidate_assets();
        assert!(cache.registered.is_empty());
        assert!(cache.sources.is_empty());
    }

    fn banner(cols: usize, visible_rows: usize) -> Vec<Vec<GridCell>> {
        [
            " ▐▛███▛█   Claude Code v2.1.237",
            "▝▜██████▀  Fable 5",
            "  ▝▝ ▝▝    ~/project",
        ]
        .iter()
        .take(visible_rows)
        .map(|text| {
            let mut row: Vec<_> = text
                .chars()
                .map(|ch| GridCell {
                    ch,
                    ..GridCell::blank()
                })
                .collect();
            row.resize(cols, GridCell::blank());
            row
        })
        .collect()
    }

    #[test]
    fn visual_scene_erases_banner_and_returns_art_in_the_same_composition() {
        for cols in [16, 40, 100] {
            for visible in [2, 3] {
                let raw = banner(cols, visible);
                let mut rows = raw.clone();
                let mut slots = Vec::new();
                compose_student_banners(
                    &mut rows,
                    "아로나",
                    "arona",
                    None,
                    0.0,
                    0.0,
                    9.0,
                    18.0,
                    &mut slots,
                );
                assert_eq!(slots.len(), 1, "{cols} columns / {visible} rows");
                assert_eq!(slots[0].2, (0.0, visible as f32 * 18.0));
                assert!(rows
                    .iter()
                    .all(|row| row[..CLAWD_COLS].iter().all(|c| c.ch == ' ')));
                assert_eq!(raw, banner(cols, visible));
            }
        }
    }

    #[test]
    fn visual_scene_keeps_scrolled_banner_clip_and_sprite_slot_together() {
        let _subscription = visual::subscribe("%fixture");
        let mut rows = banner(50, 3)[1..].to_vec();
        let mut slots = Vec::new();
        compose_student_banners(
            &mut rows,
            "아로나",
            "arona",
            None,
            0.0,
            0.0,
            9.0,
            18.0,
            &mut slots,
        );
        assert_eq!(slots.len(), 1);
        assert!(slots[0].1 .1 < 0.0);
        let scene = TerminalComposition {
            rows,
            banner_slots: slots,
            ..Default::default()
        };
        let overlays = scene
            .web_overlays(
                "%fixture",
                50,
                2,
                (9.0, 18.0),
                1234,
                &mut VisualPump::default(),
            )
            .unwrap();
        assert_eq!(overlays.len(), 1);
        assert_eq!(overlays[0].rect.y, -1.0);
        assert_eq!(overlays[0].clip.as_ref().unwrap().height, 2.0);
        assert_eq!(overlays[0].fit, "scale-down");
        assert_eq!(overlays[0].anchor, "center");
        assert_eq!(overlays[0].motion.as_ref().unwrap().started_at_ms, 1234);
    }

    #[test]
    fn visual_scene_does_not_publish_erased_cells_without_their_asset() {
        let scene = TerminalComposition {
            profile_slots: vec![("arona", (0.0, 0.0, 18.0, 18.0))],
            ..Default::default()
        };
        assert!(scene
            .web_overlays(
                "%unregistered-scene",
                20,
                5,
                (9.0, 18.0),
                0,
                &mut VisualPump::default()
            )
            .is_none());
    }

    #[test]
    fn visual_scene_standing_clearance_survives_web_cell_ratio_changes() {
        let pane = "%standing-clearance";
        let _subscription = visual::subscribe(pane);
        let mut rows = vec![vec![GridCell::blank(); 59]; 6];
        for (cell, ch) in rows[4][52..59].iter_mut().zip("3156d31".chars()) {
            cell.ch = ch;
        }
        let left = stand_left_col(&rows, 5, 59).unwrap();
        for cell in [(9.0, 18.0), (6.506, 17.540)] {
            let scene = TerminalComposition {
                rows: rows.clone(),
                standing_slots: vec![(
                    "arona",
                    "idle",
                    standing_slot_rect(5, left, 41, (0.0, 0.0), cell),
                )],
                ..Default::default()
            };
            let overlays = scene
                .web_overlays(pane, 59, 41, cell, 1234, &mut VisualPump::default())
                .unwrap();
            let overlay = &overlays[0];
            assert!(overlay.rect.x + overlay.rect.width <= 52.001);
            assert!((overlay.rect.y - 3.0).abs() < 0.001);
            assert_eq!(overlay.fit, "contain");
            assert_eq!(overlay.anchor, "bottom");
        }
    }

    #[test]
    fn visual_scene_gpu_fit_preserves_aspect_and_bottom_anchor() {
        for slot in [(0.0, 0.0, 20.0, 60.0), (4.0, 8.0, 100.0, 20.0)] {
            let (x, y, w, h) = gpu::fit_terminal_art(slot, (96, 96), 2.0, true);
            assert_eq!(w, h);
            assert_eq!(y + h, slot.1 + slot.3);
            assert_eq!(x + w / 2.0, slot.0 + slot.2 / 2.0);
        }
        let (_, _, w, h) = gpu::fit_terminal_art((0.0, 0.0, 200.0, 200.0), (96, 96), 2.0, false);
        assert_eq!((w, h), (48.0, 48.0));
    }
}

impl App {
    /// A live consumer supplies its own offset-zero term and no desktop chrome.
    /// Placeholder erasure and the matching art always leave in the same result.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn compose_terminal_pane(
        &self,
        ws: &Workspace,
        pane: &PaneState,
        term: Option<&TerminalPane>,
        id: &String,
        tab_pid: String,
        cols_now: usize,
        rows_now: usize,
        body_left: f32,
        body_top: f32,
        font_scale: f32,
        desktop_view: bool,
        turn_headers: &std::collections::HashMap<String, crate::turnjump::TurnHeader>,
    ) -> TerminalComposition {
        let pane_scales = std::collections::HashMap::from([(id.clone(), font_scale)]);
        let normalise = |row: &Vec<GridCell>| -> Vec<GridCell> {
            let mut row = row.clone();
            row.resize(cols_now, GridCell::blank());
            row
        };
        let mut banner_slots: Vec<(&'static str, (f32, f32, f32, f32), (f32, f32))> =
            Default::default();
        let mut spinner_slots: Vec<(&'static str, (f32, f32, f32, f32))> = Default::default();
        let mut waiting_slots: Vec<(&'static str, (f32, f32, f32, f32))> = Default::default();
        let mut standing_slots: Vec<(&'static str, &'static str, (f32, f32, f32, f32))> =
            Default::default();
        let mut profile_slots: Vec<(&'static str, (f32, f32, f32, f32))> = Default::default();
        let mut inline_slots: Vec<(String, String, f32, f32, f32, f32, f32, f32, bool)> =
            Default::default();
        let mut schale_logo_slots: Vec<(f32, f32, f32, f32)> = Default::default();
        let mut title_outline_slots: Vec<(f32, f32, f32, f32, [u8; 4])> = Default::default();
        let mut status_model_icons: Vec<StatusModelIconSlot> = Default::default();
        let mut sticky_pill_slots: Vec<StickySlot> = Default::default();
        let mut turn_header_slots: Vec<TurnSlot> = Default::default();
        let mut view_shifts: Vec<(String, crate::PaneViewShift)> = Default::default();
        let mut tip_hit: Option<(String, u32, (f32, f32, f32, f32))> = Default::default();
        let mut agents_view_panes: std::collections::HashSet<String> = Default::default();
        let mut mirror_claude_panes: std::collections::HashSet<String> = Default::default();
        let mut animated_cells = false;
        let mut composed: Vec<Vec<GridCell>> = match term {
            Some(t) => t.cells.iter().take(rows_now).map(normalise).collect(),
            None => Vec::new(),
        };
        // 학생 도트·배너·스피너 같은 claude 화면 해석은 **claude 가 실제로
        // 도는 pane** 에서만 한다. 화면 모양만 보고 판정하면 남의 TUI 를
        // claude 로 오인한다 — helix 의 LSP 진행 스피너가 브라유라, 파일을
        // 편집기 pane 으로 열면 학생 도트가 편집기 상태줄 위에 올라앉았다
        // (실측). alt screen 여부로는 못 가른다: claude code 2.1.220 도
        // alt screen 을 쓴다(tmux `#{alternate_on}` 으로 helix·claude 양쪽
        // 실측 — 둘 다 1).
        // active_process_name 은 셸의 **직속** 자식이라, claude 가 안에서
        // cargo·vim 을 띄워도 여전히 claude 다(그것들의 부모는 claude).
        // 500ms 캐시가 이미 붙어 있어 매 프레임 불러도 싸다.
        // 학생 상태는 **탭 pid** 로 기록되고 이 루프가 든 `id` 는 BSP leaf 다.
        // 접지 않으면 탭에서 도는 클로드가 안 잡혀, 프사·전신·배너 도트가
        // 통째로 안 뜬다(거노 2026-08-07). 아래 ordinal 도 같은 키를 쓴다.
        let agent_kind = self
            .pty
            .get(tab_pid.as_str())
            .and_then(|p| p.active_agent())
            // 이사 간 거울 pane — claude 는 저쪽 기계에서 돌아 로컬 프로세스
            // 테이블에 없다(active_agent=None). 그대로 두면 학생 그림 전부
            // (배너·학생색·standing·프사)가 이 게이트에서 잘려 「이사하면
            // 테마가 안 보인다」(2026-09-01). 위 주석의 「화면 모양 판정 금지」
            // 는 **아무 pane 이나** 모양으로 판정하지 말라는 것이고, 여기는
            // ①원격 링크로 확정된 pane 에서만 ②우리 statusline 훅이 심는
            // U+FFFC 표식(브라유 스피너류와 달리 남의 TUI 가 안 찍는 값)을
            // 본다 — 거울 화면에 그 표식이 실려 오면 저쪽에서 claude 가
            // 도는 것이 정본이고, 저쪽 claude 가 꺼지면 표식도 사라져
            // 그림이 함께 걷힌다.
            .or_else(|| {
                (kasa_mcp::remote::is_remote_pane(tab_pid.as_str())
                    && kasa_mcp::remote::cached_agent_running(tab_pid.as_str()) != Some(false)
                    && find_statusline_face(&composed).is_some())
                .then(|| {
                    mirror_claude_panes.insert(tab_pid.clone());
                    mirror_claude_panes.insert(id.clone());
                    kasa_pty::AgentKind::Claude
                })
            });
        let runs_claude = agent_kind.is_some();
        // 스크롤을 올려도 **입력창은 맨 아래에 붙잡는다**. 대체화면을 끈
        // claude 는 입력창이 대화의 마지막 줄일 뿐이라, 스크롤백을 거슬러
        // 올라가면 타이핑할 자리가 화면 밖으로 나간다(2026-08-30 지적:
        // "노플리커 끄니까 하단 채팅창 고정되는게 안된다"). 위쪽 sticky
        // 띠가 지나간 질문을 붙잡는 것과 같은 원리로, 살아 있는 화면에서
        // 입력박스를 떠다 뷰포트 맨 아래 행에 덮는다.
        //
        // 스크롤이 0 이면 아무것도 안 한다 — 그때는 원래 자리에 있다.
        // 대체화면 앱(vim·helix)은 스크롤백이 없어 offset 이 늘 0 이므로
        // 여기 들어오지 않는다. 기본 설정의 claude 도 대체화면을 쓰므로
        // 평소엔 잠들어 있다 — `CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=1` 로
        // classic 을 켠 pane 에서만 깨어난다.
        // classic claude 가 화면 밑에 남긴 빈 줄만큼 화면을 아래로 당겨,
        // 상태줄이 pane 바닥에 붙게 한다 — 근거는 `bottom_pull_rows`.
        // 이 pane 을 어떻게 옮겨 그렸는지 — 복사가 되짚을 유일한 기록.
        let mut view_shift = crate::PaneViewShift::default();
        let pulled = match term.filter(|_| desktop_view) {
            Some(t) => {
                let above = self.bottom_pull_rows(tab_pid.as_str(), t, rows_now);
                let n = above.len();
                if n > 0 && n < composed.len() {
                    composed.truncate(composed.len() - n);
                    // rows_above 는 가까운 순([0] = 뷰포트 위 1줄)이라, 앞에
                    // 차례로 밀어 넣으면 먼 줄이 저절로 위로 간다.
                    for r in above {
                        let row = normalise(&r);
                        // composed 와 **같은 순서**로 쌓는다 — 둘 다 앞에
                        // 밀어 넣어야 화면 맨 위 행이 서로 같은 줄을 가리킨다.
                        view_shift.above.insert(0, row.clone());
                        composed.insert(0, row);
                    }
                    n
                } else {
                    0
                }
            }
            None => 0,
        };
        if desktop_view && runs_claude && !composed.is_empty() {
            if let Some(sess) = self.pty.get(tab_pid.as_str()) {
                if sess.view_state().0 > 0 {
                    let live_all: Vec<Vec<GridCell>> = sess
                        // 아래에서 `pulled` 만큼 걷어내므로 그만큼 더 떠 온다 —
                        // 안 그러면 많이 당긴 pane 에서 스캔 폭이 좁아져 입력박스
                        // 위 테두리를 놓치고 붙잡기가 조용히 쉰다.
                        .live_tail_rows(PINNED_INPUT_SCAN_ROWS + pulled)
                        .iter()
                        .map(normalise)
                        .collect();
                    // 뷰포트에서 꼬리 빈 줄을 걷어 냈으면 살아 있는 화면에서도
                    // 같은 수를 걷는다 — 안 그러면 얹는 순간 그 빈 줄이 다시
                    // 바닥에 들어와 입력창만 그만큼 떠오른다.
                    let live = &live_all[..live_all.len().saturating_sub(pulled)];
                    if let Some(top) = crate::screenread::pinned_input_top(live) {
                        // 두 화면은 높이가 같다 — 살아 있는 화면의 **아래
                        // 몇 줄**을 뷰포트의 같은 수만큼에 그대로 얹으면
                        // 입력창이 원래 있던 줄에 정확히 앉는다. 글자가
                        // 남은 데서 끊으면 화면 밑 빈 줄만큼 밀려 내려간다.
                        let h = (live.len() - top).min(composed.len());
                        let base = composed.len() - h;
                        composed[base..].clone_from_slice(&live[live.len() - h..]);
                        view_shift.pinned = live[live.len() - h..].to_vec();
                    }
                }
            }
        }
        view_shift.rows = composed.len();
        // 「하단바 위치가 이상하다」를 잡는 계측(2026-09-05). 화면이 몇 줄
        // 당겨졌는지는 프레임마다 다시 재는 값이라, 그 값이 흔들리면 입력창과
        // 상태줄이 함께 오르내린다. **바뀔 때만** 찍는다 — 매 프레임 찍으면
        // 초당 수십 줄이라 로그가 못 쓰게 된다.
        if desktop_view && std::env::var_os("KASATERM_VIEWSHIFT_DEBUG").is_some() {
            let prev = self.pane_view_shift.get(id.as_str());
            let changed = prev.map_or(true, |p| {
                p.above.len() != view_shift.above.len()
                    || p.pinned.len() != view_shift.pinned.len()
                    || p.rows != view_shift.rows
            });
            if changed {
                eprintln!(
                    "[viewshift] pane={id} pulled={} pinned={} rows={} claude={runs_claude}",
                    view_shift.above.len(),
                    view_shift.pinned.len(),
                    view_shift.rows,
                );
            }
        }
        view_shifts.push((id.clone(), view_shift));
        // Codex 프로세스 판정은 부팅 직후 statusline보다 한두 프레임 늦을 수
        // 있다. 엄격한 `model effort · … · Context N%` 문법을
        // 재작성 함수가 자체 검증하므로 모든 pane에 시도하고, Claude·셸 등
        // 다른 화면은 false로 그대로 둔다. 이래야 첫 상태줄부터 안 밀린다.
        let cwd = self.pane_cwd_cache.get(id.as_str()).cloned();
        let project = cwd
            .as_ref()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .map(str::to_string);
        let branch = cwd.as_ref().and_then(|path| {
            self.window_git
                .lock()
                .ok()
                .and_then(|badges| badges.get(path).map(|badge| badge.branch.clone()))
        });
        let codex_status = restyle_codex_status_line(&mut composed, project.as_deref(), branch.as_deref());
        if kasa_mcp::remote::is_remote_pane(tab_pid.as_str())
            && (agent_kind == Some(kasa_pty::AgentKind::Codex) || codex_status)
        {
            localize_codex_prompt_background(&mut composed, theme::surface());
        }
        {
            let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
            if let Some(slot) = take_status_model_icon_slot(
                &mut composed,
                body_left,
                body_top,
                self.cell.w * fs,
                self.cell.h * fs,
            ) {
                status_model_icons.push(slot);
            }
        }
        // 인라인 이미지(OSC 1337) — PTY 가 절대 줄 앵커를 뷰포트 좌표로
        // 환산해 준 그대로 그린다. GUI 는 스크롤 상태를 모르므로 여기서
        // 계산을 더하면 반드시 어긋난다(정본은 alacritty Term). 클립은
        // pane 셀 영역 — 스크롤로 반쯤 나간 그림이 셀과 함께 잘린다.
        if let Some(t) = term.filter(|t| !t.inline_images.is_empty()) {
            let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
            let (icw, ich) = (self.cell.w * fs, self.cell.h * fs);
            let clip_y0 = body_top;
            let clip_y1 = body_top + rows_now as f32 * ich;
            for v in &t.inline_images {
                // 앵커는 **뷰포트** 좌표다. 화면을 아래로 당긴 pane 은 그림도
                // 같은 만큼 내려야 글 흐름과 안 어긋난다 — 커서·조합 오버레이가
                // `pulled` 를 더하는 것과 같은 이유다. classic claude 는 OSC
                // 1337 을 안 써서 지금은 셸 pane 만 이 길로 오지만(그쪽은 당김이
                // 없다), 보정을 빼 두면 나중에 조용히 어긋난다.
                let vrow = v.row as usize + pulled;
                inline_slots.push((
                    format!("inline:{}:{}:{}", tab_pid, v.id, v.path),
                    v.path.clone(),
                    body_left + v.col as f32 * icw,
                    body_top + vrow as f32 * ich,
                    v.cols as f32 * icw,
                    v.rows as f32 * ich,
                    clip_y0,
                    clip_y1,
                    false,
                ));
            }
        }
        // 글 흐름 안 그림 — `[[img:<경로>:<행수>]]` 표식이 잡은 자리에 얹는다.
        // OSC 1337 을 못 쓰는 claude pane 을 위한 길이라(그쪽 함수 주석) 셸
        // pane 에도 그대로 열어 둔다: `echo '[[img:a.png:12]]'` 로도 뜬다.
        {
            let blocks = find_image_blocks(&composed);
            if !blocks.is_empty() {
                let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
                let (icw, ich) = (self.cell.w * fs, self.cell.h * fs);
                let clip_y0 = body_top;
                let clip_y1 = body_top + rows_now as f32 * ich;
                for b in &blocks {
                    blank_image_block(&mut composed, b);
                    let h = b.rows as f32 * ich;
                    // 가로는 3:1 까지만 벌린다. 박스를 pane 폭으로 두면
                    // 넓은 창에서 그림이 한가운데로 밀려(contain-fit 은 중앙
                    // 정렬) 글 흐름에서 떨어져 보인다. 스크린샷 대부분이
                    // 16:9(1.78) 라 이 안에 들어 왼쪽에서 시작한다.
                    let w = (h * 3.0).min(cols_now as f32 * icw);
                    inline_slots.push((
                        format!("mdimg:{tab_pid}:{}", b.path),
                        b.path.clone(),
                        body_left,
                        body_top + b.row as f32 * ich,
                        w,
                        h,
                        clip_y0,
                        clip_y1,
                        true,
                    ));
                }
            }
        }
        // `[Image #N]` 위에 멎은 커서. 셀 역산은 이 pane 의 원점·폰트배율로
        // 하고 행·열 범위로 잘라 낸다 — 옆 pane 위의 커서는 이 pane 의 셀
        // 범위를 넘어서므로 여기서 걸러진다. 참조 탐색은 커서가 이 pane 의
        // 셀 안에 있을 때만 돈다(그리드 전수 스캔이라 매 프레임 모든 pane
        // 에 돌릴 일이 아니다).
        // 게이트가 claude 여부가 아니라 **세션이 묶였나**인 이유: 그림은
        // 그 세션의 transcript 에만 있어서, sid 가 없으면 찾아 봐야 없다.
        if desktop_view && tip_hit.is_none() && self.pane_claude_sid.contains_key(id.as_str()) {
            let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
            let (icw, ich) = (self.cell.w * fs, self.cell.h * fs);
            let (rx, ry) = (self.cursor_px.0 - body_left, self.cursor_px.1 - body_top);
            if rx >= 0.0 && ry >= 0.0 && icw > 0.0 && ich > 0.0 {
                let (cc, cr) = ((rx / icw) as usize, (ry / ich) as usize);
                if composed.get(cr).is_some_and(|row| cc < row.len()) {
                    if let Some(r) = find_image_refs(&composed)
                        .into_iter()
                        .find(|r| r.row == cr && (r.col0..=r.col1).contains(&cc))
                    {
                        tip_hit = Some((
                            id.clone(),
                            r.n,
                            (
                                body_left + r.col0 as f32 * icw,
                                body_top + r.row as f32 * ich,
                                (r.col1 - r.col0 + 1) as f32 * icw,
                                ich,
                            ),
                        ));
                    }
                }
            }
        }
        // Claude Code 스크롤 sticky prompt → 웹뷰풍 pill. mouse-tracking
        // 중이라 뷰포트 스크롤 여부를 직접 못 안다 — "Jump to bottom" 힌트로
        // 게이트한다(find_sticky_prompt). 감지 행 셀은 스냅샷에서 blank 처리해
        // 원본 흐릿한 텍스트를 지우고, 그 자리에 pill 을 얹는다. 클릭 rect 는
        // 아래 chrome 패스에서 STICKY_PILLS 로 mouse handler 에 넘긴다.
        //
        // pill 은 **프롬프트 띠 재도색과 같은 테마 스타일**로 칠한다 — 흰
        // pill 은 없애기로 했다(거노 2026-08-19: "흰색없애기로했었는데 클릭은
        // 되게하면서"). 08-15 재도색이 흰 pill 을 덮으면서 pill 이 박아 둔
        // 검은 글자만 남아 줄이 통째로 안 보였는데, 그 답은 흰색 복원이
        // 아니라 pill 자체를 테마 띠로 그리는 것이다. 클릭 rect·↑↓·seek 는
        // 색과 무관하게 그대로 산다.
        //
        // 재도색 스캔은 이 행을 건너뛴다 — pill 이 여기서 fg 까지 완성하므로
        // (재도색은 fg 를 ❯ 만 만진다) 다시 칠하면 이 선명화가 무너진다.
        let mut sticky_pill_row: Option<usize> = None;
        // 게이트를 먼저 본다. 열렸을 때만 프롬프트 목록을 깊게 다시 읽게
        // 표시를 남긴다 — 512KB 꼬리에는 일하던 창의 질문이 하나밖에 안
        // 들어가서, 그대로 두면 띠가 늘 그 하나를 그린다. 평상시(닫힌
        // 게이트)에는 이 줄에서 끝나므로 매 프레임 비용은 종전과 같다.
        // 화면 안내가 정본이고, 방금 우리가 넘긴 위쪽 스크롤이 그것이 그려지기
        // 전까지의 빈틈을 메운다(`scroll_forwarded_recently` 주석 참조).
        let scrolled = crate::screenread::scrolled_gate(&composed)
            || (desktop_view && crate::screenread::scroll_forwarded_recently(id.as_str()));
        let web_sticky =
            std::cell::RefCell::new(std::collections::HashMap::<String, String>::new());
        let sticky_turn = if desktop_view {
            &self.pane_sticky_turn
        } else {
            &web_sticky
        };
        if scrolled && desktop_view {
            // seek 이 「더 올라갈 데가 없다」를 알아채려면 화면이 움직였는지를
            // 알아야 한다 — 그 유일한 단서를 여기서 적는다.
            crate::screenread::note_sticky_view(id.as_str(), &composed);
            self.pane_deep_want.borrow_mut().insert(id.to_string());
        } else {
            // 맨 아래로 돌아왔으면 기억을 버린다 — 다음에 올려다볼 때는
            // 그때 본 머리줄로 새로 확정해야 한다.
            sticky_turn.borrow_mut().remove(id.as_str());
        }
        let sticky = scrolled.then(|| {
            let mut memo = sticky_turn.borrow_mut();
            let slot = memo.entry(id.to_string()).or_default();
            let mut cur = (!slot.is_empty()).then(|| slot.clone());
            let got = find_sticky_prompt(&composed, self.pane_prompts(id.as_str()), &mut cur);
            *slot = cur.unwrap_or_default();
            got
        });
        // 목적지에 도착했으면 띠를 감춘다 — 실물이 화면에 있으면 띠는 할
        // 일을 마쳤고, 같은 문장이 두 번 뜨면 어느 쪽이 지금 자리인지 되레
        // 헷갈린다(2026-09-05 지시: 「길잡이 띠 없이 코덱스처럼 딱 붙게」).
        // 띠가 빠지면 그 줄이 화면 맨 위에 딱 붙는다.
        let sticky = sticky
            .flatten()
            .filter(|s| !crate::screenread::sticky_prompt_visible_below(&composed, s.row, &s.text));
        if let Some(sticky) = sticky {
            let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
            let scw = self.cell.w * fs;
            let sch = self.cell.h * fs;
            let ncols = composed.get(sticky.row).map_or(0, |r| r.len());
            sticky_pill_row = Some(sticky.row);
            // 흰 배경 pill 을 pane 양끝(col 0..ncols)까지 채운다(거노: "흰색
            // 바탕 pane 양끝으로 다 채워"). 클릭 rect 도 행 전체 폭 — 흰 바탕
            // 어디를 눌러도 seek(begin_sticky_seek)가 걸린다.
            let px = body_left;
            let py = body_top + sticky.row as f32 * sch;
            let arrow_rect = |c: usize| {
                // 한 칸은 손가락으로 누르기 좁다 — 좌우 반 칸씩 넓혀 잡는다.
                (px + c as f32 * scw - scw * 0.5, py, scw * 2.0, sch)
            };
            let (a_up, a_down) = crate::turnjump::sticky_arrow_cols(ncols);
            sticky_pill_slots.push((
                px,
                py,
                ncols as f32 * scw,
                sch,
                sticky.text.clone(),
                id.clone(),
                a_up.map(arrow_rect),
                a_down.map(arrow_rect),
            ));
            if let Some(row) = composed.get_mut(sticky.row) {
                // 원본 셀(등폭 그리드)을 지우지 않고 그 자리에서 선명화만
                // 한다 — draw_text(proportional)로 다시 그리던 옛 방식은
                // 한글 wide glyph 를 ink 폭으로 tighten 해 자간이 어긋났다
                // (거노: "딱 안 맞아 자간 이상"). 그리드 셀은 등폭이라
                // 폭·자간이 원본과 정확히 일치한다.
                //
                // 색은 프롬프트 띠 재도색과 **같은 공식**(테마 배경에 학생
                // accent 를 살짝 섞은 fill + accent ❯) — sticky 는 「내가
                // 친 프롬프트 줄」의 대리이니 같은 시각 언어여야 하고, 흰
                // pill 은 없애기로 했다(위 주석). 본문 글자만 테마 텍스트색
                // 으로 밝힌다 — claude 원본은 흐릿한 회색이라 띠 위에서
                // 안 읽힌다. ↑↓(앞뒤 질문 건너뛰기)는 accent 로 세워 이
                // 줄이 일반 띠가 아니라 조작 가능한 pill 임을 말한다.
                // ⚠️`pane.character` 로 폴백하지 마라 — 그 필드는 매 화면
                // 업데이트마다 `ws.pane_character` 를 **날것으로 복사**한
                // 것이라(session.rs), 폴백하는 순간 바로 윗줄에서 지난
                // 관문이 무효가 된다(2026-08-22). 셸 pane 에 남의 학생색이
                // 둘리면 「저기 누가 있다」로 잘못 읽힌다(pane_accent 주석).
                let accent = self
                    .display_tab_char(&ws, &tab_pid)
                    .as_deref()
                    .and_then(|n| {
                        theme::character_accent_n(
                            n,
                            theme::character_ordinal(&ws.pane_character, &tab_pid),
                        )
                    })
                    .unwrap_or_else(|| theme::accent_color(theme::accent_name()));
                let base = theme::bg();
                let light = base[0] as u16 + base[1] as u16 + base[2] as u16 > 380;
                let amount = if light { 0.10 } else { 0.18 };
                let fill = tint_toward([base[0], base[1], base[2]], accent, amount);
                let text = theme::text();
                let (up_col, down_col) = crate::turnjump::sticky_arrow_cols(row.len());
                // 그 질문 줄이 화면에 그려져 있으면 **셀을 그대로 옮긴다.**
                // 눌러서 그 줄이 맨 위에 서면 띠와 본문이 같은 그림이라 딱
                // 겹친다(2026-09-03 지시: 「코덱스처럼 딱붙게 클로드도」).
                // 다시 칠하면 같은 질문이 두 모양으로 보인다.
                if let Some(src) = &sticky.cells {
                    let blank = kasa_bridge::screen::Cell::blank();
                    for c in row.iter_mut() {
                        *c = blank.clone();
                    }
                    for (dst, s) in row.iter_mut().zip(src.iter()) {
                        *dst = s.clone();
                    }
                    // 화살표는 그 칸 배경을 그대로 두고 글자만 얹는다.
                    for (at, ch) in [(up_col, '\u{2191}'), (down_col, '\u{2193}')] {
                        if let Some(i) = at {
                            if let Some(c) = row.get_mut(i) {
                                c.ch = ch;
                                c.fg = kasa_bridge::screen::Color::Rgb(
                                    accent[0], accent[1], accent[2],
                                );
                                c.bold = true;
                                c.dim = false;
                            }
                        }
                    }
                } else {
                    // 질문이 화면 밖이라 옮겨 올 셀이 없다 — 그때만 우리가 칠한다.
                    use unicode_width::UnicodeWidthChar;
                    let room = up_col.unwrap_or(row.len()).saturating_sub(2);
                    for cell in row.iter_mut() {
                        cell.ch = ' ';
                    }
                    let mut w = 1usize; // 왼쪽 한 칸 들여쓴다
                    for ch in format!("\u{276f} {}", sticky.text).chars() {
                        let cw = ch.width().unwrap_or(1).max(1);
                        if w + cw > room {
                            break;
                        }
                        row[w].ch = ch;
                        // wide 글리프의 뒤칸은 **공백 셀**로 둔다 — 이 레포의
                        // 셀 표현 관례다(`paint_header_row` 와 같은 모양).
                        if cw == 2 {
                            row[w + 1].ch = ' ';
                        }
                        w += cw;
                    }
                    for (i, cell) in row.iter_mut().enumerate() {
                        cell.dim = false;
                        cell.inverse = false;
                        cell.bg = fill.clone();
                        cell.fg =
                            if cell.ch == '\u{276f}' || Some(i) == up_col || Some(i) == down_col {
                                kasa_bridge::screen::Color::Rgb(accent[0], accent[1], accent[2])
                            } else {
                                kasa_bridge::screen::Color::Rgb(text[0], text[1], text[2])
                            };
                        if Some(i) == up_col {
                            cell.ch = '\u{2191}';
                            cell.bold = true;
                            continue;
                        }
                        if Some(i) == down_col {
                            cell.ch = '\u{2193}';
                            cell.bold = true;
                        }
                    }
                }
            }
        }
        // 대화 턴 헤더 — 터미널 스크롤백을 올려다볼 때만 첫 행을 덮어쓴다.
        // 라이브 바닥이면 `turn_headers` 에 항목 자체가 없어서 평소 화면은
        // 손대지 않는다. 바로 위 sticky pill 과 자리를 다툴 일은 없다 —
        // 저쪽은 claude 가 **자기 버퍼를** 스크롤할 때뿐이고, 그때 터미널
        // 쪽 offset 은 0 이라 이 헤더가 아예 안 뜬다.
        if let Some(h) = turn_headers.get(id.as_str()) {
            let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
            let (hcw, hch) = (self.cell.w * fs, self.cell.h * fs);
            if let Some(row) = composed.get_mut(0) {
                let cols = crate::turnjump::paint_header_row(row, h);
                let rect_at = |c: usize| {
                    // 화살표 한 칸은 손가락으로 누르기엔 좁다 — 좌우로 반 칸씩
                    // 넓혀 잡는다. 그래도 서로 두 칸 떨어져 있어 안 겹친다.
                    (
                        body_left + c as f32 * hcw - hcw * 0.5,
                        body_top,
                        hcw * 2.0,
                        hch,
                    )
                };
                turn_header_slots.push((
                    id.clone(),
                    (body_left, body_top, cols_now as f32 * hcw, hch),
                    cols.up.map(rect_at),
                    cols.down.map(rect_at),
                    h.clone(),
                ));
            }
        }
        // agents 목록 뷰 판정. 목록 뷰면 아래 학생 스프라이트(배너·스피너·
        // standing)·본문 틴트를 모두 건너뛰고 SCHALE 조직 정체성(타이틀·
        // 테두리)만 준다.
        //
        // 화면 신호(`screen_is_agents_list`)가 정본이고 statusline 프사 슬롯
        // (U+FFFC) 유무는 보지 않는다. 세션 **안에서** `← for agents` 로 여는
        // 목록은 맨 아래 statusline 한 칸이 남아, 예전처럼 `!has_profile_slot`
        // 을 AND 로 걸면 화면 신호가 잡혀도 판정이 꺼졌다 → 학생 그림이 목록
        // 위에 그대로 그려져 내용을 덮었다(거노 2026-08-20 「claude agents 치면
        // 사진이 내용을 다가려」).
        //
        // argv(`is_claude_agents`)는 화면 신호가 아직 안 그려진 프레임을 메우는
        // 폴백으로만 남는다. 이쪽엔 `!has_profile_slot` 이 여전히 필요하다 —
        // argv 는 목록에서 세션으로 **진입해도 그대로 agents** 라, 그 조건이
        // 없으면 대화 화면까지 관리 화면으로 오인해 학생 표시가 영영 안 돌아온다.
        let has_profile_slot = composed
            .iter()
            .any(|row| row.iter().any(|c| c.ch == '\u{fffc}'));
        let agents_view = screen_is_agents_list(&composed)
            || (!has_profile_slot
                && self
                    .pty
                    .get(id.as_str())
                    .map(|p| p.is_claude_agents())
                    .unwrap_or(false));
        if agents_view {
            agents_view_panes.insert(id.clone());
            // 관리 화면 = SCHALE 조직 정체성. claude 캐릭터(Clawd) 자리에 SCHALE
            // 로고를 얹는다(거노: 그 자리가 비어 보임). Clawd 블록아트가 있으면 그
            // 자리를 지우고 동일 위치에, 없으면(agents 목록) "Claude Code" 헤더
            // 왼쪽 여백에 앵커한다. 로고는 정사각이라 폭을 셀 비율로 맞춘다.
            let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
            let scw = self.cell.w * fs;
            let sch = self.cell.h * fs;
            let logo_rows = CLAWD_ROWS;
            let logo_cols = ((logo_rows as f32 * sch / scw).round() as usize).max(3);
            // SCHALE 로고는 클립 경로가 없어 완전 노출 배너만 쓴다 —
            // 스크롤로 잘린 배너는 헤더 앵커 폴백(원본 글리프 유지).
            let clawd = find_clawd_banners(&composed);
            let anchor = clawd
                .iter()
                .find(|&&(br, _)| br >= 0 && br as usize + CLAWD_ROWS <= composed.len())
                .map(|&(br, bc)| (br as usize, bc));
            let anchor = if let Some((br, bc)) = anchor {
                for row in composed[br..br + CLAWD_ROWS].iter_mut() {
                    for cell in row.iter_mut().skip(bc).take(CLAWD_COLS) {
                        *cell = GridCell::blank();
                    }
                }
                Some((br, bc))
            } else {
                find_agents_header_anchor(&composed, logo_cols)
            };
            if let Some((br, bc)) = anchor {
                schale_logo_slots.push((
                    body_left + bc as f32 * scw,
                    body_top + br as f32 * sch,
                    logo_cols as f32 * scw,
                    logo_rows as f32 * sch,
                ));
            }
        }
        // Claude Code 시작 배너의 Clawd 아트 → 이 pane 학생의 도트로.
        // 학생 배정 pane(=claude 용도로 spawn된 pane)만 스캔한다.
        // 감지된 셀은 스냅샷에서 blank 처리해 자리를 비우고, 그
        // 자리에 도트 이미지를 queue한다 — 이미지 패스는 셀/chrome
        // 보다 먼저 그려지므로 비워진 셀 밑으로 도트가 보인다.
        // "터미널은 파싱만"(거노): claude sessionId 바인딩 우선, 뷰 pane 은
        // 파싱 전 스폰 랜덤 미표시 — display_pane_char(chrome.rs)가 규칙 정본.
        let true_char = self.display_tab_char(&ws, &tab_pid);
        if let Some((name, slug)) = true_char
            .as_deref()
            .filter(|_| !agents_view && runs_claude)
            // **활성 밖까지**(`_any`) — 이 `if let` 이 프사·테두리·스피너 색을
            // 통째로 지고 있어서, 좁은 조회로 None 이 되면 다른 테마 학생으로
            // 바꾼 pane 은 그 셋이 전부 안 그려진다(2026-08-25: star-rail 로
            // 바꾼 %12 가 「미니맵 인포는 바뀌었는데 스피너·색은 앞 학생」이었다).
            .and_then(|n| theme::character_slug_any(n).map(|s| (n, s)))
        {
            // 같은 학생 pane 이 여럿이면(지정 스폰 중복 허용) 순번 변주색.
            //
            // 단 **연결이 끊겨 멈췄으면 빨강이 이긴다**. 학생색은 「누구의
            // 자리인가」를 말하지만 멈춘 자리에서 그건 급한 정보가 아니고,
            // 테두리·프사·스피너가 한 색을 쓰므로 여기 한 번만 덮으면 셋이
            // 함께 빨개진다(2026-08-26 지시).
            let stalled = self
                .pane_activity
                .get(id.as_str())
                .is_some_and(|a| a.stalled.is_some());
            let accent = if stalled {
                Some(theme::danger())
            } else {
                theme::character_accent_n(name, theme::character_ordinal(&ws.pane_character, &id))
            };
            let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
            let scw = self.cell.w * fs;
            let sch = self.cell.h * fs;
            // 그림이 없는 학생은 배너를 건드리지 않는다 — 지운 뒤 못
            // 그리면 원래 있던 Clawd 배너까지 사라진다.
            // claude 의 Clawd 아트와 agy 의 Antigravity 로고는 모양도 크기도
            // 다르다. 어느 하네스로 떴는지 따지지 않고 둘 다 훑는다 — 화면에
            // 실제로 그려진 로고가 정본이고, 한 pane 에 둘이 함께 뜰 일은 없다.
            compose_student_banners(
                &mut composed,
                name,
                slug,
                accent,
                body_left,
                body_top,
                scw,
                sch,
                &mut banner_slots,
            );
            // codex 시작 패널: 세울 아트가 없어 이름표만 바꾼다. 도트 유무와
            // 무관하니 위 로고 루프 밖이고, codex pane 일 때만 훑는다 —
            // 배너와 달리 화면 전체를 봐야 해서 공짜가 아니다.
            if agent_kind == Some(kasa_pty::AgentKind::Codex) {
                let n = composed.len();
                replace_banner_title(&mut composed, 0, 0, 0, n, CODEX_TITLE, name, accent);
            }
            // working 스피너 자리 → 학생이 제자리 걸음으로 "작업 중".
            // 스피너 글리프 셀은 스냅샷에서 비우고, 그 자리(스피너 행
            // 바닥 정렬, 2행 높이)에 walk 도트를 icon 패스로 얹는다.
            // 스피너가 없고 승인 프롬프트가 떠 있으면 → 질문 행 텍스트
            // 끝 옆에서 폴짝 바운스("선생님, 승인 기다려요!"). pane
            // 우상단은 collab 승인 토스트(윈도우 우상단)와 겹친다.
            // 스피너 walk·승인대기 바운스가 뜨는 동안은 standing 도트를
            // 숨긴다 — 같은 학생이 화면에 두 명 서 있으면 버그로 보인다.
            let mut pet_busy = false;
            // 본판정 + 프로브 확정 후보(턴 시작 첫 ~3초의 괄호 없는
            // `✢ Transmuting…`). 확정은 refresh_pane_activity 가 글리프
            // 변화로 세운다 — 여기서는 읽기만.
            // 프로브 확정 전이라도 이 pane 에 방금 제출(Enter)이 있었으면
            // 후보를 그 프레임부터 신뢰한다 — refresh 틱(100~300ms)을
            // 기다리는 동안 claude 원색 스피너가 그대로 보이던 마지막
            // 깜빡임 조각(거노 2026-08-20 「치자마자 0.1초동안
            // 적용안되는거」). runs_claude 게이트 안이라 셸 출력 오탐
            // 걱정은 없다.
            let spinner_hit = find_claude_spinner(&composed).or_else(|| {
                let trusted = self
                    .spinner_probe
                    .get(tab_pid.as_str())
                    .is_some_and(|&(_, _, confirmed, _)| confirmed)
                    || self
                        .pty
                        .get(tab_pid.as_str())
                        .and_then(|p| p.last_submit())
                        .is_some_and(|s| s.elapsed() < Self::SUBMIT_TRUST);
                trusted
                    .then(|| unconfirmed_spinner_row(&composed))
                    .flatten()
                    .map(|(r, c, _)| (r, c))
                    .or_else(|| {
                        // 박동 확정 pane 은 글리프 집합 없이 위치만 잡는다
                        // (lenient_spinner_row 머리말) — 스피너 모양이 또
                        // 바뀌어도 도트·테마가 함께 죽지 않게.
                        self.pty
                            .get(tab_pid.as_str())
                            .is_some_and(|p| p.output_heartbeat_fresh())
                            .then(|| lenient_spinner_row(&composed))
                            .flatten()
                    })
            });
            if let Some((sr, sc)) = spinner_hit {
                pet_busy = true;
                // 스피너 행 텍스트("Cerebrating… · esc to interrupt")를
                // 학생 accent 색으로 — walk 도트 + 텍스트색이 함께
                // "이 학생이 작업 중"임을 말한다. 여기에 glow shimmer:
                // accent 위로 밝은 밴드가 좌→우로 흐른다(claude code 의
                // 반짝이는 텍스트). 밴드 중심은 시간에 따라 이동하고 각
                // 셀은 중심과의 거리(가우시안)만큼 흰색에 lerp 된다.
                // working 중엔 walk 애니 33ms 펌프가 재렌더를 이미 돌려
                // 애니 비용이 추가로 들지 않는다.
                if let Some(a) = accent {
                    animated_cells = true;
                    use kasa_bridge::screen::Color;
                    let t = self.version_anim_start.elapsed().as_secs_f32();
                    let row = &composed[sr];
                    // glow/색은 동사 문구("Cerebrating…")까지만 — 뒤의
                    // "(esc to interrupt · N tokens)" 는 원래 dim 색을 둔다
                    // (거노: 문구만 glow). 줄임표(…) 다음을 경계로, 없으면
                    // "(" 앞, 그것도 없으면 행 끝.
                    let end = row
                        .iter()
                        .position(|c| c.ch == '…')
                        .map(|p| p + 1)
                        .or_else(|| row.iter().position(|c| c.ch == '('))
                        .unwrap_or(row.len());
                    let first = row
                        .iter()
                        .take(end)
                        .position(|c| !matches!(c.ch, ' ' | '\0'))
                        .unwrap_or(0);
                    let lastc = row
                        .iter()
                        .take(end)
                        .rposition(|c| !matches!(c.ch, ' ' | '\0'))
                        .unwrap_or(first);
                    let span = lastc.saturating_sub(first).max(1) as f32;
                    const PERIOD: f32 = 2.0; // 한 번 스윕(초)
                    const SIGMA: f32 = 2.0; // 밴드 폭(셀)
                    const GLOW: f32 = 0.9; // 밴드 중심 밝기(흰색 비율)
                                           // 밴드가 문구 왼쪽 밖에서 오른쪽 밖으로 완전히 지나가게.
                    let sweep = (t / PERIOD).fract();
                    let center = first as f32 - SIGMA * 2.0 + sweep * (span + SIGMA * 4.0);
                    for (idx, cell) in composed[sr].iter_mut().enumerate().take(end) {
                        if matches!(cell.ch, ' ' | '\0') {
                            continue;
                        }
                        let d = idx as f32 - center;
                        let g = (-(d * d) / (2.0 * SIGMA * SIGMA)).exp() * GLOW;
                        let mix = |b: u8| (b as f32 + (255.0 - b as f32) * g).round() as u8;
                        cell.fg = Color::Rgb(mix(a[0]), mix(a[1]), mix(a[2]));
                    }
                    // 꼬리("(49s · thinking some more…)")도 학생 색 언어로 —
                    // glow 는 여전히 문구까지만(거노: 문구만 glow)이고, 꼬리는
                    // accent 를 테마 배경에 눕힌 차분한 톤. claude 가 제 주황을
                    // 남겨 두면 학생색 줄 한가운데 남의 색이 선다(2026-08-16
                    // 「almost done thinking 같은 거도 색 바꿔줘」).
                    let bg = theme::bg();
                    let tail = crate::screenread::tint_toward(
                        [bg[0], bg[1], bg[2]],
                        [a[0], a[1], a[2], 255],
                        0.6,
                    );
                    for cell in composed[sr].iter_mut().skip(end) {
                        if matches!(cell.ch, ' ' | '\0') {
                            continue;
                        }
                        cell.fg = tail.clone();
                    }
                }
                // 스피너 글리프를 지우는 건 그 자리에 학생을 세울 수
                // 있을 때만. 못 세우면 도는 표시가 통째로 없어진다.
                if student_has_sprite(slug, "walk") {
                    composed[sr][sc] = GridCell::blank();
                    let top_r = sr.saturating_sub(1);
                    spinner_slots.push((
                        slug,
                        (
                            body_left + sc as f32 * scw,
                            body_top + top_r as f32 * sch,
                            2.0 * scw,
                            (sr - top_r + 1) as f32 * sch,
                        ),
                    ));
                }
            } else if !crate::input::rows_show_working(&composed)
                && crate::input::rows_show_approval_prompt(&composed).is_some()
            {
                if let Some((ar, ac)) = approval_anchor(&composed) {
                    pet_busy = true;
                    const DOT: f32 = 40.0;
                    if student_has_sprite(slug, "wave") {
                        let pane_w = cols_now as f32 * scw;
                        let pane_h = rows_now as f32 * sch;
                        let dot = DOT.min(pane_w).min(pane_h);
                        if dot >= scw.min(sch) {
                            let x = (body_left + (ac + 2) as f32 * scw)
                                .clamp(body_left, body_left + pane_w - dot);
                            let y = (body_top + (ar + 1) as f32 * sch - dot)
                                .clamp(body_top, body_top + pane_h - dot);
                            waiting_slots.push((slug, (x, y, dot, dot)));
                        }
                    }
                }
            }
            // 입력창 위 standing 앵커. claude 는 statusline 표식(U+FFFC)에서
            // 출발하지만 codex 는 그게 없어(위 `find_filled_standing_anchor`
            // 주석) 입력행에서 바로 잡는다. 둘 다 못 잡으면 안 세운다.
            let mut stand_anchor: Option<(usize, f32)> = None;
            if let Some((sr, sc, len)) = find_statusline_face(&composed) {
                for cell in composed[sr].iter_mut().skip(sc).take(len) {
                    *cell = GridCell::blank();
                }
                // 프사는 여기 안 그린다(거노 2026-08-11: "클로드코드 상태줄
                // 학생프사는 없애자"). statusline 은 이제 `● 이름` 을 직접
                // 찍고, 남은 U+FFFC 한 칸은 **신호**다 — 위 blank 로 지우고
                // `sr` 만 standing 앵커로 쓴다. 자리표시자를 아예 없애면
                // agents 뷰 판정(`has_profile_slot`)·stale statusline 복구
                // (socket.rs)·이 앵커가 한꺼번에 죽는다.
                let _ = (sc, len);
                // 입력박스 위에 서 있는 학생(전신 idle) — 프롬프트 위
                // 스페이서 행(effort 칩·context 경고가 뜨는 자리) 우측.
                // statusline 바로 위 행이 아래 테두리(전폭 '─')면 그
                // 위로 첫 '─' 행이 입력박스 윗 테두리다 — ❯ 영역이
                // 여러 줄로 자라도 스캔이라 따라간다. 발은 윗 테두리
                // 줄에 닿고, 칩이 떠 있으면 그 왼쪽으로 비켜 선다.
                // working/승인대기 중엔 스피너 walk·바운스 도트가 이미
                // 학생을 그리므로(pet_busy) 세우지 않는다. 앵커 규칙은
                // 앵커 규칙은 `find_standing_anchor` 한 곳에 둔다.
                // `KASATERM_STUDENT_DEBUG=1` — 왜 학생이 안 서는지 앱이
                // 직접 말한다. 이 자리는 조건 셋(스피너 감지·pet_busy·앵커)이
                // 겹쳐 있고 실패하면 **아무것도 안 그려** 밖에서 원인을 가릴
                // 수 없다. 정적 스프라이트(프사)만 뜨고 애니가 안 뜬다는
                // 신고를 받고도 코드 읽기로는 못 좁혔다(2026-08-05).
                if std::env::var_os("KASATERM_STUDENT_DEBUG").is_some() {
                    use std::sync::{Mutex, OnceLock};
                    static LAST: OnceLock<Mutex<std::time::Instant>> = OnceLock::new();
                    let last = LAST.get_or_init(|| Mutex::new(std::time::Instant::now()));
                    let mut g = last.lock().unwrap();
                    if g.elapsed() >= std::time::Duration::from_millis(1000) {
                        *g = std::time::Instant::now();
                        let a = find_standing_anchor(&composed, sr, cols_now as usize);
                        eprintln!(
                            "[student-debug] pane={id} slug={slug} face_row={sr} cols={cols_now} pet_busy={pet_busy} spinner={} anchor={a:?} rows={}",
                            find_claude_spinner(&composed).is_some(),
                            composed.len(),
                        );
                        if sr >= 4 {
                            let rule = |r: usize| {
                                let row = &composed[r];
                                let d = row.iter().filter(|c| c.ch == '─').count();
                                let l = row
                                    .iter()
                                    .filter(|c| !matches!(c.ch, '─' | ' ' | '\0'))
                                    .count();
                                format!("dash={d}/{} label={l}", row.len())
                            };
                            eprintln!(
                                "[student-debug]   아래테두리 rows[{}] {}",
                                sr - 1,
                                rule(sr - 1)
                            );
                        }
                    }
                }
                stand_anchor = find_standing_anchor(&composed, sr, cols_now as usize);
            }
            // statusline 자리표시자가 없는 하네스(codex) — 입력행에서 바로.
            if stand_anchor.is_none() {
                stand_anchor = find_filled_standing_anchor(&composed, cols_now as usize);
            }
            // agy — 자리표시자도 채운 입력행도 없다. 모양은 claude 와 같은
            // 대시 보더 두 줄인데 마커가 ASCII `>` 라 공용 판정에서 빠져 있다
            // (인용문·diff 오인 방지, 2026-07-22). 앵커 규칙은 그대로 쓰고
            // statusline 자리만 「맨 아래 보더 다음 행」으로 잡아 준다.
            if stand_anchor.is_none() {
                stand_anchor = find_agy_standing_anchor(&composed, cols_now as usize);
            }
            {
                if !pet_busy {
                    if let Some((anchor, left_c)) = stand_anchor {
                        {
                            // 턴 완료 직후 ~1.8s(notify_flash)는 양팔 만세
                            // cheer, 그 뒤로 계속 대기하면 손 흔들며 기다리는
                            // wave("선생님, 다음 지시 기다려요"). 학생 pane 은
                            // bypass 모드라 승인 프롬프트가 안 떠 우상단 wave
                            // 트리거가 사실상 죽어 있다 — wave 를 standing 순환에
                            // 넣어야 "입력 기다림"이 보인다. 사용자가 이 pane 에
                            // 타이핑하면 idle 로.
                            let motion = if self.turn_done_panes.contains(id) {
                                if self.notify_flash_factor(&id).is_some() {
                                    "cheer"
                                } else {
                                    "wave"
                                }
                            } else {
                                "idle"
                            };
                            if student_has_sprite(slug, motion) {
                                standing_slots.push((
                                    slug,
                                    motion,
                                    standing_slot_rect(
                                        anchor,
                                        left_c,
                                        rows_now,
                                        (body_left, body_top),
                                        (scw, sch),
                                    ),
                                ));
                            }
                        }
                    }
                }
            }
        }
        // ` ultracode ` 배지는 지운다 — 모드는 입력박스 글로우가 이미
        // 말하므로 글자는 중복이고, 그 자리는 /rename 세션명 자리라 이름이
        // 바뀐 것처럼 읽힌다(2026-08-12 지적 「/rename 그자리에 ultracode
        // 써진다」). find_titled_rule 보다 먼저 — 지운 뒤엔 순수 rule 이다.
        erase_ultracode_badge(&mut composed);
        // /rename 세션명 아웃라인 — claude 입력박스 위 "── 세션명 ──" 구분선의
        // 이름 텍스트 섬을 찾아 그 셀 범위를 rename/학생 색 사각 테두리로 두른다
        // (거노). 순수 '─' rule·statusline·입력행은 걸러진다. 테두리 패스에서 소비.
        if let Some((tr, c0, c1)) = find_titled_rule(&composed) {
            let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
            let scw = self.cell.w * fs;
            let sch = self.cell.h * fs;
            // 가로는 셀 경계 딱 맞게(대시와 안 겹침, 이름 양옆 공백이 패딩 역할),
            // 세로만 살짝 여백.
            let pad_x = 0.0;
            let pad_y = 2.0;
            let col = pane
                .color
                .or_else(|| {
                    // `true_char` 하나만 쓴다. `pane.character` 폴백이 있던
                    // 동안 둘이 어긋났다 — 그 필드는 pane 단위라 탭이 둘이면
                    // **마지막에 출력한 탭**이 이기고, 게다가 관문을 안 지난
                    // 날것이라(session.rs 가 매 업데이트 복사) 셸 pane 에도
                    // 색이 둘렸다(2026-08-22).
                    true_char.as_deref().and_then(|n| {
                        theme::character_accent_n(
                            n,
                            theme::character_ordinal(&ws.pane_character, &tab_pid),
                        )
                    })
                })
                .unwrap_or_else(theme::border);
            // ultracode pane 은 이 테두리도 입력박스 보더와 같은 위상으로
            // 숨쉰다(2026-08-17 「리네임되는 부분 테두리도 같이 숨쉬기되게」).
            let col = if self.pane_ultracode.contains(&tab_pid) {
                ultracode_breath(Some(col), self.version_anim_start.elapsed().as_secs_f32())
            } else {
                col
            };
            title_outline_slots.push((
                body_left + c0 as f32 * scw - pad_x,
                body_top + tr as f32 * sch - pad_y,
                (c1 - c0 + 1) as f32 * scw + pad_x * 2.0,
                sch + pad_y * 2.0,
                col,
            ));
        }
        // /resume 피커 학생 프사 — 스위퍼(resume_visibility)가 세션 행
        // 설명줄 끝에 스탬프한 ` · #학생이름` 태그를 지우고 그 자리에
        // 프사(bust)를 얹는다(거노: 이름 말고 프사). 세션 행 아래는
        // 구분 빈 줄이라 2행 키로 아래로 내려 그린다. pane 학생과
        // 무관하게 행마다 태그된 학생의 얼굴 — profile_slots(statusline
        // 프사와 같은 이미지 패스)로 소비된다.
        {
            let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
            let scw = self.cell.w * fs;
            let sch = self.cell.h * fs;
            let rows_n = composed.len();
            let mut faces = 0usize;
            for r in 0..rows_n {
                if faces >= 40 {
                    break; // 폭주 방어 — 화면에 이보다 많을 수 없다
                }
                let Some((c0, end, tag_slug)) = picker_student_tag(&composed[r]) else {
                    continue;
                };
                for cell in composed[r][c0..=end].iter_mut() {
                    *cell = GridCell::blank();
                }
                let row_w = composed[r].len() as f32 * scw;
                let face_w = 4.0 * scw;
                let face_h = 2.0 * sch;
                let x = (body_left + c0 as f32 * scw)
                    .min(body_left + row_w - face_w)
                    .max(body_left);
                // 바닥 정렬(statusline 프사 공식) — 얼굴 발을 설명줄
                // 바닥에 붙이고 위(제목행 끝자락)로 서게. 아래로 내리면
                // 구분 빈 줄에 매달려 다음 세션 것처럼 보인다(거노).
                let y = (body_top + (r + 1) as f32 * sch - face_h).max(body_top);
                profile_slots.push((tag_slug, (x, y, face_w, face_h)));
                faces += 1;
            }
        }
        // agents 목록 뷰(claude agents 피커)의 세션 행에 캐릭터 칩 — resume
        // 피커의 `· #학생` 태그가 없어, 캐시된 세션 name→sid→캐릭터로 역추적
        // 한다(호시노 청사진). 각 행의 실제 텍스트에서 캐시 name 을 substring
        // 검색해 세션 행을 식별(그룹 헤더·빈 줄은 매칭 안 됨), 동명세션은 캐시
        // 에서 이미 드롭돼 스킵된다. 얼굴은 name 시작 셀 왼쪽(마커 자리)에 얹어
        // 세션명은 가리지 않는다. 긴 이름이 …로 잘린 행은 매칭 실패로 스킵.
        if agents_view {
            let name_sids = crate::socket::agents_name_sids_cached();
            if !name_sids.is_empty() {
                let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
                let scw = self.cell.w * fs;
                let sch = self.cell.h * fs;
                let mut faces = 0usize;
                'agents_rows: for r in 0..composed.len() {
                    if faces >= 40 {
                        break;
                    }
                    let (text, cols) = row_text_cells(&composed[r]);
                    let text_chars: Vec<char> = text.chars().collect();
                    for (name, sid) in &name_sids {
                        let name_chars: Vec<char> = name.chars().collect();
                        if name_chars.len() < 2 || name_chars.len() > text_chars.len() {
                            continue;
                        }
                        let Some(cpos) = text_chars
                            .windows(name_chars.len())
                            .position(|w| w == name_chars.as_slice())
                        else {
                            continue;
                        };
                        let Some(slug) = kasa_mcp::character::session_character(sid)
                            .and_then(|c| student_face_slug(&c))
                        else {
                            continue;
                        };
                        let cell_col = cols[cpos];
                        // 이름 왼쪽 여백(불릿·공백)을 지우고 그 자리에 얼굴 —
                        // 세션명 첫 글자와 겹치지 않게 얼굴 오른끝을 이름 시작에
                        // 맞춘다(여백이 좁으면 body_left 로 클램프).
                        let row_len = composed[r].len();
                        for cell in composed[r][..cell_col.min(row_len)].iter_mut() {
                            *cell = GridCell::blank();
                        }
                        // 정사각(누끼 bust 96×96 왜곡 방지). 얼굴은 이름 왼쪽
                        // 여백(불릿·공백)에만 앉혀 세션명을 안 가린다 — 변은
                        // 여백폭에 맞추되(최대 2행) 이름 시작을 넘지 않는다.
                        // 밀집 단행이라 세로는 행 중앙 정렬(투명 여백만 이웃 행에).
                        let name_x = body_left + cell_col as f32 * scw;
                        let side = (name_x - body_left).min(2.0 * sch).max(sch);
                        let x = body_left;
                        let y = (body_top + r as f32 * sch + (sch - side) / 2.0).max(body_top);
                        profile_slots.push((slug, (x, y, side, side)));
                        faces += 1;
                        continue 'agents_rows;
                    }
                }
            }
        }
        // 접힌 팀메시지("› Message from @이름", verbose OFF) — 보낸 학생
        // 색으로 "@ 이름❯ 본문…" 인라인 전개(거노: verbose 안 켜고도
        // 읽고 싶다. 클로드코드에 팀메시지만 펼치는 설정은 없음 —
        // verbosity 카테고리는 bash/agent/todo 뿐이라 그리드 재작성으로).
        // 본문은 이 pane transcript tail 의 <teammate-message> 태그에서.
        // 그리드는 reflow 불가라 접힌 줄 아래 빈 여백(blank_run)에만 다줄
        // 전개 — 최신 메시지(하단, 빈 공간 큼)일수록 더 많이 보인다.
        {
            let msg_path = self.pane_claude_sid.get(id.as_str()).and_then(|sid| {
                let cwd = self
                    .pane_view_cwd
                    .get(id.as_str())
                    .or_else(|| self.pane_cwd_cache.get(id.as_str()))?;
                crate::socket::project_jsonl(cwd, sid)
            });
            // 긴 팀메시지를 스크롤하면 헤더가 화면 위로 나가 아래 본문이
            // 무테마로 남는다(2026-08-24 거노 스샷: 「위에는 적용되는데
            // 밑에는 sm인지 모르니까 적용안되는데」). 화면 첫 행이 wrap
            // 연속이면 스크롤백을 올려다 헤더를 찾아 같은 색으로 잇는다.
            // runs_claude 게이트: 셸 pane 의 들여쓴 출력(로그 등)이 첫 행에
            // 걸릴 때마다 스크롤백을 읽는 낭비·오탐을 막는다.
            if runs_claude
                && (composed.first().is_some_and(|r| tell_wrap_continuation(r))
                    || (!composed.is_empty() && msg_paragraph_gap(&composed, 0)))
            {
                let above = self
                    .pty
                    .get(tab_pid.as_str())
                    .map(|p| {
                        if desktop_view {
                            p.rows_above(240)
                        } else {
                            p.rows_above_live(240)
                        }
                    })
                    .unwrap_or_default();
                let accent = match carried_message_header(&above) {
                    Some(CarriedHeader::Tell(name)) => theme::character_accent_any(&name),
                    Some(CarriedHeader::Native(label)) => {
                        // 인정 규칙은 화면 안 헤더와 동일 — transcript 최신
                        // 태그와 라벨 대조, 아니면 로스터 agent 이름꼴만.
                        let msg = msg_path
                            .as_deref()
                            .and_then(|p| latest_teammate_msg(p, PEER_LABEL));
                        let norm = |s: &str| -> String {
                            s.chars().filter(|c| !c.is_whitespace()).collect()
                        };
                        let label_hit = msg.as_ref().is_some_and(|m| {
                            m.from_label.as_deref().map(&norm) == Some(norm(&label))
                                || m.from_pid.as_deref().map(&norm) == Some(norm(&label))
                        });
                        // 세 번째 관문: 라벨이 사람이 붙인 pane 이름인 경우
                        // (`@ diff❯`) — 명부에 그 이름의 세션이 하나뿐이면 인정.
                        let named = !label_hit
                            && !label_is_roster_agent(&label)
                            && peer_character_by_label(&label, &self.pane_claude_sid, &ws)
                                .is_some();
                        (label_hit || label_is_roster_agent(&label) || named).then(|| {
                            native_sender_accent(
                                &label,
                                label_hit,
                                msg.as_ref(),
                                &self.pane_claude_sid,
                                &ws,
                            )
                            .1
                        })
                    }
                    None => None,
                };
                if let Some(accent) = accent {
                    let mut r = 0;
                    while r < composed.len() {
                        if tell_wrap_continuation(&composed[r]) {
                            tint_row(&mut composed[r], accent);
                            r += 1;
                        } else if msg_paragraph_gap(&composed, r) {
                            r += 1;
                        } else {
                            break;
                        }
                    }
                }
            }
            for r in 0..composed.len() {
                let Some((c0, _count, sender)) = teammate_collapsed_line(&composed[r]) else {
                    continue;
                };
                let msg = msg_path
                    .as_deref()
                    .and_then(|p| latest_teammate_msg(p, &sender));
                // 화면에 `@peer` 로 떴어도 태그에서 진짜 발신자를 찾았으면 그것으로
                // 그린다 — 이름이 바뀌어야 아래 색·프사·전개가 전부 걸린다.
                let sender = msg
                    .as_ref()
                    .and_then(|m| m.sender.clone())
                    .unwrap_or(sender);
                // 그 이름으로도 학생을 못 찾으면 **세션 id 로 pane 을 되짚는다.**
                // 명부의 이름은 세션 제목이라 자동 요약에 덮인다 — 실측으로
                // 모모이 pane 은 `mcp, skill사이드바` 였고, 로스터가 아는 글자가
                // 하나도 없어 색도 프사도 안 걸렸다(거노 2026-08-11: "sm테마는 왜
                // 안됐어"). 앞서 이름 파싱을 고친 것은 이름에 슬러그가 들어 있을
                // 때만 듣는 반쪽이었다. pane 을 되짚으면 제목이 뭐로 바뀌든 맞는다.
                //
                // ⚠️ `pane_character_if_known` 을 부르면 안 된다 — 그 안에서
                // `ws` 를 다시 잠그는데 여기는 이미 그 락 안(557~1788)이라
                // 재진입 데드락이다. 들고 있는 `ws` 를 그대로 쓴다.
                let sender = if teammate_sender_slug(&sender).is_some() {
                    sender
                } else {
                    msg.as_ref()
                        .and_then(|m| m.peer_sid.as_deref())
                        .and_then(|sid| {
                            self.pane_claude_sid
                                .iter()
                                .find(|(_, s)| s.as_str() == sid)
                                .map(|(p, _)| ws.active_tab_pid(p))
                        })
                        .and_then(|key| ws.pane_character.get(&key).cloned())
                        // 발신 프로세스가 죽으면 소켓 명부 파일이 사라져
                        // peer_sid 를 영영 못 얻는다(재시작 전에 온 메시지,
                        // 2026-08-31 실측: 명부에 2386.json 없음). 그때는
                        // **이름**으로 살아 있는 같은 이름 세션을 되짚는다
                        // — 같은 대화가 --resume 으로 살아 있으면 sid 가
                        // 같아 맞는 학생이 나온다. 태그 원문 라벨(공백판)
                        // 이 정확하고, 화면 이름은 하이픈판이라 둘 다 민다.
                        .or_else(|| {
                            let by_label =
                                |l: &str| peer_character_by_label(l, &self.pane_claude_sid, &ws);
                            msg.as_ref()
                                .and_then(|m| m.from_label.as_deref())
                                .and_then(by_label)
                                .or_else(|| by_label(&sender))
                                .or_else(|| by_label(&sender.replace('-', " ")))
                        })
                        .unwrap_or(sender)
                };
                let accent =
                    teammate_sender_accent(&sender, msg.as_ref().and_then(|m| m.color.as_deref()));
                // 접힌 줄이 다음 행들로 감겨 넘어간 꼬리 — 첫 행만 재작성하면
                // 원문 회색 잔해가 남는다(2026-08-31 스샷). 본문을 다시 그릴
                // 수 있으면 꼬리를 지워 전개 공간으로 넘기고(blank_run 이
                // 흡수한다), 본문이 없으면 색이라도 잇는다.
                let tail = collapsed_wrap_tail(&composed, r);
                for t in 0..tail {
                    let row = &mut composed[r + 1 + t];
                    if msg.is_some() {
                        for c in row.iter_mut() {
                            c.ch = ' ';
                        }
                    } else {
                        tint_row(row, accent);
                    }
                }
                let face_col = expand_teammate_message(
                    &mut composed,
                    r,
                    c0,
                    &sender,
                    msg.as_ref().map(|m| m.body.as_str()),
                    accent,
                );
                // 발신 학생 프사 — tell 과 같은 이미지 패스·같은 자리(첫 줄
                // 왼쪽 여백 2칸). 셀이 세로 2:1 이라 2칸×1행이 정사각.
                if let (Some(fc), Some(slug)) = (
                    face_col,
                    teammate_sender_slug(&sender).filter(|s| face_ready(s)),
                ) {
                    let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
                    let scw = self.cell.w * fs;
                    let sch = self.cell.h * fs;
                    let face_w = TELL_FACE_COLS as f32 * scw;
                    let face_h = sch;
                    let x = body_left + fc as f32 * scw;
                    let y = body_top + r as f32 * sch;
                    profile_slots.push((slug, (x, y, face_w, face_h)));
                }
            }
            // claude v2.1.228 은 처리 끝난 팀메시지를 접힌 줄이 아니라
            // `@ <발신 라벨>❯` + 들여쓴 본문으로 **펼쳐서** 그린다 — 위
            // 접힌 줄 탐지가 영영 안 걸리는 형태다(2026-08-12, 거노 스샷).
            // 화면 라벨이 transcript 태그의 from_label 과 일치할 때만
            // 남의 메시지로 인정한다 — 사용자가 직접 친 `@ …❯` 보호.
            for r in 0..composed.len() {
                let Some((c0, qcol, label)) = peer_native_header_line(&composed[r]) else {
                    continue;
                };
                let msg = msg_path
                    .as_deref()
                    .and_then(|p| latest_teammate_msg(p, PEER_LABEL));
                // 와이드 글리프 스페이서가 공백으로 섞이므로 공백 무시 대조.
                let norm =
                    |s: &str| -> String { s.chars().filter(|c| !c.is_whitespace()).collect() };
                // from-name 은 발신 세션에 제목이 없으면 통째로 빠진다(신생
                // pane 첫 메시지, 2026-08-12 실측) — 그때 claude 는 소켓
                // pid 를 라벨로 그리므로(`@ 12889❯`) pid 대조를 함께 받는다.
                let label_hit = msg.as_ref().is_some_and(|m| {
                    m.from_label.as_deref().map(&norm) == Some(norm(&label))
                        || m.from_pid.as_deref().map(&norm) == Some(norm(&label))
                });
                // 대조되는 것은 **최신 메시지 하나**뿐이라, 스크롤백의 옛
                // 메시지·tail(256KB) 밖 메시지는 대조가 영영 안 된다 — 라벨이
                // 로스터 학생의 agent 이름꼴이면 그것만으로 남의 메시지로
                // 인정한다(2026-08-20 거노 스샷: dismiss 된 미도리의 메시지가
                // 무테마로 남았다).
                // 라벨이 사람이 붙인 pane 이름이어도(`@ diff❯`) 명부에 그
                // 이름의 세션이 하나면 남의 메시지로 인정한다 — 지침이 pane
                // 마다 일감 이름을 붙이라고 시키므로 이 모양이 기본에 가깝다.
                if !label_hit
                    && !label_is_roster_agent(&label)
                    && peer_character_by_label(&label, &self.pane_claude_sid, &ws).is_none()
                {
                    continue;
                }
                // 발신자·색 해석은 헤더가 스크롤로 밀려난 이어칠하기(위
                // carried 블록)와 공유 — 접힌 경로와 같은 되짚기 규칙
                // (⚠️pane_character_if_known 금지, 위 주석).
                let (sender, accent) = native_sender_accent(
                    &label,
                    label_hit,
                    msg.as_ref(),
                    &self.pane_claude_sid,
                    &ws,
                );
                let slug = teammate_sender_slug(&sender);
                // 학생을 알면 긴 발신 라벨을 이름으로 갈아끼운다 — 라벨은
                // 발신 세션의 자동 제목이라 「sendmessage로 7유저에게…」 같은
                // 소음이다. 표시는 한글 이름으로(agent 명 "kanna-p1-qpo" 를
                // 그대로 쓰면 로마자 꼬리표가 남는다). 못 찾으면 원문 유지(색만).
                if let Some(slug) = slug {
                    let display = theme::slug_character_any(slug).unwrap_or(&sender);
                    restyle_peer_native_header(&mut composed[r], c0, qcol, display, accent);
                }
                tint_row(&mut composed[r], accent);
                let mut rr = r + 1;
                let mut face_row: Option<usize> = None;
                while rr < composed.len() {
                    if tell_wrap_continuation(&composed[rr]) {
                        face_row.get_or_insert(rr);
                        tint_row(&mut composed[rr], accent);
                        rr += 1;
                    } else if msg_paragraph_gap(&composed, rr) {
                        rr += 1;
                    } else {
                        break;
                    }
                }
                // 프사 — tell 과 같은 관례: 본문 행 왼쪽 여백(들여쓰기 2칸)에
                // 본문과 같은 행으로. 본문이 없으면(헤더뿐) 포기하고 색만.
                // 게이트는 **프사 자리에서만** — 위 이름 표시는 그림이 없어도
                // 되어야 한다(모르는 사람 취급이 되면 라벨이 소음으로 남는다).
                if let (Some(fr), Some(slug)) = (face_row, slug.filter(|s| face_ready(s))) {
                    let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
                    let scw = self.cell.w * fs;
                    let sch = self.cell.h * fs;
                    profile_slots.push((
                        slug,
                        (
                            body_left,
                            body_top + fr as f32 * sch,
                            TELL_FACE_COLS as f32 * scw,
                            sch,
                        ),
                    ));
                }
            }
        }
        // 크로스-방 tell(⟦캐릭터⟧ 본문)을 발신 학생 테마색으로 — 팀 경계를
        // 넘는 tell 은 네이티브 teammate 가 아니라 raw user 입력이라 거노 발신
        // 처럼 보인다. 마커가 유효 캐릭터면 그 행과 wrap 연속 행을 발신자
        // accent 로 칠하고, 마커 자리에 발신 학생 프사(bust)를 얹는다 —
        // profile_slots(statusline·resume 피커와 같은 이미지 패스)로 소비.
        {
            let fs = pane_scales.get(id.as_str()).copied().unwrap_or(1.0);
            let scw = self.cell.w * fs;
            let sch = self.cell.h * fs;
            let mut r = 0;
            while r < composed.len() {
                if let Some((marker_start, marker_end, name)) = tell_marker_line(&composed[r]) {
                    if let Some(accent) = theme::character_accent_any(&name) {
                        let face_col = restyle_tell_line(
                            &mut composed[r],
                            marker_start,
                            marker_end,
                            &name,
                            accent,
                        );
                        if let (Some(c0), Some(slug)) = (face_col, student_face_slug(&name)) {
                            // 프사는 본문 왼쪽 여백(`❯` 자리, 2칸)에 같은
                            // 행으로 — 셀은 세로 2:1 이라 2칸×1행이 정사각.
                            // 본문은 첫 줄부터 wrap 연속 행과 같은 col.
                            let face_w = TELL_FACE_COLS as f32 * scw;
                            let face_h = sch;
                            let x = body_left + c0 as f32 * scw;
                            let y = body_top + r as f32 * sch;
                            profile_slots.push((slug, (x, y, face_w, face_h)));
                        }
                        r += 1;
                        while r < composed.len() && tell_wrap_continuation(&composed[r]) {
                            tint_row(&mut composed[r], accent);
                            r += 1;
                        }
                        continue;
                    }
                }
                r += 1;
            }
        }
        // 학생 완료 보고 줄(`[완료] 미도리(%4) — …`, socket.rs pane_done
        // 주입)도 보고한 학생색으로 — 어느 학생의 보고인지 색으로 읽힌다
        // (거노 2026-08-20 「이왕하는거면 학생테마에 맞게 색상 다 해」).
        // 캐릭터를 모르는 옛 형식(`[완료] %4(%4)`)은 원색 유지 — 엉뚱한
        // 색보다 낫다. 반면 **이름은 아는데 명부만 다른** 경우(테마를 바꾼
        // 뒤 옛 이름 pane 의 보고)는 그 이름의 색이 실재하므로 합집합으로
        // 찾아 칠한다 — 그쪽까지 원색으로 두면 색이 곧 학생이라는 규칙이
        // 화면 절반에서만 성립한다.
        {
            let mut r = 0;
            while r < composed.len() {
                let accent =
                    done_report_line(&composed[r]).and_then(|n| theme::character_accent_any(&n));
                let Some(accent) = accent else {
                    r += 1;
                    continue;
                };
                tint_row(&mut composed[r], accent);
                r += 1;
                while r < composed.len() && tell_wrap_continuation(&composed[r]) {
                    tint_row(&mut composed[r], accent);
                    r += 1;
                }
            }
        }
        // 학생 accent 는 입력박스 보더·@배지 도색에만(거노 2026-07-18:
        // 응답 본문·"Reading 1 file" 상태줄까지 학생색이면 헷갈린다 —
        // 출력 글자는 테마 기본 fg. 옛 본문 틴트 폐기). 게이트는 pane
        // 테두리와 동일: 배정 캐릭터 + claude 가 foreground 일 때만
        // (active_process_name=="claude", 500ms 캐시 — 순정 셸 오염
        // 방지, 거노 실사고). agents 목록 뷰는 중립.
        // resume 피커(claude 시스템 UI)는 `╭─╮ Search ╰─╯` 박스가 pane
        // 입력박스로 오인돼 학생 accent 후처리가 오발동한다(거노: 빈 초록
        // 사각형). agents 목록 뷰처럼 학생 accent·세션 제목 인레이를 끈다.
        let resume_picker = screen_is_resume_picker(&composed);
        // AskUserQuestion picker 도 `❯ 1. …` 옵션줄 + 하단 힌트 박스가
        // 입력박스로 오인돼 accent 사각형이 남는다(거노: "question 이나
        // resume" 둘 다). team member/bg 세션 입력박스는 @칩 대신 세션
        // 제목이 상단보더에 와서 @칩 게이트론 못 가른다 → 화면 시그니처
        // ("Chat about this" 등)로 감지해 resume 와 동일하게 accent 를 끈다.
        let ask_picker = screen_is_ask_picker(&composed);
        let prompt_accent = if agents_view || resume_picker || ask_picker {
            None
        } else {
            // 관문은 아래 `filter`(active_agent)가 이미 지고 있다. 폴백만
            // 걷어낸 이유는 정확도다 — `pane.character` 는 pane 단위라 탭이
            // 둘이면 마지막 출력 탭이 이겨, 접힌 `true_char` 와 색이 갈렸다.
            true_char
                .as_deref()
                .and_then(|n| {
                    theme::character_accent_n(
                        n,
                        theme::character_ordinal(&ws.pane_character, &tab_pid),
                    )
                })
                .filter(|_| {
                    self.pty
                        .get(tab_pid.as_str())
                        .and_then(|p| p.active_agent())
                        .is_some()
                })
        };
        // ultracode 는 학생 배정과 무관한 pane 상태다 — 학생 accent 게이트
        // (Some 일 때만 칠함) 안쪽에 두면 미배정 pane 은 마커가 있어도 영영
        // 안 칠해진다(2026-08-12 조사). 피커 게이트는 prompt_accent 와 같은
        // 조건을 그대로 쓴다 — resume/ask 피커 오탐 방지 유지.
        let ultra = self.pane_ultracode.contains(&tab_pid)
            && !(agents_view || resume_picker || ask_picker)
            && self
                .pty
                .get(tab_pid.as_str())
                .and_then(|p| p.active_agent())
                .is_some();
        if ultra {
            animated_cells = true;
            let t = self.version_anim_start.elapsed().as_secs_f32();
            // 학생색 ↔ 보라 **순환** 숨쉬기(2026-08-17 「학생색 유지되면서
            // 순환하는 형식으로」 — 통보라 숨쉬기는 누구 pane 인지 잃어서
            // 이상했다). 골에서는 학생색 그대로, 마루에서 보라로 씻겼다가
            // 돌아온다. 「ultracode」 라벨은 상시고(깜빡임 반려, 2026-08-17)
            // 색만 보더와 함께 숨쉰다. 라벨 심기가 페인트보다 먼저다 —
            // 글자를 먼저 놓아야 style_prompt_box 가 같은 색을 입혀 준다.
            overlay_ultracode_label(&mut composed);
            style_prompt_box(&mut composed, ultracode_breath(prompt_accent, t));
        } else if let Some(accent) = prompt_accent {
            style_prompt_box(&mut composed, accent);
            // 칩 제거는 위 `runs_claude` 블록에서 이미 끝났다 — 여기서 한 번
        }
        // codex 자리의 세션 이름 배지. claude 는 CLI 가 스스로 위보더 우측에
        // 그리지만 codex 는 안 그려서, 화면만 보고는 무슨 일을 하는 자리인지
        // 알 수가 없었다(2026-09-05 지적). 피커 화면은 입력박스 오탐이 있어
        // accent 와 같은 조건으로 끈다.
        //
        // **핀이 선 제목만** 쓴다 — OSC 로 들어온 폴더 이름을 배지로 띄우면
        // 창마다 「kasaterm」 이 반복될 뿐이다. 핀은 사람의 개명이나 스캔이
        // 찾은 이름(`sync_codex_titles`)에만 선다.
        if !(agents_view || resume_picker || ask_picker)
            && self
                .pty
                .get(tab_pid.as_str())
                .and_then(|p| p.active_agent())
                .is_some_and(|k| matches!(k, kasa_pty::AgentKind::Codex))
        {
            if let Some(name) = ws
                .panes
                .get(tab_pid.as_str())
                .filter(|p| p.title_pinned)
                .and_then(|p| p.title.as_deref())
            {
                overlay_codex_session_label(
                    &mut composed,
                    name,
                    prompt_accent.unwrap_or_else(|| theme::accent_color(theme::accent_name())),
                );
            }
        }
        // 내가 친 프롬프트 띠 재도색 — claude 테마의 전폭 띠(라이트=씻긴
        // 회백, 다크=흰 띠)를 kasaterm 테마·학생색으로(2026-08-15 지시
        // 「색상이랑 디자인 바꾸자」). 디자인: 띠는 본문 폭까지만(전폭
        // 꼬리는 기본 배경으로), 바탕은 학생 accent 를 테마 배경에 살짝
        // 섞은 톤, `❯` 는 accent 원색. 픽커/목록 화면은 선택 강조가
        // (`❯`+배경) 오탐되므로 통째로 건너뛴다.
        if !(agents_view || resume_picker || ask_picker) {
            let accent = prompt_accent.unwrap_or_else(|| theme::accent_color(theme::accent_name()));
            let base = theme::bg();
            let light = base[0] as u16 + base[1] as u16 + base[2] as u16 > 380;
            let amount = if light { 0.10 } else { 0.18 };
            let fill = tint_toward([base[0], base[1], base[2]], accent, amount);
            let mut r = 0;
            while r < composed.len() {
                // sticky pill 행은 재도색 금지 — 위 sticky 블록 주석 참고.
                if Some(r) == sticky_pill_row {
                    r += 1;
                    continue;
                }
                let Some(band) = user_prompt_band(&composed[r]) else {
                    r += 1;
                    continue;
                };
                loop {
                    restyle_user_prompt_row(&mut composed[r], &fill, accent);
                    r += 1;
                    if r >= composed.len() || band_bg(&composed[r]).as_ref() != Some(&band) {
                        break;
                    }
                }
            }
        }
        TerminalComposition {
            animated_cells,
            rows: composed,
            banner_slots,
            spinner_slots,
            waiting_slots,
            standing_slots,
            profile_slots,
            inline_slots,
            schale_logo_slots,
            title_outline_slots,
            status_model_icons,
            sticky_pill_slots,
            turn_header_slots,
            view_shifts,
            tip_hit,
            agents_view_panes,
            mirror_claude_panes,
            agents_view,
            runs_claude,
            true_char,
            tab_pid,
        }
    }
}
