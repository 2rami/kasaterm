//! KASA-share 안의 이름 규칙. 목록에 오가는 경로는 `/` 로 잇고 NFC 로 적는다 —
//! 맥은 한글 이름을 NFD 로 두기도 해서, 그대로 보내면 윈도우에서 다른 파일이 된다.

use unicode_normalization::UnicodeNormalization;

pub fn nfc(path: &str) -> String {
    path.nfc().collect()
}

/// 같은 파일인지 가르는 열쇠. 맥·윈도우 둘 다 대소문자를 안 가리므로 열쇠도 안 가린다.
pub fn key(path: &str) -> String {
    nfc(path).to_lowercase()
}

/// 동기화가 건드리지 않는 이름 — 숨김·쓰는 중인 임시 파일·OS 가 흘리는 부스러기.
pub fn ignored(name: &str) -> bool {
    let lower = name.to_lowercase();
    name.starts_with('.')
        || name.starts_with("~$")
        || name == "Icon\r"
        || lower == "thumbs.db"
        || lower == "desktop.ini"
        || [".tmp", ".part", ".crdownload"].iter().any(|ext| lower.ends_with(ext))
}

/// 다른 기기가 보낸 경로를 이 기기 디스크에 써도 되는가. 어느 OS 에서든 지킨다 —
/// 어긋나면 공유 폴더 밖에 쓰게 된다.
pub fn safe(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains('\0')
        && path.split('/').all(|part| !part.is_empty() && part != "." && part != ".." && !ignored(part))
        && !path.split('/').next().is_some_and(|first| first.len() == 2 && first.ends_with(':'))
}

const RESERVED: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// 윈도우가 담을 수 있는 이름인가. 맥에서 만든 `a:b.png` 같은 이름은 윈도우에서만
/// 건너뛴다 — 바꿔 적으면 새 경로가 생겨 원래 기기로 되돌아간다.
pub fn portable(path: &str) -> bool {
    path.split('/').all(|part| {
        let stem = part.split('.').next().unwrap_or(part).to_lowercase();
        !part.chars().any(|c| c < ' ' || "<>:\"|?*".contains(c))
            && !part.ends_with('.')
            && !part.ends_with(' ')
            && !RESERVED.contains(&stem.as_str())
    })
}

/// 충돌에서 진 쪽 내용이 갈 이름: `dir/name (기기).ext`. 이미 있으면 ` 2`, ` 3` …
pub fn conflict_name(path: &str, label: &str, taken: impl Fn(&str) -> bool) -> String {
    let (dir, file) = match path.rsplit_once('/') {
        Some((dir, file)) => (format!("{dir}/"), file),
        None => (String::new(), path),
    };
    let (stem, ext) = match file.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (file, String::new()),
    };
    let label: String = label.chars().map(|c| if "/\\<>:\"|?*".contains(c) { '-' } else { c }).collect();
    let mut n = 1;
    loop {
        let tag = if n == 1 { label.clone() } else { format!("{label} {n}") };
        let candidate = format!("{dir}{stem} ({tag}){ext}");
        if !taken(&key(&candidate)) {
            return candidate;
        }
        n += 1;
    }
}

/// 폰이 어떻게 열지 고르는 종류.
pub fn kind(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "heic" => "image",
        "md" | "markdown" => "markdown",
        "html" | "htm" => "html",
        "txt" | "log" | "json" | "csv" | "tsv" | "yaml" | "yml" | "toml" | "xml" | "rs" | "py"
        | "js" | "ts" | "dart" | "sh" | "swift" | "kt" | "go" | "java" | "c" | "h" | "cpp" | "css" => "text",
        "pdf" => "pdf",
        "mp4" | "mov" | "webm" | "m4v" => "video",
        "mp3" | "wav" | "m4a" | "aac" | "ogg" | "flac" => "audio",
        _ => "other",
    }
}

pub fn content_type(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "heic" => "image/heic",
        "svg" => "image/svg+xml",
        "md" | "markdown" => "text/markdown; charset=utf-8",
        "html" | "htm" => "text/html; charset=utf-8",
        "json" => "application/json",
        // nosniff 를 달아 두었으니 형식이 틀리면 브라우저가 html 시안의 스타일을 버린다.
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "pdf" => "application/pdf",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        _ if kind(path) == "text" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn korean_nfd_and_case_map_to_one_key() {
        let nfd: String = "시안/A.png".nfd().collect();
        assert_ne!(nfd, "시안/A.png");
        assert_eq!(key(&nfd), key("시안/a.PNG"));
        assert_eq!(nfc(&nfd), "시안/A.png");
    }

    #[test]
    fn unsafe_paths_are_rejected() {
        for bad in ["", "/etc/passwd", "../x", "a/../b", "a//b", "C:/x", "a\\b", "./a", ".kasaterm/index.json", "a/.git/x", "a/b.part"] {
            assert!(!safe(bad), "{bad:?}");
        }
        assert!(safe("2026-09-28-시안/1 짝눈.png"));
    }

    #[test]
    fn windows_only_names() {
        for bad in ["a:b.png", "CON.txt", "con", "x/lpt1.log", "끝점.", "끝공백 ", "q?.md"] {
            assert!(!portable(bad), "{bad:?}");
        }
        assert!(portable("2026-09-28-시안/console.txt"));
    }

    #[test]
    fn ignore_list() {
        for name in [".DS_Store", "~$문서.docx", "a.tmp", "b.PART", "c.crdownload", "Thumbs.db", "desktop.ini", "Icon\r"] {
            assert!(ignored(name), "{name:?}");
        }
        assert!(!ignored("시안.png"));
    }

    #[test]
    fn conflict_names() {
        assert_eq!(conflict_name("d/a.png", "맥미니", |_| false), "d/a (맥미니).png");
        assert_eq!(conflict_name("README", "A", |_| false), "README (A)");
        assert_eq!(conflict_name(".hidden", "A", |_| false), ".hidden (A)");
        let taken = key("d/a (A).png");
        assert_eq!(conflict_name("d/a.png", "A", |k| k == taken), "d/a (A 2).png");
        assert_eq!(conflict_name("a.png", "x/y:z", |_| false), "a (x-y-z).png");
    }

    #[test]
    fn kinds() {
        assert_eq!(kind("a/b.PNG"), "image");
        assert_eq!(kind("readme.md"), "markdown");
        assert_eq!(kind("x.svg"), "other");
        assert_eq!(content_type("x.rs"), "text/plain; charset=utf-8");
        assert_eq!(content_type("a/style.CSS"), "text/css; charset=utf-8");
    }
}
