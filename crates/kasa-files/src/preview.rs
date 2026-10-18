//! 고른 항목 미리보기. 큰 파일은 앞부분만 읽는다.

use std::io::Read;
use std::path::Path;

const MAX_BYTES: u64 = 64 * 1024;
const MAX_LINES: usize = 200;

#[derive(Clone, Debug, PartialEq)]
pub enum Preview {
    Dir(Vec<String>),
    Text(Vec<String>),
    /// 터미널 글로는 못 보이는 것 — 종류와 크기만 알리고 열기를 권한다.
    Opaque { kind: &'static str, size: u64 },
    Error(String),
}

fn kind_by_extension(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "heic" | "tiff" => Some("그림"),
        "pdf" => Some("PDF"),
        "mp4" | "mov" | "webm" | "mkv" => Some("영상"),
        "mp3" | "wav" | "flac" | "m4a" | "ogg" => Some("소리"),
        "zip" | "gz" | "tar" | "xz" | "7z" | "dmg" => Some("압축 파일"),
        _ => None,
    }
}

pub fn size_label(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1}GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1}MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.1}KB", b as f64 / 1024.0),
        b => format!("{b}B"),
    }
}

/// 탭은 네 칸으로 펴고 제어 문자는 지운다 — 미리보기 글이 화면을 움직이면 안 된다.
fn clean(line: &str) -> String {
    line.replace('\t', "    ").chars().filter(|c| !c.is_control()).collect()
}

pub fn load(path: &Path, show_hidden: bool) -> Preview {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(error) => return Preview::Error(error.to_string()),
    };
    if meta.is_dir() {
        let mut names: Vec<(bool, String)> = match std::fs::read_dir(path) {
            Ok(entries) => entries
                .flatten()
                .map(|e| (e.path().is_dir(), e.file_name().to_string_lossy().into_owned()))
                .filter(|(_, name)| name != ".git" && (show_hidden || !name.starts_with('.')))
                .collect(),
            Err(error) => return Preview::Error(error.to_string()),
        };
        names.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));
        return Preview::Dir(
            names.into_iter().take(MAX_LINES).map(|(dir, name)| if dir { format!("{name}/") } else { name }).collect(),
        );
    }
    if let Some(kind) = kind_by_extension(path) {
        return Preview::Opaque { kind, size: meta.len() };
    }
    let mut bytes = Vec::new();
    if let Err(error) = std::fs::File::open(path).and_then(|f| f.take(MAX_BYTES).read_to_end(&mut bytes)) {
        return Preview::Error(error.to_string());
    }
    if bytes.contains(&0) {
        return Preview::Opaque { kind: "바이너리", size: meta.len() };
    }
    Preview::Text(String::from_utf8_lossy(&bytes).lines().take(MAX_LINES).map(clean).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_binary_media_and_folder_previews() {
        let dir = std::env::temp_dir().join(format!("kasa-files-preview-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.rs"), "fn main() {\n\tlet x = 1;\x1b[31m\n}\n").unwrap();
        std::fs::write(dir.join("blob.bin"), [1u8, 0, 2]).unwrap();
        std::fs::write(dir.join("shot.PNG"), [137u8, 80, 78, 71]).unwrap();
        std::fs::write(dir.join(".secret"), "x").unwrap();
        assert_eq!(
            load(&dir.join("a.rs"), false),
            Preview::Text(vec!["fn main() {".into(), "    let x = 1;[31m".into(), "}".into()])
        );
        assert_eq!(load(&dir.join("blob.bin"), false), Preview::Opaque { kind: "바이너리", size: 3 });
        assert_eq!(load(&dir.join("shot.PNG"), false), Preview::Opaque { kind: "그림", size: 4 });
        assert_eq!(
            load(&dir, false),
            Preview::Dir(vec!["sub/".into(), "a.rs".into(), "blob.bin".into(), "shot.PNG".into()])
        );
        assert!(matches!(load(&dir, true), Preview::Dir(names) if names.contains(&".secret".to_string())));
        assert!(matches!(load(&dir.join("missing"), false), Preview::Error(_)));
    }

    #[test]
    fn sizes_read_like_a_file_list() {
        assert_eq!(size_label(512), "512B");
        assert_eq!(size_label(1536), "1.5KB");
        assert_eq!(size_label(3 << 20), "3.0MB");
    }
}
