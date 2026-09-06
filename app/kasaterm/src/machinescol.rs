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
fn open_screen_share(host: &str) {
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
fn parse_hex(s: &str) -> Option<[u8; 4]> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8, 255])
}

/// 상태점 색 — 앱 상태 언어 그대로: 기다림=attention, 도는 중=accent, 그 외=success.
fn status_color(status: &str) -> [u8; 4] {
    match status {
        "waiting" | "blocked" => theme::attention(),
        "working" | "thinking" | "building" => theme::accent(),
        _ => theme::success(),
    }
}

impl App {
    /// 이사 칼럼 데이터를 다시 조립한다. 탭이 보일 때만, 1초 스로틀 —
    /// 기계 쪽은 폴링 캐시(`machines::snapshot`)라 읽기 자체는 공짜다.
    pub(crate) fn refresh_machines_col(&mut self) {
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
            if !self.pane_claude_ready(id) {
                continue;
            }
            let Some(name) = self.pane_character_if_known(id) else {
                continue;
            };
            let win = pane_window.get(id).copied().unwrap_or(self.active_window);
            let row = state::MachinesColRow {
                pane: id.clone(),
                remote_id: String::new(),
                remote_cwd: String::new(),
                color: theme::character_accent_any(&name),
                name,
                title: self.pane_row_label(id),
                status: self
                    .pane_activity
                    .get(id)
                    .map(|v| v.status.clone())
                    .unwrap_or_default(),
                room: room_of(win),
            };
            match kasa_mcp::remote::remote_info(id) {
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
                let remote = m
                    .get("panes")
                    .and_then(|p| p.as_array())
                    .map(|arr| {
                        // (원격 방 인덱스, 행) — 로컬과 같은 이유로 방 순서로 이어 앉힌다.
                        let mut rows: Vec<(u64, state::MachinesColRow)> = arr
                            .iter()
                            .filter_map(|p| {
                                let rid = p.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                if mirror_ids.contains(rid) {
                                    return None; // 이사 간 학생의 원격 반쪽 — 미러 행이 대표한다.
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
                                let room = cwd
                                    .rsplit('/')
                                    .next()
                                    .filter(|t| !t.is_empty())
                                    .map(str::to_string)
                                    .or_else(|| win.map(|w| format!("방 {}", w + 1)))
                                    .unwrap_or_default();
                                let color = p
                                    .get("color")
                                    .and_then(|v| v.as_str())
                                    .and_then(parse_hex)
                                    .or_else(|| theme::character_accent_any(name));
                                Some((
                                    win.unwrap_or(u64::MAX),
                                    state::MachinesColRow {
                                        pane: String::new(), // 로컬 자리가 없다 — 데려오기 대상이 못 된다.
                                        // GUI pane(`%…`)만 거울 대상이다 — 헤드리스 웹 셸은
                                        // 그 기계 화면의 방이 아니다.
                                        remote_id: rid.starts_with('%').then(|| rid.to_string()).unwrap_or_default(),
                                        remote_cwd: cwd,
                                        name: if name.is_empty() {
                                            "이름 없는 캐릭터".to_string()
                                        } else {
                                            name.to_string()
                                        },
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
                                        color,
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

    /// 메뉴 항목 하나를 실행한다. 학생 줄 우클릭 메뉴(보내기·데려오기)도 여기로
    /// 온다 — 이사의 busy·note·토스트 규칙이 한 곳에 있어야 두 메뉴가 같이 논다.
    pub(crate) fn machines_col_act(&mut self, btn: state::MachinesColBtn) -> bool {
        if let state::MachinesColBtn::Screen { host, kvm } = &btn {
            // KVM 주소가 있으면 그쪽이 우선이다(거노 지시 2026-09-01) — IP KVM 은
            // OS 밖 물리 콘솔이라 로그인 전·부팅 화면·OS 죽음까지 보인다. 기본
            // 브라우저로 열고 채널 선택은 PiKVM 웹 UI 에 맡긴다.
            if let Some(url) = kvm {
                self.set_toast("KVM 화면 여는 중".to_string());
                let _ = crate::proc::command("open").arg(url).spawn();
                return true;
            }
            // 화면공유는 OS 에 맡긴다 — macOS 화면공유 앱이 vnc:// 를 연다.
            // 이사와 달리 즉시 끝나는 조작이라 busy 대열에 안 세운다. 다만 굳은
            // 앱을 먼저 걷어내야 해서(open_screen_share) fork 가 몇 번 돌고,
            // 기다릴 결과가 없으니 스레드로 뺀다 — 이 자리에서 기다리면 그
            // 프레임이 통째로 멈춘다.
            self.set_toast(format!("화면공유 여는 중 — {host}"));
            let host = host.clone();
            std::thread::spawn(move || open_screen_share(&host));
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
            | state::MachinesColBtn::Unfold { .. }
            | state::MachinesColBtn::Mirror { .. } => {
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
            | state::MachinesColBtn::Unfold { .. }
            | state::MachinesColBtn::Mirror { .. } => {
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
