use super::*;

pub(super) fn read_preview(
    engine: &kasa_mcp::share::Engine,
    path: &str,
) -> Result<Preview, String> {
    use std::io::Read;
    let full = engine.local_path(path).map_err(|error| error.to_string())?;
    match kasa_mcp::share::path::kind(path) {
        "image" => {
            if std::fs::metadata(&full)
                .map_err(|error| error.to_string())?
                .len()
                > 16 << 20
            {
                return Err("큰 이미지는 폴더에서 열어 주세요".into());
            }
            let mut reader = image::ImageReader::open(full)
                .map_err(|error| error.to_string())?
                .with_guessed_format()
                .map_err(|error| error.to_string())?;
            let mut limits = image::Limits::default();
            limits.max_alloc = Some(64 << 20);
            limits.max_image_width = Some(8192);
            limits.max_image_height = Some(8192);
            reader.limits(limits);
            let image = reader
                .decode()
                .map_err(|_| "이 이미지 형식은 폴더에서 열어 주세요".to_string())?
                .thumbnail(1024, 768)
                .to_rgba8();
            Ok(Preview::Image {
                width: image.width(),
                height: image.height(),
                rgba: image.into_raw(),
            })
        }
        "markdown" | "text" | "html" => {
            let mut bytes = Vec::new();
            std::fs::File::open(full)
                .map_err(|error| error.to_string())?
                .take(64 << 10)
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            Ok(Preview::Text(String::from_utf8_lossy(&bytes).into_owned()))
        }
        _ => Ok(Preview::Other),
    }
}

pub(super) fn perform(action: Action, selected: &str) -> Result<Option<String>, String> {
    let engine = kasa_mcp::share::engine().ok_or("공유 기능이 아직 준비되지 않았어요")?;
    match action {
        Action::Reveal | Action::CopyPath => {
            let path = engine
                .local_path(selected)
                .map_err(|error| error.to_string())?;
            if action == Action::CopyPath {
                arboard::Clipboard::new()
                    .and_then(|mut clipboard| {
                        clipboard.set_text(path.to_string_lossy().into_owned())
                    })
                    .map_err(|error| error.to_string())?;
                return Ok(Some("경로를 복사했어요".into()));
            }
            #[cfg(target_os = "macos")]
            let result = std::process::Command::new("/usr/bin/open")
                .arg("-R")
                .arg(path)
                .status();
            #[cfg(windows)]
            let result = std::process::Command::new("explorer.exe")
                .arg(format!("/select,{}", path.display()))
                .status();
            #[cfg(all(unix, not(target_os = "macos")))]
            let result = std::process::Command::new("xdg-open")
                .arg(if path.is_dir() {
                    path.as_path()
                } else {
                    path.parent().unwrap_or(&path)
                })
                .status();
            if !result.map_err(|error| error.to_string())?.success() {
                return Err("파일 관리자를 열지 못했어요".into());
            }
            Ok(None)
        }
        Action::Desktop | Action::HideShortcut | Action::ChooseLocation => {
            let location = match action {
                Action::Desktop => Location::Desktop,
                Action::HideShortcut => Location::Hidden,
                _ => match choose_folder()? {
                    Some(path) => Location::Folder(path),
                    None => return Ok(None),
                },
            };
            engine
                .set_shortcut(location)
                .map_err(|error| error.to_string())?;
            Ok(Some(
                "바로가기 위치를 바꿨어요. 공유 파일은 그대로예요".into(),
            ))
        }
        _ => Ok(None),
    }
}

fn choose_folder() -> Result<Option<std::path::PathBuf>, String> {
    #[cfg(target_os = "macos")]
    let output = std::process::Command::new("/usr/bin/osascript")
        .args([
            "-e",
            "POSIX path of (choose folder with prompt \"KASA-share 바로가기를 둘 폴더\")",
        ])
        .output();
    #[cfg(windows)]
    let output = std::process::Command::new("powershell.exe").args(["-NoLogo", "-NoProfile", "-STA", "-Command", "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; Add-Type -AssemblyName System.Windows.Forms; $d=New-Object System.Windows.Forms.FolderBrowserDialog; $d.Description='KASA-share shortcut'; if($d.ShowDialog() -eq 'OK'){[Console]::Write($d.SelectedPath)}"]).output();
    #[cfg(all(unix, not(target_os = "macos")))]
    let output = std::process::Command::new("zenity")
        .args(["--file-selection", "--directory", "--title=KASA-share"])
        .output();
    let output = output.map_err(|_| "폴더 선택창을 열지 못했어요".to_string())?;
    if !output.status.success() {
        return Ok(None);
    }
    let path = String::from_utf8(output.stdout)
        .map_err(|_| "선택한 폴더 이름을 읽지 못했어요".to_string())?;
    Ok((!path.trim().is_empty()).then(|| std::path::PathBuf::from(path.trim())))
}
