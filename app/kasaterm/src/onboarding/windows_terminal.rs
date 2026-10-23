//! Windows Terminal 의 기본 프로필(글꼴·색 테마)을 온보딩 가져오기 꼴로 — 맥 Terminal·iTerm2
//! 가져오기와 같은 `ImportSource` 를 낸다.
//!
//! 값은 Windows Terminal 과 같은 차례로 겹쳐 읽는다: 그 프로필 → 사용자 `profiles.defaults` →
//! 터미널이 그 프로필에 싣는 기본값(`defaults.json`) → 전역 기본(Campbell · Cascadia Mono 12pt).
//! 사용자 settings.json 에는 사람이 바꾼 것만 들어 있어서, 내장 테마(Campbell 등)의 색은 설치된
//! 꾸러미의 `defaults.json` — 터미널 자신이 읽는 원본 — 에서 찾는다. 그걸 못 읽는 판(압축판 등)은
//! 기본 테마 Campbell 만 안다. 원본 설정은 읽기만 한다.
//!
//! 읽는 규칙은 OS 와 무관한 순수 함수라 어디서나 시험이 돈다. 파일 자리 찾기만 Windows 전용이다.
#![cfg_attr(not(windows), allow(dead_code))]

use super::ImportSource;
use serde_json::Value;

/// 기본 테마. 꾸러미의 defaults.json 을 못 읽을 때만 쓴다(1.24 defaults.json 에서 옮긴 값).
const CAMPBELL: [(&str, &str); 19] = [
    ("foreground", "#CCCCCC"), ("background", "#0C0C0C"), ("cursorColor", "#FFFFFF"),
    ("black", "#0C0C0C"), ("red", "#C50F1F"), ("green", "#13A10E"), ("yellow", "#C19C00"),
    ("blue", "#0037DA"), ("purple", "#881798"), ("cyan", "#3A96DD"), ("white", "#CCCCCC"),
    ("brightBlack", "#767676"), ("brightRed", "#E74856"), ("brightGreen", "#16C60C"), ("brightYellow", "#F9F1A5"),
    ("brightBlue", "#3B78FF"), ("brightPurple", "#B4009E"), ("brightCyan", "#61D6D6"), ("brightWhite", "#F2F2F2"),
];
/// ANSI 16색이 테마에 적히는 이름과 차례.
const ANSI: [&str; 16] = [
    "black", "red", "green", "yellow", "blue", "purple", "cyan", "white",
    "brightBlack", "brightRed", "brightGreen", "brightYellow", "brightBlue", "brightPurple", "brightCyan", "brightWhite",
];

/// 스토어판·미리 보기판·압축판 자리를 본다. 스토어판은 설정이 없어도 늘 한 줄을 내고(맥처럼 「없음」을
/// 보여 준다), 미리 보기판은 설정이 있을 때만 낸다.
#[cfg(windows)]
pub(super) fn sources() -> Vec<ImportSource> {
    let Some(local) = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from) else {
        return vec![ImportSource::unavailable("windows-terminal", "Windows Terminal", false, "LOCALAPPDATA 를 찾지 못했어요")];
    };
    let read = |path: &std::path::Path| std::fs::read_to_string(path).ok();
    let family = "Microsoft.WindowsTerminal_8wekyb3d8bbwe";
    let stable = read(&local.join("Packages").join(family).join("LocalState/settings.json"))
        .or_else(|| read(&local.join("Microsoft/Windows Terminal/settings.json")));
    let defaults = package_root(family).and_then(|root| read(&root.join("defaults.json")));
    let mut out = vec![match stable {
        Some(text) => source("windows-terminal", "Windows Terminal", &text, defaults.as_deref()),
        None => ImportSource::unavailable("windows-terminal", "Windows Terminal", defaults.is_some(), "저장된 Windows Terminal 설정이 없어요"),
    }];
    let preview = "Microsoft.WindowsTerminalPreview_8wekyb3d8bbwe";
    if let Some(text) = read(&local.join("Packages").join(preview).join("LocalState/settings.json")) {
        let defaults = package_root(preview).and_then(|root| read(&root.join("defaults.json")));
        out.push(source("windows-terminal-preview", "Windows Terminal Preview", &text, defaults.as_deref()));
    }
    out
}

/// 설치된 꾸러미의 폴더. `WindowsApps` 는 목록 보기가 막혀 있어 이름으로 찾아야 한다.
#[cfg(windows)]
fn package_root(family: &str) -> Option<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Storage::Packaging::Appx::{GetPackagePathByFullName, GetPackagesByPackageFamily};
    let family: Vec<u16> = family.encode_utf16().chain(std::iter::once(0)).collect();
    let (mut count, mut length) = (0u32, 0u32);
    // SAFETY: 처음엔 크기만 묻고(ERROR_INSUFFICIENT_BUFFER), 그 크기로 잡은 버퍼를 넘긴다.
    // 이름 포인터들은 `buffer` 안을 가리키므로 `buffer` 가 살아 있는 동안만 읽는다.
    unsafe {
        GetPackagesByPackageFamily(family.as_ptr(), &mut count, std::ptr::null_mut(), &mut length, std::ptr::null_mut());
        if count == 0 || length == 0 {
            return None;
        }
        let mut names = vec![std::ptr::null_mut::<u16>(); count as usize];
        let mut buffer = vec![0u16; length as usize];
        if GetPackagesByPackageFamily(family.as_ptr(), &mut count, names.as_mut_ptr(), &mut length, buffer.as_mut_ptr()) != 0 {
            return None;
        }
        let mut size = 0u32;
        GetPackagePathByFullName(names[0], &mut size, std::ptr::null_mut());
        let mut path = vec![0u16; size as usize];
        if size == 0 || GetPackagePathByFullName(names[0], &mut size, path.as_mut_ptr()) != 0 {
            return None;
        }
        let end = path.iter().position(|&c| c == 0).unwrap_or(path.len());
        Some(std::ffi::OsString::from_wide(&path[..end]).into())
    }
}

/// settings.json 한 벌(+ 그 판의 defaults.json) → 가져오기 한 줄.
pub(super) fn source(id: &str, label: &str, settings: &str, defaults: Option<&str>) -> ImportSource {
    let Some(settings) = parse_jsonc(settings) else {
        return ImportSource::unavailable(id, label, true, "Windows Terminal 설정을 읽지 못했어요");
    };
    let defaults = defaults.and_then(parse_jsonc);
    let list: &[Value] = settings["profiles"]["list"].as_array().or_else(|| settings["profiles"].as_array()).map_or(&[], Vec::as_slice);
    let wanted = settings["defaultProfile"].as_str().unwrap_or_default();
    let same_guid = |p: &Value, guid: &str| p["guid"].as_str().is_some_and(|g| g.eq_ignore_ascii_case(guid));
    // 기본 프로필은 guid 로도 이름으로도 적힌다. 못 찾으면 터미널처럼 목록 첫 줄이다.
    let Some(profile) = list.iter().find(|p| same_guid(p, wanted) || p["name"].as_str() == Some(wanted))
        .or_else(|| list.iter().find(|p| p["hidden"] != Value::Bool(true)))
    else {
        return ImportSource::unavailable(id, label, true, "Windows Terminal 프로필을 찾지 못했어요");
    };
    let builtin = profile["guid"].as_str().and_then(|guid| {
        defaults.as_ref()?["profiles"].as_array()?.iter().find(|p| same_guid(p, guid)).cloned()
    });
    let layers: Vec<&Value> = [Some(profile), settings["profiles"].get("defaults"), builtin.as_ref()].into_iter().flatten().collect();
    let first = |pick: &dyn Fn(&Value) -> Option<Value>| layers.iter().find_map(|layer| pick(layer));
    // 글꼴은 새 꼴(`font.face`)과 옛 꼴(`fontFace`)이 섞여 있다. 쉼표로 대체 글꼴을 이어 적을 수 있어 첫 이름만 쓴다.
    let face = first(&|l: &Value| l["font"].get("face").or_else(|| l.get("fontFace")).cloned())
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "Cascadia Mono".to_string());
    let face = face.split(',').next().unwrap_or_default().trim().to_string();
    let size = first(&|l: &Value| l["font"].get("size").or_else(|| l.get("fontSize")).cloned())
        .and_then(|v| v.as_f64()).unwrap_or(12.0) as f32;
    // 밝게/어둡게 짝(`{"light":…,"dark":…}`)이면 터미널의 기본 앱 테마인 어둡게 쪽을 쓴다.
    let scheme_name = first(&|l: &Value| l.get("colorScheme").cloned())
        .and_then(|v| v.as_str().map(str::to_string).or_else(|| v["dark"].as_str().or(v["light"].as_str()).map(str::to_string)))
        .unwrap_or_else(|| "Campbell".to_string());
    let named = |root: Option<&Value>| root?["schemes"].as_array()?.iter().find(|s| s["name"].as_str() == Some(scheme_name.as_str())).cloned();
    let scheme = named(Some(&settings)).or_else(|| named(defaults.as_ref())).or_else(|| {
        (scheme_name == "Campbell").then(|| Value::Object(CAMPBELL.iter().map(|(k, v)| (k.to_string(), Value::from(*v))).collect()))
    });
    // 프로필이 테마 위에 바탕·글자·커서 색을 따로 덮어 적을 수 있다.
    let color = |key: &str| first(&|l: &Value| l.get(key).cloned()).and_then(|v| hex(v.as_str()?))
        .or_else(|| hex(scheme.as_ref()?[key].as_str()?));
    let ansi16: Vec<String> = scheme.as_ref().map(|s| ANSI.iter().filter_map(|k| hex(s[*k].as_str()?)).collect()).unwrap_or_default();
    let font_path = super::resolve_font_family(&face).map(|f| f.path.to_string_lossy().into_owned());
    ImportSource {
        id: id.to_string(),
        label: label.to_string(),
        available: true,
        detected: false,
        profile: profile["name"].as_str().map(str::to_string),
        font_family: (!face.is_empty()).then_some(face),
        font_path,
        font_size: Some(size),
        theme_label: Some(scheme_name),
        background: color("background"),
        foreground: color("foreground"),
        cursor: color("cursorColor"),
        ansi16,
        support: String::new(),
        reason: None,
    }
    .finish_support()
}

/// `#RGB`·`#RRGGBB` → 소문자 `#rrggbb`(다른 가져오기와 같은 꼴).
fn hex(s: &str) -> Option<String> {
    let h = s.trim().strip_prefix('#')?;
    let full: String = match h.len() {
        3 => h.chars().flat_map(|c| [c, c]).collect(),
        6 => h.to_string(),
        _ => return None,
    };
    u32::from_str_radix(&full, 16).ok()?;
    Some(format!("#{}", full.to_ascii_lowercase()))
}

/// Windows Terminal 의 settings.json 은 `//`·`/* */` 주석과 끝 쉼표를 허용한다. 문자열 밖에서만
/// 걷어 내고 표준 JSON 으로 읽는다.
fn parse_jsonc(text: &str) -> Option<Value> {
    let text = text.trim_start_matches('\u{feff}');
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => {
                    if let Some(next) = chars.next() {
                        out.push(next);
                    }
                }
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_string = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for next in chars.by_ref() {
                    if next == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut last = '\0';
                for next in chars.by_ref() {
                    if last == '*' && next == '/' {
                        break;
                    }
                    last = next;
                }
            }
            _ => out.push(c),
        }
    }
    // 끝 쉼표: 공백만 사이에 두고 `}`·`]` 앞에 선 쉼표를 뺀다(주석은 이미 걷혔고 문자열 안은 건드리지 않는다).
    let mut clean = String::with_capacity(out.len());
    let mut in_string = false;
    let bytes: Vec<char> = out.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            clean.push(c);
            if c == '\\' && i + 1 < bytes.len() {
                clean.push(bytes[i + 1]);
                i += 1;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            clean.push(c);
        } else if c == ',' && bytes[i + 1..].iter().find(|ch| !ch.is_whitespace()).is_some_and(|ch| matches!(ch, '}' | ']')) {
            // 버린다.
        } else {
            clean.push(c);
        }
        i += 1;
    }
    serde_json::from_str(&clean).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 이 기기 실측 꼴: 프로필은 글꼴만 적고 테마·크기는 비워 두며, 기본 프로필은 guid 다.
    const SETTINGS: &str = r##"{
        "$schema": "https://aka.ms/terminal-profiles-schema",
        // 사람이 단 주석
        "defaultProfile": "{61c54bbd-c2c6-5271-96e7-009a87ff44bf}",
        "profiles": {
            "defaults": { "font": { "face": "D2CodingLigature Nerd Font, Consolas" } },
            "list": [
                { "guid": "{0caa0dad-35be-5f56-a8ff-afceeeaa6101}", "name": "Command Prompt" },
                { "guid": "{61C54BBD-C2C6-5271-96E7-009A87FF44BF}", "name": "Windows PowerShell", /* 끝 쉼표 */ },
            ],
        },
        "schemes": [],
    }"##;
    const DEFAULTS: &str = r##"{
        "profiles": [
            { "guid": "{61c54bbd-c2c6-5271-96e7-009a87ff44bf}", "colorScheme": "Campbell Powershell", "fontFace": "Cascadia Mono", "fontSize": 12 }
        ],
        "schemes": [
            { "name": "Campbell Powershell", "foreground": "#CCCCCC", "background": "#012456", "cursorColor": "#FFFFFF",
              "black": "#0C0C0C", "red": "#C50F1F", "green": "#13A10E", "yellow": "#C19C00", "blue": "#0037DA", "purple": "#881798",
              "cyan": "#3A96DD", "white": "#CCCCCC", "brightBlack": "#767676", "brightRed": "#E74856", "brightGreen": "#16C60C",
              "brightYellow": "#F9F1A5", "brightBlue": "#3B78FF", "brightPurple": "#B4009E", "brightCyan": "#61D6D6", "brightWhite": "#F2F2F2" }
        ]
    }"##;

    #[test]
    fn layers_resolve_like_the_terminal_does() {
        let s = source("windows-terminal", "Windows Terminal", SETTINGS, Some(DEFAULTS));
        assert_eq!(s.profile.as_deref(), Some("Windows PowerShell"), "guid 는 대소문자를 안 가린다");
        // 글꼴은 사용자 defaults 가 내장 기본(Cascadia Mono)을 덮고, 대체 글꼴 목록은 첫 이름만.
        assert_eq!(s.font_family.as_deref(), Some("D2CodingLigature Nerd Font"));
        assert_eq!(s.font_size, Some(12.0), "크기는 아무도 안 적으면 내장 기본");
        // 테마는 사용자 쪽에 없으니 그 프로필의 내장 기본 테마를 defaults.json 에서 찾는다.
        assert_eq!(s.theme_label.as_deref(), Some("Campbell Powershell"));
        assert_eq!(s.background.as_deref(), Some("#012456"));
        assert_eq!(s.ansi16.len(), 16);
        assert_eq!(s.ansi16[1], "#c50f1f");
        assert_eq!(s.support, "full");
    }

    #[test]
    fn without_defaults_json_only_campbell_is_known() {
        let s = source("windows-terminal", "Windows Terminal", r#"{"profiles":{"list":[{"name":"cmd"}]}}"#, None);
        assert_eq!(s.theme_label.as_deref(), Some("Campbell"));
        assert_eq!(s.background.as_deref(), Some("#0c0c0c"));
        assert_eq!(s.font_family.as_deref(), Some("Cascadia Mono"));
        assert_eq!(s.support, "full");
        let other = source("windows-terminal", "Windows Terminal", r#"{"profiles":{"list":[{"name":"cmd","colorScheme":"Tango Dark"}]}}"#, None);
        assert!(other.background.is_none(), "모르는 테마 색을 지어내지 않는다");
        assert_eq!(other.support, "partial", "글꼴만은 가져온다");
    }

    #[test]
    fn profile_overrides_and_light_dark_pairs() {
        let settings = r##"{"defaultProfile":"Mine","profiles":[{"name":"Mine","colorScheme":{"light":"Campbell","dark":"Night"},"background":"#123","fontSize":14}],
            "schemes":[{"name":"Night","foreground":"#EEEEEE","background":"#000000","cursorColor":"#FF0000"}]}"##;
        let s = source("windows-terminal", "Windows Terminal", settings, None);
        assert_eq!(s.theme_label.as_deref(), Some("Night"), "짝이면 어둡게 쪽");
        assert_eq!(s.background.as_deref(), Some("#112233"), "프로필의 바탕색이 테마를 덮는다");
        assert_eq!(s.cursor.as_deref(), Some("#ff0000"));
        assert_eq!(s.font_size, Some(14.0));
    }

    #[test]
    fn comments_and_trailing_commas_inside_strings_survive() {
        let v = parse_jsonc("{ \"a\": \"http://x/*y*/,]\", // c\n \"b\": [1, 2,], }").unwrap();
        assert_eq!(v["a"], "http://x/*y*/,]");
        assert_eq!(v["b"], serde_json::json!([1, 2]));
    }

    /// 이 기기에 설치된 Windows Terminal 을 실제로 읽는다(없는 기기에선 「없음」 한 줄이면 된다).
    #[cfg(windows)]
    #[test]
    fn the_installed_terminal_reads_without_inventing_values() {
        let sources = sources();
        let stable = &sources[0];
        assert_eq!(stable.id, "windows-terminal");
        if stable.detected {
            assert_eq!(stable.ansi16.len(), 16, "{stable:?}");
            assert!(stable.font_family.is_some());
        }
    }
}
