//! `kasa-device` — 앱 없이 이 기계를 관문 계정에 붙이고 코딩 에이전트 계정 목록을 나눈다.
//!
//!   kasa-device login [<계정>]   비밀번호를 물어 기기 토큰을 받는다(`~/.config/kasaterm/device.json`)
//!   kasa-device status           로그인 상태(토큰 값은 안 보인다)
//!   kasa-device agents           이 기기 슬롯을 관문에 올리고 합친 목록을 보인다
//!   kasa-device logout
//!   kasa-device work <동작> ['<JSON>']   계정에 연결한 GitHub 일(관문이 대신 부른다, 쓰기는 사람 승인 대기)
//!     동작: connections · pr_create {repo,base,head,title,body,draft} · reject {id} · connections_audit
//!
//! 승인은 여기서 못 한다 — 앱 설정 화면에서만 난다(docs/account-connections.md).
//!
//! 설정 화면을 못 여는 기기(화면 없는 기기·ssh 로만 닿는 기기)용이다 — 앱 CLI 의 login 은
//! 2026-09-29 정리로 빠졌다. 앱과 같은 파일·같은 함수(`device_auth`·`agent_accounts`)를 쓰므로
//! 여기서 한 로그인을 새 판 앱이 그대로 이어 쓴다.

use kasa_mcp::{agent_accounts, device_auth, relay_auth};
use serde_json::json;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let verb = args.first().map(String::as_str).unwrap_or("status");
    let out = match verb {
        "login" => {
            let account = match args.get(1) {
                Some(a) => a.clone(),
                None => read_line("관문 계정: ")?,
            };
            let password = relay_auth::read_secret("비밀번호: ")?;
            device_auth::handle(&json!({ "op": "login", "account": account, "password": password }))?
        }
        "logout" => device_auth::handle(&json!({ "op": "logout" }))?,
        "status" => device_auth::status(),
        "work" => {
            let op = args.get(1).map(String::as_str).unwrap_or("connections");
            anyhow::ensure!(
                matches!(op, "connections" | "connections_audit" | "pr_create" | "reject"),
                "kasa-device work connections|pr_create|reject|connections_audit ['<JSON>']"
            );
            let mut params: serde_json::Value = match args.get(2) {
                Some(raw) => serde_json::from_str(raw)?,
                None => json!({}),
            };
            anyhow::ensure!(params.is_object(), "JSON 은 객체여야 해요");
            params["op"] = json!(op);
            device_auth::handle(&params)?
        }
        "agents" => {
            let v = device_auth::handle(&json!({ "op": "agents" }))?;
            print_agents(&v);
            return Ok(());
        }
        _ => anyhow::bail!("kasa-device login [<계정>] | status | agents | logout | work <동작> ['<JSON>']"),
    };
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

fn read_line(prompt: &str) -> anyhow::Result<String> {
    use std::io::Write as _;
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

fn print_agents(v: &serde_json::Value) {
    let list: Vec<agent_accounts::Shared> = serde_json::from_value(v["accounts"].clone()).unwrap_or_default();
    if list.is_empty() {
        println!("관문에 올라온 계정이 없어요");
    }
    for a in list {
        let here = a.devices.iter().find(|d| d.current).map(|d| format!("이 기기 {}", if d.slot.is_empty() { "기본" } else { &d.slot }));
        let name = if a.label.is_empty() { a.email.clone() } else { format!("{} · {}", a.label, a.email) };
        println!(
            "{:<7} {:<40} {}",
            a.provider,
            name,
            here.unwrap_or_else(|| format!("로그인 필요 — {}", a.elsewhere().join(", ")))
        );
    }
}
