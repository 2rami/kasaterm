//! Native terminal composition shared with grid viewers without a GUI request per frame.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use kasa_bridge::screen::{Color, Row, ScreenUpdate};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::watch;

#[derive(Clone, Debug, Serialize)]
pub struct VisualRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum VisualAsset {
    Sprite { slug: String, motion: String, frame: u16 },
    Avatar { slug: String },
    Builtin { name: String },
    Solid { color: [u8; 4] },
    Inline { id: String },
    Animation { frames: Vec<String> },
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VisualMotion {
    pub frames: u16,
    pub frame_ms: u32,
    pub started_at_ms: u64,
    pub looping: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct VisualOverlay {
    pub id: String,
    /// Layout slot in cells; fit preserves bitmap aspect across different font metrics.
    pub rect: VisualRect,
    pub clip: Option<VisualRect>,
    pub z: i32,
    pub fit: String,
    pub anchor: String,
    pub asset: VisualAsset,
    pub motion: Option<VisualMotion>,
}

#[derive(Clone, Debug)]
pub struct PaneVisualFrame {
    pub pane_id: String,
    pub source_key: String,
    pub scene_revision: u64,
    pub cols: u16,
    pub rows: u16,
    pub offset: usize,
    pub composed_cells: Vec<Row>,
    pub overlays: Vec<VisualOverlay>,
}

fn hash_color(hash: &mut Sha256, color: &Color) {
    match color {
        Color::Default => hash.update([0]),
        Color::Idx(index) => hash.update([1, *index]),
        Color::Rgb(r, g, b) => hash.update([2, *r, *g, *b]),
    }
}

/// Only a complete raw snapshot identifies a composition; a dirty diff is insufficient.
pub fn source_key(source: &ScreenUpdate, offset: usize) -> Option<String> {
    if source.cols == 0 || source.rows == 0 || source.dirty.len() != source.rows as usize {
        return None;
    }
    let mut rows: Vec<Option<&Row>> = vec![None; source.rows as usize];
    for (index, row) in &source.dirty {
        let slot = rows.get_mut(*index as usize)?;
        if slot.is_some() || row.len() != source.cols as usize {
            return None;
        }
        *slot = Some(row);
    }
    let mut hash = Sha256::new();
    hash.update(b"kasaterm-native-scene-v1\0");
    hash.update((source.pane_id.len() as u64).to_le_bytes());
    hash.update(source.pane_id.as_bytes());
    hash.update(source.cols.to_le_bytes());
    hash.update(source.rows.to_le_bytes());
    hash.update((offset as u64).to_le_bytes());
    hash.update(source.cursor_col.to_le_bytes());
    hash.update(source.cursor_row.to_le_bytes());
    hash.update([
        source.cursor_visible as u8, source.alt_screen as u8, source.mouse_enabled as u8,
        source.mouse_sgr as u8, source.app_cursor as u8, source.bracketed_paste as u8,
    ]);
    for row in rows {
        for cell in row? {
            hash.update((cell.ch as u32).to_le_bytes());
            hash_color(&mut hash, &cell.fg);
            hash_color(&mut hash, &cell.bg);
            hash.update([
                cell.bold as u8, cell.italic as u8, cell.underline as u8, cell.inverse as u8,
                cell.dim as u8, cell.hidden as u8, cell.wrapped as u8,
            ]);
        }
    }
    hash.update((source.inline_images.len() as u64).to_le_bytes());
    for image in &source.inline_images {
        hash.update(image.id.to_le_bytes());
        hash.update(image.row.to_le_bytes());
        hash.update(image.col.to_le_bytes());
        hash.update(image.cols.to_le_bytes());
        hash.update(image.rows.to_le_bytes());
        hash.update((image.path.len() as u64).to_le_bytes());
        hash.update(image.path.as_bytes());
    }
    Some(format!("{:x}", hash.finalize()))
}

struct InlineAsset {
    bytes: Arc<[u8]>,
    mime: &'static str,
}

struct PaneScenes {
    subscribers: HashSet<u64>,
    changed: watch::Sender<u64>,
    frame: Option<Arc<PaneVisualFrame>>,
    assets: HashMap<String, InlineAsset>,
}

#[derive(Default)]
struct Registry {
    panes: HashMap<String, PaneScenes>,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

static PRODUCER: OnceLock<Arc<dyn Fn() + Send + Sync>> = OnceLock::new();
static TOKEN: AtomicU64 = AtomicU64::new(1);

pub fn register_producer(wake: Arc<dyn Fn() + Send + Sync>) {
    let _ = PRODUCER.set(wake);
}

pub fn producer_available() -> bool {
    PRODUCER.get().is_some()
}

pub struct VisualSubscription {
    pane: String,
    token: u64,
    pub changed: watch::Receiver<u64>,
}

impl Drop for VisualSubscription {
    fn drop(&mut self) {
        let mut registry = registry().lock().unwrap();
        if let Some(entry) = registry.panes.get_mut(&self.pane) {
            entry.subscribers.remove(&self.token);
            if entry.subscribers.is_empty() {
                registry.panes.remove(&self.pane);
            }
        }
    }
}

pub fn subscribe(pane: &str) -> VisualSubscription {
    let token = TOKEN.fetch_add(1, Ordering::Relaxed);
    let mut registry = registry().lock().unwrap();
    let entry = registry.panes.entry(pane.to_string()).or_insert_with(|| PaneScenes {
        subscribers: HashSet::new(), changed: watch::channel(0).0,
        frame: None, assets: HashMap::new(),
    });
    entry.subscribers.insert(token);
    let subscription = VisualSubscription { pane: pane.to_string(), token, changed: entry.changed.subscribe() };
    drop(registry);
    if let Some(wake) = PRODUCER.get() { wake(); }
    subscription
}

pub fn subscribed_panes() -> Vec<String> {
    registry().lock().unwrap().panes.keys().cloned().collect()
}

fn valid_key(key: &str) -> bool {
    key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit())
}

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty() && slug.len() <= 128
        && slug.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn valid_rect(rect: &VisualRect) -> bool {
    [rect.x, rect.y, rect.width, rect.height].iter().all(|v| v.is_finite() && v.abs() <= 10000.0)
        && rect.width > 0.0 && rect.height > 0.0
}

fn asset_ids(asset: &VisualAsset) -> &[String] {
    match asset {
        VisualAsset::Inline { id } => std::slice::from_ref(id),
        VisualAsset::Animation { frames } => frames,
        _ => &[],
    }
}

pub fn publish(frame: PaneVisualFrame) -> bool {
    if frame.offset != 0 || !valid_key(&frame.source_key) || frame.cols == 0 || frame.rows == 0
        || frame.composed_cells.len() != frame.rows as usize
        || frame.composed_cells.iter().any(|row| row.len() != frame.cols as usize)
        || frame.overlays.len() > 128
        || frame.overlays.iter().any(|overlay| {
            !valid_rect(&overlay.rect) || overlay.clip.as_ref().is_some_and(|r| !valid_rect(r))
                || overlay.id.len() > 128
                || !matches!(overlay.fit.as_str(), "contain" | "cover" | "fill" | "scale-down")
                || !matches!(overlay.anchor.as_str(), "center" | "bottom")
                || overlay.motion.as_ref().is_some_and(|m| m.frames == 0 || m.frames > 512 || m.frame_ms < 10)
                || match &overlay.asset {
                    VisualAsset::Sprite { slug, motion, frame } => !valid_slug(slug) || !valid_slug(motion) || *frame >= 512,
                    VisualAsset::Avatar { slug } => !valid_slug(slug),
                    VisualAsset::Builtin { name } => builtin_asset(name).is_none(),
                    VisualAsset::Solid { .. } => false,
                    VisualAsset::Inline { id } => !valid_key(id),
                    VisualAsset::Animation { frames } => frames.is_empty() || frames.len() > 512
                        || frames.iter().any(|id| !valid_key(id))
                        || overlay.motion.as_ref().is_some_and(|m| m.frames as usize != frames.len()),
                }
        })
    {
        return false;
    }
    let mut registry = registry().lock().unwrap();
    let Some(entry) = registry.panes.get_mut(&frame.pane_id) else { return false };
    if entry.frame.as_ref().is_some_and(|old| old.scene_revision >= frame.scene_revision) {
        return false;
    }
    if frame.overlays.iter().flat_map(|overlay| asset_ids(&overlay.asset)).any(|id| !entry.assets.contains_key(id)) {
        return false;
    }
    entry.assets.retain(|id, _| frame.overlays.iter().any(|overlay| asset_ids(&overlay.asset).contains(id)));
    let revision = frame.scene_revision;
    entry.frame = Some(Arc::new(frame));
    entry.changed.send_replace(revision);
    true
}

pub fn matching_scene(source: &ScreenUpdate, offset: usize) -> Option<Arc<PaneVisualFrame>> {
    if offset != 0 { return None; }
    let key = source_key(source, offset)?;
    matching_source_key(source, &key)
}

pub(crate) fn matching_source_key(source: &ScreenUpdate, key: &str) -> Option<Arc<PaneVisualFrame>> {
    registry().lock().unwrap().panes.get(&source.pane_id)?.frame.as_ref()
        .filter(|frame| frame.offset == 0 && frame.cols == source.cols && frame.rows == source.rows
            && frame.source_key == key).cloned()
}

/// Bytes come from the native image cache, never a caller-supplied filesystem path.
pub fn register_inline_asset(pane: &str, image_id: u64, bytes: Arc<[u8]>) -> Option<String> {
    if bytes.len() > 32 * 1024 * 1024 { return None; }
    let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") { "image/png" }
        else if bytes.starts_with(b"\xff\xd8\xff") { "image/jpeg" }
        else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") { "image/gif" }
        else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") { "image/webp" }
        else { return None; };
    let mut hash = Sha256::new();
    hash.update(pane.as_bytes());
    hash.update(image_id.to_le_bytes());
    hash.update(&bytes);
    let id = format!("{:x}", hash.finalize());
    let mut registry = registry().lock().unwrap();
    let entry = registry.panes.get_mut(pane)?;
    if !entry.assets.contains_key(&id)
        && entry.assets.values().map(|asset| asset.bytes.len()).sum::<usize>() + bytes.len() > 64 * 1024 * 1024
    { return None; }
    entry.assets.insert(id.clone(), InlineAsset { bytes, mime });
    Some(id)
}

pub fn inline_asset(pane: &str, id: &str) -> Option<(Arc<[u8]>, &'static str)> {
    if !valid_key(id) { return None; }
    let registry = registry().lock().unwrap();
    let asset = registry.panes.get(pane)?.assets.get(id)?;
    Some((asset.bytes.clone(), asset.mime))
}

pub fn builtin_asset(name: &str) -> Option<(&'static [u8], &'static str)> {
    let svg: &'static [u8] = match name {
        "claude" => include_bytes!("../../../app/kasaterm/assets/icons/claude.svg"),
        "codex" => include_bytes!("../../../app/kasaterm/assets/icons/codex.svg"),
        "terminal" => include_bytes!("../../../app/kasaterm/assets/icons/terminal.svg"),
        "server" => include_bytes!("../../../app/kasaterm/assets/icons/server.svg"),
        "laptop" => include_bytes!("../../../app/kasaterm/assets/icons/laptop.svg"),
        "schale-logo" => return Some((include_bytes!("../../../app/kasaterm/assets/students/schale-logo.png"), "image/png")),
        "schale-classroom" => return Some((include_bytes!("../../../app/kasaterm/assets/schale-classroom.png"), "image/png")),
        _ => return None,
    };
    Some((svg, "image/svg+xml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kasa_bridge::screen::Cell;

    fn raw() -> ScreenUpdate {
        let mut row = vec![Cell::blank(); 4];
        for (cell, ch) in row.iter_mut().zip("LOGO".chars()) { cell.ch = ch; }
        ScreenUpdate {
            pane_id: format!("visual-test-{}", uuid::Uuid::new_v4()),
            cols: 4, rows: 2, dirty: vec![(0, row), (1, vec![Cell::blank(); 4])],
            cursor_visible: true, ..Default::default()
        }
    }

    fn frame(raw: &ScreenUpdate, revision: u64) -> PaneVisualFrame {
        PaneVisualFrame {
            pane_id: raw.pane_id.clone(), source_key: source_key(raw, 0).unwrap(),
            scene_revision: revision, cols: raw.cols, rows: raw.rows, offset: 0,
            composed_cells: vec![vec![Cell::blank(); raw.cols as usize]; raw.rows as usize],
            overlays: vec![VisualOverlay {
                id: "native-logo".into(), rect: VisualRect { x: 0.0, y: 0.0, width: 4.0, height: 1.0 },
                clip: None, z: 1, fit: "fill".into(), anchor: "center".into(),
                asset: VisualAsset::Solid { color: [20, 30, 40, 255] }, motion: None,
            }],
        }
    }

    #[test]
    fn source_fingerprint_covers_grid_modes_offset_and_rejects_partial_snapshots() {
        let original = raw();
        let key = source_key(&original, 0).unwrap();
        let mut reordered = original.clone();
        reordered.dirty.reverse();
        assert_eq!(source_key(&reordered, 0).unwrap(), key);
        assert_ne!(source_key(&original, 1).unwrap(), key);
        for change in 0..6 {
            let mut changed = original.clone();
            match change {
                0 => changed.dirty[0].1[0].ch = 'X',
                1 => changed.dirty[0].1[0].fg = Color::Rgb(1, 2, 3),
                2 => changed.dirty[0].1[0].hidden = true,
                3 => changed.cursor_col = 1,
                4 => changed.alt_screen = true,
                _ => changed.pane_id.push('x'),
            }
            assert_ne!(source_key(&changed, 0).unwrap(), key);
        }
        let mut partial = original.clone();
        partial.dirty.pop();
        assert!(source_key(&partial, 0).is_none());
        partial.dirty.push(partial.dirty[0].clone());
        assert!(source_key(&partial, 0).is_none(), "duplicate rows are not a full snapshot");
    }

    #[test]
    fn composed_cells_and_overlay_are_atomic_and_mismatch_restores_raw_logo() {
        let source = raw();
        let _subscription = subscribe(&source.pane_id);
        assert!(publish(frame(&source, 1)));
        let encoded = crate::gridwire::encode_visual(&source);
        assert_eq!(encoded["scene"]["revision"], 1);
        assert_eq!(encoded["sourceKey"], encoded["scene"]["sourceKey"]);
        assert_eq!(encoded["sceneRevision"], encoded["scene"]["revision"]);
        assert!(encoded["dirty"][0][1].as_array().unwrap().is_empty());
        let mut changed = source.clone();
        changed.cursor_row = 1;
        let raw = crate::gridwire::encode_visual(&changed);
        assert!(raw["scene"].is_null());
        assert!(raw["sceneRevision"].is_null());
        assert_eq!(raw["dirty"][0][1][0][0], "LOGO");
        assert_eq!(raw["dirty"].as_array().unwrap().len(), 2);
        assert!(matching_scene(&source, 1).is_none());
        let mut resized = source.clone();
        resized.rows = 1;
        resized.dirty.pop();
        assert!(crate::gridwire::encode_visual(&resized)["scene"].is_null());
        let other_pane = raw_for_other_pane(&source);
        assert!(crate::gridwire::encode_visual(&other_pane)["scene"].is_null());
    }

    fn raw_for_other_pane(source: &ScreenUpdate) -> ScreenUpdate {
        let mut other = source.clone();
        other.pane_id.push_str("-other");
        other
    }

    #[test]
    fn scene_revisions_and_subscription_lifetime_prevent_resurrection() {
        let source = raw();
        assert!(!publish(frame(&source, 1)), "no cache is retained without a viewer");
        let first = subscribe(&source.pane_id);
        let mut second = subscribe(&source.pane_id);
        assert!(publish(frame(&source, 2)));
        assert!(second.changed.has_changed().unwrap());
        assert_eq!(*second.changed.borrow_and_update(), 2);
        assert!(!publish(frame(&source, 1)));
        let mut scrollback = frame(&source, 3);
        scrollback.offset = 1;
        assert!(!publish(scrollback));
        drop(first);
        assert!(matching_scene(&source, 0).is_some());
        drop(second);
        assert!(matching_scene(&source, 0).is_none());
        let _reconnected = subscribe(&source.pane_id);
        assert!(matching_scene(&source, 0).is_none(), "old connection cache must not reappear");
    }

    #[test]
    fn inline_assets_are_pane_scoped_expire_and_never_accept_paths_or_svg() {
        let source = raw();
        let subscription = subscribe(&source.pane_id);
        let bytes: Arc<[u8]> = Arc::from(&b"\x89PNG\r\n\x1a\nexample"[..]);
        let id = register_inline_asset(&source.pane_id, 3, bytes.clone()).unwrap();
        assert_eq!(register_inline_asset(&source.pane_id, 3, bytes).unwrap(), id);
        assert_eq!(inline_asset(&source.pane_id, &id).unwrap().1, "image/png");
        assert!(inline_asset("another-pane", &id).is_none());
        assert!(inline_asset(&source.pane_id, "../../etc/passwd").is_none());
        assert!(builtin_asset("../icons/claude.svg").is_none());
        assert!(register_inline_asset(&source.pane_id, 4, Arc::from(&b"<svg onload='x'/>"[..])).is_none());
        let mut next = frame(&source, 1);
        next.overlays[0].asset = VisualAsset::Inline { id: id.clone() };
        assert!(publish(next));
        drop(subscription);
        assert!(inline_asset(&source.pane_id, &id).is_none());
    }

    #[test]
    fn animation_frame_sets_remain_cached_without_server_animation_ticks() {
        let source = raw();
        let mut subscription = subscribe(&source.pane_id);
        let ids: Vec<_> = (0..3).map(|index| {
            register_inline_asset(&source.pane_id, index,
                Arc::from(&b"\x89PNG\r\n\x1a\nframe"[..])).unwrap()
        }).collect();
        let mut scene = frame(&source, 1);
        scene.overlays[0].asset = VisualAsset::Animation { frames: ids.clone() };
        scene.overlays[0].motion = Some(VisualMotion {
            frames: 3, frame_ms: 80, started_at_ms: 1700000000000, looping: true,
        });
        assert!(publish(scene));
        subscription.changed.borrow_and_update();
        for id in &ids { assert!(inline_asset(&source.pane_id, id).is_some()); }
        assert!(!subscription.changed.has_changed().unwrap(), "asset fetches must not emit new scenes");
        assert!(publish(frame(&source, 2)));
        for id in &ids { assert!(inline_asset(&source.pane_id, id).is_none()); }
    }
}
