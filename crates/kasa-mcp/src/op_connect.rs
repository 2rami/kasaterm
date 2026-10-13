//! 「1Password 연결」 — 맥 1Password 앱의 CLI 연동으로 학생 전용 금고와 그 금고만 읽는 서비스 계정을 만들고, 받은
//! 토큰을 곧장 [`crate::op_approval::set_token`] 에 넘긴다(docs/op-faceid-approval.md 「연결」). 토큰은 `op` 의 표준
//! 출력에서 이 프로세스로만 오고 화면·로그·감사 줄 어디에도 안 적는다.
//!
//! 1Password 의 CLI 연동 허락은 tty 와 그 세션의 시작 시각에 묶인다. 명령마다 프로세스를 새로 띄우면 명령마다 1Password
//! 창이 뜨므로, 가짜 터미널 하나에 세션 지도자 셸을 세우고 명령을 한 줄씩 넣는다 — 허락은 한 번이고, 셸을 닫으면 그 tty 의
//! 허락도 같이 끝난다.

use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail};
use serde_json::Value;

pub const VAULT: &str = "KASA 학생";
const VAULT_ABOUT: &str = "kasaterm 학생이 폰 Face ID 승인으로 읽는 비밀만 두는 금고";
/// 사람이 1Password 창에서 허락할 시간까지.
const ASK_WAIT: Duration = Duration::from_secs(120);
const STEP_WAIT: Duration = Duration::from_secs(60);
/// 1Password 가 서명한 `op` 만 부른다 — 토큰이 그 출력으로 나온다.
const OP_REQUIREMENT: &str = r#"identifier "com.1password.op" and anchor apple generic and certificate leaf[subject.OU] = "2BUA8C4S2C""#;
const RS: u8 = 0x1e;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub id: String,
    pub url: String,
    pub email: String,
}

pub struct Made {
    pub token: String,
    pub account: String,
}

/// 쓸 `op`. 검증 리그는 `KASATERM_OP_CLI` 로 가짜를 준다(격리 실행에서만, 그때만 서명을 안 본다).
fn cli() -> anyhow::Result<PathBuf> {
    if !crate::device_auth::sync_environment_allowed() {
        return std::env::var_os("KASATERM_OP_CLI").map(PathBuf::from).ok_or_else(|| anyhow!("cli_missing"));
    }
    let path = ["/opt/homebrew/bin/op", "/usr/local/bin/op"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .ok_or_else(|| anyhow!("cli_missing"))?;
    let signed = Command::new("/usr/bin/codesign")
        .arg("--verify")
        .arg(format!("-R={OP_REQUIREMENT}"))
        .arg(&path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    anyhow::ensure!(signed, "cli_unverified");
    Ok(path)
}

/// 금고를 찾거나 만들고 서비스 계정을 만들어 토큰을 넣는다. 계정이 여럿이면 [`choose`] 가 고른다(취소면 None).
pub fn connect(choose: &mut dyn FnMut(&[Account]) -> Option<usize>) -> anyhow::Result<String> {
    let made = make(&cli()?, &host(), choose)?;
    // 막 만든 서비스 계정이 1Password 쪽에 퍼지기까지 몇 초 걸릴 수 있다.
    let mut last = None;
    for wait in [0, 2, 4, 8, 16] {
        std::thread::sleep(Duration::from_secs(wait));
        match crate::op_approval::set_token(&made.token) {
            Ok(vault) => return Ok(format!("{vault} · {}", made.account)),
            Err(error) if error.to_string().starts_with("resolve_failed") => last = Some(error),
            Err(error) => return Err(anyhow!("made_unusable: {error}")),
        }
    }
    Err(anyhow!("made_unusable: {}", last.map(|e| e.to_string()).unwrap_or_default()))
}

fn host() -> String {
    let mut buf = [0u8; 256];
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
    let name = if ok { String::from_utf8_lossy(&buf[..buf.iter().position(|&b| b == 0).unwrap_or(0)]).into_owned() } else { String::new() };
    let name = name.split('.').next().unwrap_or_default().trim().to_string();
    if name.is_empty() { "mac".into() } else { name }
}

pub fn make(op: &Path, host: &str, choose: &mut dyn FnMut(&[Account]) -> Option<usize>) -> anyhow::Result<Made> {
    let op = sq(op.to_str().ok_or_else(|| anyhow!("cli_missing"))?);
    let mut sh = Shell::open()?;
    let accounts = accounts(&sh.run(&format!("{op} account list --format=json"), ASK_WAIT)?)?;
    let account = match accounts.len() {
        0 => bail!("no_account"),
        1 => accounts[0].clone(),
        _ => choose(&accounts).and_then(|i| accounts.get(i).cloned()).ok_or_else(|| anyhow!("cancelled"))?,
    };
    let acc = sq(&account.id);
    let vaults = sh.run(&format!("{op} vault list --account {acc} --format=json"), ASK_WAIT)?;
    let vault = match vault_id(&vaults, VAULT) {
        Some(id) => id,
        None => {
            let made = sh.run(
                &format!("{op} vault create {} --description {} --account {acc} --format=json", sq(VAULT), sq(VAULT_ABOUT)),
                STEP_WAIT,
            )?;
            serde_json::from_str::<Value>(&made).ok().and_then(|v| v["id"].as_str().map(str::to_string)).ok_or_else(|| anyhow!("cli_failed"))?
        }
    };
    let name = sq(&format!("kasaterm {host}"));
    let token = sh
        .run(&format!("{op} service-account create {name} --vault {} --account {acc} --raw", sq(&format!("{vault}:read_items"))), STEP_WAIT)
        .map_err(|e| anyhow!("service_account: {e}"))?
        .trim()
        .to_string();
    anyhow::ensure!(token.starts_with("ops_"), "service_account: no token");
    let _ = sh.run(&format!("{op} signout --account {acc}"), Duration::from_secs(10));
    Ok(Made { token, account: account.url })
}

fn accounts(text: &str) -> anyhow::Result<Vec<Account>> {
    let list: Value = serde_json::from_str(text.trim()).map_err(|_| anyhow!("cli_failed"))?;
    Ok(list
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let url = a["url"].as_str().unwrap_or_default().to_string();
            let id = a["account_uuid"].as_str().filter(|s| !s.is_empty()).map(str::to_string).unwrap_or_else(|| url.clone());
            (!id.is_empty()).then(|| Account { id, url, email: a["email"].as_str().unwrap_or_default().to_string() })
        })
        .collect())
}

fn vault_id(text: &str, name: &str) -> Option<String> {
    let list: Value = serde_json::from_str(text.trim()).ok()?;
    list.as_array()?.iter().find(|v| v["name"] == name).and_then(|v| v["id"].as_str().map(str::to_string))
}

fn sq(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// 1Password 가 남긴 마지막 오류 한 줄 — 「[ERROR] 2026/10/13 12:00:00 」 머리와 색 코드는 벗긴다.
fn last_error(tty: &[u8]) -> String {
    let text = String::from_utf8_lossy(tty);
    let mut plain = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        plain.push(c);
    }
    let line = plain.lines().map(str::trim).filter(|l| !l.is_empty()).last().unwrap_or_default();
    let line = match line.strip_prefix("[ERROR]") {
        Some(rest) => rest.trim_start().splitn(3, ' ').nth(2).unwrap_or(rest).trim(),
        None => line,
    };
    line.chars().take(240).collect()
}

/// 가짜 터미널에 세운 세션 지도자 셸 — 명령은 표준 입력으로, 결과는 표준 출력으로, `op` 의 말은 tty 로 간다.
struct Shell {
    child: Child,
    input: ChildStdin,
    out: Receiver<Vec<u8>>,
    tty: Arc<Mutex<Vec<u8>>>,
    pending: Vec<u8>,
}

impl Shell {
    fn open() -> anyhow::Result<Self> {
        let (mut master, mut slave) = (0, 0);
        let opened = unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) };
        anyhow::ensure!(opened == 0, "pty_failed");
        let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        for fd in [&master, &slave] {
            unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-s").env_clear().env("PATH", "/usr/bin:/bin").env("LANG", "en_US.UTF-8");
        for key in ["HOME", "USER", "LOGNAME", "TMPDIR"] {
            if let Some(value) = std::env::var_os(key) {
                cmd.env(key, value);
            }
        }
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::from(slave.try_clone()?));
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 || libc::ioctl(2, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn().map_err(|_| anyhow!("pty_failed"))?;
        drop(slave);
        let input = child.stdin.take().ok_or_else(|| anyhow!("pty_failed"))?;
        let mut stdout = child.stdout.take().ok_or_else(|| anyhow!("pty_failed"))?;
        let (tx, out) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n @ 1..) = stdout.read(&mut buf) {
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        let tty = Arc::new(Mutex::new(Vec::new()));
        let sink = tty.clone();
        let mut master = std::fs::File::from(master);
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n @ 1..) = master.read(&mut buf) {
                let mut kept = sink.lock().unwrap_or_else(|e| e.into_inner());
                kept.extend_from_slice(&buf[..n]);
                let over = kept.len().saturating_sub(16 * 1024);
                kept.drain(..over);
            }
        });
        Ok(Self { child, input, out, tty, pending: Vec::new() })
    }

    /// 한 줄 돌리고 그 표준 출력을 준다. 0 이 아니면 1Password 가 tty 에 남긴 마지막 말이 오류가 된다.
    fn run(&mut self, line: &str, wait: Duration) -> anyhow::Result<String> {
        self.tty.lock().unwrap_or_else(|e| e.into_inner()).clear();
        writeln!(self.input, "{line} </dev/tty 2>/dev/tty; printf '\\036%d\\n' \"$?\"").map_err(|_| anyhow!("cli_failed"))?;
        self.input.flush().map_err(|_| anyhow!("cli_failed"))?;
        let deadline = Instant::now() + wait;
        loop {
            if let Some(at) = self.pending.iter().position(|&b| b == RS) {
                if let Some(nl) = self.pending[at..].iter().position(|&b| b == b'\n') {
                    let code = String::from_utf8_lossy(&self.pending[at + 1..at + nl]).trim().parse::<i32>().unwrap_or(-1);
                    let out = String::from_utf8_lossy(&self.pending[..at]).into_owned();
                    self.pending.drain(..at + nl + 1);
                    if code == 0 {
                        return Ok(out);
                    }
                    // tty 를 읽는 실은 따로 돌아 마지막 말이 조금 늦게 닿을 수 있다.
                    std::thread::sleep(Duration::from_millis(150));
                    let said = last_error(&self.tty.lock().unwrap_or_else(|e| e.into_inner()));
                    bail!("cli_error: {said}");
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            match self.out.recv_timeout(left) {
                Ok(chunk) => self.pending.extend(chunk),
                Err(RecvTimeoutError::Timeout) => bail!("cli_timeout"),
                Err(RecvTimeoutError::Disconnected) => bail!("cli_failed"),
            }
        }
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        let _ = writeln!(self.input, "exit");
        let _ = self.input.flush();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 가짜 `op` — 받은 인자와 그때의 tty·세션을 적고, 1Password 처럼 JSON·토큰을 낸다.
    fn fake(dir: &Path, accounts: &str, vaults: &str, sa_fails: bool) -> PathBuf {
        let path = dir.join("op");
        let log = dir.join("calls");
        let script = format!(
            r#"#!/bin/sh
echo "$(tty) $(ps -o sess= -p $$ 2>/dev/null | tr -d ' ') $*" >> '{log}'
case "$1 $2" in
  "account list") printf '%s' '{accounts}' ;;
  "vault list") printf '%s' '{vaults}' ;;
  "vault create") printf '{{"id":"newvault1","name":"%s"}}' "$3" ;;
  "service-account create")
    if [ "{fails}" = "1" ]; then echo '[ERROR] 2026/10/13 12:00:00 You do not have permission to create service accounts' >&2; exit 1; fi
    printf 'ops_faketoken123' ;;
  "signout --account") ;;
  *) echo "unknown $*" >&2; exit 2 ;;
esac
"#,
            log = log.display(),
            fails = if sa_fails { "1" } else { "0" },
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        path
    }

    struct Dir(PathBuf);

    impl Dir {
        fn new() -> Self {
            static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("kasa-op-connect-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn calls(dir: &Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("calls")).unwrap_or_default().lines().map(str::to_string).collect()
    }

    #[test]
    fn one_tty_session_makes_the_vault_and_a_read_only_service_account() {
        let dir = Dir::new();
        let op = fake(dir.path(), r#"[{"url":"my.1password.com","email":"a@b","account_uuid":"ACC1"}]"#, r#"[{"id":"v0","name":"Private"}]"#, false);
        let made = make(&op, "mini", &mut |_| panic!("한 계정인데 물었다")).unwrap();
        assert_eq!(made.token, "ops_faketoken123");
        assert_eq!(made.account, "my.1password.com");
        let calls = calls(dir.path());
        assert_eq!(calls.len(), 5, "{calls:#?}");
        assert!(calls[2].contains("vault create KASA 학생"), "{calls:#?}");
        assert!(calls[3].contains("service-account create kasaterm mini --vault newvault1:read_items --account ACC1 --raw"), "{calls:#?}");
        // 1Password 허락이 한 번으로 끝나려면 모든 op 가 같은 tty·같은 세션에서 돌아야 한다.
        let ttys: Vec<_> = calls.iter().map(|c| c.split(' ').take(2).collect::<Vec<_>>().join(" ")).collect();
        assert!(ttys[0].starts_with("/dev/tty"), "{calls:#?}");
        assert!(ttys.iter().all(|t| t == &ttys[0]), "{calls:#?}");
    }

    #[test]
    fn an_existing_vault_is_reused_and_the_person_picks_among_accounts() {
        let dir = Dir::new();
        let op = fake(
            dir.path(),
            r#"[{"url":"my.1password.com","email":"a@b","account_uuid":"ACC1"},{"url":"team.1password.com","email":"a@team","account_uuid":"ACC2"}]"#,
            r#"[{"id":"keep1","name":"KASA 학생"}]"#,
            false,
        );
        let mut asked = Vec::new();
        let made = make(&op, "book", &mut |list| {
            asked = list.to_vec();
            Some(1)
        })
        .unwrap();
        assert_eq!(asked.len(), 2);
        assert_eq!(made.account, "team.1password.com");
        let calls = calls(dir.path());
        assert!(!calls.iter().any(|c| c.contains("vault create")), "{calls:#?}");
        assert!(calls.iter().any(|c| c.contains("--vault keep1:read_items --account ACC2")), "{calls:#?}");
    }

    #[test]
    fn a_refusal_from_1password_reads_as_its_own_words_and_cancel_makes_nothing() {
        let dir = Dir::new();
        let op = fake(dir.path(), r#"[{"url":"my.1password.com","email":"a@b","account_uuid":"ACC1"}]"#, "[]", true);
        let error = make(&op, "mini", &mut |_| None).err().unwrap().to_string();
        assert_eq!(error, "service_account: cli_error: You do not have permission to create service accounts");

        let dir = Dir::new();
        let op = fake(dir.path(), r#"[{"account_uuid":"A"},{"account_uuid":"B"}]"#, "[]", false);
        assert_eq!(make(&op, "mini", &mut |_| None).err().unwrap().to_string(), "cancelled");
        assert_eq!(calls(dir.path()).len(), 1, "취소했는데 계정 목록 다음 명령이 나갔다");
    }

    #[test]
    fn names_are_quoted_for_the_shell() {
        assert_eq!(sq("a'b c"), r"'a'\''b c'");
        assert_eq!(last_error(b"\x1b[31m[ERROR] 2026/10/13 12:00:00 nope\x1b[0m\r\n"), "nope");
    }
}
