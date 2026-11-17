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

/// 새 일 브리프 끝의 완료 보고 안내가 여는 말. 옛 판의 문구가 달라도 이것으로 알아본다(화면 `assigned_brief`·폰과 같은 규칙).
const DONE_HINT_HEAD: &str = "끝나면 `kasaterm-cli done ";

/// 시작 안내를 실행 지침으로 받은 대화에 가는 브리프에서 겹치는 안내를 걷는다. 보내는 쪽은 받는 대화가 받았는지 몰라
/// 늘 붙이므로 받는 기기가 고른다 — 새 일마다 같은 문단이 대화에 쌓이던 자리다. 시작 안내가 완료 보고(`done`)도
/// 말해 완료 보고 줄도 겹치지만, 보낸 학생 표식(`⟦이름⟧`)이 없는 브리프(칸 밖 프로그램)는 그 줄이 「맡긴 일」의
/// 유일한 표지라 남긴다.
pub fn without_known(body: &str) -> String {
    let body = body.replacen(&format!("\n\n{}", START.trim()), "", 1);
    match body.rsplit_once("\n\n") {
        Some((head, hint)) if body.trim_start().starts_with('⟦') && hint.starts_with(DONE_HINT_HEAD) => head.to_string(),
        _ => body,
    }
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
    #[test]
    fn a_guided_conversation_gets_only_the_work() {
        let start = super::START.trim();
        let hint = "끝나면 `kasaterm-cli done succeeded '끝난 것·남은 것'`으로 한 번 보고해 주세요.";
        let marked = format!("⟦호시노@맥미니⟧ 폰 알림 고쳐\n\n둘째 문단\n\n{start}\n\n{hint}");
        assert_eq!(super::without_known(&marked), "⟦호시노@맥미니⟧ 폰 알림 고쳐\n\n둘째 문단");
        let unmarked = format!("폰 알림 고쳐\n\n{start}\n\n{hint}");
        assert_eq!(super::without_known(&unmarked), format!("폰 알림 고쳐\n\n{hint}"), "표식 없는 브리프는 완료 보고 줄이 맡긴 일 표지다");
        let reply = "⟦호시노⟧ 답장 한 줄\n\n둘째 문단";
        assert_eq!(super::without_known(reply), reply);
    }
}
