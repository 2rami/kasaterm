use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

pub(crate) const PRESETS: &[(&str, &str)] = &[
    ("auto", "자동"),
    ("laptop", "노트북"),
    ("monitor", "데스크톱"),
    ("server", "서버"),
    ("smartphone", "모바일"),
];
const MAX_SVG_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct Row {
    pub label: String,
    pub local: bool,
    pub icon: String,
    pub choice: String,
}

#[derive(Default)]
struct Icons {
    choices: HashMap<String, String>,
    names: HashMap<String, String>,
    svg: HashMap<String, Arc<str>>,
}

static ICONS: RwLock<Option<Icons>> = RwLock::new(None);
static IMPORTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
type ImportResult = (String, Result<Option<String>, String>);
static IMPORT: Mutex<Option<ImportResult>> = Mutex::new(None);

fn normalize(label: &str) -> String {
    label.trim().to_lowercase()
}

fn automatic(label: &str) -> &'static str {
    let label = normalize(label);
    if ["macbook", "맥북", "laptop"]
        .iter()
        .any(|part| label.contains(part))
    {
        "laptop"
    } else if ["mini", "미니", "server", "서버"]
        .iter()
        .any(|part| label.contains(part))
    {
        "server"
    } else if ["phone", "mobile", "폰", "모바일"]
        .iter()
        .any(|part| label.contains(part))
    {
        "smartphone"
    } else {
        "monitor"
    }
}

fn validate_svg(svg: &str) -> Result<(), String> {
    if svg.len() > MAX_SVG_BYTES {
        return Err("SVG는 256KB 이하로 골라 주세요".into());
    }
    // 가져온 파일을 옮겨도 아이콘이 유지되어야 하므로 외부 그림을 참조하는 SVG는 받지 않는다.
    let lower = svg.to_lowercase();
    if lower.contains("<image") || lower.contains("<!entity") {
        return Err("외부 그림 참조 없이 경로로 구성된 SVG를 골라 주세요".into());
    }
    let pixels = crate::gpu::GpuRenderer::rasterize_icon(svg, 24)
        .ok_or("읽을 수 있는 SVG 파일을 골라 주세요")?;
    if !pixels.chunks_exact(4).any(|pixel| pixel[3] > 0) {
        return Err("그림이 있는 SVG 파일을 골라 주세요".into());
    }
    Ok(())
}

fn from_settings(settings: &serde_json::Value) -> Icons {
    use std::hash::{Hash, Hasher};
    let mut icons = Icons::default();
    let Some(saved) = settings.get("device_icons").and_then(|v| v.as_object()) else {
        return icons;
    };
    for (label, value) in saved {
        let label = normalize(label);
        if let Some(name) = value
            .as_str()
            .filter(|name| PRESETS.iter().any(|(key, _)| key == name) && *name != "auto")
        {
            icons.names.insert(label.clone(), name.into());
            icons.choices.insert(label, name.into());
        } else if let Some(svg) = value
            .get("svg")
            .and_then(|v| v.as_str())
            .filter(|svg| validate_svg(svg).is_ok())
        {
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            svg.hash(&mut hash);
            let name = format!("device-custom:{:016x}", hash.finish());
            icons.svg.insert(name.clone(), Arc::from(svg));
            icons.names.insert(label.clone(), name);
            icons.choices.insert(label, "custom".into());
        }
    }
    icons
}

pub(crate) fn reload(settings: &serde_json::Value) {
    let icons = from_settings(settings);
    if let Ok(mut cached) = ICONS.write() {
        *cached = Some(icons);
    }
}

fn ensure_loaded() {
    if ICONS.read().ok().is_some_and(|icons| icons.is_none()) {
        reload(&crate::socket::read_settings());
    }
}

pub(crate) fn icon(label: &str) -> String {
    ensure_loaded();
    ICONS
        .read()
        .ok()
        .and_then(|icons| icons.as_ref()?.names.get(&normalize(label)).cloned())
        .unwrap_or_else(|| automatic(label).into())
}

pub(crate) fn rows(devices: &[crate::render::pane_identity::DeviceColorRow]) -> Vec<Row> {
    ensure_loaded();
    devices
        .iter()
        .map(|device| Row {
            label: device.label.clone(),
            local: device.local,
            icon: icon(&device.label),
            choice: ICONS
                .read()
                .ok()
                .and_then(|icons| {
                    icons
                        .as_ref()?
                        .choices
                        .get(&normalize(&device.label))
                        .cloned()
                })
                .unwrap_or_else(|| "auto".into()),
        })
        .collect()
}

pub(crate) fn custom_svg(name: &str) -> Option<Arc<str>> {
    if !name.starts_with("device-custom:") {
        return None;
    }
    ensure_loaded();
    ICONS.read().ok()?.as_ref()?.svg.get(name).cloned()
}

fn save(label: &str, value: Option<serde_json::Value>) -> Result<(), String> {
    let mut settings = crate::socket::read_settings();
    let mut saved = settings
        .get("device_icons")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let key = normalize(label);
    saved.retain(|label, _| normalize(label) != key);
    if let Some(value) = value {
        saved.insert(key, value);
    }
    let value = serde_json::Value::Object(saved);
    crate::socket::write_settings_patch_atomic(&[("device_icons", value.clone())])
        .map_err(|_| "기기 아이콘을 저장하지 못했어요. 다시 선택해 주세요")?;
    settings["device_icons"] = value;
    reload(&settings);
    Ok(())
}

pub(crate) fn set_preset(label: &str, preset: &str) -> Result<(), String> {
    if !PRESETS.iter().any(|(key, _)| *key == preset) {
        return Err("아이콘 종류를 다시 골라 주세요".into());
    }
    save(label, (preset != "auto").then(|| serde_json::json!(preset)))
}

fn pick_svg() -> Result<Option<String>, String> {
    #[cfg(target_os = "macos")]
    let output = crate::proc::command("osascript")
        .args([
            "-e",
            r#"try
POSIX path of (choose file of type {"svg"} with prompt "기기 아이콘 SVG 선택")
on error number -128
return ""
end try"#,
        ])
        .output();
    #[cfg(windows)]
    let output = crate::proc::command("powershell.exe").args(["-NoProfile", "-STA", "-Command",
        "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; Add-Type -AssemblyName System.Windows.Forms; $picker = New-Object System.Windows.Forms.OpenFileDialog; $picker.Filter = 'SVG (*.svg)|*.svg'; if ($picker.ShowDialog() -eq 'OK') { [Console]::Write($picker.FileName) }"]).output();
    #[cfg(not(any(target_os = "macos", windows)))]
    let output = crate::proc::command("zenity")
        .args([
            "--file-selection",
            "--title=기기 아이콘 SVG 선택",
            "--file-filter=SVG | *.svg",
        ])
        .output();
    let output = output.map_err(|_| "파일 선택 창을 열지 못했어요")?;
    #[cfg(not(any(target_os = "macos", windows)))]
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    if !output.status.success() {
        return Err("파일 선택 창을 열지 못했어요".into());
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        return Ok(None);
    }
    let metadata = std::fs::metadata(&path).map_err(|_| "고른 SVG 파일을 읽지 못했어요")?;
    if !metadata.is_file() || metadata.len() > MAX_SVG_BYTES as u64 {
        return Err("256KB 이하 SVG 파일을 골라 주세요".into());
    }
    let svg = std::fs::read_to_string(path).map_err(|_| "고른 SVG 파일을 읽지 못했어요")?;
    validate_svg(&svg)?;
    Ok(Some(svg))
}

pub(crate) fn begin_import(label: String) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    if IMPORTING.swap(true, Ordering::SeqCst) {
        return Err("열려 있는 파일 선택 창에서 SVG를 골라 주세요".into());
    }
    std::thread::spawn(move || {
        let result = pick_svg();
        if let Ok(mut pending) = IMPORT.lock() {
            *pending = Some((label, result));
        } else {
            IMPORTING.store(false, Ordering::SeqCst);
        }
    });
    Ok(())
}

impl crate::App {
    pub(crate) fn poll_device_icon_import(&mut self) {
        let Some((label, result)) = IMPORT.lock().ok().and_then(|mut pending| pending.take())
        else {
            return;
        };
        IMPORTING.store(false, std::sync::atomic::Ordering::SeqCst);
        let result = match result {
            Ok(Some(svg)) => save(&label, Some(serde_json::json!({"svg": svg}))),
            Ok(None) => return,
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => {
                self.settings_scene.refresh_palette_cache();
                self.set_toast(format!("{label} 아이콘을 바꿨어요"));
            }
            Err(error) => self.set_toast(error),
        }
        self.chrome_dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_override_device_name_heuristics_and_unknown_values_fall_back() {
        let icons = from_settings(&serde_json::json!({"device_icons": {
            " My MacBook ": "server", "phone": "laptop", "bad": "arbitrary-file"
        }}));
        assert_eq!(
            icons.names.get("my macbook").map(String::as_str),
            Some("server")
        );
        assert_eq!(icons.names.get("phone").map(String::as_str), Some("laptop"));
        assert!(!icons.names.contains_key("bad"));
        assert_eq!(automatic("my phone"), "smartphone");
    }

    #[test]
    fn custom_svg_survives_settings_roundtrip_without_source_path() {
        let svg = include_str!("../assets/icons/laptop.svg");
        let saved = serde_json::json!({"device_icons": {"work": {"svg": svg}}});
        let persisted = serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
        let icons = from_settings(&persisted);
        let key = icons.names.get("work").unwrap();
        assert!(key.starts_with("device-custom:"));
        assert_eq!(icons.svg.get(key).unwrap().as_ref(), svg);
    }

    #[test]
    fn invalid_empty_and_oversized_svgs_are_rejected() {
        assert!(validate_svg("not svg").is_err());
        assert!(validate_svg(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"/>"#
        )
        .is_err());
        assert!(validate_svg(&" ".repeat(MAX_SVG_BYTES + 1)).is_err());
        assert!(validate_svg(r#"<svg><image href="file:///example.png"/></svg>"#).is_err());
    }
}
