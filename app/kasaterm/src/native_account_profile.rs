//! 설정 「계정」의 프로필 — 프사·닉네임, 로그인 방법(Google·GitHub·아이디)마다 이 계정에 이어졌는지와
//! 그 메일·아이디, 로그인 아이디·비밀번호 바꾸기(docs/account-oauth.md 「프로필」). 관문 계정 열쇠는
//! 그대로고 사람이 보고 치는 이름만 바뀐다.

use super::*;
use std::time::{Duration, Instant};

const REFRESH: Duration = Duration::from_secs(60);
/// 화면에 올릴 프사 텍스처의 한 변(40 논리 px 의 레티나 세 배).
const FACE_PX: u32 = 120;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Form {
    Profile,
    Login,
    Password,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Act {
    Open(Form),
    Close,
    SaveNickname,
    PickPhoto,
    UseProvider(Provider),
    RemovePhoto,
    SaveLogin,
    SavePassword,
}

/// 원으로 잘라 둔 프사 RGBA. `key` 는 관문이 준 사진의 판(올린 그림 판·공급자 주소)이다.
#[derive(Clone)]
pub(crate) struct Face {
    key: String,
    rgba: Arc<Vec<u8>>,
}

type Job = Receiver<Result<(serde_json::Value, String), String>>;

#[derive(Default)]
pub(crate) struct State {
    data: serde_json::Value,
    error: Option<String>,
    fetched: Option<Instant>,
    fetch: Option<Receiver<Result<serde_json::Value, String>>>,
    busy: Option<Job>,
    pub(crate) form: Option<Form>,
    pub(crate) nickname: String,
    pub(crate) new_login: String,
    pub(crate) new_password: String,
    pub(crate) new_password2: String,
    message: Option<(String, bool)>,
    face: Option<Face>,
    face_want: Option<String>,
    face_fetch: Option<Receiver<(String, Option<Face>)>>,
    fixture: bool,
}

#[derive(Clone, Default)]
pub(crate) struct View {
    data: serde_json::Value,
    error: Option<String>,
    pub(crate) form: Option<Form>,
    nickname: String,
    new_login: String,
    new_password: String,
    new_password2: String,
    busy: bool,
    message: Option<(String, bool)>,
    face: Option<Face>,
}

fn readable(error: &str) -> String {
    match error {
        "bad_credentials" => "지금 비밀번호가 맞지 않아요".into(),
        "login_taken" => "이미 누가 쓰는 아이디예요. 다른 아이디를 골라 주세요".into(),
        "invalid_login" => "아이디는 소문자·숫자·-·_ 로 2~32자예요".into(),
        "weak_password" => format!("비밀번호는 {}자 이상으로 해 주세요", kasa_mcp::relay_auth::MIN_PASSWORD_CHARS),
        "rate_limited" => "시도가 많았어요. 잠시 뒤 다시 해 주세요".into(),
        "no_password" => "Google·GitHub 로 만든 계정이라 비밀번호가 없어요".into(),
        "invalid_nickname" => "닉네임은 40자까지예요".into(),
        "not_an_image" | "too_large" | "unreadable_image" => "PNG·JPEG·WebP 사진을 골라 주세요".into(),
        "update_required" => "관문이 이 기능을 아직 몰라요. 관문 업데이트가 필요해요".into(),
        "isolated_run" => "검증 실행에서는 실제 계정을 바꾸지 않아요".into(),
        "picker_failed" => "사진 고르는 창을 열지 못했어요".into(),
        "gateway_unreachable" | "invalid_response" => "관문에 닿지 못했어요. 연결 상태를 확인해 주세요".into(),
        _ => "요청을 마치지 못했어요. 다시 시도해 주세요".into(),
    }
}

impl State {
    pub(crate) fn view(&self) -> View {
        View {
            data: self.data.clone(),
            error: self.error.clone(),
            form: self.form,
            nickname: self.nickname.clone(),
            new_login: self.new_login.clone(),
            new_password: mask(&self.new_password),
            new_password2: mask(&self.new_password2),
            busy: self.busy.is_some(),
            message: self.message.clone(),
            face: self.face.clone(),
        }
    }

    pub(crate) fn refresh_now(&mut self) {
        self.fetched = None;
    }

    pub(crate) fn hide(&mut self) {
        self.form = None;
        self.new_password.clear();
        self.new_password2.clear();
    }

    /// 검증 실행 화면: Google 은 이어졌고 GitHub 은 아니며, 바꾼 아이디와 닉네임이 있다.
    pub(crate) fn fixture(&mut self, form: Option<Form>) {
        self.fixture = true;
        self.data = serde_json::json!({"ok":true,"account":"fixture","login":"kasa","has_password":true,
            "nickname":"건호","display_name":"건호","avatar":null,
            "identities":[{"provider":"google","display":"me@example.com","picture":null}]});
        self.form = form;
        if form == Some(Form::Profile) {
            self.nickname = "건호".into();
        } else if form == Some(Form::Login) {
            self.new_login = "kasa".into();
        }
        self.face = fixture_face();
    }

    fn poll(&mut self, visible: bool, signed_in: bool) -> bool {
        let mut changed = false;
        if visible && signed_in && !self.fixture && self.fetch.is_none()
            && self.fetched.is_none_or(|at| at.elapsed() >= REFRESH)
        {
            self.fetched = Some(Instant::now());
            let (tx, rx) = mpsc::channel();
            self.fetch = Some(rx);
            std::thread::spawn(move || {
                let result = kasa_mcp::device_auth::handle(&serde_json::json!({"op":"profile"}))
                    .map_err(|error| error.to_string());
                let _ = tx.send(result);
            });
        }
        if !signed_in && !self.fixture && !self.data.is_null() {
            *self = State::default();
            return true;
        }
        if let Some(Ok(result)) = self.fetch.as_ref().map(Receiver::try_recv) {
            self.fetch = None;
            changed = true;
            match result {
                Ok(data) => {
                    self.data = data;
                    self.error = None;
                }
                Err(error) => self.error = Some(error),
            }
        }
        if let Some(Ok(result)) = self.busy.as_ref().map(Receiver::try_recv) {
            self.busy = None;
            changed = true;
            match result {
                Ok((data, message)) => {
                    if !data.is_null() {
                        self.data = data;
                    }
                    self.hide();
                    self.message = Some((message, false));
                }
                Err(error) => self.message = Some((readable(&error), true)),
            }
        }
        if let Some(Ok((key, face))) = self.face_fetch.as_ref().map(Receiver::try_recv) {
            self.face_fetch = None;
            changed = true;
            if self.face_want.as_deref() == Some(key.as_str()) {
                self.face = face;
            }
        }
        if !self.fixture {
            changed |= self.want_face();
        }
        changed
    }

    /// 관문이 고른 사진이 바뀌었으면 새로 받는다. 받는 동안은 앞 사진을 그대로 둔다.
    fn want_face(&mut self) -> bool {
        let avatar = self.data["avatar"].clone();
        let key = match avatar["source"].as_str() {
            Some("upload") => avatar["rev"].as_str().map(|rev| format!("upload:{rev}")),
            Some(_) => avatar["url"].as_str().map(str::to_string),
            None => None,
        };
        if key == self.face_want || self.face_fetch.is_some() {
            return false;
        }
        self.face_want = key.clone();
        let Some(key) = key else {
            self.face = None;
            return true;
        };
        let (tx, rx) = mpsc::channel();
        self.face_fetch = Some(rx);
        std::thread::spawn(move || {
            let face = kasa_mcp::device_auth::profile::picture(&avatar)
                .ok()
                .and_then(|bytes| round_face(&bytes))
                .map(|rgba| Face { key: key.clone(), rgba: Arc::new(rgba) });
            let _ = tx.send((key, face));
        });
        false
    }

    fn run(&mut self, work: impl FnOnce() -> Result<(serde_json::Value, String), String> + Send + 'static) {
        let (tx, rx) = mpsc::channel();
        self.busy = Some(rx);
        self.message = None;
        std::thread::spawn(move || {
            let _ = tx.send(work());
        });
    }
}

fn call(params: serde_json::Value, done: &'static str) -> Result<(serde_json::Value, String), String> {
    kasa_mcp::device_auth::handle(&params)
        .map(|value| (value, done.to_string()))
        .map_err(|error| error.to_string())
}

/// 가운데를 정사각형으로 잘라 줄인다.
fn square(bytes: &[u8], side: u32) -> Option<image::RgbaImage> {
    let image = image::load_from_memory(bytes).ok()?;
    let (w, h) = (image.width(), image.height());
    let edge = w.min(h);
    if edge == 0 {
        return None;
    }
    let cropped = image.crop_imm((w - edge) / 2, (h - edge) / 2, edge, edge);
    Some(cropped.resize_exact(side, side, image::imageops::FilterType::Triangle).to_rgba8())
}

/// 원 밖을 투명하게 — 가장자리 한 픽셀은 덮인 만큼만 비친다.
fn round_face(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut image = square(bytes, FACE_PX)?;
    let r = FACE_PX as f32 / 2.0;
    for (px, py, pixel) in image.enumerate_pixels_mut() {
        let d = ((px as f32 + 0.5 - r).powi(2) + (py as f32 + 0.5 - r).powi(2)).sqrt();
        let cover = (r - d + 0.5).clamp(0.0, 1.0);
        pixel[3] = (pixel[3] as f32 * cover) as u8;
    }
    Some(image.into_raw())
}

fn fixture_face() -> Option<Face> {
    let bytes = crate::sprites::student_profile_png("arona")?;
    Some(Face { key: "fixture".into(), rgba: Arc::new(round_face(bytes)?) })
}

/// 고른 사진을 관문에 올릴 크기(정사각 256 PNG)로 만든다.
fn upload_bytes(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let image = square(bytes, kasa_mcp::device_auth::profile::AVATAR_PX).ok_or("unreadable_image")?;
    let mut out = std::io::Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Png).map_err(|_| "unreadable_image")?;
    Ok(out.into_inner())
}

fn pick_photo() -> Result<Option<Vec<u8>>, String> {
    #[cfg(target_os = "macos")]
    let output = crate::proc::command("osascript")
        .args([
            "-e",
            r#"try
POSIX path of (choose file of type {"png", "jpg", "jpeg", "webp"} with prompt "프사로 쓸 사진 선택")
on error number -128
return ""
end try"#,
        ])
        .output();
    #[cfg(windows)]
    let output = crate::proc::command("powershell.exe").args(["-NoProfile", "-STA", "-Command",
        "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; Add-Type -AssemblyName System.Windows.Forms; $picker = New-Object System.Windows.Forms.OpenFileDialog; $picker.Filter = 'Image (*.png;*.jpg;*.jpeg;*.webp)|*.png;*.jpg;*.jpeg;*.webp'; if ($picker.ShowDialog() -eq 'OK') { [Console]::Write($picker.FileName) }"]).output();
    #[cfg(not(any(target_os = "macos", windows)))]
    let output = crate::proc::command("zenity")
        .args(["--file-selection", "--title=프사로 쓸 사진 선택", "--file-filter=Image | *.png *.jpg *.jpeg *.webp"])
        .output();
    let output = output.map_err(|_| "picker_failed")?;
    #[cfg(not(any(target_os = "macos", windows)))]
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    if !output.status.success() {
        return Err("picker_failed".into());
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        return Ok(None);
    }
    let metadata = std::fs::metadata(&path).map_err(|_| "unreadable_image")?;
    if !metadata.is_file() || metadata.len() > 30 * 1024 * 1024 {
        return Err("too_large".into());
    }
    std::fs::read(path).map(Some).map_err(|_| "unreadable_image".into())
}

impl App {
    pub(crate) fn account_profile_poll(&mut self) -> bool {
        let visible = self.settings_room_active();
        let signed_in = self.device_account.status["logged_in"] == true;
        self.device_account.profile.poll(visible, signed_in)
    }

    pub(crate) fn account_profile_action(&mut self, act: Act) {
        let account = &mut self.device_account;
        if account.profile.busy.is_some() {
            return;
        }
        account.profile.message = None;
        match act {
            Act::Open(form) => {
                account.password.clear();
                let profile = &mut account.profile;
                profile.form = Some(form);
                profile.new_password.clear();
                profile.new_password2.clear();
                let field = match form {
                    Form::Profile => {
                        profile.nickname = profile.data["nickname"].as_str().unwrap_or("").into();
                        SettingsInput::DeviceAccountNickname
                    }
                    Form::Login => {
                        profile.new_login = profile.data["login"].as_str().unwrap_or("").into();
                        SettingsInput::DeviceAccountNewLogin
                    }
                    Form::Password => SettingsInput::DeviceAccountPassword,
                };
                self.native_settings_focus(field);
            }
            Act::Close => {
                account.password.clear();
                account.profile.hide();
                self.native_settings_blur();
            }
            Act::SaveNickname => {
                self.native_settings_blur();
                let nickname = self.device_account.profile.nickname.trim().to_string();
                self.device_account.profile.run(move || {
                    call(serde_json::json!({"op":"profile_update","nickname":nickname}), "닉네임을 바꿨어요")
                });
            }
            Act::UseProvider(provider) => {
                let done = if provider == Provider::Google { "Google 사진으로 바꿨어요" } else { "GitHub 사진으로 바꿨어요" };
                account.profile.run(move || {
                    call(serde_json::json!({"op":"profile_update","avatar":provider.name()}), done)
                });
            }
            Act::RemovePhoto => {
                account.profile.run(|| call(serde_json::json!({"op":"avatar_remove"}), "올린 사진을 지웠어요"));
            }
            Act::PickPhoto => {
                account.profile.run(|| {
                    let Some(bytes) = pick_photo()? else {
                        return Ok((serde_json::Value::Null, "사진을 고르지 않았어요".into()));
                    };
                    let png = upload_bytes(&bytes)?;
                    kasa_mcp::device_auth::profile::upload(png)
                        .map(|value| (value, "사진을 바꿨어요".to_string()))
                        .map_err(|error| error.to_string())
                });
            }
            Act::SaveLogin => {
                let login = account.profile.new_login.trim().to_lowercase();
                let password = std::mem::take(&mut account.password);
                if login.is_empty() || password.is_empty() {
                    account.profile.message = Some(("새 아이디와 지금 비밀번호를 입력해 주세요".into(), true));
                } else {
                    self.native_settings_blur();
                    self.device_account.profile.run(move || {
                        call(serde_json::json!({"op":"login_change","login":login,"password":password}),
                            "로그인 아이디를 바꿨어요. 다음 로그인부터 새 아이디를 써요")
                    });
                }
            }
            Act::SavePassword => {
                let profile = &mut account.profile;
                let message = if account.password.is_empty() || profile.new_password.is_empty() {
                    Some("지금 비밀번호와 새 비밀번호를 입력해 주세요".to_string())
                } else if profile.new_password != profile.new_password2 {
                    Some("새 비밀번호 두 칸이 달라요".to_string())
                } else if profile.new_password.chars().count() < kasa_mcp::relay_auth::MIN_PASSWORD_CHARS {
                    Some(readable("weak_password"))
                } else {
                    None
                };
                if let Some(message) = message {
                    profile.message = Some((message, true));
                } else {
                    let password = std::mem::take(&mut account.password);
                    let new_password = std::mem::take(&mut profile.new_password);
                    profile.new_password2.clear();
                    self.native_settings_blur();
                    self.device_account.profile.run(move || {
                        call(serde_json::json!({"op":"password_change","password":password,"new_password":new_password}),
                            "비밀번호를 바꿨어요. 다른 기기의 로그인은 그대로예요")
                    });
                }
            }
        }
        self.chrome_dirty = true;
    }
}

fn act(a: Act) -> Target {
    Target::Setting(SettingsAction::DeviceAccount(Action::Profile(a)))
}

/// 오른쪽 끝에 붙는 버튼 줄. 이름 폭만큼 버튼을 세우고 왼쪽 끝 x 를 돌려준다.
fn right_buttons(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    right: f32,
    y: f32,
    buttons: &[(&str, Target, bool)],
) -> f32 {
    let mut bx = right;
    for (label, target, primary) in buttons.iter().rev() {
        let bw = g.measure_chrome_text(label, 12.0, *primary) + 32.0;
        bx -= bw;
        button(g, s, hits, (bx, y, bw, CTL_H), label, target.clone(), *primary);
        bx -= 8.0;
    }
    bx + 8.0
}

/// 왼쪽부터 흘러가다 넘치면 다음 줄로 내리는 버튼 줄.
fn flow_buttons(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    x: f32,
    y: &mut f32,
    w: f32,
    buttons: &[(&str, Target, bool)],
) {
    let mut bx = x;
    for (label, target, primary) in buttons {
        let bw = (g.measure_chrome_text(label, 12.0, *primary) + 32.0).min(w);
        if bx > x && bx + bw > x + w {
            bx = x;
            *y += ROW_H;
        }
        button(g, s, hits, (bx, *y, bw, CTL_H), label, target.clone(), *primary);
        bx += bw + 8.0;
    }
    *y += ROW_H;
}

fn face(g: &mut gpu::GpuRenderer, v: &View, x: f32, y: f32, size: f32, name: &str) {
    if let Some(face) = &v.face {
        let key = format!("account-face:{}", face.key);
        if !g.has_image(&key) {
            g.upload_image(&key, &face.rgba, FACE_PX, FACE_PX);
        }
        g.queue_image_above(&key, x, y, size, size);
        return;
    }
    circle_rect(g, x, y, size, theme::surface_hover());
    let initial: String = name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
    let tw = g.measure_chrome_text(&initial, 15.0, true);
    draw_text(g, x + (size - tw) / 2.0, y + size / 2.0 - 10.0, &initial, 15.0, theme::text_dim(), true);
}

fn name_of(data: &serde_json::Value, status: &serde_json::Value) -> String {
    [&data["nickname"], &data["display_name"], &data["login"], &status["display_name"], &status["account"]]
        .into_iter()
        .find_map(|value| value.as_str().filter(|name| !name.is_empty()))
        .unwrap_or("KASA 계정")
        .to_string()
}

/// 얼굴 한 줄(56): 사진 40 · 닉네임 13 굵게 · 아이디 10.5, 오른쪽에 「프로필 바꾸기」. 펼치면 닉네임·사진.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_profile(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    caret: &mut Option<Rect>,
    x: f32,
    y: &mut f32,
    w: f32,
) {
    let v = &s.device_account.profile;
    let status = &s.device_account.status;
    let name = name_of(&v.data, status);
    face(g, v, x, *y + 8.0, 40.0, &name);
    let open = v.form == Some(Form::Profile);
    let label_w = g.measure_chrome_text("프로필 바꾸기", 12.0, false) + 32.0;
    let text_w = w - 52.0 - if open || v.busy { 0.0 } else { label_w + 12.0 };
    let shown = fit(g, &name, text_w, 13.0, true);
    draw_text(g, x + 52.0, *y + 10.0, &shown, 13.0, theme::text(), true);
    let sub = match v.data["login"].as_str() {
        Some(login) => format!("아이디 {login}"),
        None if v.data.is_null() => String::new(),
        None => "Google·GitHub 로 들어오는 계정".into(),
    };
    let sub = fit(g, &sub, text_w, 10.5, false);
    draw_text(g, x + 52.0, *y + 30.0, &sub, 10.5, theme::text_dim(), false);
    if !open && !v.busy && !v.data.is_null() {
        right_buttons(g, s, hits, x + w, *y + 15.0, &[("프로필 바꾸기", act(Act::Open(Form::Profile)), false)]);
    }
    *y += 56.0;
    if !open {
        return;
    }
    text_field(g, s, hits, caret, x, *y, w, "닉네임", &v.nickname,
        SettingsInput::DeviceAccountNickname, s.settings_caret, false);
    *y += ROW_H;
    let pictures: Vec<Provider> = v.data["identities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|identity| identity["picture"].is_string())
        .filter_map(|identity| match identity["provider"].as_str()? {
            "google" => Some(Provider::Google),
            "github" => Some(Provider::Github),
            _ => None,
        })
        .collect();
    let mut photo: Vec<(&str, Target, bool)> = vec![("사진 고르기", act(Act::PickPhoto), false)];
    for provider in pictures {
        let label = if provider == Provider::Google { "Google 사진" } else { "GitHub 사진" };
        photo.push((label, act(Act::UseProvider(provider)), false));
    }
    if v.data["avatar"]["source"] == "upload" {
        photo.push(("올린 사진 지우기", act(Act::RemovePhoto), false));
    }
    if v.busy {
        draw_text(g, x, *y + 4.0, "처리 중…", 12.0, theme::text_dim(), false);
        *y += ROW_H;
    } else {
        flow_buttons(g, s, hits, x, y, w, &photo);
        flow_buttons(g, s, hits, x, y, w, &[("닉네임 저장", act(Act::SaveNickname), true), ("닫기", act(Act::Close), false)]);
    }
    info_slab(g, x, y, w,
        "닉네임과 사진은 이 계정의 모든 기기·폰에 같이 보여요. 올린 사진이 없으면 이어진 Google·GitHub 사진을 써요.");
    *y += 8.0;
}

fn provider_row(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    x: f32,
    y: &mut f32,
    w: f32,
    provider: Provider,
) {
    let account = &s.device_account;
    let v = &account.profile;
    let identity = v.data["identities"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|identity| identity["provider"] == provider.name());
    let name = if provider == Provider::Google { "Google" } else { "GitHub" };
    if provider == Provider::Google {
        g.queue_icon_colored("google", x, *y + 12.0, 16.0, 1.0);
    } else {
        g.queue_icon("github", x, *y + 12.0, 16.0, theme::text());
    }
    let enabled = provider_enabled(&account.providers, provider);
    let (label, primary) = if identity.is_some() { ("다시 연결", false) } else { (if provider == Provider::Google { "Google 연결" } else { "GitHub 연결" }, true) };
    let label_w = g.measure_chrome_text(label, 12.0, primary) + 32.0;
    let text_w = w - 26.0 - label_w - 34.0;
    draw_text(g, x + 26.0, *y + 4.0, name, 12.0, theme::text(), false);
    let sub = match identity {
        Some(identity) => identity["display"].as_str().filter(|d| !d.is_empty()).unwrap_or("연결됨").to_string(),
        None if v.data.is_null() && v.error.is_none() => "확인 중…".into(),
        None if v.data.is_null() => "연결 상태를 확인하지 못했어요".into(),
        None => "연결 안 됨".into(),
    };
    let sub = fit(g, &sub, text_w, 10.5, false);
    draw_text(g, x + 26.0, *y + 21.0, &sub, 10.5, theme::text_dim(), false);
    let cy = *y + (ROW_H - CTL_H) / 2.0;
    let left = if enabled && !account.busy {
        right_buttons(g, s, hits, x + w, cy, &[(label, Target::Setting(SettingsAction::DeviceAccount(Action::OAuth(provider))), primary)])
    } else {
        let rect = (x + w - label_w, cy, label_w, CTL_H);
        crate::native_controls::text_button(g, rect, s.cursor, label,
            crate::native_controls::Style { enabled: false, ..Default::default() });
        rect.0
    };
    if identity.is_some() {
        g.queue_icon("check", left - 24.0, *y + 13.0, 14.0, theme::success());
    }
    *y += ROW_H;
}

/// 「로그인 방법」 — Google·GitHub·아이디 줄. 이어진 것은 메일·아이디와 체크, 아니면 「연결」.
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_methods(
    g: &mut gpu::GpuRenderer,
    s: &Snapshot,
    hits: &mut Vec<Hit>,
    caret: &mut Option<Rect>,
    x: f32,
    y: &mut f32,
    w: f32,
) {
    let account = &s.device_account;
    let v = &account.profile;
    draw_text(g, x, *y + 4.0, "로그인 방법", 11.0, theme::text_dim(), false);
    *y += 24.0;
    provider_row(g, s, hits, x, y, w, Provider::Google);
    provider_row(g, s, hits, x, y, w, Provider::Github);

    g.queue_icon("shield", x, *y + 12.0, 16.0, theme::text_dim());
    draw_text(g, x + 26.0, *y + 4.0, "아이디·비밀번호", 12.0, theme::text(), false);
    let login = v.data["login"].as_str();
    let buttons: Vec<(&str, Target, bool)> = if login.is_some() && v.form.is_none() && !v.busy {
        vec![("아이디 바꾸기", act(Act::Open(Form::Login)), false),
            ("비밀번호 바꾸기", act(Act::Open(Form::Password)), false)]
    } else {
        Vec::new()
    };
    let buttons_w: f32 = buttons.iter().map(|(l, _, p)| g.measure_chrome_text(l, 12.0, *p) + 40.0).sum();
    let fits = buttons_w <= w * 0.6;
    let text_w = w - 26.0 - if fits { buttons_w + 8.0 } else { 0.0 };
    let sub = match login {
        Some(login) => format!("아이디 {login}"),
        None if v.data.is_null() => String::new(),
        None => "없음 · Google·GitHub 로만 들어와요".into(),
    };
    let sub = fit(g, &sub, text_w, 10.5, false);
    draw_text(g, x + 26.0, *y + 21.0, &sub, 10.5, theme::text_dim(), false);
    if fits {
        if !buttons.is_empty() {
            right_buttons(g, s, hits, x + w, *y + (ROW_H - CTL_H) / 2.0, &buttons);
        }
        *y += ROW_H;
    } else {
        *y += ROW_H;
        flow_buttons(g, s, hits, x + 26.0, y, w - 26.0, &buttons);
    }

    match v.form {
        Some(Form::Login) => {
            text_field(g, s, hits, caret, x, *y, w, "새 아이디", &v.new_login,
                SettingsInput::DeviceAccountNewLogin, s.settings_caret, false);
            *y += ROW_H;
            text_field(g, s, hits, caret, x, *y, w, "지금 비밀번호", &account.password_mask,
                SettingsInput::DeviceAccountPassword, s.settings_caret, false);
            *y += ROW_H;
            if !v.busy {
                flow_buttons(g, s, hits, x, y, w, &[("아이디 바꾸기", act(Act::SaveLogin), true), ("취소", act(Act::Close), false)]);
            }
            info_slab(g, x, y, w,
                "기기 로그인은 그대로예요. 옛 아이디는 로그인에 안 쓰이고, 다른 사람이 가져가지 못하게 묶어 둬요.");
        }
        Some(Form::Password) => {
            text_field(g, s, hits, caret, x, *y, w, "지금 비밀번호", &account.password_mask,
                SettingsInput::DeviceAccountPassword, s.settings_caret, false);
            *y += ROW_H;
            text_field(g, s, hits, caret, x, *y, w, "새 비밀번호", &v.new_password,
                SettingsInput::DeviceAccountNewPassword, s.settings_caret, false);
            *y += ROW_H;
            text_field(g, s, hits, caret, x, *y, w, "한 번 더", &v.new_password2,
                SettingsInput::DeviceAccountNewPassword2, s.settings_caret, false);
            *y += ROW_H;
            if !v.busy {
                flow_buttons(g, s, hits, x, y, w, &[("비밀번호 바꾸기", act(Act::SavePassword), true), ("취소", act(Act::Close), false)]);
            }
            info_slab(g, x, y, w, "이미 로그인한 다른 기기·폰은 그대로 쓸 수 있어요.");
        }
        _ => {}
    }
    if v.busy && v.form != Some(Form::Profile) {
        draw_text(g, x, *y + 4.0, "처리 중…", 12.0, theme::text_dim(), false);
        *y += ROW_H;
    }
    if let Some((message, error)) = &v.message {
        for line in wrap_words(g, message, w, 10.5) {
            draw_text(g, x, *y, &line, 10.5, if *error { theme::danger() } else { theme::text_dim() }, false);
            *y += 18.0;
        }
    }
    if v.error.as_deref() == Some("update_required") {
        info_slab(g, x, y, w, &readable("update_required"));
    }
    *y += 8.0;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(w, h, image::Rgba([200, 100, 50, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        image.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn a_picked_photo_becomes_a_square_png_and_the_face_is_round() {
        let upload = upload_bytes(&png(400, 300)).unwrap();
        let back = image::load_from_memory(&upload).unwrap();
        assert_eq!((back.width(), back.height()), (256, 256));
        assert!(upload_bytes(b"not an image").is_err());
        let face = round_face(&png(64, 64)).unwrap();
        let alpha = |x: u32, y: u32| face[((y * FACE_PX + x) * 4 + 3) as usize];
        assert_eq!(alpha(0, 0), 0, "a corner stayed visible");
        assert_eq!(alpha(FACE_PX / 2, FACE_PX / 2), 255);
    }

    #[test]
    fn secrets_are_masked_in_the_view_and_dropped_when_closing() {
        let mut state = State {
            new_password: "secret-one".into(),
            new_password2: "secret-one".into(),
            form: Some(Form::Password),
            ..State::default()
        };
        let view = state.view();
        assert!(!view.new_password.contains("secret"));
        assert_eq!(view.new_password.chars().count(), 10);
        state.hide();
        assert!(state.new_password.is_empty() && state.new_password2.is_empty() && state.form.is_none());
    }

    #[test]
    fn the_shown_name_prefers_the_nickname_then_the_login() {
        let status = serde_json::json!({"account":"oauth_abc"});
        assert_eq!(name_of(&serde_json::json!({"nickname":"건호","login":"kasa"}), &status), "건호");
        assert_eq!(name_of(&serde_json::json!({"login":"kasa"}), &status), "kasa");
        assert_eq!(name_of(&serde_json::Value::Null, &status), "oauth_abc");
    }
}
