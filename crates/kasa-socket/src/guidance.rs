//! 카사텀 지침 — Claude·Codex 실행과 새 일 브리프에는 짧은 시작 안내(`START`)만 싣고, 나머지는
//! `kasaterm-cli guide <주제>` 로 필요할 때 하나씩 읽는다. 전체 규약을 매 실행 붙이면 일과 무관한 글이
//! 대화마다 쌓인다. 정본은 kasapane 스킬 폴더이고, 바이너리에 실어 어느 폴더·기기에서 불러도 같은 글이 나온다.

pub const START: &str = include_str!("../../../skills/kasapane/references/start.md");

pub struct Topic {
    pub name: &'static str,
    pub summary: &'static str,
    pub text: &'static str,
}

pub const TOPICS: &[Topic] = &[
    Topic { name: "collab", summary: "학생에게 일 맡기기·보고·쪽지 전달", text: include_str!("../../../skills/kasapane/collab.md") },
    Topic { name: "panes", summary: "칸·방 조회, 만들기, 이름·배치·닫기", text: include_str!("../../../skills/kasapane/references/panes.md") },
    Topic { name: "jobs", summary: "빌드·검사·서버 등 긴 잡 실행", text: include_str!("../../../skills/kasapane/references/jobs.md") },
    Topic { name: "media", summary: "완성한 이미지·음성·영상 전달", text: include_str!("../../../skills/kasapane/references/media.md") },
    Topic { name: "transfer", summary: "다른 기기로 세션 옮기기", text: include_str!("../../../skills/kasapane/references/transfer.md") },
    Topic { name: "develop", summary: "카사텀 자체의 코드·화면 수정", text: include_str!("../../../skills/kasapane/references/develop.md") },
];

pub fn topic(name: &str) -> Option<&'static Topic> {
    TOPICS.iter().find(|t| t.name == name)
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
