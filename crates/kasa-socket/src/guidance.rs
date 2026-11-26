//! 카사텀 지침 — 칸의 Claude·Codex 는 kasapane 스킬로 받는다. 늘 지킬 규칙(완료 보고·폰·다른 기기 학생)은
//! 스킬 설명에 짧게 두어 늘 실리고, 나머지는 `kasaterm-cli guide <주제>` 로 필요할 때 하나씩 읽는다. 실행 지침에
//! 붙이면 일과 무관한 글이 대화마다 쌓인다. 정본은 kasapane 스킬 폴더이고, 바이너리에 실어 어느 폴더·기기에서
//! 불러도 같은 글이 나오고 앱이 칸마다 같은 스킬을 깐다.

pub struct Topic {
    pub name: &'static str,
    /// 스킬 폴더 안 자리 — 칸에 까는 스킬이 SKILL.md 의 링크를 그대로 쓰게.
    pub file: &'static str,
    pub summary: &'static str,
    pub text: &'static str,
}

pub const TOPICS: &[Topic] = &[
    Topic { name: "collab", file: "collab.md", summary: "캐릭터에게 일 맡기기·보고·쪽지 전달", text: include_str!("../../../skills/kasapane/collab.md") },
    Topic { name: "panes", file: "references/panes.md", summary: "칸·방 조회, 만들기, 이름·배치·닫기", text: include_str!("../../../skills/kasapane/references/panes.md") },
    Topic { name: "jobs", file: "references/jobs.md", summary: "빌드·검사·서버 등 긴 잡 실행", text: include_str!("../../../skills/kasapane/references/jobs.md") },
    Topic { name: "media", file: "references/media.md", summary: "완성한 그림·음성·영상·PDF·웹 카드 전달", text: include_str!("../../../skills/kasapane/references/media.md") },
    Topic { name: "transfer", file: "references/transfer.md", summary: "다른 기기로 세션 옮기기", text: include_str!("../../../skills/kasapane/references/transfer.md") },
    Topic { name: "develop", file: "references/develop.md", summary: "카사텀 자체의 코드·화면 수정", text: include_str!("../../../skills/kasapane/references/develop.md") },
];

pub fn topic(name: &str) -> Option<&'static Topic> {
    TOPICS.iter().find(|t| t.name == name)
}

const SKILL: &str = include_str!("../../../skills/kasapane/SKILL.md");
const MANIFEST: &str = include_str!("../../../skills/kasapane/.claude-plugin/plugin.json");

/// 칸에 깔 kasapane 스킬 폴더의 (자리, 내용) — claude 는 `--plugin-dir` 로, codex 는 칸 홈 `skills/` 로 싣는다.
pub fn skill_files() -> Vec<(&'static str, &'static str)> {
    let mut files = vec![("SKILL.md", SKILL), (".claude-plugin/plugin.json", MANIFEST)];
    files.extend(TOPICS.iter().map(|t| (t.file, t.text)));
    files
}

/// `guide` 무인자·`--help` 가 내는 목록.
pub fn index() -> String {
    let width = TOPICS.iter().map(|t| t.name.len()).max().unwrap_or(0);
    let mut out = String::from("kasaterm-cli guide <주제> — 카사텀 지침을 주제 하나만 읽는다(앱·소켓 없이).\n\n");
    for t in TOPICS {
        out.push_str(&format!("  {:width$}  {}\n", t.name, t.summary));
    }
    out
}

#[cfg(test)]
mod tests {
    /// 스킬 설명이 늘 실리는 유일한 자리다 — 완료 보고 규칙이 빠지면 폰·맥 알림과 `board --wait` 가 멎고, 카드 명령이
    /// 빠지면 결과물이 경로로만 건너간다(본문을 열어야 보이던 때 자주 빠졌다).
    /// codex 는 설명이 1024 바이트를 넘는 스킬을 싣지 않는다.
    #[test]
    fn the_skill_description_carries_the_standing_rules() {
        let description = super::SKILL.lines().find_map(|l| l.strip_prefix("description: ")).unwrap();
        for rule in ["kasaterm-cli done", "⟦폰⟧", "kasaterm-cli tell 이름@기기", "imgopen --block", "mediaopen --block", "PDF"] {
            assert!(description.contains(rule), "{rule}");
        }
        assert!(description.len() <= 1024, "{}", description.len());
        let files = super::skill_files();
        for link in ["collab.md", "references/panes.md", "references/media.md"] {
            assert!(super::SKILL.contains(&format!("]({link})")) && files.iter().any(|(path, _)| *path == link), "{link}");
        }
    }
}
