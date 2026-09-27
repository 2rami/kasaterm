//! `kasa-relay` — 폰 관문 서버. 앱이 업링크로 붙고 폰은 `/u/<slug>/…` 로 들어온다.
//! 배포 위치(넷버드망·클러스터)와 무관 — 어디서 실행하든 코드는 같다.
//!
//!   kasa-relay [--bind <주소>] [--port <n>] [--state <파일>]
//!   (기본 127.0.0.1 · 8790 · ~/.config/kasaterm/relay-state.json)
//!
//!   kasa-relay account add|passwd|disable|enable|list [<이름>] [--accounts <파일>]
//!   (기본 ~/.config/kasaterm/relay-accounts.json — 돌고 있는 관문이 바뀐 것을 알아서 다시 읽는다)
//!
//! 관문 자체는 주소(slug)를 인증하지 않는다 — 그건 각 앱이 한다(`gateway.rs` 머리말). 계정은
//! 기기 로그인(`POST /relay/login`)에만 쓴다.

use kasa_mcp::relay_auth::{self, Account};

fn config_file(name: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config/kasaterm").join(name))
}

fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("account") {
        args.remove(0);
        return account_cmd(args);
    }
    let mut bind = "127.0.0.1".to_string();
    let mut port: u16 = 8790;
    // 관문 slug 소유 기록. 기본 ~/.config/kasaterm/relay-state.json.
    let mut state = config_file("relay-state.json");
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        if a == "--port" {
            if let Some(p) = it.next().and_then(|s| s.parse().ok()) {
                port = p;
            }
        } else if a == "--bind" {
            if let Some(b) = it.next() {
                bind = b;
            }
        } else if a == "--state" {
            state = it.next().map(std::path::PathBuf::from);
        }
    }
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(kasa_mcp::relay::serve(&bind, port, state))
}

fn account_cmd(mut args: Vec<String>) -> anyhow::Result<()> {
    let mut path = config_file("relay-accounts.json");
    if let Some(i) = args.iter().position(|a| a == "--accounts") {
        args.remove(i);
        if i < args.len() {
            path = Some(args.remove(i).into());
        }
    }
    let path = path.ok_or_else(|| anyhow::anyhow!("HOME 이 없어 계정 파일 자리를 모르겠어요 — --accounts 로 주세요"))?;
    let verb = args.first().cloned().unwrap_or_default();
    let name = args.get(1).map(|s| s.trim().to_lowercase());
    let mut file = relay_auth::load_accounts(&path);
    let need_name = || -> anyhow::Result<String> {
        let n = name.clone().ok_or_else(|| anyhow::anyhow!("계정 이름을 주세요: kasa-relay account {verb} <이름>"))?;
        if !relay_auth::valid_account_name(&n) {
            anyhow::bail!("계정 이름은 소문자·숫자·-·_ 2~32자예요");
        }
        Ok(n)
    };
    match verb.as_str() {
        "add" | "passwd" => {
            let n = need_name()?;
            let exists = file.accounts.contains_key(&n);
            if verb == "add" && exists {
                anyhow::bail!("{n} 계정이 이미 있어요 — 비밀번호를 바꾸려면 passwd");
            }
            if verb == "passwd" && !exists {
                anyhow::bail!("{n} 계정이 없어요 — 먼저 add");
            }
            let pw = read_new_password()?;
            let created = file.accounts.get(&n).map_or_else(relay_auth::now_secs, |a| a.created);
            let disabled = file.accounts.get(&n).is_some_and(|a| a.disabled);
            file.accounts.insert(n.clone(), Account { pbkdf2_sha256: relay_auth::hash_password(&pw), created, disabled });
            relay_auth::save_accounts(&path, &file)?;
            println!("{n} 계정을 {}어요. 기기는 이 아이디·비밀번호로 로그인하면 돼요.", if verb == "add" { "만들었" } else { "고쳤" });
        }
        "disable" | "enable" => {
            let n = need_name()?;
            let a = file.accounts.get_mut(&n).ok_or_else(|| anyhow::anyhow!("{n} 계정이 없어요"))?;
            a.disabled = verb == "disable";
            relay_auth::save_accounts(&path, &file)?;
            println!(
                "{n} 계정을 {}어요.",
                if verb == "disable" { "막았어요 — 이 계정의 기기 토큰도 바로 안 통하게 됐" } else { "다시 열었" }
            );
        }
        "list" => {
            let mut names: Vec<_> = file.accounts.iter().collect();
            names.sort_by_key(|(n, _)| n.as_str());
            for (n, a) in names {
                println!("{n}{}", if a.disabled { "  (막힘)" } else { "" });
            }
        }
        _ => anyhow::bail!("kasa-relay account add|passwd|disable|enable|list [<이름>] [--accounts <파일>]"),
    }
    Ok(())
}

fn read_new_password() -> anyhow::Result<String> {
    let pw = read_password("새 비밀번호: ")?;
    if pw.chars().count() < 8 {
        anyhow::bail!("비밀번호는 8자 이상이어야 해요");
    }
    if is_tty() && read_password("한 번 더: ")? != pw {
        anyhow::bail!("두 번 친 비밀번호가 달라요");
    }
    Ok(pw)
}

fn is_tty() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// 화면에 안 찍히게 한 줄을 읽는다. 터미널이 아니면(파이프) 그냥 읽는다.
fn read_password(prompt: &str) -> anyhow::Result<String> {
    use std::io::Write as _;
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    #[cfg(unix)]
    let saved = if is_tty() {
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut t) == 0 {
                let old = t;
                t.c_lflag &= !libc::ECHO;
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &t);
                Some(old)
            } else {
                None
            }
        }
    } else {
        None
    };
    let mut line = String::new();
    let read = std::io::stdin().read_line(&mut line);
    #[cfg(unix)]
    if let Some(old) = saved {
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &old);
        }
        eprintln!();
    }
    read?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}
