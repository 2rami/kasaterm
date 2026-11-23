//! 검증 리그 손잡이 — env 가 있을 때만 켜진다. 보기마다 1초에 한 줄 상태(JSON)를 쓰고, 정한 때에 창을 PNG 로 뜨고, 정한
//! 조작(키·글·누름)을 그 기기에 넣고, 정한 때에 끝난다. 조작은 `ScreenView` 의 보내기를 바로 부른다 — winit 키 판정은 거치지 않는다.

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use kasa_castview::ScreenView;
use kasa_gridview::GridRenderer;
use serde_json::json;
use winit::keyboard::KeyCode;

enum Action {
    Key(String, u8),
    Text(String),
    /// 그 기기 화면 안 비율(0–1) — 보이는 장 자리로 옮겨 누른다.
    Click(f64, f64),
}

pub enum PickerAction {
    Hover(f64, f64),
    Click(f64, f64),
    Key(KeyCode),
}

impl PickerAction {
    fn parse(action: &str, argument: &str) -> Option<Self> {
        match action {
            "hover" | "click" => {
                let (x, y) = argument.split_once(',')?;
                let (x, y) = (x.parse::<f64>().ok()?, y.parse::<f64>().ok()?);
                if !(0.0..1.0).contains(&x) || !(0.0..1.0).contains(&y) {
                    return None;
                }
                Some(if action == "hover" { Self::Hover(x, y) } else { Self::Click(x, y) })
            }
            "key" => Some(Self::Key(match argument {
                "Left" => KeyCode::ArrowLeft,
                "Right" => KeyCode::ArrowRight,
                "Up" => KeyCode::ArrowUp,
                "Down" => KeyCode::ArrowDown,
                "Enter" => KeyCode::Enter,
                _ => return None,
            })),
            _ => None,
        }
    }
}

pub struct Rig {
    started: Instant,
    log: Option<PathBuf>,
    logged: Option<Instant>,
    quit_at: Option<Instant>,
    capture: Option<(Instant, String)>,
    input: Vec<(Instant, Action)>,
    picker_input: Option<Vec<(Instant, PickerAction)>>,
    /// 키·글은 이 파일이 생긴 뒤에만 보낸다 — 리그가 시험 창이 맨 앞인 것을 확인하고 만든다(사람 앱으로 키가 새지 않게).
    gate: Option<PathBuf>,
    pub silent_audio: bool,
    /// 띄우자마자 로그인 판을 연다 — 판 모양을 뜨는 리그.
    pub login_panel: bool,
    /// 띄우자마자 이 아이디·비밀번호로 로그인한다(`아이디:비밀번호`) — 로그인 판의 「로그인」과 같은 길.
    pub login: Option<(String, String)>,
}

fn ms(v: &str) -> Option<Duration> {
    v.trim().parse::<u64>().ok().map(Duration::from_millis)
}

impl Rig {
    pub fn from_env() -> Self {
        let started = Instant::now();
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let capture = env("KASATERM_VIEW_TEST_CAPTURE").and_then(|v| {
            let (at, path) = v.split_once(':')?;
            Some((started + ms(at)?, path.to_string()))
        });
        // `ms:key:KeyA:mods;ms:text:가;ms:click:fx,fy` — 누름 자리는 그 기기 화면 안 비율.
        let input = env("KASATERM_VIEW_TEST_INPUT")
            .map(|v| {
                v.split(';')
                    .filter_map(|step| {
                        let mut parts = step.splitn(3, ':');
                        let at = started + ms(parts.next()?)?;
                        let action = match (parts.next()?, parts.next()?) {
                            ("key", rest) => {
                                let (k, m) = rest.split_once(':').unwrap_or((rest, "0"));
                                Action::Key(k.to_string(), m.parse().ok()?)
                            }
                            ("text", rest) => Action::Text(rest.to_string()),
                            ("click", rest) => {
                                let (x, y) = rest.split_once(',')?;
                                Action::Click(x.parse().ok()?, y.parse().ok()?)
                            }
                            _ => return None,
                        };
                        Some((at, action))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let picker_input = env("KASATERM_VIEW_TEST_PICKER_INPUT")
            .filter(|_| env("KASATERM_VIEW_STATE").is_some() && env("KASATERM_VIEW_KEY").is_some())
            .map(|v| v.split(';').filter_map(|step| {
                let mut parts = step.splitn(3, ':');
                let at = started + ms(parts.next()?)?;
                let action = PickerAction::parse(parts.next()?, parts.next()?)?;
                Some((at, action))
            }).collect());
        Self {
            started,
            log: env("KASATERM_VIEW_TEST_LOG").map(PathBuf::from),
            logged: None,
            quit_at: env("KASATERM_VIEW_TEST_QUIT_MS").and_then(|v| ms(&v)).map(|d| started + d),
            capture,
            input,
            picker_input,
            gate: env("KASATERM_VIEW_TEST_INPUT_GATE").map(PathBuf::from),
            silent_audio: env("KASATERM_TEST_SCREEN_AUDIO_SILENT").is_some(),
            login_panel: env("KASATERM_VIEW_TEST_LOGIN_PANEL").is_some(),
            login: env("KASATERM_VIEW_TEST_LOGIN").and_then(|v| v.split_once(':').map(|(a, b)| (a.to_string(), b.to_string()))),
        }
    }

    /// 루프 한 바퀴마다. 끝낼 때면 true.
    pub fn tick(&mut self, views: &mut [&mut ScreenView]) -> bool {
        let now = Instant::now();
        if let Some(view) = views.first_mut() {
            let due: Vec<Action> = {
                let (due, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.input).into_iter().partition(|(at, _)| *at <= now);
                self.input = rest;
                due.into_iter().map(|(_, a)| a).collect()
            };
            for action in due {
                let gated = matches!(action, Action::Key(..) | Action::Text(_));
                if gated && self.gate.as_ref().is_some_and(|g| !g.exists()) {
                    if self.started.elapsed() < Duration::from_secs(120) {
                        self.input.push((now + Duration::from_millis(300), action));
                    } else {
                        self.write(json!({ "t": self.started.elapsed().as_secs_f64(), "input": "gate closed — dropped" }));
                    }
                    continue;
                }
                let line = match action {
                    Action::Key(k, m) => {
                        view.key(&k, m);
                        format!("key {k} {m}")
                    }
                    Action::Text(t) => {
                        view.text(&t);
                        format!("text {t}")
                    }
                    Action::Click(fx, fy) => {
                        let Some(l) = view.layout else {
                            self.input.push((now + Duration::from_millis(500), Action::Click(fx, fy)));
                            continue;
                        };
                        let (x, y, w, h) = l.image;
                        let (u0, v0, u1, v1) = l.uv;
                        let p = (
                            f64::from(x) + (fx - f64::from(u0)) / f64::from(u1 - u0) * f64::from(w),
                            f64::from(y) + (fy - f64::from(v0)) / f64::from(v1 - v0) * f64::from(h),
                        );
                        view.pointer(p, 1, 0);
                        view.pointer(p, 0, 0);
                        format!("click {fx},{fy}")
                    }
                };
                self.write(json!({ "t": self.started.elapsed().as_secs_f64(), "input": line }));
            }
        }
        if self.log.is_some() && self.logged.is_none_or(|at| now.duration_since(at) >= Duration::from_secs(1)) {
            self.logged = Some(now);
            for v in views.iter() {
                let line = json!({
                    "t": self.started.elapsed().as_secs_f64(),
                    "label": v.label,
                    "status": v.status(),
                    "has_frame": v.has_frame(),
                    "hud": v.hud(),
                    "uploads": v.uploads,
                    "upload_ms": v.upload_ms,
                    "zero_copy": v.zero_copy,
                    "frame": v.frame_size(),
                    "windows": v.remote_windows(),
                    "input_note": v.input_note(),
                    "audio": v.audio.stats(),
                });
                self.write(line);
            }
        }
        self.quit_at.is_some_and(|at| now >= at)
    }

    pub fn next_wake(&self, now: Instant) -> Option<Instant> {
        [self.quit_at, self.capture.as_ref().map(|(at, _)| *at)]
            .into_iter().flatten().filter(|at| *at > now)
            .chain(self.picker_input.iter().flatten().map(|(at, _)| *at)).min()
    }

    pub fn drives_picker(&self) -> bool {
        self.picker_input.is_some()
    }

    pub fn take_picker_actions(&mut self, now: Instant) -> Vec<PickerAction> {
        let Some(input) = &mut self.picker_input else { return Vec::new() };
        let (ready, later): (Vec<_>, Vec<_>) =
            std::mem::take(input).into_iter().partition(|(at, _)| *at <= now);
        *input = later;
        ready.into_iter().map(|(_, action)| action).collect()
    }

    pub fn capture_due(&self, now: Instant) -> bool {
        self.capture.as_ref().is_some_and(|(at, _)| now >= *at)
    }

    /// 정한 때가 지난 첫 장을 PNG 로 — 렌더러가 그린 그대로(권한 없는 자기 뜨기).
    pub fn capture(&mut self, g: &mut GridRenderer) {
        if self.capture.as_ref().is_some_and(|(at, _)| Instant::now() >= *at) {
            g.capture_next = self.capture.take().map(|(_, p)| p);
        }
    }

    fn write(&self, line: serde_json::Value) {
        let Some(path) = &self.log else { return };
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{line}");
        }
    }
}
