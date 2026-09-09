//! Remote character identity and images, without replacing the viewer's theme.
use super::*;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

#[derive(Clone, PartialEq, Eq)]
struct Character {
    original: String,
    slug: &'static str,
    color: Option<[u8; 4]>,
    complete: bool,
}

#[derive(Clone, Default)]
struct Appearance {
    tokens: serde_json::Value,
    characters: HashMap<String, Character>,
    assets: HashMap<(String, String, usize), Arc<[u8]>>,
    fetched: Option<Instant>,
    images_fetched: Option<Instant>,
    revision: u64,
}

#[derive(Default)]
struct Cache {
    sources: HashMap<String, Appearance>,
    active: Option<String>,
    pending: Option<String>,
    applied: Option<(String, u64)>,
    checked: Option<Instant>,
    interned: HashMap<String, &'static str>,
}

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn focus_source(c: &mut Cache, base: Option<&str>) -> bool {
    let active = base.map(str::to_owned);
    let marker = base.and_then(|base| c.sources.get(base).map(|a| (base.to_owned(), a.revision)));
    let changed = c.active != active || c.applied != marker;
    c.active = active;
    c.applied = marker;
    changed
}

pub(crate) fn character_slug(name: &str) -> Option<&'static str> {
    let c = cache().lock().ok()?;
    c.sources.get(c.active.as_ref()?)?.characters.get(name).map(|v| v.slug)
}

pub(crate) fn character_color(name: &str) -> Option<[u8; 4]> {
    let c = cache().lock().ok()?;
    c.sources.get(c.active.as_ref()?)?.characters.get(name)?.color
}

pub(crate) fn asset(slug: &str, motion: &str, frame: usize) -> Option<Arc<[u8]>> {
    if !slug.starts_with("mirror-") { return None; }
    let c = cache().lock().ok()?;
    c.sources.values().find_map(|a| a.assets.get(&(slug.to_string(), motion.to_string(), frame)).cloned())
}

fn color(value: &serde_json::Value) -> Option<[u8; 4]> {
    let text = value.as_str()?.strip_prefix('#')?;
    if text.len() != 6 { return None; }
    let n = u32::from_str_radix(text, 16).ok()?;
    Some([(n >> 16) as u8, (n >> 8) as u8, n as u8, 255])
}

fn fetch(base: &str, path: &str) -> Option<Vec<u8>> {
    kasa_mcp::remote::fetch_mirror_appearance(base, path).ok()
}

fn fetch_json(base: &str, path: &str) -> Option<serde_json::Value> {
    serde_json::from_slice(&fetch(base, path)?).ok()
}

fn fetch_appearance(base: &str, rows: Vec<serde_json::Value>, mut old: Appearance, publish: impl Fn(&Appearance)) -> Appearance {
    let started = Instant::now();
    let previous_tokens = old.tokens.clone();
    let previous_characters = old.characters.clone();
    if let Some(tokens) = fetch_json(base, "/design-tokens") { old.tokens = tokens; }
    if old.tokens != previous_tokens {
        old.revision = old.revision.wrapping_add(1);
        publish(&old);
    }
    let refresh_images = old.images_fetched.is_none_or(|t| t.elapsed() >= Duration::from_secs(60));
    let mut chars = HashMap::new();
    for row in rows {
        let Some(name) = row.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else { continue };
        if chars.contains_key(name) { continue; }
        let Some(original) = row.get("slug").and_then(|v| v.as_str()).filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')) else { continue };
        if started.elapsed() > Duration::from_secs(15) {
            if let Some(previous) = old.characters.get(name) { chars.insert(name.to_string(), previous.clone()); }
            continue;
        }
        let accent = row.get("color").or_else(|| row.get("header_color")).and_then(color)
            .or_else(|| old.tokens.get("character_accents")?.get(name).and_then(color));
        if !refresh_images {
            if let Some(previous) = old.characters.get(name).filter(|v| v.original == original && v.complete) {
                chars.insert(name.to_string(), Character { color: accent, ..previous.clone() });
                continue;
            }
        }
        let Some(status) = fetch_json(base, &format!("/character-sprite-status?slug={original}")) else {
            if let Some(previous) = old.characters.get(name) { chars.insert(name.to_string(), previous.clone()); }
            continue;
        };
        let mut images = Vec::new();
        let mut total_bytes = 0usize;
        let mut complete = true;
        let mut motions: Vec<_> = status.get("motions").and_then(|v| v.as_array()).into_iter().flatten().collect();
        motions.sort_by_key(|m| m.get("motion").and_then(|v| v.as_str()) != Some("profile"));
        let needs_profile = motions.iter().any(|m| m.get("motion").and_then(|v| v.as_str()) == Some("profile") && m.get("source").and_then(|v| v.as_str()) != Some("none"));
        for motion in motions {
            let Some(m) = motion.get("motion").and_then(|v| v.as_str()).filter(|m| ["profile", "idle", "walk", "wave", "cheer", "gif"].contains(m)) else { continue };
            if motion.get("source").and_then(|v| v.as_str()) == Some("none") { continue; }
            let count = motion.get("frames").and_then(|v| v.as_u64()).unwrap_or(0).min(6) as usize;
            let mut frames = Vec::new();
            for i in 0..count {
                if started.elapsed() > Duration::from_secs(15) { break; }
                let Some(bytes) = fetch(base, &format!("/character-sprite?slug={original}&motion={m}&frame={i}")) else { break };
                total_bytes += bytes.len();
                if total_bytes > 24 << 20 { break; }
                frames.push((m.to_string(), i, bytes));
            }
            if frames.len() == count { images.extend(frames); } else { complete = false; }
        }
        if images.is_empty() || (needs_profile && !images.iter().any(|(m, _, _)| m == "profile")) {
            if let Some(previous) = old.characters.get(name) { chars.insert(name.to_string(), previous.clone()); }
            continue;
        }
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        base.hash(&mut hash);
        original.hash(&mut hash);
        for (m, i, bytes) in &images { m.hash(&mut hash); i.hash(&mut hash); bytes.hash(&mut hash); }
        let key = format!("mirror-{:016x}-{original}", hash.finish());
        let slug = {
            let mut c = cache().lock().unwrap();
            *c.interned.entry(key.clone()).or_insert_with(|| Box::leak(key.into_boxed_str()))
        };
        for (m, i, bytes) in images { old.assets.insert((slug.to_string(), m, i), bytes.into()); }
        chars.insert(name.to_string(), Character { original: original.to_string(), slug, color: accent, complete });
    }
    old.characters = chars;
    old.assets.retain(|(slug, _, _), _| old.characters.values().any(|v| v.slug == slug));
    old.fetched = Some(Instant::now());
    if refresh_images { old.images_fetched = old.fetched; }
    if old.tokens != previous_tokens || old.characters != previous_characters {
        old.revision = old.revision.wrapping_add(1);
    }
    old
}

fn apply_remote_assignments(ws: &mut Workspace, assignments: impl IntoIterator<Item = (String, String)>) -> bool {
    let mut changed = false;
    for (id, name) in assignments {
        if ws.pane_character.get(&id) != Some(&name) {
            ws.pane_character.insert(id, name);
            changed = true;
        }
    }
    changed
}

impl App {
    pub(crate) fn refresh_mirror_theme(&mut self) {
        {
            let mut c = cache().lock().unwrap();
            if c.checked.is_some_and(|t| t.elapsed() < Duration::from_millis(300)) { return; }
            c.checked = Some(Instant::now());
        }
        // A restored mirror can carry yesterday's local assignment. Refresh
        // every mirror, including background tabs and rooms, from its host.
        let assignments: Vec<_> = self.pty.keys().filter_map(|id| {
            let info = kasa_mcp::remote::remote_info(id)?;
            let label = if info.label.is_empty() { kasa_mcp::machines::label_for_base(&info.base)? } else { info.label };
            let row = kasa_mcp::machines::cached_pane(&label, &info.remote_id)?;
            let name = row.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty())?;
            Some((id.clone(), name.to_string()))
        }).collect();
        {
            let mut ws = self.ws.lock().unwrap();
            self.chrome_dirty |= apply_remote_assignments(&mut ws, assignments);
        }
        let target = {
            let ws = self.ws.lock().unwrap();
            ws.active_pane.as_ref().and_then(|id| kasa_mcp::remote::remote_info(&ws.active_tab_pid(id)))
        };
        let mut c = cache().lock().unwrap();
        if focus_source(&mut c, target.as_ref().map(|v| v.base.as_str())) {
            drop(c);
            // Only the selected character assets changed. Palette, mode,
            // contrast, shape and active local previews belong to this device.
            self.web_visual.invalidate_assets();
            self.repaint_all();
            if let Some(w) = &self.window { w.request_redraw(); }
            c = cache().lock().unwrap();
        }
        let Some(target) = target else { return; };
        if c.pending.is_some() || c.sources.get(&target.base).and_then(|a| a.fetched).is_some_and(|t| t.elapsed() < Duration::from_secs(5)) { return; }
        let old = c.sources.get(&target.base).cloned().unwrap_or_default();
        c.pending = Some(target.base.clone());
        drop(c);
        let label = if target.label.is_empty() { kasa_mcp::machines::label_for_base(&target.base).unwrap_or_default() } else { target.label };
        let mut rows = kasa_mcp::machines::snapshot().into_iter()
            .find(|m| m.get("label").and_then(|v| v.as_str()) == Some(label.as_str()))
            .and_then(|m| m.get("panes").and_then(|v| v.as_array()).cloned()).unwrap_or_default();
        rows.sort_by_key(|row| row.get("id").and_then(|v| v.as_str()) != Some(target.remote_id.as_str()));
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let appearance = fetch_appearance(&target.base, rows, old, |appearance| {
                let mut c = cache().lock().unwrap();
                c.sources.insert(target.base.clone(), appearance.clone());
                c.checked = None;
                drop(c);
                let _ = proxy.send_event(UserEvent::Redraw);
            });
            let mut c = cache().lock().unwrap();
            c.sources.insert(target.base, appearance);
            c.pending = None;
            c.checked = None;
            drop(c);
            let _ = proxy.send_event(UserEvent::Redraw);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn mirror_runtime_keeps_viewer_mode_and_palette_while_loading_host_character() {
        const CHILD: &str = "KASATERM_MIRROR_RUNTIME_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            // Palette slots are process-global. Isolate each local mode.
            let dir = std::env::temp_dir().join(format!("kasaterm-mirror-runtime-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&dir).unwrap();
            let settings = dir.join("settings.json");
            for mode in ["dark", "light"] {
                let contents = serde_json::json!({"theme": mode, "accent": "blue", "shape": "rounded", "unrelated": "keep-me"}).to_string();
                std::fs::write(&settings, &contents).unwrap();
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "mirror_theme::tests::mirror_runtime_keeps_viewer_mode_and_palette_while_loading_host_character", "--nocapture"])
                    .env(CHILD, "1")
                    .env("KASATERM_SETTINGS_FILE", &settings)
                    .env_remove("KASATERM_SHAPE")
                    .output().unwrap();
                assert!(output.status.success(), "{mode} child failed:\n{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
                assert_eq!(std::fs::read_to_string(&settings).unwrap(), contents, "viewing a mirror rewrote local settings");
            }
            std::fs::remove_file(&settings).unwrap();
            std::fs::remove_dir(&dir).unwrap();
            return;
        }

        crate::theme::apply_from_settings_read_only();
        let local = crate::theme::tokens_json();
        assert_eq!(crate::theme::min_contrast(), 2.5);
        let mut remote = local.clone();
        remote["palette"]["bg"] = serde_json::json!(if crate::theme::current_is_light() { "#121212" } else { "#fafafa" });
        remote["palette"]["fg"] = serde_json::json!("#123456");
        remote["palette"]["border"] = serde_json::json!("#abcdef70");
        remote["palette"]["accent"] = serde_json::json!("#cc5500");
        remote["ansi"][4] = serde_json::json!("#2468ac");
        remote["min_contrast"] = serde_json::json!(7.0);

        let image = image::RgbaImage::from_pixel(8, 8, image::Rgba([11, 222, 33, 255]));
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image).write_to(&mut png, image::ImageFormat::Png).unwrap();
        let remote_slug = "mirror-runtime-test-momoi";
        let mut appearance = Appearance { tokens: remote.clone(), ..Default::default() };
        appearance.characters.insert("모모이".into(), Character {
            original: "momoi".into(), slug: remote_slug, color: Some([204, 85, 0, 255]), complete: true,
        });
        appearance.assets.insert((remote_slug.into(), "profile".into(), 0), png.into_inner().into());
        {
            let mut c = cache().lock().unwrap();
            c.sources.insert("test-host".into(), appearance);
            assert!(focus_source(&mut c, Some("test-host")));
        }
        for key in ["theme", "accent_name", "palette", "ansi", "shape", "min_contrast"] {
            assert_eq!(crate::theme::tokens_json()[key], local[key], "remote focus changed local {key}");
        }
        assert_eq!(character_color("모모이"), Some([204, 85, 0, 255]));
        assert_eq!(crate::theme::character_slug_any("모모이"), Some(remote_slug));
        // Rendering resolves the host's actual image, not the viewer's bundled
        // Momoi or stale Yuuka profile. Inbox identities retain canonical slugs.
        assert_eq!(crate::theme::agent_slug("모모이"), "momoi");
        let (pixels, width, height) = crate::sprites::student_profile_rgba(remote_slug).unwrap();
        assert!(width > 0 && height > 0);
        assert!(pixels.chunks_exact(4).all(|p| p == [11, 222, 33, 255]));

        let mut ws = Workspace::default();
        ws.pane_character.insert("%local-42".into(), "유우카".into());
        assert!(apply_remote_assignments(&mut ws, [("%local-42".into(), "모모이".into())]));
        assert_eq!(ws.pane_character.get("%local-42").map(String::as_str), Some("모모이"));
        assert!(!apply_remote_assignments(&mut ws, [("%local-42".into(), "모모이".into())]));

        {
            let mut c = cache().lock().unwrap();
            assert!(!focus_source(&mut c, Some("test-host")));
            c.sources.get_mut("test-host").unwrap().revision += 1;
            assert!(focus_source(&mut c, Some("test-host")));
            assert!(focus_source(&mut c, Some("second-host")));
            assert!(focus_source(&mut c, None));
        }
        assert_eq!(crate::theme::tokens_json(), local);
        assert_eq!(crate::theme::min_contrast(), 2.5);
        assert_ne!(crate::theme::character_slug_any("모모이"), Some(remote_slug));
    }

    #[test]
    fn remote_palette_publishes_before_character_download_and_uses_host_identity() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let tokens = serde_json::json!({"theme": "custom:remote", "character_accents": {"모모이": "#abcdef"}});
        let expected = tokens.clone();
        let server = std::thread::spawn(move || {
            for index in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 4096];
                let n = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..n]);
                let body = match index {
                    0 => { assert!(request.starts_with("GET /design-tokens ")); tokens.to_string().into_bytes() },
                    1 => { assert!(request.starts_with("GET /character-sprite-status?slug=momoi ")); serde_json::json!({"motions": [{"motion":"profile", "frames":1, "source":"user"}]}).to_string().into_bytes() },
                    _ => { assert!(request.starts_with("GET /character-sprite?slug=momoi&motion=profile&frame=0 ")); b"remote-avatar".to_vec() },
                };
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        let published = std::sync::atomic::AtomicBool::new(false);
        let row = serde_json::json!({"id":"%16", "name":"모모이", "slug":"momoi", "harness":"codex"});
        let result = fetch_appearance(&base, vec![row], Appearance::default(), |a| {
            assert_eq!(a.tokens, expected);
            assert!(a.characters.is_empty());
            published.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        server.join().unwrap();
        assert!(published.load(std::sync::atomic::Ordering::Relaxed));
        let student = result.characters.get("모모이").unwrap();
        assert!(student.slug.starts_with("mirror-"));
        assert_eq!(student.original, "momoi");
        assert_eq!(student.color, Some([0xab, 0xcd, 0xef, 255]));
        assert!(!result.characters.contains_key("유우카"));
        assert_eq!(&*result.assets[&(student.slug.to_string(), "profile".into(), 0)], b"remote-avatar");
    }
}
