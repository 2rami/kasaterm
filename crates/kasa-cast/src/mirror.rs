//! 화면 거울·화면 스트림이 함께 쓰는 낱말 — 화면 목록, JPEG 한 장, 조작(`docs/screen-mirror.md` 「조작」), 못 하는 까닭.
//! 소켓·역압은 `kasa-remote` 의 화면 거울이, OS 부분은 `kasa-cast-host` 가 맡는다.

use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Display {
    pub id: u32,
    /// 화면 좌표(포인트) 크기. 레티나면 픽셀은 이보다 크다.
    pub width: u32,
    pub height: u32,
    pub main: bool,
}

pub struct Shot {
    /// 줄인 픽셀의 지문. 다음 [`Ask::prev`] 로 돌아와 같은 그림을 다시 인코딩하지 않게 한다.
    pub hash: u64,
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Denied {
    /// macOS 화면 기록 권한이 없다. 권한 없이도 뜨이기는 하지만 배경화면만 담겨 거짓 화면이 된다.
    Permission,
    Unavailable,
}

impl Denied {
    pub fn reason(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ask {
    pub display: u32,
    pub max_w: u32,
    pub max_h: u32,
    pub quality: u8,
    pub prev: u64,
}

/// 보는 쪽이 넣는 조작. 좌표는 그 화면 안의 비율(0–1)이라 받은 그림 크기와 상관없다.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    /// 단추 상태(1 왼쪽·2 오른쪽·4 가운데)를 통째로 보낸다. `prev` 는 서버가 붙인 직전 상태 — 둘을 견줘
    /// 누름·뗌·끌기를 가른다. `mods` 는 1 shift·2 control·4 option(alt)·8 command(win).
    Pointer { x: f64, y: f64, buttons: u8, prev: u8, mods: u8 },
    /// 휠 — 화면 좌표(포인트) 단위. 양수 dy 는 위로 굴림(내용이 내려온다).
    Scroll { x: f64, y: f64, dx: i32, dy: i32 },
    /// 이름 붙은 키(`Enter`·`KeyA`… 웹 `KeyboardEvent.code`). `down` 이 없으면 눌렀다 뗀다.
    Key { code: String, down: Option<bool>, mods: u8 },
    /// 글자째 넣기 — 폰 자판·보는 쪽 입력기가 이미 조합한 글이라 그 기기 입력기를 거치지 않는다.
    Text(String),
}

pub trait Source: Send + Sync {
    fn displays(&self) -> Vec<Display>;
    /// 비율을 지켜 `max_w × max_h` 안으로 줄여 뜬다(키우지는 않는다). 픽셀이 `ask.prev` 와 같으면 `Ok(None)`.
    fn capture(&self, ask: &Ask) -> Result<Option<Shot>, Denied>;
    /// 그 기기 앞에 권한 창을 띄운다. OS 는 앱마다 처음 한 번만 띄우므로 연결마다 한 번만 부른다.
    fn request_permission(&self) {}
    /// 조작을 그 화면에 넣는다. macOS 는 손쉬운 사용 권한이 없으면 `Permission`.
    fn input(&self, _display: u32, _input: &Input) -> Result<(), Denied> {
        Err(Denied::Unavailable)
    }
    /// 조작 권한 창을 그 기기 앞에 띄운다 — 연결마다 한 번.
    fn request_input_permission(&self) {}
    /// 보는 사람이 있는 동안 화면을 켜 둔다(사람이 만진 것처럼 잠들기 시계를 되감는다). 잠든 화면은 뜰 장면이
    /// 없어 폰이 「화면을 받는 중」에서 멈췄다 — 그 기기 앞에 아무도 없을 때 보려는 기능인데 바로 그때 못 봤다.
    fn keep_awake(&self) {}
}

pub enum Control {
    Ack(u32),
    Tune(serde_json::Value),
    /// `Pointer.prev` 는 아직 비어 있다 — 연결이 쥔 단추 상태로 채운다.
    Input(Input),
}

/// 글자 넣기 한 번의 상한 — 폰 입력줄 한 번에 보내는 글이라 이보다 길 일이 없다.
pub const TEXT_MAX: usize = 4096;

/// 조작 메시지 하나(`pointer`·`scroll`·`key`·`text`) — 화면 스트림이 같은 말을 쓴다(`screen_stream`).
pub fn input_of(raw: &str) -> Option<Input> {
    match control(raw)? {
        Control::Input(input) => Some(input),
        _ => None,
    }
}

pub fn control(raw: &str) -> Option<Control> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let unit = |k: &str| v.get(k).and_then(serde_json::Value::as_f64).filter(|f| f.is_finite()).map(|f| f.clamp(0.0, 1.0));
    let small = |k: &str| v.get(k).and_then(serde_json::Value::as_u64).unwrap_or(0).min(255) as u8;
    let wheel = |k: &str| v.get(k).and_then(serde_json::Value::as_i64).unwrap_or(0).clamp(-10_000, 10_000) as i32;
    match v.get("t")?.as_str()? {
        "ack" => Some(Control::Ack(u32::try_from(v.get("seq")?.as_u64()?).ok()?)),
        "tune" => Some(Control::Tune(v)),
        "pointer" => Some(Control::Input(Input::Pointer {
            x: unit("x")?,
            y: unit("y")?,
            buttons: small("b") & 7,
            prev: 0,
            mods: small("m") & 15,
        })),
        "scroll" => Some(Control::Input(Input::Scroll { x: unit("x")?, y: unit("y")?, dx: wheel("dx"), dy: wheel("dy") })),
        "key" => {
            let code = v.get("k")?.as_str()?;
            (!code.is_empty() && code.len() <= 32).then(|| {
                Control::Input(Input::Key { code: code.to_string(), down: v.get("down").and_then(serde_json::Value::as_bool), mods: small("m") & 15 })
            })
        }
        "text" => {
            let s = v.get("s")?.as_str()?;
            (!s.is_empty() && s.len() <= TEXT_MAX).then(|| Control::Input(Input::Text(s.to_string())))
        }
        _ => None,
    }
}

