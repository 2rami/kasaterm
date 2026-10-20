//! 대화 기록에 남은 인라인 결과물 — `imgopen --block`·`mediaopen --block` 이 낸 표시 블록의 첫 줄
//! (`[[ki:<id>:00/NN]]`·`[[km:<id>:00/NN]]`). 칸·방의 결과물 모아 보기(PC Info·폰 허브)는 기기 캐시 폴더가 아니라
//! 그 칸의 대화에서 출발한다 — 캐시를 늘어놓으면 남의 칸·다른 기기 것이 섞인다. 기록 줄은 JSON 이라도 표식에는
//! 이스케이프될 글자가 없어 날 글에서 그대로 찾는다.

use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Audio,
    Video,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Image => "image",
            Kind::Audio => "audio",
            Kind::Video => "video",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    pub kind: Kind,
    pub id: String,
}

/// `text` 에서 처음 나온 차례대로 결과물을 `out` 에 더한다. `seen` 에 있는 것은 건너뛴다 — 도구 결과와 답에 같은
/// 블록이 두 번 실리고, 기록을 이어 읽을 때도 같은 통을 넘긴다.
pub fn scan(text: &str, seen: &mut HashSet<String>, out: &mut Vec<Output>) {
    let mut rest = text;
    while let Some(at) = rest.find("[[k") {
        let tail = &rest[at..];
        rest = &rest[at + 3..];
        // `[[ki:` + 16자 id + `:00/` + 두 자리 + `]]` = 29자.
        let Some(marker) = tail.get(..29) else { break };
        let b = marker.as_bytes();
        let kind_byte = b[3];
        if !matches!(kind_byte, b'i' | b'm') || b[4] != b':' || &marker[21..25] != ":00/" || &marker[27..] != "]]" {
            continue;
        }
        let id = &marker[5..21];
        let total = &marker[25..27];
        if !id.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()) || !total.bytes().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let kind = match (kind_byte, total) {
            (b'i', _) => Kind::Image,
            (_, "04") => Kind::Audio,
            _ => Kind::Video,
        };
        if seen.insert(format!("{}:{id}", kind_byte as char)) {
            out.push(Output { kind, id: id.to_string() });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_each_output_once_in_order_from_the_first_row() {
        let text = concat!(
            r#"{"type":"user","content":"```text\n[[ki:0123456789abcdef:00/08]]\n[[ki:0123456789abcdef:01/08]]\n```"}"#, "\n",
            r#"{"type":"assistant","text":"영상이에요\n```text\n[[km:1111111111111111:00/12]]\n```\n소리\n[[km:2222222222222222:00/04]]"}"#, "\n",
            r#"{"text":"다시 [[ki:0123456789abcdef:00/08]] 와 깨진 [[ki:XYZ:00/08]] [[km:3333333333333333:05/12]]"}"#,
        );
        let (mut seen, mut out) = (HashSet::new(), Vec::new());
        scan(text, &mut seen, &mut out);
        let got: Vec<(Kind, &str)> = out.iter().map(|o| (o.kind, o.id.as_str())).collect();
        assert_eq!(got, vec![
            (Kind::Image, "0123456789abcdef"),
            (Kind::Video, "1111111111111111"),
            (Kind::Audio, "2222222222222222"),
        ]);
        // 이어 읽어도 같은 결과물은 다시 안 센다.
        scan("[[km:2222222222222222:00/04]]", &mut seen, &mut out);
        assert_eq!(out.len(), 3);
    }
}
