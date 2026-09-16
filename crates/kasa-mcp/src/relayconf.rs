//! 릴레이 설정(`~/.config/kasaterm/relay.json` · env `KASATERM_RELAY`)에서 **이 기계의
//! 이름**만 읽는다. 세션 중계(유령 명부·`/relay/send`)는 2026-09-16 에 걷어냈고 기기 간
//! 소통은 `tell` 하나다 — 남은 쓰임은 폰 화면에 이 기계를 부르는 이름뿐이다(`mobile.rs`).

fn parse_machine_id(text: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    v.get("machine_id")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// 설정의 `machine_id`. env 가 먼저, 없으면 파일. 둘 다 없으면 None.
pub(crate) fn machine_id() -> Option<String> {
    if let Ok(s) = std::env::var("KASATERM_RELAY") {
        if let Some(id) = parse_machine_id(&s) {
            return Some(id);
        }
    }
    let path = kasa_socket::home_dir()?.join(".config/kasaterm/relay.json");
    parse_machine_id(&std::fs::read_to_string(path).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_id_comes_from_json_and_ignores_blank() {
        assert_eq!(parse_machine_id(r#"{"machine_id":" 맥북 ","base":"x"}"#).as_deref(), Some("맥북"));
        assert_eq!(parse_machine_id(r#"{"machine_id":"  "}"#), None);
        assert_eq!(parse_machine_id("not json"), None);
    }
}
