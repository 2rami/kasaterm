use super::*;
use std::sync::{Mutex, OnceLock};
use std::sync::atomic::{AtomicBool, Ordering};

static PICKING: AtomicBool = AtomicBool::new(false);

fn cache() -> &'static Mutex<Option<(Instant, Vec<crate::chrome::PetModel>)>> {
    static CACHE: OnceLock<Mutex<Option<(Instant, Vec<crate::chrome::PetModel>)>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

pub(crate) fn invalidate() { *cache().lock().unwrap() = None; }

pub(crate) fn models() -> Vec<crate::chrome::PetModel> {
    let mut cache = cache().lock().unwrap();
    if let Some((at, models)) = &*cache {
        if at.elapsed() < std::time::Duration::from_millis(250) { return models.clone(); }
    }
    let models = crate::chrome::pet_models();
    *cache = Some((Instant::now(), models.clone()));
    models
}

fn pick(folder: bool) -> Result<Option<std::path::PathBuf>, String> {
    #[cfg(target_os = "macos")]
    let output = crate::proc::command("osascript").args(["-e", if folder {
        "try\nPOSIX path of (choose folder with prompt \"Live2D 모델 폴더 선택\")\non error number -128\nreturn \"\"\nend try"
    } else {
        "try\nPOSIX path of (choose file of type {\"zip\"} with prompt \"받아 둔 Live2D 모델 ZIP 선택\")\non error number -128\nreturn \"\"\nend try"
    }]).output();
    #[cfg(windows)]
    let output = crate::proc::command("powershell.exe").args(["-NoProfile", "-STA", "-Command", if folder {
        "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; Add-Type -AssemblyName System.Windows.Forms; $p=New-Object System.Windows.Forms.FolderBrowserDialog; if($p.ShowDialog() -eq 'OK'){[Console]::Write($p.SelectedPath)}"
    } else {
        "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8; Add-Type -AssemblyName System.Windows.Forms; $p=New-Object System.Windows.Forms.OpenFileDialog; $p.Filter='Live2D ZIP (*.zip)|*.zip'; if($p.ShowDialog() -eq 'OK'){[Console]::Write($p.FileName)}"
    }]).output();
    #[cfg(not(any(target_os = "macos", windows)))]
    let output = {
        let mut command = crate::proc::command("zenity");
        command.args(["--file-selection", "--title=Live2D 모델 선택"]);
        if folder { command.arg("--directory"); } else { command.arg("--file-filter=ZIP | *.zip"); }
        command.output()
    };
    let output = output.map_err(|error| format!("파일 선택 창을 열지 못했어요: {error}"))?;
    if !output.status.success() {
        if output.status.code() == Some(1) { return Ok(None); }
        return Err("파일 선택을 완료하지 못했어요".into());
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok((!path.is_empty()).then(|| path.into()))
}

impl App {
    pub(crate) fn import_pet_model(&mut self, name: String, folder: bool) {
        if PICKING.swap(true, Ordering::SeqCst) { return; }
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = pick(folder).and_then(|path| {
                let Some(path) = path else { return Ok(None) };
                let name = if name.is_empty() { path.file_stem().and_then(|v| v.to_str()).unwrap_or_default().to_string() } else { name };
                crate::chrome::install_pet_model_from(&name, &path)?;
                invalidate();
                Ok(Some("Live2D 모델을 설치하고 있어요".to_string()))
            });
            PICKING.store(false, Ordering::SeqCst);
            if let Some(message) = match result { Ok(message) => message, Err(error) => Some(error) } {
                let _ = proxy.send_event(UserEvent::RepoCatchup(message));
            }
        });
    }

    pub(crate) fn tick_pet_downloads(&mut self) {
        let active = crate::chrome::pet_download_active() || crate::chrome::pet_start_pending();
        let now = Instant::now();
        let repaint = if active {
            let at = self.statusbar.pet_refresh.get_or_insert(now);
            if now >= *at { *at = now + std::time::Duration::from_millis(250); true } else { false }
        } else { self.statusbar.pet_refresh.take().is_some() };
        if repaint {
            invalidate();
            self.chrome_dirty = true;
            if let Some(window) = &self.window { window.request_redraw(); }
        }
    }
}
