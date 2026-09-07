//! 지금 판 — kasaterm 이 적어 두는 상태 한 낱말과 말풍선 한 줄을 읽는다.
//!
//! 펫은 별개 프로세스라 앱의 메모리를 못 본다. 소켓을 뚫는 대신 파일 한 장을 읽는
//! 이유는, 앱이 꺼져 있으면 그 파일이 그냥 안 바뀌고 펫은 그것을 「조용하다」로 읽으면
//! 그만이기 때문이다 — 끊긴 연결을 다루는 코드가 아예 안 생긴다.

/// 캐릭터가 지금 지어야 할 표정.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mood {
    /// 다들 조용하다.
    #[default]
    Idle,
    /// 누가 일하는 중.
    Busy,
    /// 누가 답을 기다린다 — 사람 손이 필요하다.
    Wait,
    /// 누가 막혔다.
    Error,
    /// 오래 아무 일도 없었다.
    Sleep,
}

impl Mood {
    fn parse(s: &str) -> Self {
        match s {
            "busy" => Mood::Busy,
            "wait" => Mood::Wait,
            "error" => Mood::Error,
            "sleep" => Mood::Sleep,
            _ => Mood::Idle,
        }
    }

    /// 모션을 고를 때 쓰는 자리 번호. 모델이 가진 모션 수로 나눠 쓴다 — 공식 샘플은
    /// 무리 이름이 `Idle`·`TapBody` 뿐이라 「Busy 모션」 같은 것이 애초에 없다.
    pub fn slot(self) -> usize {
        match self {
            Mood::Idle => 0,
            Mood::Busy => 1,
            Mood::Wait => 2,
            Mood::Error => 3,
            Mood::Sleep => 4,
        }
    }
}

/// 파일 한 장을 읽어 상태와 할 말로. 파일이 없거나 깨졌으면 조용한 것으로 친다.
pub fn read(path: &std::path::Path) -> (Mood, String) {
    let Ok(t) = std::fs::read_to_string(path) else {
        return (Mood::Idle, String::new());
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) else {
        return (Mood::Idle, String::new());
    };
    let mood = Mood::parse(v.get("state").and_then(|s| s.as_str()).unwrap_or(""));
    let text = v
        .get("text")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    (mood, text)
}

/// 오래 조용하면 잠든다. 8분은 대화 탭이 「먼저 말 걸기」에 쓰는 것과 같은 기준이라
/// 화면 두 곳이 서로 다른 시각에 잠들지 않는다.
pub const SLEEP_AFTER: std::time::Duration = std::time::Duration::from_secs(8 * 60);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_broken_file_reads_as_quiet() {
        let (m, t) = read(std::path::Path::new("/그런/파일/없다.json"));
        assert_eq!(m, Mood::Idle);
        assert!(t.is_empty());
    }

    #[test]
    fn every_state_gets_its_own_motion_slot() {
        let all = [Mood::Idle, Mood::Busy, Mood::Wait, Mood::Error, Mood::Sleep];
        let mut slots: Vec<usize> = all.iter().map(|m| m.slot()).collect();
        slots.sort();
        slots.dedup();
        assert_eq!(slots.len(), all.len(), "상태마다 자리가 달라야 갈린다");
    }

    #[test]
    fn unknown_state_falls_back_to_quiet() {
        assert_eq!(Mood::parse("뭔지모를것"), Mood::Idle);
    }
}
