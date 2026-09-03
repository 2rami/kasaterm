//! 이사 칼럼 — 우측 패널 `SideTab::Machines` 본문.
//!
//! 처음엔 Persona 식 자식 웹뷰(`/arona-ui/machines.html`)였는데 같은 날 네이티브로
//! 뒤집었다(2026-08-29 지적 「타우리 말고 wgpu로 해야되지않나」). 우측 패널의 다른
//! 탭이 전부 셀 렌더인데 이 탭만 OS 뷰라 테마·z순서·캡처가 다 갈렸고, 무엇보다
//! 데이터(기계 캐시·pane 상태)가 **같은 프로세스**에 있어 HTTP 왕복이 낭비였다.
//!
//! 데이터 조립(`refresh_machines_col`)은 페인트 루프 밖(handler 프레임 끝)에서 한다
//! — 페인트는 gpu 를 빌린 상태라 `&self` 메서드를 못 부른다(사이드바 스냅샷과 같은
//! 이유). 그리기(`draw_machines_col`)는 sesscol/mcpcol 과 같은 자유함수 관례다.

use super::*;

/// ago 초를 사람 말로 — 아로나 판(MachinesTab)의 agoLabel 과 같은 문구.
fn ago_label(secs: Option<u64>) -> String {
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
        if !self.machines_tab_active() {
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
                                            "이름 없는 학생".to_string()
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

    /// 이사 진행 한 줄 — 단계마다 불러 화면을 그 자리에서 한 프레임 굴린다.
    /// 이사는 GUI 스레드 동기라 이걸 안 부르면 「눌렸다」 한 프레임 뒤 끝날 때까지
    /// 화면이 얼어붙은 채 무소식이다(2026-08-30 지시: 「진행도가 보이면 좋겠어」).
    ///
    /// 부르는 곳이 `session.rs` 의 이사 본체뿐이고 그쪽이 `#[cfg(unix)]` 이라
    /// 게이트를 맞춰 둔다 — 안 맞추면 Windows 빌드에서만 dead_code 경고가 뜬다.
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

/// `max` 폭에 들어가게 뒤를 …로 자른다. 렌더러에 폭 제한 그리기가 없어서
/// 문자열 쪽에서 자르는 것 — 행 텍스트가 버튼 밑으로 흐르지 않게.
fn truncate_to(g: &mut gpu::GpuRenderer, s: &str, size: f32, bold: bool, max: f32) -> String {
    if g.measure_chrome_text(s, size, bold) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    for ch in s.chars() {
        out.push(ch);
        if g.measure_chrome_text(&format!("{out}…"), size, bold) > max {
            out.pop();
            out.push('…');
            return out;
        }
    }
    out
}

/// 섹션 라벨 한 줄("이 맥북" / 기계 이름). y 를 소비한 높이만큼 돌려준다.
fn section_label(g: &mut gpu::GpuRenderer, x: f32, y: f32, text: &str) -> f32 {
    g.draw_text(
        x,
        y,
        text,
        gpu::DrawOpts {
            font_size: 10.0,
            color: theme::text_mute(),
            bold: true,
            italic: false,
        },
    );
    y + 20.0
}

/// 방 머리줄 — 같은 방 학생들 위에 한 번. `last` 와 같으면(연속) 안 그린다.
fn room_label(g: &mut gpu::GpuRenderer, x: f32, y: f32, room: &str, last: &mut String) -> f32 {
    if room.is_empty() || room == last {
        *last = room.to_string();
        return y;
    }
    *last = room.to_string();
    g.draw_text(
        x,
        y,
        room,
        gpu::DrawOpts {
            font_size: 9.5,
            color: theme::text_dim(),
            bold: false,
            italic: false,
        },
    );
    y + 15.0
}

/// 학생 한 행. 버튼 rect 는 `mc.btn_rects` 에 쌓는다.
#[allow(clippy::too_many_arguments)]
fn student_row(
    g: &mut gpu::GpuRenderer,
    cursor: (f32, f32),
    mc: &mut state::MachinesColState,
    row: &state::MachinesColRow,
    buttons: &[(state::MachinesColBtn, String, bool)], // (동작, 라벨, 눌림 가능)
    x: f32,
    right: f32,
    y: f32,
    // 좁은 칼럼 배치 — 버튼을 이름 오른쪽이 아니라 아랫줄에 앉힌다.
    narrow: bool,
    // 줄 자체가 눌리는 행 — 원격 학생은 버튼 없이 줄을 누르면 거울 탭이 된다
    // (2026-09-03 지시). 버튼 행과 달리 표적이 줄 전체라 손이 덜 간다.
    row_action: Option<state::MachinesColBtn>,
) -> f32 {
    let busy_here = mc
        .busy
        .as_ref()
        .is_some_and(|(p, _)| !row.pane.is_empty() && *p == row.pane);
    // 줄 높이는 아래 그리기가 밟는 줄 수로 미리 센다(이름·제목·아랫줄 버튼·결과 한 줄) —
    // 호버 바탕을 글 밑에 먼저 깔아야 해서다.
    let note_h = if mc
        .note
        .as_ref()
        .is_some_and(|(p, _, _)| !row.pane.is_empty() && p == &row.pane)
    {
        14.0
    } else {
        0.0
    };
    let row_h = 15.0 + 15.0 + if narrow { 19.0 } else { 0.0 } + note_h + 6.0;
    let row_rect = (x - 6.0, y - 3.0, (right - x + 6.0).max(0.0), row_h);
    let hint = "탭으로 →";
    let hint_w = if row_action.is_some() {
        g.measure_chrome_text(hint, 10.0, false) + 8.0
    } else {
        0.0
    };
    let row_hot = row_action.is_some()
        && cursor.0 >= row_rect.0
        && cursor.0 <= row_rect.0 + row_rect.2
        && cursor.1 >= row_rect.1
        && cursor.1 <= row_rect.1 + row_rect.3;
    if row_hot {
        g.hover_pointer = true;
        round_rect(
            g,
            row_rect.0,
            row_rect.1,
            row_rect.2,
            row_rect.3,
            theme::radius_sm(),
            theme::raised_on(theme::surface(), true),
        );
        g.draw_text(
            right - hint_w + 8.0,
            y + 2.0,
            hint,
            gpu::DrawOpts {
                font_size: 10.0,
                color: theme::text_mute(),
                bold: false,
                italic: false,
            },
        );
    }
    if let Some(a) = row_action {
        mc.btn_rects.push((a, row_rect));
    }
    // 상태점.
    let dot = 7.0;
    round_rect(
        g,
        x,
        y + 5.0,
        dot,
        dot,
        dot / 2.0,
        status_color(&row.status),
    );
    // 오른쪽 끝에서 버튼을 먼저 앉히고 남는 폭이 글 자리다. 좁으면 그 자리를
    // 아랫줄로 내린다 — 216px 칼럼에서 버튼을 같은 줄에 두면 이름에 100px 밖에
    // 안 남아, 학생 이름과 무슨 일을 하는지가 둘 다 말줄임이 된다.
    let btn_y = if narrow { y + 32.0 } else { y };
    let mut bx = right;
    if busy_here {
        let t = mc.busy.as_ref().map(|(_, g)| g.clone()).unwrap_or_default();
        let tw = g.measure_chrome_text(&t, 10.0, true);
        bx -= tw;
        g.draw_text(
            bx,
            btn_y + 2.0,
            &t,
            gpu::DrawOpts {
                font_size: 10.0,
                color: theme::accent(),
                bold: true,
                italic: false,
            },
        );
    } else {
        for (btn, label, enabled) in buttons.iter().rev() {
            let bw = g.measure_chrome_text(label, 10.0, true) + 12.0;
            let bh = 17.0;
            bx -= bw;
            let hov = *enabled
                && cursor.0 >= bx
                && cursor.0 <= bx + bw
                && cursor.1 >= btn_y
                && cursor.1 <= btn_y + bh;
            g.hover_pointer |= hov;
            let fill = theme::raised_on(theme::surface(), hov);
            round_rect(g, bx, btn_y, bw, bh, theme::radius_sm(), fill);
            g.draw_text(
                bx + 6.0,
                btn_y + 2.0,
                label,
                gpu::DrawOpts {
                    font_size: 10.0,
                    color: if *enabled {
                        theme::text()
                    } else {
                        theme::text_mute()
                    },
                    bold: true,
                    italic: false,
                },
            );
            if *enabled {
                mc.btn_rects.push((btn.clone(), (bx, btn_y, bw, bh)));
            }
            bx -= 4.0;
        }
    }
    let text_x = x + dot + 7.0;
    let text_max = if narrow {
        (right - text_x).max(0.0)
    } else {
        (bx - 8.0 - text_x - hint_w).max(0.0)
    };
    let name = truncate_to(g, &row.name, 12.0, true, text_max);
    g.draw_text(
        text_x,
        y,
        &name,
        gpu::DrawOpts {
            font_size: 12.0,
            // 학생색 — 사이드바가 이름을 캐릭터 테마색으로 칠하는 것과 같은 언어.
            color: row.color.unwrap_or_else(theme::text),
            bold: true,
            italic: false,
        },
    );
    let mut yy = y + 15.0;
    if !row.title.is_empty() {
        // 제목 줄은 버튼과 세로로 살짝 겹치는 자리라 같은 폭으로 자른다.
        let title = truncate_to(g, &row.title, 10.5, false, text_max);
        g.draw_text(
            text_x,
            yy,
            &title,
            gpu::DrawOpts {
                font_size: 10.5,
                color: theme::text_mute(),
                bold: false,
                italic: false,
            },
        );
    }
    yy += 15.0;
    // 아랫줄로 내린 버튼이 차지한 만큼 — 그 자리를 안 비우면 다음 학생이 그 위에
    // 겹쳐 앉는다.
    if narrow {
        yy += 19.0;
    }
    // 이 행의 최근 이사 결과 한 줄.
    if let Some((p, ok, note)) = &mc.note {
        if !row.pane.is_empty() && p == &row.pane {
            g.draw_text(
                text_x,
                yy,
                note,
                gpu::DrawOpts {
                    font_size: 10.0,
                    color: if *ok {
                        theme::success()
                    } else {
                        theme::danger()
                    },
                    bold: false,
                    italic: false,
                },
            );
            yy += 14.0;
        }
    }
    yy + 6.0
}

/// 이사 칼럼 본문. sesscol/mcpcol 과 같은 호출 관례(자유함수, 상태만 받아 그림).
pub(crate) fn draw_machines_col(
    g: &mut gpu::GpuRenderer,
    cursor: (f32, f32),
    mc: &mut state::MachinesColState,
    x: f32,
    w: f32,
    top: f32,
    bottom: f32,
) {
    let x0 = x + 14.0;
    let right = x + w - 12.0;
    // 칼럼이 좁으면 학생 행이 두 단(글 / 버튼)으로 선다.
    let narrow = w < GIT_DENSE_COMPACT;
    mc.btn_rects.clear();
    let vis_h = (bottom - top).max(0.0);
    g.push_clip(x, top, w, vis_h);
    let mut y = top + 8.0 - mc.scroll;

    if mc.machines.is_empty() {
        g.pop_clip();
        // 좁으면 짧게 끊는다 — 한 줄짜리 안내가 칼럼 밖으로 뻗으면 정작 파일
        // 이름이 잘려, 무엇을 적어야 하는지가 사라진다.
        let msg: &[&str] = if narrow {
            &[
                "등록된 기계가 없어요",
                "machines.json 에 적으면",
                "여기 떠요",
            ]
        } else {
            &[
                "등록된 기계가 없어요 —",
                "~/.config/kasaterm/machines.json 에 적으면 여기 떠요",
            ]
        };
        let mut my = top + vis_h * 0.4;
        for line in msg.iter().copied() {
            let tw = g.measure_chrome_text(line, 11.0, false);
            g.draw_text(
                x + (w - tw) / 2.0,
                my,
                line,
                gpu::DrawOpts {
                    font_size: 11.0,
                    color: theme::text_mute(),
                    bold: false,
                    italic: false,
                },
            );
            my += 17.0;
        }
        return;
    }

    // ── 이 맥북 ───────────────────────────────────────────────────────────
    y = section_label(g, x0, y, "이 맥북");
    if mc.locals.is_empty() {
        g.draw_text(
            x0,
            y,
            "학생 없음",
            gpu::DrawOpts {
                font_size: 11.0,
                color: theme::text_mute(),
                bold: false,
                italic: false,
            },
        );
        y += 22.0;
    } else {
        let machines_meta: Vec<(String, bool)> = mc
            .machines
            .iter()
            .map(|m| (m.label.clone(), m.online))
            .collect();
        let locals = mc.locals.clone();
        let mut last_room = String::new();
        for row in &locals {
            y = room_label(g, x0, y, &row.room, &mut last_room);
            let buttons: Vec<(state::MachinesColBtn, String, bool)> = machines_meta
                .iter()
                .map(|(label, online)| {
                    (
                        state::MachinesColBtn::Send {
                            pane: row.pane.clone(),
                            label: label.clone(),
                        },
                        format!("→ {label}"),
                        *online,
                    )
                })
                .collect();
            y = student_row(g, cursor, mc, row, &buttons, x0 + 8.0, right, y, narrow, None);
        }
    }

    // ── 기계별 섹션 ───────────────────────────────────────────────────────
    let machines = mc.machines.clone();
    for m in &machines {
        y += 10.0;
        let label_w = g.measure_chrome_text(&m.label, 10.0, true);
        y = section_label(g, x0, y, &m.label);
        let label_w = label_w.min((right - x0) * 0.5);
        // 연결 칩 — 라벨 오른쪽에.
        let chip_y = y - 20.0;
        let chip_text = if m.online {
            "연결됨"
        } else {
            "연결 안 닿아요"
        };
        let cw = g.measure_chrome_text(chip_text, 9.0, true) + 10.0;
        let chip_x = x0 + label_w + 8.0;
        round_rect(
            g,
            chip_x,
            chip_y - 2.0,
            cw,
            14.0,
            7.0,
            if m.online {
                theme::success()
            } else {
                theme::surface()
            },
        );
        g.draw_text(
            chip_x + 5.0,
            chip_y,
            chip_text,
            gpu::DrawOpts {
                font_size: 9.0,
                color: if m.online {
                    [255, 255, 255, 255]
                } else {
                    theme::text_mute()
                },
                bold: true,
                italic: false,
            },
        );
        // 「화면 보기」가 오른쪽 끝을 이미 쓴다 — 머리줄에 더 얹는 글은 그 앞에서
        // 끝나야 한다. 폭을 여기서 한 번 재고 아래 둘이 같이 쓴다.
        let screen_bl = "화면 보기";
        // 문이 둘 중 하나라도 있으면 버튼이 선다 — kvm(IP KVM 웹) 또는 host(화면공유).
        let has_screen_door = m.kvm.is_some() || !m.host.is_empty();
        let screen_bw = if !has_screen_door {
            0.0
        } else {
            g.measure_chrome_text(screen_bl, 9.0, true) + 12.0 + 8.0
        };
        // 「방 펼치기」 — 살아 있는 기계에 아직 거울로 안 뜬 학생이 있을 때만.
        // 머리줄 글이 버튼을 뚫지 않게 폭을 여기서 같이 잰다(화면 보기와 동일 규칙).
        let unfold_bl = "방 펼치기";
        let has_unfold = m.online && !m.remote.is_empty();
        let unfold_bw = if !has_unfold {
            0.0
        } else {
            g.measure_chrome_text(unfold_bl, 9.0, true) + 12.0 + 8.0
        };
        let meta_right = right - screen_bw - unfold_bw;
        if !m.online {
            let t = crate::info::fit_text(
                g,
                &ago_label(m.ago_secs),
                (meta_right - (chip_x + cw + 8.0)).max(0.0),
                9.0,
                false,
            );
            g.draw_text(
                chip_x + cw + 8.0,
                chip_y,
                &t,
                gpu::DrawOpts {
                    font_size: 9.0,
                    color: theme::text_mute(),
                    bold: false,
                    italic: false,
                },
            );
        }
        // 프로그램 낡음 경고 — 변경 실은 이사가 「저쪽을 갱신하라」로 서는 상태를
        // 누르기 전에 알린다. 갱신 방법(sync-mini)까지 한 줄에 담되, 그 한 줄이
        // 안 들어가면 라벨 줄에 우겨넣지 않고 **아랫줄**로 내린다 — 종전엔 216px
        // 칼럼에서 이 문장이 「화면 보기」 버튼을 뚫고 칼럼 밖까지 뻗었다.
        if m.online && m.outdated {
            let full = "⚠ 프로그램 낡음 — scripts/sync-mini.sh 로 갈아입히면 돼요";
            let head_room = (meta_right - (chip_x + cw + 8.0)).max(0.0);
            if g.measure_chrome_text(full, 9.0, true) <= head_room {
                g.draw_text(
                    chip_x + cw + 8.0,
                    chip_y,
                    full,
                    gpu::DrawOpts {
                        font_size: 9.0,
                        color: theme::attention(),
                        bold: true,
                        italic: false,
                    },
                );
            } else {
                let t = crate::info::fit_text(g, full, (right - x0).max(0.0), 9.0, true);
                g.draw_text(
                    x0,
                    y,
                    &t,
                    gpu::DrawOpts {
                        font_size: 9.0,
                        color: theme::attention(),
                        bold: true,
                        italic: false,
                    },
                );
                y += 14.0;
            }
        }
        // 화면 보기 버튼 — 문(kvm 또는 host)이 있을 때만, 라벨 줄 오른쪽 끝.
        // 연결이 끊겨도 그린다: KVM·화면공유는 카사텀 창구와 다른 문이라 따로
        // 살 수 있고, KVM 은 오히려 기계가 죽었을 때 보라고 있는 문이다.
        if has_screen_door {
            let bl = screen_bl;
            let bw = g.measure_chrome_text(bl, 9.0, true) + 12.0;
            let bh = 16.0;
            let bx = right - bw;
            let by = chip_y - 3.0;
            let hov =
                cursor.0 >= bx && cursor.0 <= bx + bw && cursor.1 >= by && cursor.1 <= by + bh;
            g.hover_pointer |= hov;
            round_rect(
                g,
                bx,
                by,
                bw,
                bh,
                theme::radius_sm(),
                theme::raised_on(theme::surface(), hov),
            );
            g.draw_text(
                bx + 6.0,
                by + 3.0,
                bl,
                gpu::DrawOpts {
                    font_size: 9.0,
                    color: theme::text(),
                    bold: true,
                    italic: false,
                },
            );
            mc.btn_rects.push((
                state::MachinesColBtn::Screen {
                    host: m.host.clone(),
                    kvm: m.kvm.clone(),
                },
                (bx, by, bw, bh),
            ));
        }
        if has_unfold {
            let bl = unfold_bl;
            let bw = g.measure_chrome_text(bl, 9.0, true) + 12.0;
            let bh = 16.0;
            let bx = right - screen_bw - bw;
            let by = chip_y - 3.0;
            let hov =
                cursor.0 >= bx && cursor.0 <= bx + bw && cursor.1 >= by && cursor.1 <= by + bh;
            g.hover_pointer |= hov;
            round_rect(
                g,
                bx,
                by,
                bw,
                bh,
                theme::radius_sm(),
                theme::raised_on(theme::surface(), hov),
            );
            g.draw_text(
                bx + 6.0,
                by + 3.0,
                bl,
                gpu::DrawOpts {
                    font_size: 9.0,
                    color: theme::text(),
                    bold: true,
                    italic: false,
                },
            );
            mc.btn_rects.push((
                state::MachinesColBtn::Unfold {
                    label: m.label.clone(),
                },
                (bx, by, bw, bh),
            ));
        }
        if m.mirrored.is_empty() && m.remote.is_empty() {
            g.draw_text(
                x0,
                y,
                "학생 없음",
                gpu::DrawOpts {
                    font_size: 11.0,
                    color: theme::text_mute(),
                    bold: false,
                    italic: false,
                },
            );
            y += 22.0;
            continue;
        }
        // 미러(이사 간 학생)의 방은 **로컬** 방이고 원격 행의 방은 그 기계의 방이라
        // 이름이 우연히 겹칠 수 있다 — 두 목록 사이에서 머리줄 기억을 끊는다.
        let mut last_room = String::new();
        for row in &m.mirrored {
            y = room_label(g, x0, y, &row.room, &mut last_room);
            let buttons = vec![(
                state::MachinesColBtn::Bring {
                    pane: row.pane.clone(),
                },
                "← 데려오기".to_string(),
                true,
            )];
            y = student_row(g, cursor, mc, row, &buttons, x0 + 8.0, right, y, narrow, None);
        }
        let mut last_room = String::new();
        for row in &m.remote {
            y = room_label(g, x0, y, &row.room, &mut last_room);
            // 학생 하나만 거울로 — 방 펼치기(기계 단위)의 짝. 버튼이 아니라 줄을
            // 누르면 포커스된 pane 의 탭으로 열린다. 안 닿는 기계는 줄이 안 눌린다
            // (눌러도 연결이 안 된다).
            let action = (m.online && !row.remote_id.is_empty()).then(|| {
                state::MachinesColBtn::Mirror {
                    label: m.label.clone(),
                    remote_id: row.remote_id.clone(),
                    name: row.name.clone(),
                    cwd: row.remote_cwd.clone(),
                }
            });
            y = student_row(g, cursor, mc, row, &[], x0 + 8.0, right, y, false, action);
        }
    }

    // 스크롤 상한 — 다른 칼럼과 같은 규칙(상한은 렌더가 잡는다).
    let content_h = (y + mc.scroll - top).max(0.0);
    let max_scroll = (content_h - vis_h).max(0.0);
    if mc.scroll > max_scroll {
        mc.scroll = max_scroll;
    }
    g.pop_clip();
}
