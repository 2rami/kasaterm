use anyhow::{ensure, Result};
use serde_json::{json, Value};

#[derive(Default)]
pub(super) struct SummonSpot {
    pub machine: Option<String>,
    pub beside: Option<String>,
    pub new_room: Option<String>,
    pub cwd: Option<String>,
    pub tab: bool,
}

impl SummonSpot {
    pub fn params(&self) -> Result<Value> {
        ensure!(self.beside.is_some() != self.new_room.is_some(),
            "--beside <보드 주소> 또는 --new-room '작업방 이름' 중 하나로 자리를 골라 주세요. 현재 열린 방에는 임의로 넣지 않아요");
        let mut params = json!({ "tab": self.tab });
        if let Some(room) = &self.new_room {
            ensure!(
                !room.trim().is_empty() && !room.chars().any(char::is_control),
                "새 작업방 이름을 한 줄로 주세요"
            );
            ensure!(
                !self.tab,
                "새 작업방의 첫 칸은 탭으로 만들 수 없어요 — --tab 은 --beside 와 함께 쓰세요"
            );
            params["window"] = json!("new");
            if let Some(machine) = &self.machine {
                params["machine_id"] = json!(machine);
            }
        } else if let Some(anchor) = &self.beside {
            let anchor = super::view_address(Some(anchor))?;
            ensure!(
                ["machine_id", "surface_key", "surface_id", "instance_id"]
                    .iter()
                    .all(|k| anchor[*k].as_str().is_some_and(|s| !s.is_empty())),
                "--beside 는 board --all 의 전체 주소로 주세요"
            );
            ensure!(
                self.machine
                    .as_deref()
                    .is_none_or(|m| m == super::row_text(&anchor, "machine_id")),
                "--beside 칸이 --machine 기계에 없어요"
            );
            params["address"] = anchor;
        }
        if let Some(dir) = &self.cwd {
            // 절대 경로의 표기와 존재 여부는 칸을 만드는 원본 기기가 검사한다.
            params["cwd"] = json!(dir);
        }
        Ok(params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor() -> String {
        json!({"machine_id":"mini", "surface_key":"key", "surface_id":"%4", "instance_id":"boot", "session_id":"session"}).to_string()
    }

    #[test]
    fn machine_alone_never_uses_the_active_room() {
        let spot = SummonSpot {
            machine: Some("mini".into()),
            ..Default::default()
        };
        assert!(spot.params().unwrap_err().to_string().contains("--beside"));
    }

    #[test]
    fn a_room_is_explicit_and_cannot_also_be_an_anchor_or_tab() {
        let mut spot = SummonSpot {
            machine: Some("mini".into()),
            new_room: Some("library".into()),
            ..Default::default()
        };
        assert_eq!(
            spot.params().unwrap(),
            json!({"machine_id":"mini", "window":"new", "tab":false})
        );
        spot.beside = Some(anchor());
        assert!(spot.params().is_err());
        spot.beside = None;
        spot.tab = true;
        assert!(spot.params().is_err());
        spot.tab = false;
        for bad in ["", "  ", "room\nother"] {
            spot.new_room = Some(bad.into());
            assert!(spot.params().is_err());
        }
    }

    #[test]
    fn an_anchor_keeps_full_identity_and_never_becomes_a_new_room() {
        let mut spot = SummonSpot {
            beside: Some(anchor()),
            tab: true,
            ..Default::default()
        };
        let params = spot.params().unwrap();
        assert_eq!(params["address"]["session_id"], "session");
        assert_eq!(params["tab"], true);
        assert!(params.get("window").is_none());
        spot.machine = Some("wrong".into());
        assert!(spot.params().is_err());
        spot.machine = None;
        spot.beside = Some(json!({"machine_id":"mini"}).to_string());
        assert!(spot.params().is_err());
    }

    #[test]
    fn target_machine_validates_its_own_path_syntax() {
        for dir in ["/work/project", r"C:\work\project", r"\\host\work\project"] {
            let spot = SummonSpot {
                new_room: Some("project".into()),
                cwd: Some(dir.into()),
                ..Default::default()
            };
            assert_eq!(spot.params().unwrap()["cwd"], dir);
        }
    }
}
