//! 바깥주소(cloudflared named tunnel) 온오프.
//!
//! 폰이 집 밖에서 붙는 유일한 길이 이 터널인데, 상시 노출은 그때그때 켜는 것과
//! 위험 등급이 달라 부팅 자동 시작을 일부러 안 한다(~/.cloudflared/config.yml 의
//! 주석). 켜고 끄는 손이 GUI(창 우하단 칩)와 HTTP(/term/tunnel) 양쪽이라
//! 로직을 여기 한 곳에 모은다.
//!
//! **앱을 꺼도 터널은 일부러 안 죽인다.** remote 토큰을 파일로 남기는 것과 같은
//! 이유다 — 터널이 앱 재시작을 넘겨 살아 있어야 폰 북마크가 계속 붙는다. 그렇게
//! 고아가 된 프로세스는 재시작 뒤 pgrep 으로 되찾아 상태·끄기를 잇는다.

static TUNNEL_CHILD: std::sync::Mutex<Option<std::process::Child>> = std::sync::Mutex::new(None);

/// cloudflared 실행 파일. GUI 앱은 셸 PATH 를 물려받지 않으므로(Finder 실행엔
/// 셸 환경 자체가 없다) homebrew 의 두 자리를 직접 짚고, 마지막에야 PATH 를 믿는다.
fn cloudflared_bin() -> std::path::PathBuf {
    ["/opt/homebrew/bin/cloudflared", "/usr/local/bin/cloudflared"]
        .iter()
        .map(std::path::PathBuf::from)
        .find(|p| p.exists())
        .unwrap_or_else(|| std::path::PathBuf::from("cloudflared"))
}

/// 살아 있는 터널의 pid. 우리 자식 우선(끝났으면 여기서 회수해 좀비를 막는다),
/// 없으면 프로세스 목록 — 앱 재시작 전에 켜 둔 고아를 이 폴백이 되찾는다.
fn tunnel_pid() -> Option<u32> {
    {
        let mut own = TUNNEL_CHILD.lock().unwrap();
        if let Some(c) = own.as_mut() {
            match c.try_wait() {
                Ok(None) => return Some(c.id()),
                _ => *own = None,
            }
        }
    }
    let out = std::process::Command::new("pgrep")
        .args(["-f", "cloudflared tunnel run"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).lines().next()?.trim().parse().ok()
}

/// 관문(중계소)이 설정돼 있으면 「바깥」은 cloudflared 가 아니라 **업링크**다 — 앱이
/// 관문에 붙어 자기 주소를 받는다(uplink.rs). 그때 이 파일의 cloudflared 손은 안 쓴다.
fn gateway_mode() -> bool {
    crate::mobile::gateway().is_some()
}

pub fn is_on() -> bool {
    if gateway_mode() {
        return crate::mobile::published();
    }
    tunnel_pid().is_some()
}

/// 업링크가 실제로 붙어 있나(스위치가 켜진 것과 다르다 — 관문이 안 닿으면 꺼진 채다).
pub fn is_connected() -> bool {
    if gateway_mode() {
        return crate::uplink::status().connected;
    }
    tunnel_pid().is_some()
}

/// ~/.cloudflared/config.yml 의 첫 hostname — 화면 표시용. 없으면 이름 붙은
/// 터널이 아직 구축되지 않은 것이니 켜기를 거부하는 근거로도 쓴다.
pub fn host() -> Option<String> {
    if gateway_mode() {
        return crate::mobile::gateway_host();
    }
    let home = std::env::var("HOME").ok()?;
    let s = std::fs::read_to_string(
        std::path::PathBuf::from(home).join(".cloudflared/config.yml"),
    )
    .ok()?;
    s.lines().find_map(|l| {
        Some(l.trim().strip_prefix("- hostname:")?.trim().to_string())
    })
}

/// 켜거나 끈다. `Ok(현재 on 상태)`.
///
/// 끄기는 TERM 만 보내고 기다리지 않는다 — 종료가 늦으면 다음 상태 조회의
/// try_wait/pgrep 이 정리된 결과를 준다(부르는 쪽은 어차피 폴링한다).
pub fn set(on: bool) -> Result<bool, String> {
    if gateway_mode() {
        crate::mobile::set_published(on).map_err(|e| format!("스위치 저장 실패: {e}"))?;
        return Ok(on);
    }
    if !on {
        if let Some(pid) = tunnel_pid() {
            let _ = std::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .status();
        }
        return Ok(false);
    }
    if tunnel_pid().is_some() {
        return Ok(true);
    }
    if host().is_none() {
        return Err(
            "~/.cloudflared/config.yml 이 없어요 — 이름 붙은 터널부터 만들어야 해요".to_string(),
        );
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::env::temp_dir().join("kasaterm-tunnel.log"));
    let mut cmd = std::process::Command::new(cloudflared_bin());
    cmd.args(["tunnel", "run", "kasaterm"]).stdin(std::process::Stdio::null());
    match log {
        Ok(f) => {
            if let Ok(f2) = f.try_clone() {
                cmd.stderr(f2);
            }
            cmd.stdout(f);
        }
        Err(_) => {
            cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        }
    }
    match cmd.spawn() {
        Ok(child) => {
            *TUNNEL_CHILD.lock().unwrap() = Some(child);
            Ok(true)
        }
        Err(e) => Err(format!("cloudflared 실행 실패: {e} (brew install cloudflared)")),
    }
}
