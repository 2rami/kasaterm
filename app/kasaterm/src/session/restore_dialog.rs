#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Item {
    pub title: String,
    pub detail: String,
    pub character: String,
    pub icon: &'static str,
}

impl Item {
    pub(super) fn from_record(record: &serde_json::Value, agent: bool) -> Self {
        let string = |key: &str| record[key].as_str().unwrap_or("").trim();
        let character = if agent { string("character") } else { "" };
        let kind = super::restore::saved_agent_marker(record);
        let (name, icon) = match kind {
            Some("codex") => ("Codex", "codex"),
            Some("agy") => ("Antigravity", "antigravity"),
            _ if agent => ("Claude", "claude"),
            _ if record.get("web_url").is_some() => ("웹 페이지", "globe"),
            _ => ("일반 셸", "terminal"),
        };
        let cwd = string("cwd");
        let folder = cwd
            .rsplit(['/', '\\'])
            .find(|s| !s.is_empty())
            .unwrap_or("폴더 미확인");
        let title = if !string("title").is_empty() {
            string("title")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        } else if character.is_empty() {
            format!("{folder} · {name}")
        } else {
            format!("{character}의 {name} 작업")
        };
        let detail = format!(
            "{name}{} · {folder}",
            if character.is_empty() {
                String::new()
            } else {
                format!(" · {character}")
            }
        );
        Self {
            title,
            detail,
            character: character.into(),
            icon,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plain_shell_never_uses_its_old_student_face() {
        let item = Item::from_record(
            &serde_json::json!({"character":"이전 학생","cwd":"C:\\work\\프로젝트"}),
            false,
        );
        assert!(item.character.is_empty());
        assert_eq!(item.icon, "terminal");
        assert!(item.title.contains("프로젝트"));
    }
}
