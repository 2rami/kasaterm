//! 덮으면 이사 — 학생이 일하는 동안 맥북 잠자기를 막아 두고, 덮이면 학생을 다른 기기로 옮긴 뒤
//! 재운다. 정책은 앱(`kasaterm::lid_away`)이, 설치는 `kasaterm-cli lid-away` 가 하고, 둘이 같은
//! 경로·명령을 여기서 쓴다.
//!
//! 덮개 잠자기는 일반 권한으로 못 막는다 — 전원 단언은 덮개를 이기지 못하고, 잠자기 알림에 늦게
//! 답해도 30초가 끝이다. 그래서 `pmset -a disablesleep` 를 sudoers 규칙 한 줄로 허락받는다.
//! 그 값은 재부팅 뒤에도 남으므로, 앱이 죽거나 멈추면 LaunchAgent 감시가 심장 박동 파일의
//! 나이를 보고 푼다.

use anyhow::{anyhow, bail, Context, Result};
use std::path::PathBuf;
use std::process::Command;

pub const SUDOERS_PATH: &str = "/etc/sudoers.d/kasaterm-lid-away";
pub const LAUNCH_LABEL: &str = "com.kasaterm.lid-away-watchdog";
const PMSET: &str = "/usr/bin/pmset";
/// 감시가 이보다 오래 고쳐 쓰지 않은 박동을 「앱이 죽었다」로 본다. 앱은 5초마다 쓴다.
pub const HEARTBEAT_STALE_SECS: u64 = 90;
/// 리그가 진짜 잠자기 설정을 건드리지 않게 pmset 대신 부를 실행 파일.
const PMSET_OVERRIDE_ENV: &str = "KASATERM_LID_AWAY_PMSET";

/// 앱이 잠자기를 막아 둔 동안 고쳐 쓰는 박동 파일. 리그는 제 설정 파일 옆에 둬서 본판 감시가
/// 리그의 박동을 보지 않는다.
pub fn heartbeat_path() -> Option<PathBuf> {
    if let Some(settings) = std::env::var_os("KASATERM_SETTINGS_FILE").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(settings).with_file_name("lid-away.armed"));
    }
    Some(crate::home_dir()?.join(".config/kasaterm/lid-away.armed"))
}

fn pmset(args: &[&str]) -> Result<()> {
    let output = match std::env::var_os(PMSET_OVERRIDE_ENV).filter(|v| !v.is_empty()) {
        Some(fake) => Command::new(fake).args(args).output(),
        None => Command::new("/usr/bin/sudo").arg("-n").arg(PMSET).args(args).output(),
    }
    .context("pmset 실행 실패")?;
    if !output.status.success() {
        bail!("pmset {} 실패: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

pub fn set_sleep_disabled(on: bool) -> Result<()> {
    pmset(&["-a", "disablesleep", if on { "1" } else { "0" }])
}

pub fn sleep_now() -> Result<()> {
    pmset(&["sleepnow"])
}

/// sudoers 규칙이 깔려 비밀번호 없이 잠자기를 막을 수 있나. 프로세스를 띄우므로 앱은 캐시해서 쓴다.
pub fn sudo_ready() -> bool {
    if std::env::var_os(PMSET_OVERRIDE_ENV).is_some_and(|v| !v.is_empty()) {
        return true;
    }
    Command::new("/usr/bin/sudo")
        .args(["-n", "-l", PMSET, "-a", "disablesleep", "1"])
        .output()
        .is_ok_and(|o| o.status.success())
}

pub fn sudoers_rule(user: &str) -> Result<String> {
    if user.is_empty() || !user.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) {
        bail!("sudoers 에 적을 수 없는 사용자 이름이에요: {user:?}");
    }
    Ok(format!(
        "# kasaterm lid-away. Remove with: kasaterm-cli lid-away uninstall\n\
         {user} ALL=(root) NOPASSWD: {PMSET} -a disablesleep 1, {PMSET} -a disablesleep 0, {PMSET} sleepnow\n"
    ))
}

pub fn watchdog_script() -> String {
    format!(
        r#"#!/bin/sh
# kasaterm 덮으면 이사 감시. 앱이 잠자기를 막아 둔 채 죽거나 멈추면(박동이 {stale}초 넘게 멎으면) 푼다.
hb="$HOME/.config/kasaterm/lid-away.armed"
[ -f "$hb" ] || exit 0
age=$(( $(date +%s) - $(stat -f %m "$hb" 2>/dev/null || echo 0) ))
[ "$age" -lt {stale} ] && exit 0
/usr/bin/sudo -n {PMSET} -a disablesleep 0 || exit 1
rm -f "$hb"
if /usr/sbin/ioreg -r -k AppleClamshellState -d 1 | grep -q '"AppleClamshellState" = Yes'; then
  /usr/bin/sudo -n {PMSET} sleepnow
fi
exit 0
"#,
        stale = HEARTBEAT_STALE_SECS,
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn launch_agent_plist(script: &std::path::Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LAUNCH_LABEL}</string>
  <key>ProgramArguments</key>
  <array><string>/bin/sh</string><string>{}</string></array>
  <key>RunAtLoad</key><true/>
  <key>StartInterval</key><integer>30</integer>
</dict>
</plist>
"#,
        xml_escape(&script.to_string_lossy())
    )
}

struct Paths {
    script: PathBuf,
    plist: PathBuf,
}

fn paths() -> Result<Paths> {
    let home = crate::home_dir().ok_or_else(|| anyhow!("HOME 을 모르겠어요"))?;
    Ok(Paths {
        script: home.join(".config/kasaterm/lid-away-watchdog.sh"),
        plist: home.join(format!("Library/LaunchAgents/{LAUNCH_LABEL}.plist")),
    })
}

fn launchctl(args: &[&str]) -> bool {
    Command::new("/bin/launchctl").args(args).status().is_ok_and(|s| s.success())
}

fn uid() -> String {
    // SAFETY: getuid 는 실패하지 않는다.
    unsafe { libc::getuid() }.to_string()
}

/// 사람의 터미널에서 돈다 — sudo 가 비밀번호를 묻는다.
pub fn install() -> Result<String> {
    let user = std::env::var("USER").context("USER 를 모르겠어요")?;
    let rule = sudoers_rule(&user)?;
    let p = paths()?;
    for dir in [p.script.parent(), p.plist.parent()].into_iter().flatten() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&p.script, watchdog_script())?;
    std::fs::set_permissions(&p.script, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    std::fs::write(&p.plist, launch_agent_plist(&p.script))?;
    let domain = format!("gui/{}", uid());
    launchctl(&["bootout", &format!("{domain}/{LAUNCH_LABEL}")]);
    if !launchctl(&["bootstrap", &domain, &p.plist.to_string_lossy()]) {
        bail!("감시 LaunchAgent 를 못 올렸어요: {}", p.plist.display());
    }

    let tmp = std::env::temp_dir().join(format!("kasaterm-lid-away-{}.sudoers", std::process::id()));
    std::fs::write(&tmp, &rule)?;
    // 깨진 sudoers 는 sudo 전체를 막는다 — visudo 가 통과시킨 것만 남긴다.
    let installed = Command::new("/usr/bin/sudo")
        .args([
            "/bin/sh",
            "-c",
            "install -m 0440 -o root -g wheel \"$1\" \"$2\" && /usr/sbin/visudo -cf \"$2\" >/dev/null || { rm -f \"$2\"; exit 1; }",
            "sh",
        ])
        .arg(&tmp)
        .arg(SUDOERS_PATH)
        .status();
    let _ = std::fs::remove_file(&tmp);
    if !installed.is_ok_and(|s| s.success()) {
        bail!("sudoers 규칙을 못 깔았어요 — 비밀번호를 넣었는지 확인해 주세요");
    }
    if !sudo_ready() {
        bail!("규칙은 깔렸는데 sudo 가 아직 허락하지 않아요 — `sudo -n -l {PMSET} -a disablesleep 1` 로 확인해 주세요");
    }
    Ok(format!("설치했어요 — {SUDOERS_PATH}, {}", p.plist.display()))
}

pub fn uninstall() -> Result<String> {
    let p = paths()?;
    // 막아 둔 채 지우면 아무도 못 푼다.
    let _ = set_sleep_disabled(false);
    launchctl(&["bootout", &format!("gui/{}/{LAUNCH_LABEL}", uid())]);
    let _ = std::fs::remove_file(&p.plist);
    let _ = std::fs::remove_file(&p.script);
    if let Some(hb) = heartbeat_path() {
        let _ = std::fs::remove_file(hb);
    }
    let removed = Command::new("/usr/bin/sudo").args(["rm", "-f", SUDOERS_PATH]).status();
    if !removed.is_ok_and(|s| s.success()) {
        bail!("{SUDOERS_PATH} 를 못 지웠어요");
    }
    Ok("지웠어요 — 잠자기 막음도 풀었어요".into())
}

pub fn status() -> String {
    let p = paths().ok();
    let watchdog = p.as_ref().is_some_and(|p| p.plist.exists() && p.script.exists());
    let armed = heartbeat_path().is_some_and(|hb| hb.exists());
    format!(
        "sudoers 규칙: {}\n감시 LaunchAgent: {}\n지금 잠자기 막음: {}",
        if sudo_ready() { "있음" } else { "없음 — kasaterm-cli lid-away install" },
        if watchdog { "있음" } else { "없음" },
        if armed { "켜짐(학생이 일하는 중)" } else { "꺼짐" },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_allows_only_the_three_pmset_calls() {
        let rule = sudoers_rule("kasa").unwrap();
        let line = rule.lines().find(|l| !l.starts_with('#')).unwrap();
        assert_eq!(
            line,
            "kasa ALL=(root) NOPASSWD: /usr/bin/pmset -a disablesleep 1, /usr/bin/pmset -a disablesleep 0, /usr/bin/pmset sleepnow"
        );
    }

    #[test]
    fn rule_refuses_names_that_could_widen_it() {
        for bad in ["", "a b", "a,ALL", "x\nroot", "ALL=(ALL)"] {
            assert!(sudoers_rule(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn watchdog_watches_the_real_heartbeat() {
        let script = watchdog_script();
        assert!(script.contains("$HOME/.config/kasaterm/lid-away.armed"));
        assert!(script.contains(&format!("-lt {HEARTBEAT_STALE_SECS} ]")));
    }
}
