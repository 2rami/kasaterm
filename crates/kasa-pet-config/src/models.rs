//! 펫 캐릭터 목록과 「지금 띄울 캐릭터」 판정. 설정 화면·켜기·받기가 같은 답을 내야 해서 한 곳에 둔다.
//!
//! 캐릭터 하나는 펫 설정 폴더 밑의 폴더 하나다(`<dir>/<이름>/*.model3.json`). `current` 파일은
//! 사람이 고른 이름만 쥔다 — 받기나 켜기가 대신 적으면 「고른 적 없음」과 구별이 안 돼, 기본
//! 캐릭터를 바꿀 때 사람이 고른 것까지 덮게 된다.

use serde_json::{json, Value};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// 고른 적이 없을 때 뜨는 캐릭터. 없으면 다른 캐릭터로 대신하지 않고 이것을 받게 한다.
pub const DEFAULT: &str = "huohuo";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Live2D 가 CubismWebSamples 로 직접 공개한 캐릭터. 쓰는 사람이 권리자에게서 바로 받는다.
    Live2dSample,
    /// 2차 배포가 금지된 모델이라 레포·앱 번들에 못 싣는다. 기기마다 `sources.json` 에 적은
    /// 자리(zip 주소나 폴더)에서만 받는다.
    Local,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Character {
    pub name: &'static str,
    pub label: &'static str,
    pub source: Source,
    /// 권리자가 배포하는 페이지. `Source::Local` 캐릭터는 사람이 여기서 받아 온다.
    pub page: Option<&'static str>,
}

const fn sample(name: &'static str) -> Character {
    Character { name, label: name, source: Source::Live2dSample, page: None }
}

/// 받을 수 있는 캐릭터. 첫 칸이 기본이다. Live2D 캐릭터 이름은 약관상 바꾸지 않는다.
pub const CATALOG: &[Character] = &[
    Character {
        name: DEFAULT,
        label: "곽향",
        source: Source::Local,
        page: Some("https://bailyovo.booth.pm/items/5288339"),
    },
    sample("Mao"),
    sample("Hiyori"),
    sample("Haru"),
    sample("Natori"),
    sample("Rice"),
    sample("Ren"),
    sample("Wanko"),
    sample("Mark"),
];

pub fn character(name: &str) -> Option<&'static Character> {
    CATALOG.iter().find(|c| c.name == name)
}

pub fn model3_in(dir: &Path) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.to_string_lossy().ends_with(".model3.json"))
}

/// 펫 설정 폴더 바로 밑의 폴더 이름으로 쓸 수 있나. 경로를 넘나드는 이름은 받는 쪽이 엉뚱한
/// 자리에 풀게 된다.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && !name.contains(['/', '\\', '\0', ':'])
}

pub fn is_installed(dir: &Path, name: &str) -> bool {
    !name.is_empty() && !name.starts_with('.') && model3_in(&dir.join(name)).is_some()
}

/// 받아 둔 캐릭터 폴더 이름(정렬). 받는 중인 임시 폴더(`.downloading-…`)는 빼고 센다.
pub fn installed(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.file_name().to_str().map(str::to_owned))
                .filter(|n| is_installed(dir, n))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// 사람이 고른 캐릭터. 그 폴더가 사라졌으면 고른 것이 없는 것으로 본다.
pub fn chosen(dir: &Path) -> Option<String> {
    let name = fs::read_to_string(dir.join("current")).ok()?.trim().to_string();
    is_installed(dir, &name).then_some(name)
}

/// 지금 띄울 캐릭터. 기본 캐릭터가 아직 없어도 그 이름을 답한다 — 다른 것으로 대신 띄우지 않고
/// 받게 하려는 것이라, 화면도 그 칸을 고른 것으로 보여야 한다.
pub fn current(dir: &Path) -> String {
    chosen(dir).unwrap_or_else(|| DEFAULT.to_string())
}

/// 띄울 모델 파일. 고른 것 → 기본 → 캐릭터 폴더를 나누기 전 설정 폴더 바로 밑에 받아 둔 것.
/// None 이면 기본 캐릭터를 받아야 한다.
pub fn model_path(dir: &Path) -> Option<PathBuf> {
    model3_in(&dir.join(current(dir))).or_else(|| model3_in(dir))
}

/// `Source::Local` 캐릭터를 받을 자리. 이 기기의 `sources.json`(`{"huohuo": "<zip 주소나 폴더>"}`)
/// 에만 둔다 — 레포에 적으면 그 주소가 공개 배포처가 된다.
pub fn local_source(dir: &Path, name: &str) -> Option<String> {
    let doc: Value = serde_json::from_slice(&fs::read(dir.join("sources.json")).ok()?).ok()?;
    let s = doc.get(name)?.as_str()?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// 이 기기에서 받을 길이 있나. 목록에 없는(사람이 직접 넣은) 캐릭터는 받을 곳이 없다.
pub fn can_download(dir: &Path, name: &str) -> bool {
    match character(name).map(|c| c.source) {
        Some(Source::Live2dSample) => true,
        Some(Source::Local) => local_source(dir, name).is_some(),
        None => false,
    }
}

/// 이 기기의 받을 자리를 적는다. 다른 캐릭터의 자리와 모르는 키는 그대로 둔다.
pub fn set_local_source(dir: &Path, name: &str, source: &str) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let path = dir.join("sources.json");
    let mut doc = fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    doc[name] = json!(source);
    let temporary = dir.join(format!(".sources-{}.tmp", std::process::id()));
    fs::write(&temporary, serde_json::to_string_pretty(&doc)?)?;
    fs::rename(&temporary, path)
}

/// 원본 묶음에 없어 펫이 덧붙이는 것. 곽향 원본은 VTube Studio 용이라 모션·표정을 단축키로만
/// 걸고 model3.json 에는 안 적는다 — 펫은 상태 이름(Idle·Think·Busy…)의 모션 묶음을 찾으므로
/// 그대로 두면 상태가 바뀌어도 안 움직인다. 덧붙이는 표정 셋은 이 프로젝트가 만든 것이다.
const OVERLAYS: &[(&str, &[(&str, &str)], &str)] = &[(
    DEFAULT,
    &[
        ("blush.exp3.json", include_str!("../overlays/huohuo/blush.exp3.json")),
        ("flag_hide.exp3.json", include_str!("../overlays/huohuo/flag_hide.exp3.json")),
        ("soul_ghost.exp3.json", include_str!("../overlays/huohuo/soul_ghost.exp3.json")),
    ],
    include_str!("../overlays/huohuo/model3.json"),
)];

/// 받은 캐릭터에 덧붙임을 얹는다. 있는 파일·묶음·표정은 덮지 않고, 가리키는 파일이 없는 묶음은
/// 건너뛴다(원본 파일 이름이 다르면 덧붙여 봐야 깨진 모션이다).
pub fn apply_overlay(dir: &Path, name: &str) -> io::Result<()> {
    let Some((_, files, model3)) = OVERLAYS.iter().find(|(n, _, _)| *n == name) else {
        return Ok(());
    };
    let folder = dir.join(name);
    for (file, body) in *files {
        let path = folder.join(file);
        if !path.exists() {
            fs::write(path, body)?;
        }
    }
    let Some(model_path) = model3_in(&folder) else {
        return Ok(());
    };
    let mut model: Value = serde_json::from_slice(&fs::read(&model_path)?)?;
    let overlay: Value = serde_json::from_str(model3)?;
    let Some(refs) = model.get_mut("FileReferences").and_then(Value::as_object_mut) else {
        return Ok(());
    };
    let present = |entry: &Value| {
        entry.get("File").and_then(Value::as_str).is_some_and(|f| folder.join(f).is_file())
    };
    let mut changed = false;
    let motions = refs.entry("Motions").or_insert_with(|| json!({}));
    if let (Some(have), Some(add)) = (motions.as_object_mut(), overlay["Motions"].as_object()) {
        for (group, list) in add {
            let all_there = list.as_array().is_some_and(|l| !l.is_empty() && l.iter().all(present));
            if !have.contains_key(group) && all_there {
                have.insert(group.clone(), list.clone());
                changed = true;
            }
        }
    }
    let expressions = refs.entry("Expressions").or_insert_with(|| json!([]));
    if let (Some(have), Some(add)) = (expressions.as_array_mut(), overlay["Expressions"].as_array()) {
        for e in add {
            let named = |x: &Value| x.get("Name") == e.get("Name");
            if !have.iter().any(named) && present(e) {
                have.push(e.clone());
                changed = true;
            }
        }
    }
    if !changed {
        return Ok(());
    }
    let temporary = folder.join(format!(".model3-{}.tmp", std::process::id()));
    fs::write(&temporary, serde_json::to_string_pretty(&model)?)?;
    fs::rename(&temporary, model_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let p = std::env::temp_dir().join(format!(
                "pet-models-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir(&p).unwrap();
            Self(p)
        }
        fn model(&self, name: &str) {
            fs::create_dir_all(self.0.join(name)).unwrap();
            fs::write(self.0.join(name).join(format!("{name}.model3.json")), "{}").unwrap();
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn default_stands_in_even_when_only_other_characters_are_installed() {
        let d = Scratch::new();
        d.model("Mao");
        assert_eq!(current(&d.0), DEFAULT);
        assert_eq!(model_path(&d.0), None, "없는 기본 캐릭터를 마오로 대신 띄우면 안 된다");
        d.model(DEFAULT);
        assert_eq!(model_path(&d.0), Some(d.0.join("huohuo/huohuo.model3.json")));
    }

    #[test]
    fn a_chosen_character_wins_until_its_folder_is_gone() {
        let d = Scratch::new();
        d.model(DEFAULT);
        d.model("Ren");
        fs::write(d.0.join("current"), "Ren\n").unwrap();
        assert_eq!(current(&d.0), "Ren");
        fs::remove_dir_all(d.0.join("Ren")).unwrap();
        assert_eq!(current(&d.0), DEFAULT);
        assert_eq!(fs::read_to_string(d.0.join("current")).unwrap(), "Ren\n", "판정이 고른 기록을 지우면 안 된다");
    }

    #[test]
    fn half_downloaded_and_legacy_layouts() {
        let d = Scratch::new();
        d.model(".downloading-Mao.x1");
        assert!(installed(&d.0).is_empty());
        fs::write(d.0.join("current"), ".downloading-Mao.x1").unwrap();
        assert_eq!(current(&d.0), DEFAULT);
        fs::write(d.0.join("old.model3.json"), "{}").unwrap();
        assert_eq!(model_path(&d.0), Some(d.0.join("old.model3.json")));
    }

    #[test]
    fn only_this_machines_sources_file_opens_the_restricted_model() {
        let d = Scratch::new();
        assert!(can_download(&d.0, "Mao"));
        assert!(!can_download(&d.0, DEFAULT));
        assert!(!can_download(&d.0, "MyOwnModel"));
        fs::write(d.0.join("sources.json"), r#"{"huohuo":"  "}"#).unwrap();
        assert!(!can_download(&d.0, DEFAULT));
        fs::write(d.0.join("sources.json"), r#"{"huohuo":"file:///m/huohuo.zip"}"#).unwrap();
        assert_eq!(local_source(&d.0, DEFAULT).as_deref(), Some("file:///m/huohuo.zip"));
        assert!(can_download(&d.0, DEFAULT));
    }

    #[test]
    fn overlay_adds_pet_motion_groups_without_touching_what_is_there() {
        let d = Scratch::new();
        let f = d.0.join(DEFAULT);
        fs::create_dir_all(&f).unwrap();
        for m in ["Scene1", "haoqi", "zhentou", "qizi", "linghun", "yaotou"] {
            fs::write(f.join(format!("{m}.motion3.json")), "{}").unwrap();
        }
        fs::write(f.join("angry.exp3.json"), "{}").unwrap();
        fs::write(f.join("blush.exp3.json"), "mine").unwrap();
        fs::write(
            f.join("huohuo.model3.json"),
            r#"{"Version":3,"FileReferences":{"Moc":"huohuo.moc3","Motions":{"Idle":[{"File":"mine.motion3.json"}]}}}"#,
        )
        .unwrap();
        apply_overlay(&d.0, DEFAULT).unwrap();
        let v: Value = serde_json::from_slice(&fs::read(f.join("huohuo.model3.json")).unwrap()).unwrap();
        let refs = &v["FileReferences"];
        assert_eq!(refs["Moc"], "huohuo.moc3");
        assert_eq!(refs["Motions"]["Idle"][0]["File"], "mine.motion3.json");
        assert_eq!(refs["Motions"]["Think"][0]["File"], "haoqi.motion3.json");
        assert!(refs["Motions"].get("Sleep").is_none(), "keshui 가 없는 묶음은 걸지 않는다");
        let names: Vec<_> = refs["Expressions"].as_array().unwrap().iter()
            .map(|e| e["Name"].as_str().unwrap().to_string()).collect();
        assert!(names.contains(&"angry".into()) && names.contains(&"soul_ghost".into()));
        assert!(!names.contains(&"cry".into()));
        assert_eq!(fs::read_to_string(f.join("blush.exp3.json")).unwrap(), "mine");
        let before = fs::read(f.join("huohuo.model3.json")).unwrap();
        apply_overlay(&d.0, DEFAULT).unwrap();
        assert_eq!(fs::read(f.join("huohuo.model3.json")).unwrap(), before);
    }

    #[test]
    fn names_that_would_leave_the_pet_folder_are_refused() {
        for bad in ["", ".", "..", "../x", "a/b", "a\\b", ".downloading-x", "a:b"] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        assert!(valid_name("곽향 2") && valid_name("Mao"));
    }

    #[test]
    fn writing_a_source_keeps_the_rest_of_the_file() {
        let d = Scratch::new();
        fs::write(d.0.join("sources.json"), r#"{"other":"x","future":7}"#).unwrap();
        set_local_source(&d.0, DEFAULT, "/Users/me/Downloads/huohuo2.zip").unwrap();
        let v: Value = serde_json::from_slice(&fs::read(d.0.join("sources.json")).unwrap()).unwrap();
        assert_eq!((v["other"].as_str(), v["future"].as_i64()), (Some("x"), Some(7)));
        assert!(can_download(&d.0, DEFAULT));
    }
}
