use serde_json::Value;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub struct Motion {
    pub group: String,
    pub name: String,
    pub file: String,
    pub path: PathBuf,
    pub available: bool,
}

impl Motion {
    pub fn label(&self) -> String {
        let source_name = stem(&self.file);
        let translated = match (source_name.as_str(), self.group.as_str()) {
            ("zhentou", "Busy") => "쿠션",
            ("qizi", "Talk") => "깃발",
            ("linghun", "Error") => "영혼",
            (_, group) => match group {
                "Idle" => "대기",
                "Think" => "생각",
                "Busy" => "작업",
                "Talk" => "대화",
                "Error" => "곤란",
                "Touch" | "TapBody" => "쓰다듬기",
                "Sleep" => "졸기",
                _ => &self.group,
            },
        };
        format!(
            "{translated} · {} / {}{}",
            self.group,
            self.name,
            if self.available {
                ""
            } else {
                " (읽을 수 없음)"
            }
        )
    }
}

#[derive(Debug)]
pub struct Expression {
    pub name: String,
    pub file: String,
    pub path: PathBuf,
    pub parameters: BTreeSet<String>,
    pub available: bool,
    pub unlinked: bool,
}

impl Expression {
    pub fn label(&self) -> String {
        let source_name = stem(&self.file);
        let translated = match self.name.as_str() {
            "angry" => "어두운 얼굴",
            "cry" => "눈물",
            "blush" => "홍조",
            "baozhen" => "쿠션",
            "qizi1" => "깃발 들기",
            "qizi2" => "깃발 전환",
            "soul_ghost" => "영혼",
            "white_eyes" => "흰 눈",
            "flag_hide" => "깃발로 얼굴 가리기",
            _ => {
                if self.name.is_empty() {
                    &source_name
                } else {
                    &self.name
                }
            }
        };
        format!(
            "{translated} · {}{}",
            self.name,
            if self.unlinked {
                " (모델 연결 없음)"
            } else if self.available {
                ""
            } else {
                " (읽을 수 없음)"
            }
        )
    }
}

#[derive(Debug, Default)]
pub struct Catalog {
    pub motions: Vec<Motion>,
    pub expressions: Vec<Expression>,
}

#[derive(Default, Debug)]
pub struct Playback {
    pub selected: Option<usize>,
    pub repeat: bool,
}

#[derive(Default)]
pub struct Expressions {
    players: Vec<(usize, mocari::expression::ExpressionPlayer)>,
}

impl Expressions {
    pub fn indices(&self) -> Vec<usize> {
        self.players.iter().map(|(i, _)| *i).collect()
    }
    pub fn clear(&mut self) {
        self.players.clear();
    }
    pub fn toggle(&mut self, catalog: &Catalog, i: usize) -> Result<(), String> {
        if self.players.iter().any(|(index, _)| *index == i) {
            self.players.retain(|(index, _)| *index != i);
            return Ok(());
        }
        let entry = catalog.expressions.get(i).ok_or("Unknown expression")?;
        if entry.unlinked {
            return Err("Expression parameters have no model binding".into());
        }
        let expression =
            mocari::expression::load_expression(&entry.path).map_err(|e| e.to_string())?;
        self.players
            .retain(|(index, _)| !catalog.conflicts(*index, i));
        self.players
            .push((i, mocari::expression::ExpressionPlayer::new(expression)));
        Ok(())
    }
    pub fn apply(&mut self, runtime: &mut mocari::runtime::ModelRuntime, dt: f32) {
        for (_, expression) in &mut self.players {
            expression.tick(dt);
            expression.apply(runtime);
        }
    }
}

impl Playback {
    pub fn target(&self, catalog: &Catalog, automatic_group: &str) -> Option<usize> {
        self.selected.or_else(|| catalog.automatic(automatic_group))
    }

    pub fn finish_once(&mut self, finished: bool) -> bool {
        if finished && self.selected.is_some() && !self.repeat {
            self.selected = None;
            return true;
        }
        false
    }
}

impl Catalog {
    pub fn requested_motion(&mut self, dir: &Path, file: &str) -> usize {
        let path = dir.join(file);
        if let Some(i) = self.motions.iter().position(|m| m.path == path) {
            return i;
        }
        self.add_motion(dir, "직접 지정", None, file);
        self.motions.len() - 1
    }
    pub fn load(model: &Path) -> Self {
        let v = std::fs::read(model)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .unwrap_or_default();
        let dir = model.parent().unwrap_or(Path::new("."));
        let mut out = Self::default();
        if let Some(groups) = v["FileReferences"]["Motions"].as_object() {
            for (group, entries) in groups {
                for entry in entries.as_array().into_iter().flatten() {
                    if let Some(file) = entry["File"].as_str() {
                        out.add_motion(dir, group, entry["Name"].as_str(), file);
                    }
                }
            }
        }
        for entry in v["FileReferences"]["Expressions"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if let Some(file) = entry["File"].as_str() {
                out.add_expression(dir, entry["Name"].as_str(), file);
            }
        }
        // Models sometimes ship actions omitted from model3.json. Keep the file's
        // name and expose those actions without guessing a semantic group.
        let mut files = Vec::new();
        collect_files(dir, &mut files);
        files.sort();
        for path in files {
            let Ok(relative) = path.strip_prefix(dir) else {
                continue;
            };
            let file = relative.to_string_lossy();
            if file.ends_with(".motion3.json") && !out.motions.iter().any(|m| m.path == path) {
                out.add_motion(dir, "추가 파일", None, &file);
            } else if file.ends_with(".exp3.json")
                && !out.expressions.iter().any(|e| e.path == path)
            {
                out.add_expression(dir, None, &file);
            }
        }
        if let Some(unlinked) = unlinked_parameters(dir, &v) {
            for expression in &mut out.expressions {
                expression.unlinked =
                    !expression.parameters.is_empty() && expression.parameters.is_subset(&unlinked);
            }
        }
        out
    }

    fn add_motion(&mut self, dir: &Path, group: &str, name: Option<&str>, file: &str) {
        let path = dir.join(file);
        self.motions.push(Motion {
            group: group.into(),
            name: name.map(str::to_owned).unwrap_or_else(|| stem(file)),
            file: file.into(),
            available: mocari::motion::load_motion(&path).is_ok(),
            path,
        });
    }

    fn add_expression(&mut self, dir: &Path, name: Option<&str>, file: &str) {
        let path = dir.join(file);
        let parsed = mocari::expression::load_expression(&path).ok();
        let parameters = parsed
            .as_ref()
            .map(|e| e.parameters().iter().map(|p| p.id().to_string()).collect())
            .unwrap_or_default();
        self.expressions.push(Expression {
            name: name.map(str::to_owned).unwrap_or_else(|| stem(file)),
            file: file.into(),
            path,
            parameters,
            available: parsed.is_some(),
            unlinked: false,
        });
    }

    pub fn group(&self, group: &str) -> Option<usize> {
        self.motions
            .iter()
            .position(|m| m.available && m.group.eq_ignore_ascii_case(group))
    }

    pub fn automatic(&self, group: &str) -> Option<usize> {
        self.group(group).or_else(|| self.group("Idle"))
    }

    pub fn touch(&self) -> Option<usize> {
        self.group("Touch").or_else(|| self.group("TapBody"))
    }

    pub fn conflicts(&self, a: usize, b: usize) -> bool {
        !self.expressions[a]
            .parameters
            .is_disjoint(&self.expressions[b].parameters)
    }
}

/// A negative native binding means the exported parameter has no keyforms.
/// Physics can still consume it; custom schemas and pose/user-data extensions
/// are left undecided rather than incorrectly disabling third-party effects.
fn unlinked_parameters(dir: &Path, model: &Value) -> Option<BTreeSet<String>> {
    let standard_root = ["Version", "FileReferences", "Groups", "HitAreas", "Layout"];
    if model
        .as_object()?
        .keys()
        .any(|k| !standard_root.contains(&k.as_str()))
    {
        return None;
    }
    let refs = model.get("FileReferences")?.as_object()?;
    let standard_refs = [
        "Moc",
        "Textures",
        "Physics",
        "DisplayInfo",
        "Motions",
        "Expressions",
    ];
    if refs.keys().any(|k| !standard_refs.contains(&k.as_str())) {
        return None;
    }
    let mut physics_inputs = BTreeSet::new();
    if let Some(physics) = refs.get("Physics") {
        let bytes = std::fs::read(dir.join(physics.as_str()?)).ok()?;
        let physics: Value = serde_json::from_slice(&bytes).ok()?;
        for setting in physics.get("PhysicsSettings")?.as_array()? {
            for input in setting.get("Input")?.as_array()? {
                physics_inputs.insert(input.get("Source")?.get("Id")?.as_str()?.to_owned());
            }
        }
    }
    let bytes = std::fs::read(dir.join(refs.get("Moc")?.as_str()?)).ok()?;
    use mocari::moc3::{Endianness, Moc3Header, Moc3Ids, Moc3KeyformBindings, Moc3SectionOffsets};
    let header = Moc3Header::parse(&bytes).ok()?;
    let offsets = Moc3SectionOffsets::parse(&bytes).ok()?;
    let ids = Moc3Ids::parse(&bytes).ok()?;
    Moc3KeyformBindings::parse(&bytes).ok()?;
    let start = offsets.section_offset(56)? as usize;
    if start == 0 {
        return None;
    }
    let end = start.checked_add(ids.parameters().len().checked_mul(4)?)?;
    let next = offsets
        .section_offsets()
        .iter()
        .map(|n| *n as usize)
        .filter(|n| *n > start)
        .min()
        .unwrap_or(bytes.len());
    if end > next || end > bytes.len() {
        return None;
    }
    let mut unlinked = BTreeSet::new();
    for (i, id) in ids.parameters().iter().enumerate() {
        let raw: [u8; 4] = bytes
            .get(start + i * 4..start + (i + 1) * 4)?
            .try_into()
            .ok()?;
        let binding = match header.endianness() {
            Endianness::Little => i32::from_le_bytes(raw),
            Endianness::Big => i32::from_be_bytes(raw),
        };
        if binding < -1 {
            return None;
        }
        if binding == -1 && !physics_inputs.contains(id) {
            unlinked.insert(id.clone());
        }
    }
    Some(unlinked)
}

fn collect_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            collect_files(&entry.path(), files);
        } else if kind.is_file() {
            files.push(entry.path());
        }
    }
}

fn stem(file: &str) -> String {
    Path::new(file)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .trim_end_matches(".motion3.json")
        .trim_end_matches(".exp3.json")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_names_and_missing_entries_and_never_guesses_unknown_group() {
        let dir = std::env::temp_dir().join(format!("pet-catalog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let model = dir.join("tiny.model3.json");
        std::fs::write(&model, r#"{"FileReferences":{"Motions":{"Idle":[{"Name":"Original","File":"idle.motion3.json"}],"Unknown":[{"File":"missing.motion3.json"}]},"Expressions":[{"Name":"Face","File":"face.exp3.json"}]}}"#).unwrap();
        std::fs::write(dir.join("idle.motion3.json"), r#"{"Version":3,"Meta":{"Duration":1,"Fps":30,"Loop":true,"CurveCount":0,"TotalSegmentCount":0,"TotalPointCount":0,"UserDataCount":0,"TotalUserDataSize":0},"Curves":[]}"#).unwrap();
        std::fs::write(dir.join("face.exp3.json"), r#"{"Type":"Live2D Expression","Parameters":[{"Id":"Face","Value":1,"Blend":"Overwrite"}]}"#).unwrap();
        std::fs::write(
            dir.join("extra.exp3.json"),
            r#"{"Type":"Live2D Expression","Parameters":[{"Id":"Face","Value":0,"Blend":"Overwrite"}]}"#,
        )
        .unwrap();
        let mut catalog = Catalog::load(&model);
        assert_eq!(catalog.motions.len(), 2);
        assert_eq!(catalog.expressions.len(), 2);
        assert_eq!(catalog.motions[0].name, "Original");
        assert_eq!(catalog.motions[0].file, "idle.motion3.json");
        assert!(!catalog.motions[1].available);
        assert_eq!(catalog.automatic("Busy"), catalog.group("Idle"));
        assert_eq!(catalog.touch(), None);
        let requested = catalog.requested_motion(&dir, "idle.motion3.json");
        let explicit = Playback {
            selected: Some(requested),
            repeat: true,
        };
        assert_eq!(catalog.motions.len(), 2);
        assert_eq!(explicit.target(&catalog, "Error"), Some(requested));
        assert!(catalog.conflicts(0, 1));
        let mut effects = Expressions::default();
        effects.toggle(&catalog, 0).unwrap();
        effects.toggle(&catalog, 1).unwrap();
        assert_eq!(effects.indices(), vec![1]);
        effects.clear();
        assert!(effects.indices().is_empty());
        let mut selected = Playback {
            selected: Some(1),
            repeat: false,
        };
        assert_eq!(selected.target(&catalog, "Busy"), Some(1));
        assert_eq!(selected.target(&catalog, "Error"), Some(1));
        assert!(!selected.finish_once(false));
        assert!(selected.finish_once(true));
        assert_eq!(selected.target(&catalog, "Busy"), catalog.group("Idle"));
        selected.selected = Some(1);
        selected.repeat = true;
        assert!(!selected.finish_once(true));
        assert_eq!(selected.target(&catalog, "Idle"), Some(1));
        assert_eq!(catalog.expressions[0].file, "face.exp3.json");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[ignore = "Requires a local model; no licensed assets are committed"]
    fn local_model_catalog_loads_every_action() {
        let path = std::env::var("KASAPET_CATALOG_MODEL").expect("local model path");
        let catalog = Catalog::load(Path::new(&path));
        let mut model = mocari::assets::load_model_runtime(&path).expect("local model");
        let rt = model.runtime_mut();
        assert!(!catalog.motions.is_empty());
        eprintln!("cheek={:?}", rt.parameter_info("ParamCheek"));
        for m in &catalog.motions {
            assert!(m.available, "motion {}", m.file);
            let data = mocari::motion::load_motion(&m.path).unwrap();
            let duration = data.meta().duration();
            let mut once = mocari::motion::MotionPlayer::with_looping(data.clone(), false);
            let mut repeating = mocari::motion::MotionPlayer::with_looping(data, true);
            once.tick(duration + 0.1);
            repeating.tick(duration + 0.1);
            assert!(once.is_finished(), "once {}", m.file);
            assert!(!repeating.is_finished(), "repeat {}", m.file);
            rt.reset_parameters();
            rt.update_meshes().unwrap();
            let neutral = rt.meshes().to_vec();
            let mut changed_samples = Vec::new();
            for fraction in [0.0, 0.25, 0.5, 0.75, 0.99] {
                let mut sample = mocari::motion::MotionPlayer::with_looping(
                    mocari::motion::load_motion(&m.path).unwrap(),
                    false,
                );
                sample.tick(duration * fraction);
                rt.reset_parameters();
                sample.apply(rt);
                rt.update_meshes().unwrap();
                changed_samples.push(
                    neutral
                        .iter()
                        .zip(rt.meshes())
                        .filter(|(a, b)| *a != *b)
                        .count(),
                );
            }
            eprintln!("motion {} mesh samples={changed_samples:?}", m.name);
            for e in 0..catalog.expressions.len() {
                let mut effects = Expressions::default();
                if catalog.expressions[e].unlinked {
                    assert!(effects.toggle(&catalog, e).is_err());
                    effects.players.push((
                        e,
                        mocari::expression::ExpressionPlayer::new(
                            mocari::expression::load_expression(&catalog.expressions[e].path)
                                .unwrap(),
                        ),
                    ));
                } else {
                    effects.toggle(&catalog, e).unwrap();
                }
                rt.reset_parameters();
                repeating.apply(rt);
                let before_parameters = rt.parameter_values().to_vec();
                assert!(rt.update_meshes().is_some());
                let before_meshes = rt.meshes().to_vec();
                let mut previous = Vec::new();
                for _ in 0..2 {
                    rt.reset_parameters();
                    repeating.apply(rt);
                    effects.apply(rt, 2.0);
                    for (i, value) in rt.parameter_values().iter().enumerate() {
                        assert!(value.is_finite());
                        assert!(*value >= rt.parameter_minimum_by_index(i).unwrap());
                        assert!(*value <= rt.parameter_maximum_by_index(i).unwrap());
                    }
                    if !previous.is_empty() {
                        assert_eq!(previous, rt.parameter_values());
                    }
                    previous = rt.parameter_values().to_vec();
                    assert!(
                        rt.update_meshes().is_some(),
                        "mesh update {} + {}",
                        m.file,
                        catalog.expressions[e].name
                    );
                }
                if m.group == "Idle" {
                    let changed = before_meshes
                        .iter()
                        .zip(rt.meshes())
                        .filter(|(a, b)| *a != *b)
                        .count();
                    if catalog.expressions[e].name == "baozhen" {
                        assert!(changed > 0);
                    }
                    if catalog.expressions[e].unlinked {
                        assert_eq!(changed, 0);
                    }
                    let parameters = before_parameters
                        .iter()
                        .zip(rt.parameter_values())
                        .filter(|(a, b)| a != b)
                        .count();
                    eprintln!(
                        "expression {}: changed params={parameters}, drawables={changed}",
                        catalog.expressions[e].name
                    );
                }
                effects.clear();
                rt.reset_parameters();
                repeating.apply(rt);
                let baseline = rt.parameter_values().to_vec();
                effects.apply(rt, 2.0);
                assert_eq!(baseline, rt.parameter_values());
            }
        }
        for e in &catalog.expressions {
            assert!(e.available, "expression {}", e.file);
            for parameter in &e.parameters {
                assert!(
                    rt.parameter_index(parameter).is_some(),
                    "{} missing {}",
                    e.name,
                    parameter
                );
            }
        }
        eprintln!(
            "motions={} expressions={}",
            catalog.motions.len(),
            catalog.expressions.len()
        );
    }
}
