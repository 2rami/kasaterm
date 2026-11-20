//! 서비스 상태 폴더 — 서비스 카사넷 키, 앱이 넘긴 허용 목록과 설정, 업데이트 도우미가 읽는 공개 상태.
//! 앱이 없을 때(재시작 중·앱을 안 띄운 기계)도 마지막으로 받은 값으로 돈다(`docs/screen-service-split.md` 「설정과 기본값」).

use std::path::{Path, PathBuf};
use std::str::FromStr;

use kasa_net::iroh::EndpointId;
use serde_json::{json, Value};

pub const KEY_FILE: &str = "kasanet-screen.key";
const ALLOW_FILE: &str = "allow.json";
const SETTINGS_FILE: &str = "settings.json";

/// 앱이 넘긴 보는 쪽 — 명부로 배운 기기와 등록된 주인 폰(만료 유닉스 초).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Allowed {
    pub machines: Vec<EndpointId>,
    pub phones: Vec<(EndpointId, u64)>,
}

impl Allowed {
    pub fn from_json(v: &Value) -> Self {
        let id = |x: &Value| x.as_str().and_then(|s| EndpointId::from_str(s).ok());
        let machines = v.get("machines").and_then(Value::as_array).into_iter().flatten().filter_map(id).collect();
        let phones = v
            .get("phones")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|p| Some((id(p.get(0)?)?, p.get(1)?.as_u64()?)))
            .collect();
        Self { machines, phones }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "machines": self.machines.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "phones": self.phones.iter().map(|(id, until)| json!([id.to_string(), until])).collect::<Vec<_>>(),
        })
    }

    /// 지금 받을 id — 만료가 지난 폰은 뺀다(앱이 없으면 폰이 다시 등록할 곳이 없어 15분 뒤 끊긴다).
    pub fn live(&self, now_unix: u64) -> Vec<EndpointId> {
        let mut ids: Vec<EndpointId> = self.machines.clone();
        ids.extend(self.phones.iter().filter(|(_, until)| *until > now_unix).map(|(id, _)| *id));
        ids.sort();
        ids.dedup();
        ids
    }
}

/// 서비스 설정. 앱이 한 번도 안 넘겼으면 기본값 — 지금 앱 안 보내기와 같은 동작이다.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// 보는 쪽이 조작을 넣을 수 있다.
    pub input: bool,
    /// 시스템 소리를 보낸다(보는 쪽이 청할 때).
    pub audio: bool,
    /// 소리를 뜨는 동안 이 기기 스피커를 끈다.
    pub source_mute: bool,
    /// Windows 로그인 화면(아무도 로그인 안 함)도 보이고 누르게 한다.
    pub logon_screen: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { input: true, audio: true, source_mute: true, logon_screen: true }
    }
}

impl Settings {
    /// 없는 값은 기본값 그대로 둔다 — 옛 앱이 모르는 칸을 안 실어도 켜진 채다.
    pub fn from_json(v: &Value) -> Self {
        let d = Self::default();
        let flag = |k: &str, d: bool| v.get(k).and_then(Value::as_bool).unwrap_or(d);
        Self {
            input: flag("input", d.input),
            audio: flag("audio", d.audio),
            source_mute: flag("source_mute", d.source_mute),
            logon_screen: flag("logon_screen", d.logon_screen),
        }
    }

    pub fn to_json(self) -> Value {
        json!({ "input": self.input, "audio": self.audio, "source_mute": self.source_mute, "logon_screen": self.logon_screen })
    }
}

pub struct State {
    pub dir: PathBuf,
}

impl State {
    pub fn new(dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        Self { dir }
    }

    pub fn key_path(&self) -> PathBuf {
        self.dir.join(KEY_FILE)
    }

    pub fn allowed(&self) -> Allowed {
        read_json(&self.dir.join(ALLOW_FILE)).map(|v| Allowed::from_json(&v)).unwrap_or_default()
    }

    pub fn save_allowed(&self, a: &Allowed) {
        write_json(&self.dir.join(ALLOW_FILE), &a.to_json());
    }

    pub fn settings(&self) -> Settings {
        read_json(&self.dir.join(SETTINGS_FILE)).map(|v| Settings::from_json(&v)).unwrap_or_default()
    }

    pub fn save_settings(&self, s: Settings) {
        write_json(&self.dir.join(SETTINGS_FILE), &s.to_json());
    }

    /// 업데이트 도우미(관리자 아닐 수 있다)가 읽는 공개 상태 — 서비스 판과 지금 보는 수만. 상태 폴더 옆에 둔다(폴더는 잠겨 있다).
    pub fn public_status_path(&self) -> PathBuf {
        self.dir.with_file_name("screen-status.json")
    }

    /// 노드 주소(공개키·주소)도 싣는다 — 비밀이 아니고, 리그 탐침이 여기서 읽는다.
    pub fn write_public_status(&self, version: &str, viewers: usize, node: &Value) {
        write_json(&self.public_status_path(), &json!({ "version": version, "viewers": viewers, "node": node }));
    }
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// 옆 파일에 쓰고 바꿔 끼운다 — 쓰다 죽어도 반쪽 파일이 남지 않게.
fn write_json(path: &Path, v: &Value) {
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, v.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kasa_net::iroh::SecretKey;

    #[test]
    fn allowed_round_trips_and_drops_expired_phones() {
        let (m, p1, p2) = (SecretKey::generate().public(), SecretKey::generate().public(), SecretKey::generate().public());
        let a = Allowed { machines: vec![m], phones: vec![(p1, 100), (p2, 300)] };
        assert_eq!(Allowed::from_json(&a.to_json()), a);
        let live = a.live(200);
        assert!(live.contains(&m) && live.contains(&p2) && !live.contains(&p1), "만료된 폰은 뺀다");
        assert_eq!(Allowed::from_json(&json!({ "machines": ["nope", m.to_string()] })).machines, vec![m], "못 읽는 id 는 버린다");
    }

    #[test]
    fn settings_default_on_and_keep_unknown_fields_default() {
        assert_eq!(Settings::from_json(&Value::Null), Settings::default());
        let s = Settings::from_json(&json!({ "audio": false }));
        assert!(!s.audio && s.input && s.source_mute && s.logon_screen);
        assert_eq!(Settings::from_json(&s.to_json()), s);
    }

    #[test]
    fn state_files_survive_a_restart() {
        let root = std::env::temp_dir().join(format!("kasa-screen-state-{}", std::process::id()));
        let dir = root.join("screen");
        let st = State::new(dir.clone());
        assert_eq!(st.allowed(), Allowed::default(), "처음엔 아무도 못 붙는다");
        let a = Allowed { machines: vec![SecretKey::generate().public()], phones: Vec::new() };
        st.save_allowed(&a);
        st.save_settings(Settings { audio: false, ..Settings::default() });
        let again = State::new(dir.clone());
        assert_eq!(again.allowed(), a);
        assert!(!again.settings().audio);
        st.write_public_status("1.0.0", 2, &Value::Null);
        assert_eq!(read_json(&st.public_status_path()).unwrap()["viewers"], 2);
        let _ = std::fs::remove_dir_all(&root);
    }
}
