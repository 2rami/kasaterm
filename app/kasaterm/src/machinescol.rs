//! 이사 — Info 탭 「다른 기계」 절의 재료와 동작.
//!
//! 처음엔 제 탭(「원격」)이었다. Info 에 이미 기계별 요약 줄이 있고 그 줄을 누르면
//! 이 탭으로 넘어오는 구조라 같은 것을 두 자리에서 말했다. 2026-09-07 지시로 탭을
//! 걷었다 — 「목록이 두 개니까」 본문을 절 안에 펼치지 않고, 요약 줄은 그대로 두고
//! 학생 목록·거울·방 펼치기·화면 보기는 그 줄을 누르면 뜨는 메뉴에, 보내기·데려오기는
//! 학생 줄 우클릭 메뉴에 넣었다(그리기는 info.rs). 여기엔 데이터 조립과 동작만 남는다.
//!
//! 처음엔 Persona 식 자식 웹뷰(`/arona-ui/machines.html`)였는데 같은 날 네이티브로
//! 뒤집었다(2026-08-29 지적 「타우리 말고 wgpu로 해야되지않나」). 우측 패널의 다른
//! 탭이 전부 셀 렌더인데 이 탭만 OS 뷰라 테마·z순서·캡처가 다 갈렸고, 무엇보다
//! 데이터(기계 캐시·pane 상태)가 **같은 프로세스**에 있어 HTTP 왕복이 낭비였다.
//!
//! 데이터 조립(`refresh_machines_col`)은 페인트 루프 밖(handler 프레임 끝)에서 한다
//! — 페인트는 gpu 를 빌린 상태라 `&self` 메서드를 못 부른다(사이드바 스냅샷과 같은
//! 이유). 메뉴 항목이 눌리면 `machines_col_click` → `machines_col_act` 가 움직인다.

use super::*;

/// Room identity comes from the source device, including for an existing mirror.
fn remote_room(p: &serde_json::Value) -> String {
    let window = p.get("window").and_then(|v| v.as_u64());
    let label = p.get("window_name").and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| p.get("cwd").and_then(|v| v.as_str())
            .and_then(|s| s.rsplit('/').find(|s| !s.is_empty())));
    match (window, label) {
        (Some(w), Some(label)) => format!("방 {} · {label}", w + 1),
        (Some(w), None) => format!("방 {}", w + 1),
        (None, Some(label)) => label.to_string(),
        _ => String::new(),
    }
}

/// ago 초를 사람 말로 — 아로나 판(MachinesTab)의 agoLabel 과 같은 문구.
pub(crate) fn ago_label(secs: Option<u64>) -> String {
    match secs {
        None => "한 번도 못 닿았어요".to_string(),
        Some(s) if s < 60 => format!("{s}초 전까지 닿았어요"),
        Some(s) if s < 3600 => format!("{}분 전까지 닿았어요", s / 60),
        Some(s) => format!("{}시간 전까지 닿았어요", s / 3600),
    }
}

/// 화면공유를 연다 — 응답 못 하는 앱을 먼저 걷어내고.
///
/// `open vnc://` 하나로는 모자란다. 화면공유 앱이 이미 떠 있으면 macOS 는 「그 앱
/// 실행 중」으로 보고 이벤트만 넘기는데, 런루프가 멈춘 앱은 그걸 처리하지 못해
/// **아무 일도 일어나지 않는다**. 2026-08-30 에 이틀 묵은 창 하나 때문에 미니가
/// 멀쩡한데도 「연결이 안 된다」였다 — 주소·서비스·로그인 세션 전부 정상이었다.
///
/// 소켓으로는 못 가른다. 멈춘 앱도 **옛 연결을 그대로 들고 있어** `lsof` 로는
/// 붙어 있는 것처럼 보인다(실측). 반대로 연결이 하나도 없는 앱은 `open` 이 알아서
/// 새로 붙이므로 손댈 필요가 없다(실측). 그래서 프로세스 상태만 본다.
///
/// fork 를 여러 번 하므로 호출자는 GUI 스레드 밖에서 부른다.
fn open_screen_share(host: &str, anchor: Option<(f64, f64, f64, f64)>) {
    clear_ghost_vnc(host);
    let hung = hung_screen_share_pids();
    for pid in &hung {
        let _ = crate::proc::command("kill").arg(pid).status();
    }
    if !hung.is_empty() {
        std::thread::sleep(std::time::Duration::from_millis(400));
        // 멈춘 프로세스는 TERM 을 받고도 서 있을 수 있다 — 남아 있으면 KILL 로 올린다.
        for pid in &hung {
            if pid_alive(pid) {
                let _ = crate::proc::command("kill").args(["-9", pid]).status();
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let _ = crate::proc::command("open")
        .arg(format!("vnc://{host}"))
        .spawn();
    pull_screen_share_into(anchor);
}

/// 저쪽이 아직 살아 있다고 믿는 **나와의** 5900 연결 중 이쪽에 짝이 없는 것 — 유령.
/// 하나라도 있으면 저쪽 화면공유 서비스를 되살린다.
///
/// 노트북이 잠들거나 자리를 옮기면 「이 연결 끊는다」가 저쪽에 닿지 못한다. 그러면
/// 저쪽 screensharingd 는 죽은 연결을 살아 있다고 믿고 화면을 계속 밀어넣다 송신 큐가
/// 막히고(2026-09-09 실측 2.4MB), **그 뒤로 새 연결이 「연결 중…」에서 영영 선다**.
/// 저쪽 keepalive 는 꺼져 있어(`always_keepalive=0`) 스스로 못 알아채고, 재전송이
/// 포기하기까지 10분 남짓 걸린다 — 그 사이 화면 보기는 눌러도 헛돈다.
///
/// 끊는 것은 root 소켓이라 서비스를 되살리는 수밖에 없다. 그래서 저쪽 sudoers 에
/// **그 명령 하나만** 암호 없이 받는 줄을 둔다(`/etc/sudoers.d/kasaterm-screenshare`).
/// 줄이 없거나 ssh 가 안 되면 조용히 접는다 — 열기 자체는 그대로 시도한다.
fn clear_ghost_vnc(host: &str) {
    let Some(target) = ssh_target_for_host(host) else { return };
    // `SSH_CLIENT` 의 첫 칸이 **저쪽이 보는 내 주소**다. 터널을 거치면 루프백이 와서
    // 내 연결을 못 가려내므로 그때는 판정을 접는다 — 남의 세션을 끊지 않기 위해서다.
    let probe = "ip=${SSH_CLIENT%% *}; case \"$ip\" in 127.*|::1|\"\") exit 0;; esac; \
                 netstat -an | awk -v me=\"$ip.\" '$4 ~ /\\.5900$/ && $6 == \"ESTABLISHED\" && index($5, me) == 1 { print $5 }'";
    let Some(out) = ssh_capture(&target, probe) else { return };
    let remote: Vec<&str> = out
        .lines()
        .filter_map(|l| l.trim().rsplit('.').next())
        .filter(|p| !p.is_empty())
        .collect();
    if remote.is_empty() {
        return;
    }
    let local = local_vnc_ports();
    if !remote.iter().any(|p| !local.iter().any(|l| l == p)) {
        return;
    }
    let _ = ssh_capture(
        &target,
        "sudo -n /bin/launchctl kickstart -k system/com.apple.screensharing",
    );
    // 되살아난 서비스가 포트를 다시 물 때까지 잠깐 — 바로 열면 그 틈에 걸린다.
    std::thread::sleep(std::time::Duration::from_millis(1200));
}

/// 이 기계가 지금 들고 있는 5900 연결의 **내 쪽 포트**들.
fn local_vnc_ports() -> Vec<String> {
    let Ok(out) = crate::proc::command("lsof")
        .args(["-nP", "-iTCP:5900", "-sTCP:ESTABLISHED"])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().find(|f| f.contains("->")))
        .filter_map(|f| f.split("->").next())
        .filter_map(|near| near.rsplit(':').next().map(str::to_string))
        .collect()
}

/// 명부에서 이 host 의 ssh 대상. 명부에 ssh 가 없으면 host 자체를 쓴다 —
/// `user@ip` 꼴이면 그대로 붙는다.
fn ssh_target_for_host(host: &str) -> Option<String> {
    let listed = kasa_mcp::machines::snapshot().into_iter().find_map(|m| {
        (m.get("host").and_then(|v| v.as_str()) == Some(host))
            .then(|| m.get("ssh").and_then(|v| v.as_str()).map(str::to_string))
            .flatten()
    });
    listed.or_else(|| host.contains('@').then(|| host.to_string()))
}

fn ssh_capture(target: &str, script: &str) -> Option<String> {
    let out = crate::proc::command("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=5",
            target,
            script,
        ])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 화면공유 창을 **카사텀이 떠 있는 화면**으로 끌어온다.
///
/// macOS 화면공유는 창 자리를 자기가 저장해 복원한다 — 바깥 모니터에서 한 번 쓰면
/// 그 뒤로 늘 거기서 열린다. 그 모니터를 안 보고 있으면 창은 멀쩡히 열렸는데도
/// 「눌러도 아무 일이 없다」로 보인다(2026-09-09: 저장된 자리가 바깥 5K 였고, 그것이
/// 「화면공유가 안 된다」의 절반이었다). anchor 밖에 뜬 창만 옮긴다 — 이미 보이는
/// 자리에 있으면 건드리지 않는다.
///
/// 창은 「연결 중」 작은 창으로 떴다가 화면이 들어오면 커지는데, 그때 자리가 다시
/// 저장값으로 튄다. 그래서 한 번 보고 마는 대신 잠시 되묻는다.
fn pull_screen_share_into(anchor: Option<(f64, f64, f64, f64)>) {
    let Some((ax, ay, aw, ah)) = anchor else { return };
    let script = format!(
        r#"set ax to {ax:.0}
set ay to {ay:.0}
set aw to {aw:.0}
set ah to {ah:.0}
repeat 30 times
  try
    tell application "System Events" to tell process "Screen Sharing"
      repeat with w in windows
        set nm to name of w
        if nm is not "모든 연결" and nm is not "All Connections" then
          set {{wx, wy}} to position of w
          if wx < ax or wy < ay or wx > (ax + aw) or wy > (ay + ah) then
            set position of w to {{ax + 60, ay + 60}}
          end if
        end if
      end repeat
    end tell
  end try
  delay 0.5
end repeat"#
    );
    let _ = crate::proc::command("osascript")
        .args(["-e", &script])
        .output();
}

fn pid_alive(pid: &str) -> bool {
    crate::proc::command("kill")
        .args(["-0", pid])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 응답 못 하는 화면공유 앱의 PID 들. 멀쩡하면 빈 벡터 — 걷어내는 건 창을 잃는
/// 조작이라 확실할 때만 한다.
///
/// `T`(멈춤)·`U`(인터럽트 불가 대기)만 잡는다. 이 둘은 런루프가 돌 수 없는 상태다.
/// 그 밖의 형태로 굳은 앱은 `ps` 에 `S` 로 보여 여기서 못 거른다 — 그건 사람이 앱을
/// 끄는 수밖에 없다.
fn hung_screen_share_pids() -> Vec<String> {
    let Ok(out) = crate::proc::command("ps")
        .args(["-Ao", "pid=,stat=,command="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains("Screen Sharing.app"))
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?;
            let stat = it.next()?;
            let hung = stat.starts_with('T') || stat.starts_with('U');
            hung.then(|| pid.to_string())
        })
        .collect()
}

/// `#RRGGBB` → RGBA. 원격 창구가 주는 학생색(header_color)과 같은 표기만 받는다.
/// 원격 거울 pane 의 **저쪽 사실** — (기계 라벨, 폴링 캐시의 그 pane 행). 이름·제목·
/// 상태가 로컬엔 없어(프로세스가 저쪽에서 돈다) 여기서 온다. 링크가 없거나 캐시에
/// 그 pane 이 없으면 None.
pub(crate) fn remote_pane_facts(id: &str) -> Option<(String, serde_json::Value)> {
    let info = kasa_mcp::remote::remote_info(id)?;
    let label = if info.label.is_empty() {
        kasa_mcp::machines::label_for_base(&info.base).unwrap_or_else(|| info.base.clone())
    } else { info.label.clone() };
    let snap = kasa_mcp::machines::snapshot();
    let m = snap
        .iter()
        .find(|v| v.get("label").and_then(|l| l.as_str()) == Some(label.as_str()))?;
    let row = m
        .get("panes")?
        .as_array()?
        .iter()
        .find(|p| p.get("id").and_then(|v| v.as_str()) == Some(info.remote_id.as_str()))?
        .clone();
    Some((label, row))
}

impl App {
    /// 이사 칼럼 데이터를 다시 조립한다. 탭이 보일 때만, 1초 스로틀 —
    /// 기계 쪽은 폴링 캐시(`machines::snapshot`)라 읽기 자체는 공짜다.
    pub(crate) fn refresh_machines_col(&mut self) {
        self.poll_mirror_sync();
        if !self.machines_section_active() {
            return;
        }
        if self
            .info
            .machines_col
            .last_refresh
            .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(1))
        {
            return;
        }
        self.info.machines_col.last_refresh = Some(std::time::Instant::now());

        // 로컬 pane 들 — claude 가 붙어 학생이 앉은 자리만(사이드바와 같은 기준).
        let mut pane_ids: Vec<String> = self.pty.keys().cloned().collect();
        pane_ids.sort_by_key(|s| s.trim_start_matches('%').parse::<u64>().unwrap_or(u64::MAX));
        // 방(윈도우) 매핑 스냅샷 — 같은 방 학생을 이어 앉히고 머리줄을 그리는 재료.
        let pane_window: std::collections::HashMap<String, usize> =
            self.ws.lock().unwrap().pane_window.clone();
        let room_of = |w: usize| -> String {
            self.window_name_override
                .get(&w)
                .cloned()
                .or_else(|| {
                    // `window_labels.0`(OSC 타이틀)은 안 쓴다 — 사이드바와 같은 규칙:
                    // 셸만 떠 있으면 방마다 똑같이 `zsh` 가 된다. 손수 붙인 이름,
                    // 없으면 작업 폴더 꼬리가 방을 실제로 가른다.
                    let (_, cwd) = self.window_labels.get(w)?;
                    let tail = cwd.rsplit('/').next().unwrap_or(cwd);
                    (!tail.is_empty()).then(|| tail.to_string())
                })
                .unwrap_or_else(|| format!("방 {}", w + 1))
        };
        // (방 인덱스, 행) 으로 모아 방 순서로 이어 앉힌다 — pane 번호 순서만으로는
        // 방이 섞여, 머리줄 하나 아래 남의 방 학생이 선다.
        let mut locals: Vec<(usize, state::MachinesColRow)> = Vec::new();
        // 라벨 → (원격 surface id 집합, 이사 간 학생 행들). 원격 목록에서 미러와
        // 같은 pane 을 두 번 세우지 않기 위한 대조표다.
        let mut mirrored: std::collections::HashMap<
            String,
            (
                std::collections::HashSet<String>,
                Vec<state::MachinesColRow>,
            ),
        > = std::collections::HashMap::new();
        for id in &pane_ids {
            // 원격 거울은 claude 가 저쪽에서 돌아 로컬 관문(pane_claude_ready·에이전트
            // 감지)에 걸린다 — 링크가 있으면 그 자체로 학생 자리다. 이름·상태는 폴링
            // 캐시의 저쪽 행에서(2026-09-07 지적 「맥미니에서 여기로 옮기는 것도 없어」
            // — 이 관문 탓에 거울이 데려오기 목록에 안 섰다).
            let facts = remote_pane_facts(id);
            let remote = kasa_mcp::remote::remote_info(id);
            if remote.is_none() && !self.pane_claude_ready(id) {
                continue;
            }
            let remote_str = |k: &str| {
                facts
                    .as_ref()
                    .and_then(|(_, r)| r.get(k).and_then(|v| v.as_str()))
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            let Some(name) = remote_str("name").or_else(|| self.pane_character_if_known(id))
                .or_else(|| remote.as_ref().map(|_| String::new())) else {
                continue;
            };
            let win = pane_window.get(id).copied().unwrap_or(self.active_window);
            let row = state::MachinesColRow {
                pane: id.clone(),
                remote_id: remote.as_ref().map(|i| i.remote_id.clone()).unwrap_or_default(),
                remote_cwd: remote_str("cwd").unwrap_or_default(),
                name,
                // 거울이면 저쪽이 하던 일 제목 — 로컬 라벨은 이쪽 폴더라 「무엇을 하나」를
                // 못 말한다(Info 「다른 기계」 pane 목록이 이 값을 그대로 쓴다).
                title: remote_str("title").unwrap_or_else(|| self.pane_row_label(id)),
                status: remote_str("status").or_else(|| self
                    .pane_activity
                    .get(id)
                    .map(|v| v.status.clone()))
                    .unwrap_or_default(),
                room: facts.as_ref().map(|(_, p)| remote_room(p)).unwrap_or_else(|| room_of(win)),
                closed: facts
                    .as_ref()
                    .and_then(|(_, r)| r.get("closed").and_then(|v| v.as_bool()))
                    .unwrap_or(false),
            };
            match remote {
                Some(info) => {
                    let label = if info.label.is_empty() {
                        info.base.clone()
                    } else {
                        info.label.clone()
                    };
                    let slot = mirrored.entry(label).or_default();
                    slot.0.insert(info.remote_id.clone());
                    slot.1.push(row);
                }
                None => locals.push((win, row)),
            }
        }
        locals.sort_by_key(|(w, _)| *w);
        let locals: Vec<state::MachinesColRow> = locals.into_iter().map(|(_, r)| r).collect();

        // 기계 섹션 — 폴링 캐시 스냅샷을 그대로 편다. host(화면공유 주소)는
        // 캐시에 없어 명부에서 라벨로 찾는다 — 한 번만 읽어 루프에 물린다.
        let registry = kasa_mcp::machines::machines();
        let machines = kasa_mcp::machines::snapshot()
            .into_iter()
            .filter_map(|m| {
                let label = m.get("label")?.as_str()?.to_string();
                let reg = registry.iter().find(|r| r.label == label);
                let host = reg.map(|r| r.host.clone()).unwrap_or_default();
                let kvm = reg.and_then(|r| r.kvm.clone());
                let (mirror_ids, mirror_rows) = mirrored.remove(&label).unwrap_or_default();
                let panes = m.get("panes").and_then(|p| p.as_array());
                // 그 기계에서 닫힌 pane(되살리기 대열) — 화면에 없는 학생을 목록에 세우면
                // 「하나도 없는데 왜 뜨나」가 된다(2026-09-07 지적). 개수만 남긴다.
                let is_closed = |p: &serde_json::Value| {
                    p.get("closed").and_then(|v| v.as_bool()).unwrap_or(false)
                };
                let closed = panes
                    .map(|arr| arr.iter().filter(|p| is_closed(p)).count())
                    .unwrap_or(0);
                let remote = panes
                    .map(|arr| {
                        // (원격 방 인덱스, 행) — 로컬과 같은 이유로 방 순서로 이어 앉힌다.
                        let mut rows: Vec<(u64, state::MachinesColRow)> = arr
                            .iter()
                            .filter_map(|p| {
                                if p.get("mirror_of").and_then(|v| v.as_str()).is_some() {
                                    return None; // A viewer is listed under its source device.
                                }
                                let rid = p.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                if mirror_ids.contains(rid) {
                                    return None; // 이사 간 학생의 원격 반쪽 — 미러 행이 대표한다.
                                }
                                if is_closed(p) {
                                    return None;
                                }
                                // 헤드리스 웹 셸(`web-…`)은 그 기계 화면의 방이 아니다 —
                                // 「이름 없는 캐릭터」로 서서 누를 것도 없던 줄.
                                if !rid.starts_with('%') {
                                    return None;
                                }
                                let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
                                let win = p.get("window").and_then(|v| v.as_u64());
                                let cwd = p
                                    .get("cwd")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                // 방 이름은 폴더 꼬리(사이드바 규칙과 같은 원천) — cwd 를
                                // 안 주는 옛 창구에서는 방 번호로 물러선다.
                                let room = remote_room(p);
                                Some((
                                    win.unwrap_or(u64::MAX),
                                    state::MachinesColRow {
                                        pane: String::new(), // 로컬 자리가 없다 — 데려오기 대상이 못 된다.
                                        // GUI pane(`%…`)만 거울 대상이다 — 헤드리스 웹 셸은
                                        // 그 기계 화면의 방이 아니다.
                                        remote_id: rid.starts_with('%').then(|| rid.to_string()).unwrap_or_default(),
                                        remote_cwd: cwd,
                                        name: name.to_string(),
                                        title: p
                                            .get("title")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string(),
                                        status: p
                                            .get("status")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string(),
                                        room,
                                        closed: false,
                                    },
                                ))
                            })
                            .collect();
                        rows.sort_by_key(|(w, _)| *w);
                        rows.into_iter().map(|(_, r)| r).collect()
                    })
                    .unwrap_or_default();
                Some(state::MachinesColMachine {
                    label,
                    online: m.get("online").and_then(|v| v.as_bool()).unwrap_or(false),
                    ago_secs: m.get("ago_secs").and_then(|v| v.as_u64()),
                    outdated: m.get("sync_capable").and_then(|v| v.as_bool()) == Some(false),
                    host,
                    kvm,
                    mirrored: mirror_rows,
                    remote,
                    closed,
                })
            })
            .collect();
        self.info.machines_col.locals = locals;
        self.info.machines_col.machines = machines;
    }

    /// 데려오기(migrate_pane_back)의 진행 한 줄 — 그쪽은 아직 GUI 스레드 동기라
    /// 이걸 안 부르면 끝날 때까지 화면이 얼어붙은 채 무소식이다. 보내기는 워커로
    /// 넘어가 `migrate_stage` 가 대신한다(2026-09-07).
    #[cfg(unix)]
    pub(crate) fn migrate_progress(&mut self, pane: &str, msg: String) {
        self.set_toast(format!("이사 — {msg}"));
        self.info.machines_col.busy = Some((pane.to_string(), msg));
        self.chrome_dirty = true;
        self.render_frame();
    }

    /// 이사 칼럼 클릭 — 버튼에 맞으면 이사를 실행하고 true.
    ///
    /// 이사는 이 자리(GUI 스레드)에서 동기로 돈다 — CLI·아로나 경로도 결국 GUI
    /// 이벤트로 위임돼 같은 스레드에서 돌므로, 어는 정도는 기존과 같다. 대신
    /// 시작 전에 「이사 중」 상태를 한 프레임 그려 둔다.
    pub(crate) fn machines_col_click(&mut self, cx: f32, cy: f32) -> bool {
        let hit = self
            .info
            .machines_col
            .btn_rects
            .iter()
            .find(|(_, (x, y, w, h))| cx >= *x && cx <= *x + *w && cy >= *y && cy <= *y + *h)
            .map(|(b, _)| b.clone());
        let Some(btn) = hit else { return false };
        self.machines_col_act(btn)
    }

    /// 카사텀 창이 지금 떠 있는 화면의 논리 사각형 `(x, y, w, h)`. 다른 앱 창을
    /// 「보이는 자리」로 옮길 때 쓴다 — 사람이 보고 있는 화면은 이 창이 있는 화면이다.
    ///
    /// 논리 좌표로 돌려준다(Accessibility 가 그 단위로 말한다). 화면마다 배율이 다르면
    /// 원점이 조금 어긋날 수 있지만, 「어느 화면인가」를 가리는 데는 넉넉하다.
    pub(crate) fn visible_monitor_rect(&self) -> Option<(f64, f64, f64, f64)> {
        let mon = self.window.as_ref()?.current_monitor()?;
        let sf = mon.scale_factor();
        let pos = mon.position().to_logical::<f64>(sf);
        let size = mon.size().to_logical::<f64>(sf);
        Some((pos.x, pos.y, size.width, size.height))
    }

    /// 메뉴 항목 하나를 실행한다. 학생 줄 우클릭 메뉴(보내기·데려오기)도 여기로
    /// 온다 — 이사의 busy·note·토스트 규칙이 한 곳에 있어야 두 메뉴가 같이 논다.
    pub(crate) fn machines_col_act(&mut self, btn: state::MachinesColBtn) -> bool {
        if let state::MachinesColBtn::Screen { host, kvm } = &btn {
            // 「화면 보기」는 화면공유다(2026-09-07 지시 — 2026-09-01 의 KVM 우선을
            // 뒤집었다: 평소 보는 것은 그 기계의 OS 화면이라서). KVM 은 host 가 없을
            // 때, 또는 메뉴의 「KVM 보기」(host 빈값으로 온다)로만 연다 — OS 밖 물리
            // 콘솔이라 로그인 전·부팅 화면·OS 죽음까지 보이는 문은 따로 둔다.
            if host.is_empty() {
                if let Some(url) = kvm {
                    self.set_toast("KVM 화면 여는 중".to_string());
                    let _ = crate::proc::command("open").arg(url).spawn();
                }
                return true;
            }
            // 화면공유는 OS 에 맡긴다 — macOS 화면공유 앱이 vnc:// 를 연다.
            // 이사와 달리 즉시 끝나는 조작이라 busy 대열에 안 세운다. 다만 굳은
            // 앱을 먼저 걷어내야 해서(open_screen_share) fork 가 몇 번 돌고,
            // 기다릴 결과가 없으니 스레드로 뺀다 — 이 자리에서 기다리면 그
            // 프레임이 통째로 멈춘다.
            self.set_toast(format!("화면공유 여는 중 — {host}"));
            let host = host.clone();
            let anchor = self.visible_monitor_rect();
            std::thread::spawn(move || open_screen_share(&host, anchor));
            return true;
        }
        if let state::MachinesColBtn::Close { label, remote_id, name, pane } = &btn {
            if !pane.is_empty() && kasa_mcp::remote::is_view_pane(pane) {
                self.confirm_or_close_pane(pane);
                return true;
            }
            // 그 기계 pane 닫기 — 거기서 사람이 × 를 누른 것과 같다(되살리기 대열에
            // 남는다, kill 아님). 거울 행은 remote_id 를 안 실으니 링크에서 꺼낸다.
            let rid = if remote_id.is_empty() {
                kasa_mcp::remote::remote_info(pane).map(|i| i.remote_id).unwrap_or_default()
            } else {
                remote_id.clone()
            };
            let Some(m) = kasa_mcp::machines::find(label) else {
                self.set_toast(format!("기계 {label} 를 명부에서 못 찾았다"));
                return true;
            };
            if rid.is_empty() {
                self.set_toast(format!("{name} — 그 기계의 pane 번호를 몰라 못 닫는다"));
                return true;
            }
            // 이쪽 거울은 바로 걷는다 — 원격이 사라진 거울은 멈춘 화면이라 남길 이유가
            // 없고, 원격 요청은 왕복이 있어 스레드로(이 자리에서 기다리면 프레임이 선다).
            if !pane.is_empty() {
                self.close_pane(pane);
            }
            self.set_toast(format!("{label} 의 {name} 닫는 중"));
            let (base, rid_s, name_s) = (m.base.clone(), rid, name.clone());
            std::thread::spawn(move || {
                if let Err(e) = kasa_mcp::remote::close_remote_pane(&base, &rid_s, None, false) {
                    eprintln!("[machines] {name_s}({rid_s}) 원격 닫기 실패: {e:#}");
                }
            });
            // 폴링 캐시가 따라잡기 전이라 목록엔 한 박자 남는다 — 다음 새로고침에 걷힌다.
            self.info.machines_col.last_refresh = None;
            self.chrome_dirty = true;
            return true;
        }
        if self.info.machines_col.busy.is_some() {
            return true; // 한 번에 하나 — 이사 중 클릭은 삼킨다.
        }
        if let state::MachinesColBtn::Unfold { label } = &btn {
            // 이사(pane 단위)와 달리 기계 단위라 busy(pane 키) 대열엔 안 태운다 —
            // GUI 스레드 동기 실행이라 도는 동안 다른 클릭이 끼어들 수도 없다.
            // 진행 토스트·최종 토스트는 엔진(unfold_machine)이 직접 띄운다.
            let label = label.clone();
            if let Err(e) = self.unfold_machine(&label) {
                self.set_toast(format!("펼치기 실패 — {e:#}"));
            }
            self.info.machines_col.last_refresh = None; // 새 거울들을 바로 읽게.
            self.chrome_dirty = true;
            self.render_frame();
            return true;
        }
        if let state::MachinesColBtn::Fetch {
            label,
            remote_id,
            name,
            cwd,
        } = &btn
        {
            // 저쪽 태생 학생 데려오기 = 거울 열기 + 그 자리에서 역이사. 역이사가 sid 를
            // 저쪽에 물어 오므로(5830da54) 로컬에 기억이 없어도 간다.
            let (label, rid, name, cwd) = (label.clone(), remote_id.clone(), name.clone(), cwd.clone());
            self.set_toast(format!("{name} 여기로 데려오는 중 — {label}"));
            self.render_frame();
            let outcome = self.mirror_remote_pane(&label, &rid, &name, &cwd).and_then(|id| {
                #[cfg(unix)]
                {
                    self.migrate_pane_back(&id, None, false)
                }
                #[cfg(not(unix))]
                {
                    let _ = id;
                    Err::<String, _>(anyhow::anyhow!("데려오기는 아직 Windows 에서 안 된다"))
                }
            });
            match outcome {
                Ok(msg) => self.set_toast(format!("{name} 데려옴 — {msg}")),
                Err(e) => self.set_toast(format!("데려오기 실패 — {e:#}")),
            }
            self.info.machines_col.last_refresh = None;
            self.chrome_dirty = true;
            self.render_frame();
            return true;
        }
        if let state::MachinesColBtn::Mirror {
            label,
            remote_id,
            name,
            cwd,
        } = &btn
        {
            let (label, rid, name, cwd) = (label.clone(), remote_id.clone(), name.clone(), cwd.clone());
            self.set_toast(format!("{name} 거울 여는 중 — {label}, 이 pane 의 탭으로"));
            self.render_frame();
            match self.mirror_remote_pane(&label, &rid, &name, &cwd) {
                Ok(_) => self.set_toast(format!("{name} 거울 — {label} 의 화면을 이 pane 의 탭으로 열었다")),
                Err(e) => self.set_toast(format!("거울 실패 — {e:#}")),
            }
            self.info.machines_col.last_refresh = None; // 새 거울을 바로 읽게.
            self.chrome_dirty = true;
            self.render_frame();
            return true;
        }
        let (pane, going) = match &btn {
            state::MachinesColBtn::Send { pane, label } => {
                (pane.clone(), format!("{label}(으)로 보내는 중…"))
            }
            state::MachinesColBtn::Bring { pane } => (pane.clone(), "데려오는 중…".to_string()),
            state::MachinesColBtn::Screen { .. }
            | state::MachinesColBtn::Close { .. }
            | state::MachinesColBtn::Unfold { .. }
            | state::MachinesColBtn::Mirror { .. }
            | state::MachinesColBtn::Fetch { .. } => {
                unreachable!("위에서 return")
            }
        };
        self.info.machines_col.busy = Some((pane.clone(), going.clone()));
        self.info.machines_col.note = None;
        self.set_toast(format!("이사 — {going}"));
        self.chrome_dirty = true;
        self.render_frame();
        // 이사의 본체는 unix 전용이다(session.rs 의 `migrate_pane`·`migrate_pane_back`
        // 이 `#[cfg(unix)]`) — 원격 셸 철거와 claude 를 곱게 끄는 신호가 그쪽 전제다.
        // 버튼을 숨기는 대신 눌렀을 때 이유를 말하기로 했다(2026-08-31 지시): 맥과
        // 화면이 같아 렌더 분기가 안 늘고, 구현되면 이 갈래만 걷어내면 된다.
        // Err 는 아래 성공/실패 갈림을 그대로 타 note 와 토스트로 사람에게 뜬다.
        #[cfg(not(unix))]
        let outcome: anyhow::Result<String> = Err(anyhow::anyhow!(
            "이사는 아직 Windows 에서 안 된다 — 원격 셸을 다루는 unix 전용 경로다"
        ));
        #[cfg(unix)]
        let outcome = match btn {
            state::MachinesColBtn::Screen { .. }
            | state::MachinesColBtn::Close { .. }
            | state::MachinesColBtn::Unfold { .. }
            | state::MachinesColBtn::Mirror { .. }
            | state::MachinesColBtn::Fetch { .. } => {
                unreachable!("위에서 return")
            }
            state::MachinesColBtn::Send { pane, label } => (|| -> anyhow::Result<String> {
                let m = kasa_mcp::machines::find(&label).ok_or_else(|| {
                    anyhow::anyhow!("기계 {label} 를 명부에서 못 찾았다 — machines.json 확인")
                })?;
                // cwd 는 명부 roots 로 매핑 — 규칙이 없으면 막고 이유를 말한다
                // (HTTP 판 pane_migrate_handler 와 같은 정책).
                let local = self
                    .pty
                    .get(&pane)
                    .and_then(|s| s.shell_pid())
                    .and_then(socket::pid_cwd);
                let remote_cwd = match &local {
                    Some(l) => Some(
                        kasa_mcp::machines::map_local_to_remote(&m, &l.to_string_lossy())
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "{} 를 {label} 경로로 못 옮겼다 — machines.json roots 에 규칙을",
                                    l.display()
                                )
                            })?,
                    ),
                    None => None,
                };
                self.migrate_pane(&pane, &m.base, remote_cwd.as_deref(), false, None)
            })(),
            state::MachinesColBtn::Bring { pane } => self.migrate_pane_back(&pane, None, false),
        };
        // 보내기는 워커로 넘어갔다 — busy·note 는 migrate_finish 가 끝에서 정리한다.
        // 예약·검사 실패·데려오기(아직 동기)만 여기서 마무리한다.
        if outcome.is_ok() && self.migrate_running(&pane) {
            if let Ok(msg) = outcome {
                self.set_toast(msg);
            }
            self.chrome_dirty = true;
            return true;
        }
        self.info.machines_col.busy = None;
        match outcome {
            Ok(msg) => {
                self.info.machines_col.note = Some((pane, true, msg.clone()));
                self.set_toast(msg);
            }
            Err(e) => {
                let why = format!("{e:#}");
                self.info.machines_col.note = Some((pane, false, why.clone()));
                self.set_toast(format!("이사 실패 — {why}"));
            }
        }
        self.info.machines_col.last_refresh = None; // 다음 틱에 바로 새 배치를 읽게.
        true
    }
}
