//! 카사브라우저 설정 「동기화」에 보일 상태(`sync.state.put`). 로그인 금고와 북마크·탭 동기화의 상태를 한 장으로
//! 묶는다. 글은 여기서 다 쓰고 브라우저는 그대로 그린다 — 문구가 두 레포에 갈라지지 않게.
//! 사람이 눌러야 하는 일은 `asks` 로 내놓고, 브라우저의 단추는 카사텀 창에 확인 시트를 띄우게만 한다.

use serde_json::{json, Value};

fn ask(id: &str, title: &str, detail: &str, button: &str, tone: &str) -> Value {
    json!({"id": id, "title": title, "detail": detail, "button": button, "tone": tone})
}

fn row(label: &str, value: &str) -> Value {
    json!({"label": label, "value": value})
}

/// 금고 상태(`login_vault::status()`)와 북마크·탭 상태(`browser_sync::status()`)로 그릴 한 장.
pub fn display(vault: &Value, browser: &Value, now_ms: u64) -> Value {
    let fingerprint = vault["fingerprint"].as_str().unwrap_or_default();
    let message = vault["message"].as_str().filter(|m| !m.is_empty());
    let mut asks = Vec::new();
    let mut rows = Vec::new();
    let mut notes = Vec::new();
    let (phase, headline, detail) = match vault["state"].as_str().unwrap_or("checking") {
        "signed_out" => ("off", "꺼짐", "카사텀에서 KASA 계정에 로그인하면 맞춰요".to_string()),
        "no_vault" => {
            asks.push(ask("create_vault", "이 기기에서 동기화 시작", "이 기기의 비밀번호를 잠가서 계정에 올리는 첫 기기가 돼요", "시작하기", "accent"));
            ("needs_you", "아직 시작 전", "한 번 시작하면 이 계정의 기기끼리 계속 맞춰요".to_string())
        }
        "waiting_for_key" => (
            "starting",
            "열쇠를 기다리는 중",
            "이 계정에서 이미 맞추는 기기가 켜지면 열쇠를 받아 바로 맞춰요".to_string(),
        ),
        "joined" => ("starting", "열쇠를 받았어요", "곧 맞추기 시작해요".to_string()),
        "synced" | "deletions_pending" => {
            rows.push(row("비밀번호", &format!("{}개", vault["items"].as_u64().unwrap_or(0))));
            browser_rows(browser, &mut rows, &mut asks, &mut notes);
            if vault["state"] == "deletions_pending" {
                let n = vault["deletions"].as_u64().unwrap_or(0);
                asks.push(ask("confirm_login_deletions", &format!("비밀번호 지우기 {n}개 보류 중"), "이 기기에서 한꺼번에 사라졌어요 — 정말 지운 것인지 확인해 주세요", "지우기 확인", "danger"));
            }
            let held = vault["held"].as_u64().unwrap_or(0);
            if held > 0 {
                notes.push(format!("다른 기기에서 지운 비밀번호 {held}개를 이 기기 키체인이 못 지웠어요 — 비밀번호 목록에서 지워 주세요"));
            }
            (if asks.is_empty() { "on" } else { "needs_you" }, "켜짐", "비밀번호·북마크·열린 탭을 이 계정의 기기끼리 맞추고 있어요".to_string())
        }
        "clock_mismatch" => ("error", "멈췄어요", message.unwrap_or("기기 시계를 맞추면 다시 맞춰요").to_string()),
        "key_mismatch" => ("error", "열쇠가 맞지 않아요", message.unwrap_or("이 기기의 금고 열쇠가 계정 금고와 달라요").to_string()),
        "unsupported_platform" => ("off", "꺼짐", "이 OS 에서는 아직 맞추지 않아요".to_string()),
        "checking" => ("starting", "확인하는 중", String::new()),
        _ => ("error", "맞추지 못했어요", message.unwrap_or("이 기기의 비밀번호·북마크는 그대로예요").to_string()),
    };
    if !fingerprint.is_empty() {
        notes.push(format!("비밀번호는 각 기기 키체인에 두고 잠가서 올려요. 같은 계정으로 로그인한 기기가 열쇠를 받도록 관문이 열쇠도 맡아요. 열쇠 지문 {fingerprint}"));
    }
    json!({
        "at_ms": now_ms,
        "phase": phase,
        "headline": headline,
        "detail": detail,
        "rows": rows,
        "note": notes.join("\n"),
        "asks": asks,
    })
}

fn browser_rows(browser: &Value, rows: &mut Vec<Value>, asks: &mut Vec<Value>, notes: &mut Vec<String>) {
    let message = browser["message"].as_str().filter(|m| !m.is_empty());
    let bookmarks = match browser["state"].as_str().unwrap_or("checking") {
        "synced" | "deletions_pending" | "browser_closed" => format!("{}개", browser["bookmarks"].as_u64().unwrap_or(0)),
        "checking" | "needs_vault" => "확인하는 중".to_string(),
        _ => message.unwrap_or("맞추지 못했어요").to_string(),
    };
    rows.push(row("북마크", &bookmarks));
    let devices = browser["devices"].as_u64().unwrap_or(0);
    let mut tabs = row("열린 탭", &if devices > 0 { format!("다른 기기 {devices}대") } else { "보내는 중".to_string() });
    tabs["detail"] = json!("다른 기기의 탭은 방문 기록 패널에서 봐요");
    rows.push(tabs);
    if browser["state"] == "deletions_pending" {
        let n = browser["deletions"].as_u64().unwrap_or(0);
        asks.push(ask("confirm_bookmark_deletions", &format!("북마크 지우기 {n}개 보류 중"), "이 기기에서 한꺼번에 사라졌어요 — 정말 지운 것인지 확인해 주세요", "지우기 확인", "danger"));
    }
    let too_large = browser["too_large"].as_u64().unwrap_or(0);
    if too_large > 0 {
        notes.push(format!("너무 커서 못 보낸 북마크 {too_large}개 — 아주 긴 북마클릿·data: 주소는 이 기기에만 남아요"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(state: &Value) -> Vec<String> {
        state["asks"].as_array().unwrap().iter().map(|a| a["id"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn a_synced_device_shows_counts_and_asks_nothing() {
        let vault = json!({"state": "synced", "fingerprint": "YGAG-9P8B-AVKM-HYVX", "items": 20});
        let browser = json!({"state": "synced", "bookmarks": 128, "devices": 1});
        let state = display(&vault, &browser, 7);
        assert_eq!(state["phase"], "on");
        assert_eq!(state["at_ms"], 7);
        assert_eq!(state["rows"][0]["value"], "20개");
        assert_eq!(state["rows"][1]["value"], "128개");
        assert_eq!(state["rows"][2]["value"], "다른 기기 1대");
        assert!(ids(&state).is_empty());
        assert!(state["note"].as_str().unwrap().contains("YGAG-9P8B-AVKM-HYVX"));
    }

    #[test]
    fn every_human_decision_becomes_an_ask() {
        let synced = |extra: Value| {
            let mut vault = json!({"state": "synced", "fingerprint": "F", "items": 1});
            vault.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            vault
        };
        assert_eq!(ids(&display(&json!({"state": "no_vault"}), &json!({}), 0)), ["create_vault"]);
        assert!(ids(&display(&json!({"state": "waiting_for_key"}), &json!({}), 0)).is_empty(), "새 기기는 누를 것 없이 열쇠를 기다린다");
        assert!(ids(&display(&json!({"state": "key_mismatch", "message": "달라요"}), &json!({}), 0)).is_empty());
        let deletions = display(&synced(json!({"state": "deletions_pending", "deletions": 12})), &json!({"state": "deletions_pending", "deletions": 3}), 0);
        assert_eq!(ids(&deletions), ["confirm_bookmark_deletions", "confirm_login_deletions"]);
        assert_eq!(deletions["phase"], "needs_you");
    }

    #[test]
    fn nothing_names_a_site_or_account() {
        let vault = json!({"state": "synced", "fingerprint": "F", "items": 2, "held": 1});
        let browser = json!({"state": "synced", "bookmarks": 3, "too_large": 1});
        let text = display(&vault, &browser, 0).to_string();
        for leak in ["http", "@", "password"] {
            assert!(!text.contains(leak), "{leak}");
        }
    }
}
