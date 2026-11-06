use std::cell::RefCell;
use std::io::Read;
use std::process::Stdio;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}, mpsc};
use std::time::{Duration, Instant};

#[derive(Clone, Default, serde::Serialize)]
pub(crate) struct View {
    pub(crate) networks: Vec<String>,
    pub(crate) status: String,
    pub(crate) busy: bool,
}

struct Job {
    receiver: mpsc::Receiver<ResultData>,
    cancel: Arc<AtomicBool>,
    automatic: bool,
}

enum ResultData { Networks(Result<Vec<String>, String>), Connection(Result<String, String>) }

#[derive(Default)]
struct State {
    view: View,
    job: Option<Job>,
    scanned: bool,
    away_ssid: Option<String>,
}

thread_local! { static STATE: RefCell<State> = RefCell::new(State::default()); }

pub(crate) fn valid_ssid(ssid: &str) -> bool {
    !ssid.trim().is_empty() && ssid.len() <= 32 && !ssid.chars().any(char::is_control)
}

fn collect(st: &mut State) -> bool {
    let Some(job) = &st.job else { return false };
    let data = match job.receiver.try_recv() {
        Ok(data) => data,
        Err(mpsc::TryRecvError::Empty) => return false,
        Err(mpsc::TryRecvError::Disconnected) => ResultData::Connection(Err("연결 확인이 중단됐어요. 다시 확인해 주세요".into())),
    };
    if !job.cancel.load(Ordering::Acquire) {
        match data {
            ResultData::Networks(Ok(networks)) => st.view.networks = networks,
            ResultData::Networks(Err(error)) => st.view.status = error,
            ResultData::Connection(result) => st.view.status = result.unwrap_or_else(|error| error),
        }
    }
    st.job = None;
    st.view.busy = false;
    true
}

fn launch(st: &mut State, ssid: Option<String>, automatic: bool) -> Result<(), String> {
    collect(st);
    if st.job.is_some() { return Err("연결 확인이 진행 중이에요".into()) }
    let (sender, receiver) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = cancel.clone();
    st.view.busy = true;
    if ssid.is_some() { st.view.status = "등록한 핫스팟에 연결하는 중이에요".into(); }
    std::thread::spawn(move || {
        let data = match ssid {
            Some(ssid) => ResultData::Connection(connect(&ssid, &worker_cancel)),
            None => ResultData::Networks(saved_networks(&worker_cancel)),
        };
        let _ = sender.send(data);
    });
    st.job = Some(Job { receiver, cancel, automatic });
    Ok(())
}

pub(crate) fn view() -> View {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        collect(&mut st);
        if !st.scanned && st.job.is_none() {
            st.scanned = true;
            let _ = launch(&mut st, None, false);
        }
        st.view.clone()
    })
}

pub(crate) fn refresh() {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        st.scanned = false;
    });
}

pub(crate) fn test(ssid: &str) -> Result<(), String> {
    if !valid_ssid(ssid) { return Err("먼저 핫스팟을 등록해 주세요".into()) }
    STATE.with(|cell| launch(&mut cell.borrow_mut(), Some(ssid.into()), false))
}

pub(crate) fn pending() -> bool { STATE.with(|cell| cell.borrow().job.is_some()) }

pub(crate) fn cancel() {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        if let Some(job) = &st.job { job.cancel.store(true, Ordering::Release); }
        st.away_ssid = None;
        st.view.status.clear();
    });
}

pub(crate) fn tick(away_ssid: Option<&str>) -> bool {
    STATE.with(|cell| {
        let mut st = cell.borrow_mut();
        let mut changed = collect(&mut st);
        let target = away_ssid.filter(|ssid| valid_ssid(ssid));
        if target != st.away_ssid.as_deref() {
            if let Some(job) = &st.job {
                if job.automatic { job.cancel.store(true, Ordering::Release); }
            }
            if target.is_none() { st.away_ssid = None; }
        }
        // An unverified association must not repeatedly kick a working Wi-Fi connection.
        if let Some(ssid) = target {
            if st.away_ssid.as_deref() != Some(ssid) && st.job.is_none() {
                st.away_ssid = Some(ssid.into());
                let _ = launch(&mut st, Some(ssid.into()), true);
                changed = true;
            }
        }
        changed
    })
}

fn command(args: &[&str], cancel: &AtomicBool) -> Result<String, String> {
    if cancel.load(Ordering::Acquire) { return Err("연결 확인을 취소했어요".into()) }
    let program = std::env::var_os("KASATERM_LID_AWAY_NETWORKSETUP")
        .filter(|_| std::env::var_os("KASATERM_SETTINGS_FILE").is_some() && std::env::var_os("KASATERM_SESSION_FILE").is_some())
        .unwrap_or_else(|| "/usr/sbin/networksetup".into());
    let mut child = crate::proc::command(program).args(args)
        .env("LC_ALL", "C").env("LANG", "C")
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().map_err(|_| "Wi-Fi 설정 명령을 실행할 수 없어요".to_string())?;
    // Drain both pipes while waiting; saved networks can exceed a pipe buffer.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let read = |pipe: Option<_>| std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(pipe) = pipe { let _ = Read::take(pipe, 128 * 1024).read_to_end(&mut bytes); }
        bytes
    });
    let out = read(stdout);
    let err = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(pipe) = stderr { let _ = pipe.take(128 * 1024).read_to_end(&mut bytes); }
        bytes
    });
    let start = Instant::now();
    let result = loop {
        if cancel.load(Ordering::Acquire) || start.elapsed() >= Duration::from_secs(25) {
            let _ = child.kill();
            let _ = child.wait();
            break Err("핫스팟 연결 시간이 초과됐거나 취소됐어요".to_string());
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status.success()),
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => {
                let _ = child.kill(); let _ = child.wait();
                break Err("Wi-Fi 연결 결과를 읽지 못했어요".into());
            }
        }
    };
    let output = String::from_utf8_lossy(&out.join().unwrap_or_default()).into_owned();
    let error = err.join().unwrap_or_default();
    if !result? || !error.is_empty() { return Err("Wi-Fi 연결 명령이 실패했어요. Wi-Fi 설정에서 연결을 확인해 주세요".into()) }
    Ok(output)
}

fn wifi_device(output: &str) -> Option<&str> {
    output.split("\n\n").find_map(|block| {
        if !block.lines().any(|line| matches!(line.trim(), "Hardware Port: Wi-Fi" | "Hardware Port: AirPort")) { return None }
        block.lines().find_map(|line| line.strip_prefix("Device: ")).filter(|s| s.starts_with("en") && s[2..].chars().all(|c| c.is_ascii_digit()))
    })
}

fn device(cancel: &AtomicBool) -> Result<String, String> {
    let output = command(&["-listallhardwareports"], cancel)?;
    wifi_device(&output).map(str::to_owned).ok_or_else(|| "Wi-Fi 기기를 찾을 수 없어요".into())
}

fn parse_saved(output: &str) -> Result<Vec<String>, String> {
    if !output.starts_with("Preferred networks on ") { return Err("저장된 Wi-Fi 목록을 읽지 못했어요".into()) }
    Ok(output.lines().skip(1).filter_map(|line| line.strip_prefix('\t')).filter(|ssid| valid_ssid(ssid)).map(str::to_owned).collect())
}

fn saved_networks(cancel: &AtomicBool) -> Result<Vec<String>, String> {
    let device = device(cancel)?;
    parse_saved(&command(&["-listpreferredwirelessnetworks", &device], cancel)?)
}

fn join_accepted(output: &str) -> bool {
    // networksetup prints failures with exit status zero on macOS.
    output.trim().is_empty()
}

fn associated(output: &str, ssid: &str) -> bool {
    output.trim_end_matches(['\r', '\n']).strip_prefix("Current Wi-Fi Network: ") == Some(ssid)
        || output.trim_end_matches(['\r', '\n']).strip_prefix("Current AirPort Network: ") == Some(ssid)
}

fn connect(ssid: &str, cancel: &AtomicBool) -> Result<String, String> {
    let device = device(cancel)?;
    let current = command(&["-getairportnetwork", &device], cancel)?;
    if associated(&current, ssid) { return Ok(format!("{ssid} 핫스팟에 연결돼 있어요")) }
    let saved = parse_saved(&command(&["-listpreferredwirelessnetworks", &device], cancel)?)?;
    if !saved.iter().any(|name| name == ssid) {
        return Err("먼저 macOS Wi-Fi 메뉴에서 이 핫스팟에 한 번 연결해 주세요".into());
    }
    let output = command(&["-setairportnetwork", &device, ssid], cancel)?;
    if !join_accepted(&output) {
        return Err("핫스팟에 연결하지 못했어요. 폰의 개인용 핫스팟을 켜고 macOS Wi-Fi 메뉴에서 연결해 주세요".into());
    }
    let current = command(&["-getairportnetwork", &device], cancel)?;
    if associated(&current, ssid) {
        Ok(format!("{ssid} 핫스팟 연결을 확인했어요"))
    } else {
        Ok("연결 요청은 끝났지만 연결 확인은 안 됐어요. macOS가 Wi-Fi 이름을 숨길 수 있으니 Wi-Fi 메뉴에서 확인해 주세요".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_zero_text_is_not_connection_success() {
        assert!(!join_accepted("Could not find network phone.\n"));
        assert!(!join_accepted("Failed to join network phone.\n"));
        assert!(join_accepted("\n"));
        assert!(!associated("You are not associated with an AirPort network.\n", "phone"));
        assert!(!associated("Current Wi-Fi Network: another\n", "phone"));
        assert!(associated("Current Wi-Fi Network: phone\n", "phone"));
    }

    #[test]
    fn opening_cancels_the_worker_and_drops_late_success() {
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        STATE.with(|cell| *cell.borrow_mut() = State {
            job: Some(Job { receiver, cancel: cancel.clone(), automatic: true }),
            away_ssid: Some("phone".into()), ..Default::default()
        });
        tick(None);
        assert!(cancel.load(Ordering::Acquire));
        sender.send(ResultData::Connection(Ok("connected".into()))).unwrap();
        tick(None);
        STATE.with(|cell| {
            let mut st = cell.borrow_mut();
            assert!(st.view.status.is_empty());
            assert!(!st.view.busy);
            assert!(st.job.is_none());
            *st = State::default();
        });
    }

    #[test]
    fn an_unverified_join_is_not_repeated_while_closed() {
        STATE.with(|cell| *cell.borrow_mut() = State { away_ssid: Some("phone".into()), ..Default::default() });
        assert!(!tick(Some("phone")));
        assert!(!pending());
        STATE.with(|cell| *cell.borrow_mut() = State::default());
    }

    #[test]
    fn names_preserve_spaces_and_never_become_shell_code() {
        assert!(valid_ssid("phone / $(name)"));
        assert!(!valid_ssid("phone\nelse"));
        assert!(!valid_ssid(&"가".repeat(11)));
        assert_eq!(parse_saved("Preferred networks on en0:\n\t phone \n\t다른 폰\n").unwrap(), [" phone ", "다른 폰"]);
        assert!(parse_saved("An error occurred").is_err());
        assert_eq!(wifi_device("Hardware Port: Ethernet\nDevice: en3\n\nHardware Port: Wi-Fi\nDevice: en1\nEthernet Address: hidden\n"), Some("en1"));
    }
}
