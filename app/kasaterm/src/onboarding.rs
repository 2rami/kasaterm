//! First-install onboarding state and host-terminal import.
//!
//! This module never owns credentials. Authentication is reported by the
//! provider CLIs / their existing stores in `settings.rs`; only the chosen
//! provider name (`claude` or `codex`) is persisted here.

use base64::Engine as _;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) const VERSION: u64 = 1;

static BOOT_PREPARED: AtomicBool = AtomicBool::new(false);
static SHOW: AtomicBool = AtomicBool::new(false);
static OPENED: AtomicBool = AtomicBool::new(false);
static FONT_RESTART_REQUIRED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BootPlan {
    show: bool,
    mark_existing: bool,
}

fn boot_plan(settings_existed: bool, stored_version: u64, force: Option<&str>, isolated_settings: bool) -> BootPlan {
    let mark_existing = settings_existed && stored_version < VERSION;
    let show = match force {
        Some("show") => true,
        Some("hide") => false,
        _ if isolated_settings => false,
        _ => !settings_existed && stored_version < VERSION,
    };
    BootPlan { show, mark_existing }
}

/// Snapshot first-install state before shim installation creates config files.
pub(crate) fn prepare_boot() {
    let Some(path) = crate::socket::settings_file_path() else {
        BOOT_PREPARED.store(true, Ordering::Relaxed);
        SHOW.store(false, Ordering::Relaxed);
        return;
    };
    let existed = path.is_file();
    let settings = crate::socket::read_settings();
    let stored = settings.get("onboarding_version").and_then(|v| v.as_u64()).unwrap_or(0);
    let force = std::env::var("KASATERM_FORCE_ONBOARDING").ok();
    // KASATERM_SETTINGS_FILE is the documented scratch-settings seam used by
    // the screenshot/test rigs. A missing scratch file must not make every
    // unrelated verification launch an onboarding window; force=show is the
    // explicit onboarding rig.
    let isolated = std::env::var_os("KASATERM_SETTINGS_FILE").is_some() && force.is_none();
    let plan = boot_plan(existed, stored, force.as_deref(), isolated);
    if plan.mark_existing {
        let _ = crate::socket::write_settings_patch_atomic(&[("onboarding_version", serde_json::json!(VERSION))]);
    }
    SHOW.store(plan.show, Ordering::Relaxed);
    OPENED.store(false, Ordering::Relaxed);
    BOOT_PREPARED.store(true, Ordering::Relaxed);
}

pub(crate) fn launch_pending() -> bool {
    BOOT_PREPARED.load(Ordering::Relaxed) && SHOW.load(Ordering::Relaxed) && !OPENED.load(Ordering::Relaxed)
}

pub(crate) fn mark_opened() {
    OPENED.store(true, Ordering::Relaxed);
}

pub(crate) fn completed() -> bool {
    if BOOT_PREPARED.load(Ordering::Relaxed) {
        return !SHOW.load(Ordering::Relaxed);
    }
    match std::env::var("KASATERM_FORCE_ONBOARDING").as_deref() {
        Ok("show") => false,
        Ok("hide") => true,
        _ => crate::socket::read_settings().get("onboarding_version").and_then(|v| v.as_u64()).is_some_and(|v| v >= VERSION),
    }
}

pub(crate) fn complete(preferred_agent: Option<&str>) -> Result<(), String> {
    let mut patch = vec![("onboarding_version", serde_json::json!(VERSION))];
    if let Some(agent) = preferred_agent {
        if !matches!(agent, "claude" | "codex") {
            return Err("모르는 에이전트예요".to_string());
        }
        patch.push(("preferred_agent", serde_json::json!(agent)));
    }
    crate::socket::write_settings_patch_atomic(&patch).map_err(|e| e.to_string())?;
    SHOW.store(false, Ordering::Relaxed);
    Ok(())
}

pub(crate) fn skip() -> Result<(), String> {
    complete(None)
}

pub(crate) fn font_restart_required() -> bool {
    FONT_RESTART_REQUIRED.load(Ordering::Relaxed)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct ImportSource {
    pub id: String,
    pub label: String,
    pub available: bool,
    pub detected: bool,
    pub profile: Option<String>,
    pub font_family: Option<String>,
    pub font_path: Option<String>,
    pub font_size: Option<f32>,
    pub theme_label: Option<String>,
    pub background: Option<String>,
    pub foreground: Option<String>,
    pub cursor: Option<String>,
    pub ansi16: Vec<String>,
    /// `full` · `partial` · `unsupported` · `unavailable`.
    pub support: String,
    pub reason: Option<String>,
}

impl ImportSource {
    fn unavailable(id: &str, label: &str, available: bool, reason: &str) -> Self {
        Self {
            id: id.to_string(),
            label: label.to_string(),
            available,
            detected: false,
            profile: None,
            font_family: None,
            font_path: None,
            font_size: None,
            theme_label: None,
            background: None,
            foreground: None,
            cursor: None,
            ansi16: Vec::new(),
            support: if available { "unsupported" } else { "unavailable" }.to_string(),
            reason: Some(reason.to_string()),
        }
    }

    fn finish_support(mut self) -> Self {
        let colors = self.background.is_some() && self.foreground.is_some() && self.cursor.is_some() && self.ansi16.len() == 16;
        let font = self.font_family.is_some() && self.font_size.is_some();
        self.detected = colors || font;
        self.support = if colors && font {
            "full"
        } else if self.detected {
            "partial"
        } else {
            "unsupported"
        }
        .to_string();
        if self.support == "partial" && self.reason.is_none() {
            self.reason = Some("이 프로필에서 읽을 수 있는 항목만 가져와요".to_string());
        }
        if self.support == "unsupported" && self.reason.is_none() {
            self.reason = Some("기본 프로필의 색상과 폰트를 읽지 못했어요".to_string());
        }
        self
    }
}

#[derive(Debug, Clone)]
struct FontChoice {
    family: String,
    path: PathBuf,
}

fn font_extension(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "ttf" | "otf" | "ttc"))
}

fn safe_font_path(path: &Path) -> Option<PathBuf> {
    let path = path.canonicalize().ok()?;
    let meta = std::fs::metadata(&path).ok()?;
    (meta.is_file() && meta.len() > 0 && meta.len() <= 128 * 1024 * 1024 && font_extension(&path)).then_some(path)
}

fn font_label(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Font");
    for suffix in ["-Regular", " Regular", "_Regular", "-Roman", " Roman", "-Book"] {
        if let Some(s) = stem.strip_suffix(suffix) {
            return s.to_string();
        }
    }
    stem.to_string()
}

fn font_key(s: &str) -> String {
    let mut key: String = s.chars().filter(|c| c.is_ascii_alphanumeric()).flat_map(char::to_lowercase).collect();
    for (from, to) in [("nerdfontmono", "nfm"), ("nerdfont", "nf"), ("regular", "")] {
        key = key.replace(from, to);
    }
    key
}

fn likely_terminal_font(path: &Path) -> bool {
    let k = font_key(path.file_stem().and_then(|s| s.to_str()).unwrap_or_default());
    ["mono", "code", "menlo", "consol", "courier", "terminal", "d2coding", "cascadia", "hack", "fira", "iosevka", "sourcecode", "victor"].iter().any(|needle| k.contains(needle))
}

fn collect_fonts(dir: &Path, depth: usize, out: &mut Vec<FontChoice>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            collect_fonts(&path, depth - 1, out);
        } else if kind.is_file() && font_extension(&path) && likely_terminal_font(&path) {
            out.push(FontChoice { family: font_label(&path), path });
        }
    }
}

fn build_font_catalog() -> Vec<FontChoice> {
    let mut out = Vec::new();
    #[cfg(target_os = "macos")]
    {
        let home = kasa_socket::home_dir();
        if let Some(home) = home {
            collect_fonts(&home.join("Library/Fonts"), 3, &mut out);
        }
        for root in ["/Library/Fonts", "/System/Library/Fonts", "/System/Library/Fonts/Supplemental"] {
            collect_fonts(Path::new(root), 3, &mut out);
        }
    }
    #[cfg(windows)]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            collect_fonts(&PathBuf::from(local).join("Microsoft/Windows/Fonts"), 2, &mut out);
        }
        collect_fonts(Path::new(r"C:\Windows\Fonts"), 2, &mut out);
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        if let Some(home) = crate::kasa_socket::home_dir() {
            collect_fonts(&home.join(".local/share/fonts"), 3, &mut out);
        }
        collect_fonts(Path::new("/usr/share/fonts"), 4, &mut out);
    }
    out.retain(|f| safe_font_path(&f.path).is_some());
    out.sort_by(|a, b| a.family.to_lowercase().cmp(&b.family.to_lowercase()));
    out.dedup_by(|a, b| font_key(&a.family) == font_key(&b.family));
    out
}

fn font_catalog() -> &'static [FontChoice] {
    static CATALOG: std::sync::OnceLock<Vec<FontChoice>> = std::sync::OnceLock::new();
    CATALOG.get_or_init(build_font_catalog)
}

fn resolve_font_family(family: &str) -> Option<FontChoice> {
    let wanted = font_key(family);
    let fonts = font_catalog();
    fonts.iter().find(|f| font_key(&f.family) == wanted).cloned().or_else(|| {
        fonts
            .iter()
            .find(|f| {
                let have = font_key(&f.family);
                have.starts_with(&wanted) || wanted.starts_with(&have)
            })
            .cloned()
    })
}

pub(crate) fn font_families() -> Vec<String> {
    font_catalog().iter().map(|f| f.family.clone()).collect()
}

pub(crate) fn current_font_family() -> Option<String> {
    crate::socket::read_settings().get("font_family").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string).or_else(|| crate::socket::read_font_path().map(|p| font_label(&p)))
}

pub(crate) fn apply_font_family(family: &str) -> Result<(), String> {
    let choice = resolve_font_family(family).ok_or_else(|| "설치된 폰트를 찾지 못했어요".to_string())?;
    apply_font_choice(&choice.family, &choice.path)
}

pub(crate) fn apply_font_path(path: &str) -> Result<(), String> {
    let path = safe_font_path(Path::new(path)).ok_or_else(|| "읽을 수 있는 폰트 파일이 아니에요".to_string())?;
    let family = font_label(&path);
    apply_font_choice(&family, &path)
}

fn apply_font_choice(family: &str, path: &Path) -> Result<(), String> {
    let previous = crate::socket::read_font_path();
    crate::socket::write_settings_patch_atomic(&[("font_family", serde_json::json!(family)), ("font_path", serde_json::json!(path.to_string_lossy()))]).map_err(|e| e.to_string())?;
    if previous.as_deref() != Some(path) {
        FONT_RESTART_REQUIRED.store(true, Ordering::Relaxed);
    }
    Ok(())
}

fn plutil_raw(path: &Path, key: &str) -> Option<String> {
    let out = std::process::Command::new("/usr/bin/plutil").args(["-extract", key, "raw"]).arg(path).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn archive_xml(encoded: &str) -> Option<String> {
    use std::io::Write as _;
    use std::process::Stdio;
    let compact: String = encoded.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD.decode(compact).ok()?;
    let mut child = std::process::Command::new("/usr/bin/plutil").args(["-convert", "xml1", "-o", "-", "--", "-"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    child.stdin.take()?.write_all(&bytes).ok()?;
    let out = child.wait_with_output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

fn xml_tag_after_key<'a>(xml: &'a str, key: &str, tags: &[&str]) -> Option<&'a str> {
    let at = xml.find(&format!("<key>{key}</key>"))?;
    let tail = &xml[at..];
    tags.iter().find_map(|tag| {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        let start = tail.find(&open)? + open.len();
        let end = tail[start..].find(&close)? + start;
        Some(tail[start..end].trim())
    })
}

fn xml_strings(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<string>") {
        let tail = &rest[start + "<string>".len()..];
        let Some(end) = tail.find("</string>") else {
            break;
        };
        out.push(xml_unescape(tail[..end].trim()));
        rest = &tail[end + "</string>".len()..];
    }
    out
}

fn parse_terminal_font_archive_xml(xml: &str) -> Option<(String, f32)> {
    let size = xml_tag_after_key(xml, "NSSize", &["real", "integer"])?.parse().ok()?;
    let family = xml_strings(xml).into_iter().find(|s| !s.is_empty() && !matches!(s.as_str(), "$null" | "NSKeyedArchiver" | "NSFont" | "NSObject"))?;
    Some((family, size))
}

fn parse_terminal_color_archive_xml(xml: &str) -> Option<String> {
    let key = if xml.contains("<key>NSRGB</key>") { "NSRGB" } else { "NSWhite" };
    let encoded = xml_tag_after_key(xml, key, &["data"])?;
    let compact: String = encoded.chars().filter(|c| !c.is_whitespace()).collect();
    let raw = base64::engine::general_purpose::STANDARD.decode(compact).ok()?;
    let s = String::from_utf8_lossy(&raw);
    let vals: Vec<f32> = s.trim_matches(char::from(0)).split_whitespace().filter_map(|v| v.parse().ok()).collect();
    let rgb = if key == "NSWhite" {
        let v = *vals.first()?;
        [v, v, v]
    } else {
        [*vals.first()?, *vals.get(1)?, *vals.get(2)?]
    };
    Some(rgb_hex(rgb))
}

fn rgb_hex(rgb: [f32; 3]) -> String {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", byte(rgb[0]), byte(rgb[1]), byte(rgb[2]))
}

fn parse_iterm_font(s: &str) -> Option<(String, f32)> {
    let (family, size) = s.trim().rsplit_once(' ')?;
    let size = size.parse::<f32>().ok()?;
    (!family.trim().is_empty() && (4.0..=96.0).contains(&size)).then(|| (family.trim().to_string(), size))
}

#[cfg(target_os = "macos")]
fn apple_terminal_source() -> ImportSource {
    let home = match kasa_socket::home_dir() {
        Some(h) => h,
        None => return ImportSource::unavailable("apple-terminal", "Apple Terminal", false, "홈 폴더를 찾지 못했어요"),
    };
    let path = home.join("Library/Preferences/com.apple.Terminal.plist");
    let app_available = Path::new("/System/Applications/Utilities/Terminal.app").exists() || path.is_file();
    if !path.is_file() {
        return ImportSource::unavailable("apple-terminal", "Apple Terminal", app_available, "저장된 Terminal 프로필이 없어요");
    }
    let Some(profile) = plutil_raw(&path, "Default Window Settings").filter(|s| !s.is_empty()) else {
        return ImportSource::unavailable("apple-terminal", "Apple Terminal", true, "기본 Terminal 프로필을 찾지 못했어요");
    };
    // plutil's key-path syntax uses dots as separators. A profile containing a
    // literal dot cannot be addressed safely through this read-only seam.
    if profile.contains('.') {
        return ImportSource::unavailable("apple-terminal", "Apple Terminal", true, "이름에 점이 든 Terminal 프로필은 아직 가져올 수 없어요");
    }
    let prefix = format!("Window Settings.{profile}");
    let font = plutil_raw(&path, &format!("{prefix}.Font")).and_then(|v| archive_xml(&v)).and_then(|xml| parse_terminal_font_archive_xml(&xml));
    let color = |key: &str| plutil_raw(&path, &format!("{prefix}.{key}")).and_then(|v| archive_xml(&v)).and_then(|xml| parse_terminal_color_archive_xml(&xml));
    let ansi_keys = [
        "ANSIBlackColor",
        "ANSIRedColor",
        "ANSIGreenColor",
        "ANSIYellowColor",
        "ANSIBlueColor",
        "ANSIMagentaColor",
        "ANSICyanColor",
        "ANSIWhiteColor",
        "ANSIBrightBlackColor",
        "ANSIBrightRedColor",
        "ANSIBrightGreenColor",
        "ANSIBrightYellowColor",
        "ANSIBrightBlueColor",
        "ANSIBrightMagentaColor",
        "ANSIBrightCyanColor",
        "ANSIBrightWhiteColor",
    ];
    let ansi16: Vec<String> = ansi_keys.iter().filter_map(|key| color(key)).collect();
    let (font_family, font_size) = font.map_or((None, None), |(f, s)| (Some(f), Some(s)));
    let font_path = font_family.as_deref().and_then(resolve_font_family).map(|f| f.path.to_string_lossy().into_owned());
    ImportSource {
        id: "apple-terminal".to_string(),
        label: "Apple Terminal".to_string(),
        available: true,
        detected: false,
        profile: Some(profile.clone()),
        font_family,
        font_path,
        font_size,
        theme_label: Some(profile),
        background: color("BackgroundColor"),
        foreground: color("TextColor"),
        cursor: color("CursorColor"),
        ansi16,
        support: String::new(),
        reason: None,
    }
    .finish_support()
}

#[cfg(target_os = "macos")]
fn iterm_source() -> ImportSource {
    let home = match kasa_socket::home_dir() {
        Some(h) => h,
        None => return ImportSource::unavailable("iterm2", "iTerm2", false, "홈 폴더를 찾지 못했어요"),
    };
    let path = home.join("Library/Preferences/com.googlecode.iterm2.plist");
    let app_available = Path::new("/Applications/iTerm.app").exists() || path.is_file();
    if !path.is_file() {
        return ImportSource::unavailable("iterm2", "iTerm2", app_available, "저장된 iTerm2 프로필이 없어요");
    }
    let default_guid = plutil_raw(&path, "Default Bookmark Guid").unwrap_or_default();
    let mut picked = None;
    let mut first = None;
    for i in 0..512usize {
        let Some(guid) = plutil_raw(&path, &format!("New Bookmarks.{i}.Guid")) else {
            break;
        };
        first.get_or_insert(i);
        if guid == default_guid {
            picked = Some(i);
            break;
        }
    }
    let Some(index) = picked.or(first) else {
        return ImportSource::unavailable("iterm2", "iTerm2", true, "iTerm2 프로필을 찾지 못했어요");
    };
    let prefix = format!("New Bookmarks.{index}");
    let profile = plutil_raw(&path, &format!("{prefix}.Name")).unwrap_or_else(|| "Default".to_string());
    let font = plutil_raw(&path, &format!("{prefix}.Normal Font")).and_then(|s| parse_iterm_font(&s));
    let color = |key: &str| {
        let component = |name: &str| plutil_raw(&path, &format!("{prefix}.{key}.{name} Component"))?.parse::<f32>().ok();
        Some(rgb_hex([component("Red")?, component("Green")?, component("Blue")?]))
    };
    let ansi16: Vec<String> = (0..16).filter_map(|i| color(&format!("Ansi {i} Color"))).collect();
    let (font_family, font_size) = font.map_or((None, None), |(f, s)| (Some(f), Some(s)));
    let font_path = font_family.as_deref().and_then(resolve_font_family).map(|f| f.path.to_string_lossy().into_owned());
    ImportSource {
        id: "iterm2".to_string(),
        label: "iTerm2".to_string(),
        available: true,
        detected: false,
        profile: Some(profile.clone()),
        font_family,
        font_path,
        font_size,
        theme_label: Some(profile),
        background: color("Background Color"),
        foreground: color("Foreground Color"),
        cursor: color("Cursor Color"),
        ansi16,
        support: String::new(),
        reason: (picked.is_none() && first.is_some()).then(|| "기본 프로필 표식이 없어 첫 프로필을 읽었어요".to_string()),
    }
    .finish_support()
}

pub(crate) fn import_sources() -> Vec<ImportSource> {
    #[cfg(target_os = "macos")]
    {
        static SOURCES: std::sync::OnceLock<Vec<ImportSource>> = std::sync::OnceLock::new();
        SOURCES.get_or_init(|| vec![apple_terminal_source(), iterm_source()]).clone()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

fn mix_hex(a: &str, b: &str, amount: f32) -> Option<String> {
    let (a, b) = (parse_hex(a)?, parse_hex(b)?);
    let c = |i: usize| (f32::from(a[i]) + (f32::from(b[i]) - f32::from(a[i])) * amount).round().clamp(0.0, 255.0) as u8;
    Some(format!("#{:02x}{:02x}{:02x}", c(0), c(1), c(2)))
}

fn is_light_hex(s: &str) -> bool {
    parse_hex(s).is_some_and(|c| 0.299 * f32::from(c[0]) + 0.587 * f32::from(c[1]) + 0.114 * f32::from(c[2]) > 128.0)
}

fn merge_profile_settings(current: &serde_json::Value, source: &ImportSource) -> Result<serde_json::Value, String> {
    if !source.detected {
        return Err(source.reason.clone().unwrap_or_else(|| "가져올 설정이 없어요".to_string()));
    }
    let mut root = current.as_object().cloned().unwrap_or_default();
    let has_colors = source.background.is_some() && source.foreground.is_some() && source.ansi16.len() == 16;
    if has_colors {
        let bg = source.background.as_deref().unwrap_or("#252c35");
        let fg = source.foreground.as_deref().unwrap_or("#ffffff");
        let base = if is_light_hex(bg) { "light" } else { "dark" };
        let mut entry = crate::theme::custom_theme_seed(base, "terminal-import", source.theme_label.as_deref().unwrap_or(source.label.as_str()));
        let obj = entry.as_object_mut().ok_or_else(|| "팔레트를 만들지 못했어요".to_string())?;
        obj.insert("bg".to_string(), serde_json::json!(bg));
        obj.insert("fg".to_string(), serde_json::json!(fg));
        obj.insert("text".to_string(), serde_json::json!(fg));
        for (key, amount) in [("surface", 0.06), ("surface_hover", 0.11), ("surface_active", 0.16), ("border", 0.23), ("text_dim", 0.72), ("text_mute", 0.52)] {
            if let Some(value) = if key.starts_with("text_") { mix_hex(bg, fg, amount) } else { mix_hex(bg, fg, amount) } {
                obj.insert(key.to_string(), serde_json::json!(value));
            }
        }
        obj.insert("ansi".to_string(), serde_json::Value::Array(source.ansi16.iter().cloned().map(serde_json::Value::String).collect()));
        let mut customs = crate::theme::custom_themes(current);
        if let Some(old) = customs.iter_mut().find(|e| crate::theme::custom_slug(e) == "terminal-import") {
            *old = entry;
        } else {
            customs.push(entry);
        }
        root.insert("custom_themes".to_string(), serde_json::Value::Array(customs));
        root.insert("theme".to_string(), serde_json::json!("custom:terminal-import"));
    }
    if let Some(cursor) = source.cursor.as_deref().filter(|s| parse_hex(s).is_some()) {
        root.insert("terminal_cursor_color".to_string(), serde_json::json!(cursor));
    }
    if let Some(size) = source.font_size.filter(|s| (9.0..=32.0).contains(s)) {
        root.insert("font_size".to_string(), serde_json::json!(size));
    }
    if let Some(family) = source.font_family.as_deref().filter(|s| !s.trim().is_empty()) {
        root.insert("font_family".to_string(), serde_json::json!(family));
    }
    if let Some(path) = source.font_path.as_deref().and_then(|p| safe_font_path(Path::new(p))) {
        root.insert("font_path".to_string(), serde_json::json!(path.to_string_lossy()));
    }
    Ok(serde_json::Value::Object(root))
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ProfileApply {
    pub font_size: Option<f32>,
    pub restart_required: bool,
}

pub(crate) fn apply_terminal_profile(id: &str) -> Result<ProfileApply, String> {
    let source = import_sources().into_iter().find(|s| s.id == id).ok_or_else(|| "가져올 터미널 프로필을 찾지 못했어요".to_string())?;
    let before = crate::socket::read_settings();
    let before_font = before.get("font_path").and_then(|v| v.as_str()).map(str::to_string);
    let next = merge_profile_settings(&before, &source)?;
    crate::socket::write_settings_value_atomic(&next).map_err(|e| e.to_string())?;
    let after_font = next.get("font_path").and_then(|v| v.as_str()).map(str::to_string);
    let restart_required = after_font.is_some() && after_font != before_font;
    if restart_required {
        FONT_RESTART_REQUIRED.store(true, Ordering::Relaxed);
    }
    Ok(ProfileApply { font_size: source.font_size, restart_required })
}

fn shell_id(label: &str) -> &'static str {
    match label {
        "PowerShell 7" => "pwsh",
        "Windows PowerShell" => "powershell",
        "Command Prompt" => "cmd",
        "Git Bash" => "git-bash",
        "WSL" => "wsl",
        _ => "shell",
    }
}

pub(crate) fn windows_shells_json() -> Vec<serde_json::Value> {
    crate::available_shells()
        .into_iter()
        .map(|(label, _, path)| {
            serde_json::json!({
                "id": shell_id(label),
                "label": label,
                "path": path,
                "detected": true,
            })
        })
        .collect()
}

pub(crate) fn selected_shell() -> String {
    let selected = crate::socket::read_default_shell().unwrap_or_default();
    crate::available_shells().into_iter().find(|(_, _, path)| path.eq_ignore_ascii_case(&selected)).map(|(label, _, _)| shell_id(label).to_string()).unwrap_or(selected)
}

pub(crate) fn apply_default_shell(id: &str) -> Result<String, String> {
    let (_, _, path) = crate::available_shells().into_iter().find(|(label, _, _)| shell_id(label) == id).ok_or_else(|| "설치된 셸 목록에 없어요".to_string())?;
    crate::socket::write_settings_patch_atomic(&[("default_shell", serde_json::json!(path))]).map_err(|e| e.to_string())?;
    Ok(path)
}

pub(crate) fn command_available(name: &str) -> bool {
    if !matches!(name, "claude" | "codex") {
        return false;
    }
    static CLAUDE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    static CODEX: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let cell = if name == "claude" { &CLAUDE } else { &CODEX };
    *cell.get_or_init(|| command_available_uncached(name))
}

fn command_available_uncached(name: &str) -> bool {
    #[cfg(windows)]
    let status = std::process::Command::new("where.exe").arg(name).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
    #[cfg(not(windows))]
    let status = {
        let shell = crate::resolve_default_shell().unwrap_or_else(|| "/bin/sh".to_string());
        std::process::Command::new(shell).args(["-lc", &format!("command -v {name} >/dev/null 2>&1")]).status()
    };
    status.is_ok_and(|s| s.success())
}

pub(crate) fn base_state_json() -> serde_json::Value {
    let settings = crate::socket::read_settings();
    let current_path = crate::socket::read_font_path();
    serde_json::json!({
        "platform": if cfg!(target_os = "macos") { "macos" }
            else if cfg!(windows) { "windows" } else { "linux" },
        "completed": completed(),
        "onboarding_version": VERSION,
        "imports": import_sources(),
        "fonts": font_families(),
        "font_family": current_font_family(),
        "font_path": current_path.map(|p| p.to_string_lossy().into_owned()),
        "font_size": crate::socket::read_font_size(),
        "restart_required": font_restart_required(),
        "windows_shells": windows_shells_json(),
        "selected_shell": selected_shell(),
        "preferred_agent": settings.get("preferred_agent").and_then(|v| v.as_str()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> ImportSource {
        ImportSource {
            id: "fixture".to_string(),
            label: "Fixture Terminal".to_string(),
            available: true,
            detected: true,
            profile: Some("Night".to_string()),
            font_family: Some("Fixture Mono".to_string()),
            font_path: None,
            font_size: Some(14.0),
            theme_label: Some("Night".to_string()),
            background: Some("#101820".to_string()),
            foreground: Some("#f0f4f8".to_string()),
            cursor: Some("#ffcc00".to_string()),
            ansi16: (0..16).map(|i| format!("#{i:02x}{i:02x}{i:02x}")).collect(),
            support: "full".to_string(),
            reason: None,
        }
    }

    #[test]
    fn new_install_shows_but_existing_settings_are_silently_marked() {
        assert_eq!(boot_plan(false, 0, None, false), BootPlan { show: true, mark_existing: false });
        assert_eq!(boot_plan(true, 0, None, false), BootPlan { show: false, mark_existing: true });
        assert_eq!(boot_plan(true, VERSION, None, false), BootPlan { show: false, mark_existing: false });
    }

    #[test]
    fn force_show_hide_and_isolated_settings_are_distinct() {
        assert!(boot_plan(true, VERSION, Some("show"), true).show);
        assert!(!boot_plan(false, 0, Some("hide"), false).show);
        assert!(!boot_plan(false, 0, None, true).show);
        // A scratch rig can still opt in explicitly.
        assert!(boot_plan(false, 0, Some("show"), true).show);
    }

    #[test]
    fn apple_terminal_archive_fixtures_parse_without_unarchiving_credentials() {
        let font = include_str!("../tests/fixtures/apple-terminal-font.xml");
        let color = include_str!("../tests/fixtures/apple-terminal-color.xml");
        assert_eq!(parse_terminal_font_archive_xml(font), Some(("D2CodingLigatureNFM".to_string(), 14.0)));
        assert_eq!(parse_terminal_color_archive_xml(color).as_deref(), Some("#1a334d"));
    }

    #[test]
    fn iterm_font_fixture_splits_the_trailing_size_only() {
        assert_eq!(parse_iterm_font(include_str!("../tests/fixtures/iterm-font.txt")), Some(("D2CodingLigatureNFM".to_string(), 13.5)));
        assert_eq!(parse_iterm_font("broken"), None);
    }

    #[test]
    fn profile_merge_is_all_or_nothing() {
        let before = serde_json::json!({ "keep": "yes", "theme": "dark" });
        let next = merge_profile_settings(&before, &source()).unwrap();
        assert_eq!(before, serde_json::json!({ "keep": "yes", "theme": "dark" }));
        assert_eq!(next.get("keep").and_then(|v| v.as_str()), Some("yes"));
        assert_eq!(next.get("theme").and_then(|v| v.as_str()), Some("custom:terminal-import"));
        assert_eq!(next.get("terminal_cursor_color").and_then(|v| v.as_str()), Some("#ffcc00"));
        assert_eq!(next.get("font_size").and_then(|v| v.as_f64()), Some(14.0));

        let mut invalid = source();
        invalid.detected = false;
        invalid.reason = Some("fixture failed".to_string());
        assert!(merge_profile_settings(&before, &invalid).is_err());
        assert_eq!(before.get("theme").and_then(|v| v.as_str()), Some("dark"));
    }

    #[test]
    fn atomic_settings_writer_leaves_one_complete_json_object() {
        let dir = std::env::temp_dir().join(format!("kasaterm-onboarding-atomic-{}-{}", std::process::id(), std::thread::current().name().unwrap_or("test")));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        crate::socket::write_settings_value_atomic_at(&path, &serde_json::json!({ "old": true })).unwrap();
        let expected = merge_profile_settings(&serde_json::json!({ "old": true }), &source()).unwrap();
        crate::socket::write_settings_value_atomic_at(&path, &expected).unwrap();
        let actual: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(std::fs::read_dir(&dir).unwrap().filter_map(Result::ok).count(), 1, "sibling temp must not survive a successful replace");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shell_ids_are_stable_api_values() {
        assert_eq!(shell_id("PowerShell 7"), "pwsh");
        assert_eq!(shell_id("Windows PowerShell"), "powershell");
        assert_eq!(shell_id("Command Prompt"), "cmd");
        assert_eq!(shell_id("Git Bash"), "git-bash");
        assert_eq!(shell_id("WSL"), "wsl");
    }
}
