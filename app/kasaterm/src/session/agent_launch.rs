//! 에이전트 기동 — 칸에 학생 env 를 심고, 학생·페르소나를 바꾸고, 에이전트를 다시 띄운다.
//! 말투는 칸이 뜰 때 굳으므로 바꾸기와 재기동이 한 곳에 있다.
use super::*;

impl App {
    /// Phase C path. Spawns the shell into a direct PTY (no tmux),
    /// hooks the screens channel into the same per-pane state the
    /// renderer expects. Single-pane MVP — the workspace holds one
    /// PaneState keyed "%0" and the layout is `None` (the render path
    /// falls back to single-pane when no layout has arrived).
    /// Prepare a shell without automatically assigning a student or conversation.
    /// Only explicit selections/restored students reserve an identity here;
    /// normal allocation happens when the harness requests its launch identity.
    pub(crate) fn assign_character_env(
        &mut self,
        id: &str,
        cwd: Option<&str>,
        room: Option<&str>,
    ) -> Vec<(String, String)> {
        // A new terminal is only a shell. Do not consume a student or freeze a
        // conversation UUID before the user actually starts a harness.
        let blank = || vec![
            ("KASATERM_CHARACTER".into(), String::new()),
            ("KASATERM_PERSONA".into(), String::new()),
            ("KASATERM_SESSION_ID".into(), String::new()),
            ("KASATERM_AGENT_SLUG".into(), String::new()),
            ("KASATERM_MODEL".into(), String::new()),
            ("KASATERM_BACKEND".into(), String::new()),
            ("KASATERM_AGENT_SUFFIX".into(), crate::agent_name_suffix()),
        ];
        if self.pending_character.is_none() {
            self.ws.lock().unwrap().pane_launch_character.insert(id.into(), String::new());
            return blank();
        }
        let Some(cwd) = cwd else { return Vec::new() };
        let Some(chars) = kasa_mcp::character::roster_in_use() else {
            return Vec::new();
        };
        let rslug = kasa_mcp::character::rslug(std::path::Path::new(cwd), room);
        // 통합 풀(member_names = leader/leaders/members 병합) — god 개념 폐기(사용자
        // 2026-07-13): 아로나·프라나도 별도 클래스가 아닌 같은 배정 풀에 포함한다.
        // 배정 풀 — 골라 둔 명단이 있으면 그것만, 없으면 전원(지금까지의 동작).
        let members = kasa_mcp::character::assignable_names(&chars);
        // `KASATERM_ASSIGN_DEBUG=1` — 풀이 왜 그 크기인지 찍는다. 「골랐는데 안 고른
        // 애가 나온다」는 신고가 왔을 때 설정을 읽었는지부터 갈라야 하는데, 그걸
        // 밖에서 볼 방법이 이것 말고 없다.
        if std::env::var_os("KASATERM_ASSIGN_DEBUG").is_some() {
            let all = kasa_mcp::character::member_names(&chars).len();
            eprintln!(
                "[assign] pool={} / roster={all} — {:?}",
                members.len(),
                members
            );
        }
        // Explicit selections/restored identities stay reserved until launch.
        let Some(name) = self.pending_character.take() else { return blank() };
        self.ws.lock().unwrap().pane_next_character.insert(id.into(), name.clone());
        // 학생 명령(`시로코`)이 남긴 persona override 는 이 spawn 의 fresh env 보다
        // 오래된 정체성 — 지워서 이 pane 의 다음 claude 가 env 기준으로 돌아가게.
        if let Ok(shim) = std::env::var("KASATERM_TMUX_SHIM_DIR") {
            // 다섯을 함께 지운다 — 하나라도 남으면 그것이 새 학생에게 따라붙어,
            // 이름과 얼굴만 바뀌고 앞 학생의 모델(또는 말 거는 이름)로 도는
            // 상태가 된다.
            for ext in ["character", "persona", "model", "backend", "slug"] {
                let _ = std::fs::remove_file(
                    std::path::Path::new(&shim).join(format!("repersona-{id}.{ext}")),
                );
            }
        }
        let sid = kasa_mcp::character::new_session_id();
        self.ws.lock().unwrap().pane_launch_character.remove(id);
        let _ = kasa_mcp::character::write_marker(&rslug, id, &name);
        self.pane_session_id.insert(id.to_string(), sid.clone());
        // 세션→캐릭터 영속 바인딩(사용자 ④): 같은 세션이 --resume 등으로 다시 붙으면 같은
        // 캐릭터를 재사용하도록 스폰 시점에 기록(apply_session_character 가 조회).
        let _ = kasa_mcp::character::bind_session_character(&sid, &name);
        self.ws
            .lock()
            .unwrap()
            .pane_character
            .insert(id.to_string(), name.clone());
        let mut env = vec![
            ("KASATERM_CHARACTER".to_string(), name.clone()),
            ("KASATERM_SESSION_ID".to_string(), sid.clone()),
            // teammate 이름 꼬리 — 셰임이 `<슬러그>-p<번호>` 뒤에 그대로 붙인다.
            (
                "KASATERM_AGENT_SUFFIX".to_string(),
                crate::agent_name_suffix(),
            ),
            // 이름의 로마자 머리. 셰임에도 같은 표(`teammate_case_arms`)가 구워져
            // 있지만 그건 **앱 부팅 시점 스냅샷**이라, 그 뒤 테마를 바꾸거나 명단
            // 밖 학생이 앉으면 표에 없는 이름이 되어 셰임이 이름 붙이기를 통째로
            // 포기한다. 그러면 `kasaterm-cli tab` 이 미리 알려 준 이름과 실제로
            // 말이 닿는 이름이 갈린다(2026-08-26 실측). 배정과 같은 순간에 같은
            // 함수로 계산한 값을 여기서 내려 주는 것이 정본이고, 셰임은 이것을
            // 먼저 본다.
            (
                "KASATERM_AGENT_SLUG".to_string(),
                crate::theme::agent_slug(&name),
            ),
        ];
        // 활성 로스터에 없는 이름(재배정·resume 으로 온 다른 테마 학생)은 합집합
        // 조회로 원 소속 테마의 말투를 찾는다 — 없으면 이름만 남고 말투가 빈다.
        //
        // ⚠️ 「말투」 토글이 꺼져 있으면 **env 자체를 안 넣는다.** 소비하는 쪽이 셋이라
        // (claude shim 의 `--append-system-prompt`, codex 의 `AGENTS.md`, agy 의 agent
        // 파일) 각자 게이트를 달면 언젠가 한 곳이 빠진다 — 실제로 codex 쪽엔 없었고,
        // 그 래퍼는 정적 문자열이라 Rust 값을 박을 자리도 없다. 근원에서 한 번 막는다.
        // 캐릭터 이름·색·그림은 그대로다 — 토글의 뜻은 「말투만 끄기」다.
        if socket::read_claude_persona() {
            if let Some(p) = kasa_mcp::character::persona_for(&chars, &name)
                .or_else(|| kasa_mcp::character::persona_for_any(&name))
            {
                env.push(("KASATERM_PERSONA".to_string(), p));
            }
        }
        // 학생별 모델·실행 통로 — claude shim 이 전역 노브보다 이것을 먼저 본다
        // (2026-08-24 지시: 학생 한 명당 모델 선택). shim 은 부팅 1회 생성이라
        // 학생마다 다른 값을 구워 넣을 수가 없다. env 로 내려서 shim 안에서
        // 참조하는 것이 유일한 길이고, 그래서 이 자리가 정본이다.
        if let Some(m) = kasa_mcp::character::model_for(&chars, &name) {
            env.push(("KASATERM_MODEL".to_string(), m));
        }
        if let Some(b) = kasa_mcp::character::backend_for(&chars, &name) {
            env.push(("KASATERM_BACKEND".to_string(), b));
        }
        env
    }

    /// bind-transcript 로 pane 의 실제 세션 id 를 인지한 시점의 캐릭터 영속화(사용자 ④):
    /// 부모(포크/백그라운드)가 있으면 그 학생을 우선 상속하고, 없으면 세션 매핑으로
    /// 이름표를 교정(respawn 없음 — persona 는 스폰 시 고정, label·마커만 갱신,
    /// --resume 둔갑 방지), 그것도 없으면 현재 배정을 저장해 다음 resume 이 재사용한다.
    pub(crate) fn apply_session_character(&mut self, pane: &str, sid: &str) {
        // The model has already read these instructions. A late transcript or
        // parent lookup must not silently change only its face and name.
        let launched = self.ws.lock().unwrap().pane_launch_character.get(pane).cloned();
        if let Some(name) = launched {
            if name.is_empty() { return; } // this run ended; do not relabel its shell
            self.relabel_pane(pane, &name);
            if kasa_mcp::character::session_character(sid).is_none() {
                let _ = kasa_mcp::character::bind_session_character(sid, &name);
            }
            return;
        }
        let cur = self.ws.lock().unwrap().pane_character.get(pane).cloned();
        // 우선순위: 세션 자신의 바인딩 > 부모 상속 > env anchor. 예전엔 부모가 바인딩을
        // 덮었지만("첫 호출에 박힌 랜덤 바인딩 교정"용) — 지금 바인딩은 전부 의도적
        // 기록(ResumeSession 해석/신선 배정, lazy own, 여기 None-arm 영속화)이라 부모가
        // 이기면 오히려 진실이 뒤집힌다: 미도리로 확정된 포크 세션(2535079b)의 부모
        // (b18e41d2)가 히마리라서, BgAgentsChanged 재적용마다 미도리→히마리로 둔갑+
        // 재바인딩되는 지뢰였다(사용자 07-16). 부모는 자기 바인딩이 없을 때만.
        match kasa_mcp::character::session_character(sid) {
            Some(mapped) => {
                if cur.as_deref() != Some(mapped.as_str()) {
                    self.relabel_pane(pane, &mapped);
                }
                return;
            }
            None => {}
        }
        // 포크/백그라운드 세션은 부모 대화의 연장 — 자기 바인딩이 없으면 부모 학생을
        // 상속하고 sid 에 영속화한다. 첫 호출 때 bg_agents 가 비어(폴러 3초 주기) 이
        // 분기를 놓쳐도, 폴러의 BgAgentsChanged 재적용이 뒤늦게 부모를 물려준다.
        let parent_char = self
            .bg_agents
            .lock()
            .ok()
            .and_then(|m| m.get(sid).cloned())
            .flatten()
            .and_then(|parent| kasa_mcp::character::session_character(&parent));
        match parent_char {
            Some(pc) => {
                if cur.as_deref() != Some(pc.as_str()) {
                    self.relabel_pane(pane, &pc);
                }
                let _ = kasa_mcp::character::bind_session_character(sid, &pc);
            }
            None => {
                // stem 매핑도 부모도 없는 포크/재접속(claude 가 transcript id 를 새로 발급,
                // parentSessionId 부재) — 랜덤 cur 를 정본으로 굳히기 전에 pane 프로세스 env 의
                // KASATERM_SESSION_ID(스폰 때 학생에 바인딩된 원본 anchor, env 상속으로 보존)로
                // 진짜 학생을 복원한다(사용자: 백그라운드 재접속에서 미도리→유우카 둔갑).
                let anchored = self
                    .pty
                    .get(pane)
                    .and_then(|p| p.shell_pid())
                    .and_then(|pid| kasa_pty::process_env_var(pid, "KASATERM_SESSION_ID"))
                    .filter(|env_sid| env_sid.as_str() != sid)
                    .and_then(|env_sid| kasa_mcp::character::session_character(&env_sid));
                match anchored {
                    Some(true_char) => {
                        if cur.as_deref() != Some(true_char.as_str()) {
                            self.relabel_pane(pane, &true_char);
                        }
                        // stem 으로도 바로 잡히게 영속화 — 다음 board 폴링·재접속 안정화.
                        let _ = kasa_mcp::character::bind_session_character(sid, &true_char);
                    }
                    None => {
                        if let Some(cur) = cur.filter(|c| !c.is_empty()) {
                            let _ = kasa_mcp::character::bind_session_character(sid, &cur);
                        } else if let Some(chars) = kasa_mcp::character::roster_in_use() {
                            // Windows에서는 `ps eww`로 스폰 시점의 환경변수를 복구할 수
                            // 없으므로, 캐릭터 없이 복원된 pane은 SessionStart에서 보충한다.
                            let members = kasa_mcp::character::assignable_names(&chars);
                            if let Some(name) = self.next_auto_character(&members, pane) {
                                self.relabel_pane(pane, &name);
                                let _ = kasa_mcp::character::bind_session_character(sid, &name);
                            }
                        }
                    }
                }
            }
        }
    }

    /// pane 캐릭터 이름표 교정 — pane_character + board /tmp 마커 + redraw. 부모
    /// 상속·세션 매핑 두 경로가 공유한다. 실존 pane 만(훅 오호출·죽은 pane 가드).
    pub(crate) fn relabel_pane(&mut self, pane: &str, character: &str) {
        if self.ws.lock().unwrap().outer_for_pty(pane).is_none() {
            return;
        }
        self.ws
            .lock()
            .unwrap()
            .pane_character
            .insert(pane.to_string(), character.to_string());
        // board 도 같은 이름을 보게 /tmp 마커 동기(swap_character 의 cwd/room 관례).
        if let Some(cwd) = self.pane_cwd_cache.get(pane).cloned() {
            let room = self.ws.lock().unwrap().pane_room.get(pane).cloned();
            let rslug = kasa_mcp::character::rslug(&cwd, room.as_deref());
            let _ = kasa_mcp::character::write_marker(&rslug, pane, character);
        }
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// pane 캐릭터 재배정, respawn 없음 — 학생 명령(`시로코`)이 claude 실행 직전에
    /// `/repersona` 로 호출한다. persona 는 래퍼가 override 파일로 직접 싣고 여기선
    /// GUI 상태(헤더·테두리·board 마커·세션 바인딩)만 새 캐릭터로 맞춘다. 중복 허용
    /// — 같은 학생 pane 은 색 변주(character_ordinal)로 구분(사용자).
    /// 이 pane 의 다음 claude 가 쓸 정체성을 파일로 남긴다(학생 명령과 같은 규약).
    /// spawn 때 지워지므로 새 pane 에는 안 따라간다.
    ///
    /// 「말투」 토글이 꺼져 있으면 **빈 파일**을 쓴다 — 파일이 아예 없으면 shim 이
    /// spawn 때의 env 로 되돌아가 옛 말투가 되살아난다. 빈 내용은 「말투 없음」이라는
    /// 명시적 뜻이다.

    pub(crate) fn repersona_pane(&mut self, pane: &str, character: &str) {
        if !self.ws.lock().unwrap().panes.contains_key(pane) {
            return;
        }
        // 로스터 밖 이름 가드 — 엔드포인트로 들어오는 자유 문자열이 헤더/마커를
        // 오염하지 않게. 활성만 보면 진행 중 pane 을 다른 테마 학생으로 바꾸는
        // 기능(2026-08-24 지시)이 죽으므로, 아는 명부의 합집합(활성∪번들∪설치
        // 테마)으로 본다 — 렌더 쪽 이름 조회와 같은 경계다.
        if crate::theme::character_slug_any(character).is_none() {
            eprintln!("[repersona] unknown character '{character}' — ignored");
            return;
        }
        // A shell has no loaded voice, and persona-off deliberately allows
        // visual-only changes. Do not retain a previous run's identity latch.
        if !socket::read_claude_persona()
            || self.pty.get(pane).and_then(|session| session.active_agent()).is_none()
        {
            self.ws.lock().unwrap().pane_launch_character.remove(pane);
        }
        self.ws
            .lock()
            .unwrap()
            .pane_character
            .insert(pane.to_string(), character.to_string());
        if let Some(cwd) = self.pane_cwd_cache.get(pane).cloned() {
            let room = self.ws.lock().unwrap().pane_room.get(pane).cloned();
            let rslug = kasa_mcp::character::rslug(&cwd, room.as_deref());
            let _ = kasa_mcp::character::write_marker(&rslug, pane, character);
        }
        // --resume 가 같은 캐릭터로 돌아오게 세션 바인딩도 갱신 — pane 이 물고 있는 sid 를
        // **모두** 맞춘다. spawn anchor(pane_session_id)와 claude transcript stem
        // (pane_claude_sid)은 다를 수 있는데(claude 가 자기 세션 id 를 새로 발급), info
        // 그림과 persona 재주입은 **stem** 을 읽는다(chrome.rs display_tab_char·http.rs
        // /persona). stem 을 빼먹으면 재배정해도 옛 테마 캐릭터의 얼굴·말투가 남는다
        // (사용자 실측: 배정은 히후미인데 info·말투는 고블린).
        for sid in [
            self.pane_session_id.get(pane),
            self.pane_claude_sid.get(pane),
        ]
        .into_iter()
        .flatten()
        {
            let _ = kasa_mcp::character::bind_session_character(sid, character);
            // **사람이 고른 자리**라고 적어 둔다. 이게 없으면 명단을 정리할 때
            // 자동 배정의 잔재와 구별되지 않아 함께 쓸려 나간다(2026-08-26 지시).
            kasa_mcp::character::mark_manual_pick(sid);
        }
        // 말투도 새 캐릭터 것으로 — **다음에 이 pane 에서 claude 가 뜰 때부터**.
        //
        // 도는 프로세스의 시스템 프롬프트는 못 바꾸므로 지금 대화 중인 상대는 옛
        // 말투 그대로다(그걸 바꾸려면 대화를 끊어야 한다 — 그게 「캐릭터 교체」다).
        // 그런데 바인딩만 갱신하면 **다시 띄워도 옛 말투로 뜬다**: shim 이 spawn 때
        // 고정된 `KASATERM_PERSONA`(옛 캐릭터)로 `--append-system-prompt` 를 붙이고,
        // SessionStart 훅은 그 인자가 보이면 자기 주입을 건너뛰기 때문이다. 학생
        // 명령(`시로코`)이 쓰는 override 파일에 새 말투를 남겨 그 사슬을 끊는다.
        write_persona_override(pane, character);
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// 학생 교체 진입점 — 말투가 켜져 있으면 **다시 띄울지 먼저 묻는다**.
    ///
    /// 이름·얼굴·색은 `repersona_pane` 만으로 즉시 바뀌지만 말투는 그렇지 않다.
    /// 그래서 말투가 켜져 있고 되띄울 에이전트가 실제로 도는 pane 일 때만 카드를
    /// 띄운다 — 「말투 오프돼있으면 그냥 껍데기만바뀌게」(2026-08-25 지시). 셸만
    /// 있는 pane 도 같다: 되띄울 대화가 없으니 물을 것이 없다.
    pub(crate) fn ask_or_repersona(&mut self, pane: &str, name: &str) {
        let agent = self.pty.get(pane).and_then(|p| p.active_agent());
        let agent = agent.as_ref().map(|a| a.as_str());
        // `restart_pane_agent` 와 **같은 규칙으로** 미리 잰다. 여기서 다르게 재면
        // 카드가 「대화는 그대로」라고 약속해 놓고 실제로는 새로 시작한다.
        let has_convo = self.pane_claude_sid.get(pane).is_some_and(|s| {
            if agent == Some("codex") {
                socket::codex_rollout_for_session(s).is_some()
            } else {
                socket::transcript_path_for_session(s).is_some()
            }
        });
        let resumable = match plan_character_swap(socket::read_claude_persona(), agent, has_convo) {
            SwapPlan::Now => {
                self.repersona_pane(pane, name);
                self.set_toast(format!("{pane} → {name}"));
                return;
            }
            SwapPlan::Ask { resumable } => resumable,
        };
        self.character_swap_confirm = Some(PendingCharacterSwap {
            pane: pane.to_string(),
            to: name.to_string(),
            resumable,
            rects: Vec::new(),
        });
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// 카드에서 고른 결과. 취소는 **아무 일도 안 한다**.
    pub(crate) fn character_swap_pick(&mut self, btn: CharacterSwapBtn) {
        let Some(p) = self.character_swap_confirm.take() else {
            return;
        };
        let previous = self.ws.lock().unwrap().pane_character.get(&p.pane).cloned();
        if btn != CharacterSwapBtn::Cancel {
            // 어느 쪽이든 마커·바인딩·말투 파일이 먼저 새 학생으로 서야 한다 —
            // 되띄우기가 `assign_character_env` 로 env 를 다시 세울 때 그것을 읽는다.
            self.repersona_pane(&p.pane, &p.to);
        }
        let msg = match btn {
            CharacterSwapBtn::Cancel => None,
            CharacterSwapBtn::Relaunch => Some({
                // **되띄우기가 캐릭터를 다시 고르지 못하게 못 박는다.** 그 경로는
                // `assign_character_env` 로 env 를 새로 세우는데, 그 함수는 고른
                // 명단(`assignable_names`)에서 뽑으므로 **명단 밖 학생으로 바꾼
                // 경우 방금 지정한 이름이 그 자리에서 다른 학생으로 갈아치워진다**
                // (2026-08-26 지시: 「다른거로 바꿔도 테마 적용돼있으면 다른테마
                // 캐릭터로 안바뀌어」). pending 은 그 선택보다 우선한다.
                self.pending_character = Some(p.to.clone());
                let ok = self.restart_pane_agent(&p.pane);
                // 되띄우기가 실패하면 pending 이 남아 **다음에 뜨는 엉뚱한 pane** 이
                // 그 학생을 물고 간다. 쓰였든 아니든 여기서 걷는다.
                self.pending_character = None;
                if ok {
                    format!("{} → {} · 대화를 이어서 다시 띄웠어요", p.pane, p.to)
                } else {
                    if let Some(previous) = previous {
                        self.repersona_pane(&p.pane, &previous);
                    }
                    format!("{} · 다시 띄우지 못해 학생을 바꾸지 않았어요", p.pane)
                }
            }),
        };
        if let Some(m) = msg {
            self.set_toast(m);
        }
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// pane 캐릭터 교체 — persona 는 셸 spawn 시 고정이라 PTY 를 새 persona 로 respawn
    /// 한다(대화 리셋, 사용자 확인 후). 같은 pane id·leaf 유지라 레이아웃·자리 그대로,
    /// 헤더/board 캐릭터만 다음 화면에 갱신(assign_character_env 가 ws.pane_character·마커
    /// 를 덮음).
    pub(crate) fn swap_character(&mut self, pane: &str, character: &str) {
        let cwd = self
            .pane_cwd_cache
            .get(pane)
            .map(|p| p.to_string_lossy().into_owned());
        let room = self.ws.lock().unwrap().pane_room.get(pane).cloned();
        let (cols, rows) = self.window_cells();
        // old PTY 종료(셸·claude 죽음). pump 스레드는 EOF 로 빠진다.
        self.pty.remove(pane);
        // 새 persona 강제 — assign_character_env 가 pending 우선 사용해 마커·env 갱신.
        self.pending_character = Some(character.to_string());
        let mut env = crate::proxy_env(pane);
        if let Some(ref r) = room {
            env.push(("KASATERM_ROOM".to_string(), r.clone()));
        }
        env.extend(self.assign_character_env(pane, cwd.as_deref(), room.as_deref()));
        match kasa_pty::PtySession::start(kasa_pty::PtyOptions {
            shell: resolve_default_shell(),
            cwd,
            cols,
            rows,
            env,
            pane_id: pane.to_string(),
            initial_scrollback: Vec::new(),
        }) {
            Ok(session) => {
                let sess = Arc::new(session);
                self.pump_pty_screens(
                    sess.screens.clone(),
                    pane.to_string(),
                    std::sync::Arc::downgrade(&sess),
                );
                self.insert_pty(pane.to_string(), sess.clone());
                // old PTY 의 EOF 가 이 pane id 를 dead_panes 에 넣었을 수 있다 — 같은 id 로
                // respawn 했으니 그 stale 죽음표시를 지워 reap 이 새 pane 을 닫지 않게(사용자:
                // 캐릭터 변경하면 pane 이 닫히던 버그). reap 에 contains_key 가드도 있지만 명시.
                self.dead_panes.lock().unwrap().retain(|x| x != pane);
                // 새 PTY 는 셸 프롬프트만 — 교체는 돌던 claude 를 죽이므로, 프롬프트가 뜰 즈음
                // claude 를 직접 주입해 새 persona 로 다시 시작한다(사용자: 캐릭터 교체 = claude 새로.
                // 초기 부팅은 셸만 띄워도 됐지만, 교체는 claude 가 꺼진 채 셸만 남던 게 버그였다).
                let at = std::time::Instant::now() + std::time::Duration::from_millis(900);
                self.pending_restores
                    .push((sess, "claude\r".to_string(), at));
                self.resize_backend(cols, rows);
                self.publish_pty_layout();
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            Err(e) => eprintln!("[swap_character] respawn failed: {e:#}"),
        }
    }
    /// 도는 에이전트를 **같은 pane 자리에서** 새 계정으로 다시 띄운다.
    ///
    /// 계정은 `CLAUDE_SECURESTORAGE_CONFIG_DIR` = 프로세스 env 라 pane 이 뜰 때 박히고,
    /// 도는 프로세스의 env 는 누구도 못 바꾼다. 그래서 계정을 전환해도 이미 열려 있는
    /// pane 은 옛 계정으로 계속 돌았다(사용자 2026-08-13: "전환하면 인포랑 하단은 바뀌는데
    /// pane안에 세션이 인식못하나봐"). Orca 도 같은 한계를 재시작으로 푼다
    /// (`CodexRestartChip` → `queueCodexPaneRestarts`) — 자동 승계는 저쪽에도 없다.
    ///
    /// `swap_character` 와 같은 골격이다: 같은 pane id 로 PTY 를 갈아끼우고 셸 프롬프트가
    /// 뜰 즈음 명령을 주입한다. 다른 점은 주입하는 명령뿐 — 캐릭터는 그대로 두고
    /// `restore_agent_command` 로 **하던 대화를 이어서** 띄운다(claude·codex·agy 각각).
    ///
    /// 대화 파일이 없으면 resume 을 걸지 않는다. 그 상태로 `--resume` 하면 claude 가
    /// "No conversation found" 를 뱉고 빈 셸만 남아 학생 pane 이 통째 죽는다 — 대화를
    /// 잃더라도 fresh 로 띄우는 편이 낫다(restore_leaf 와 같은 판단).
    ///
    /// 반환값은 「정말 다시 띄웠나」다. false 면 부르는 쪽이 알림을 되돌려야 한다 —
    /// 재시작이 조용히 실패하면 사용자는 옛 계정으로 도는 pane 을 새 계정이라 믿는다.
    pub(crate) fn restart_pane_agent(&mut self, pane: &str) -> bool {
        // 지금 그 pane 에서 **실제로 도는** 하네스. 셸만 있는 pane 은 되띄울 것이 없다.
        let Some(agent) = self.pty.get(pane).and_then(|p| p.active_agent()) else {
            return false;
        };
        let agent = agent.as_str();
        let sid = self.pane_claude_sid.get(pane).cloned();
        let resumable = sid.as_deref().is_some_and(|s| {
            if agent == "codex" {
                socket::codex_rollout_for_session(s).is_some()
            } else {
                socket::transcript_path_for_session(s).is_some()
            }
        });
        let cwd = self
            .pane_cwd_cache
            .get(pane)
            .map(|p| p.to_string_lossy().into_owned());
        let room = self.ws.lock().unwrap().pane_room.get(pane).cloned();
        // 끄기 직전 이 pane 이 쓰던 모델·effort — 되띄울 때 그대로 잇는다. 안 실으면
        // shim 기본 모델로 떨어져 사용자가 /model·/effort 를 다시 쳐야 했다(2026-08-16
        // 「모델이랑 에포트도 안됐었어」). 세션 저장이 leaf 에 싣는 것과 같은 스냅샷
        // 이고, 옛 PTY 를 지우기 전에 떠야 값이 남아 있다.
        let (model, effort) = self
            .agent_cfg_snapshot()
            .get(pane)
            .cloned()
            .unwrap_or_default();
        // 권한 모드도 끄기 전에 화면에서 뜬다 — 계정을 갈았다고 bypass 학생이
        // 물어보는 모드로 떨어질 이유가 없다(복원·이사와 같은 승계 원칙).
        // claude 가 이미 죽었어도 얼어붙은 화면에 푸터가 남아 있어 읽힌다.
        let bypass = { let ws = self.ws.lock().unwrap(); self.pane_bypass_on(&ws, pane) };
        let (cols, rows) = self.window_cells();
        // 옛 PTY 종료 — 여기서 옛 계정 토큰을 문 프로세스가 사라진다. pump 스레드는
        // EOF 로 빠진다.
        self.pty.remove(pane);
        let mut env = crate::proxy_env(pane);
        if let Some(ref r) = room {
            env.push(("KASATERM_ROOM".to_string(), r.clone()));
        }
        // 캐릭터는 유지한다 — 계정이 바뀌었다고 학생이 바뀔 이유가 없다.
        env.extend(self.assign_character_env(pane, cwd.as_deref(), room.as_deref()));
        match kasa_pty::PtySession::start(kasa_pty::PtyOptions {
            shell: resolve_default_shell(),
            cwd,
            cols,
            rows,
            env,
            pane_id: pane.to_string(),
            initial_scrollback: Vec::new(),
        }) {
            Ok(session) => {
                let sess = Arc::new(session);
                self.pump_pty_screens(
                    sess.screens.clone(),
                    pane.to_string(),
                    std::sync::Arc::downgrade(&sess),
                );
                self.insert_pty(pane.to_string(), sess.clone());
                // 옛 PTY 의 EOF 가 이 id 를 dead_panes 에 넣었을 수 있다 — 같은 id 로
                // 되띄웠으니 지운다(swap_character 와 같은 이유).
                self.dead_panes.lock().unwrap().retain(|x| x != pane);
                // 빈 값 거르기는 restore_agent_command 몫이다(빈 문자열이면 플래그를
                // 아예 안 붙인다). 명령 끝 '\r' 도 거기서 이미 붙는다.
                let mut cmd = restore_agent_command(
                    Some(agent),
                    sid.as_deref(),
                    resumable,
                    Some(model.as_str()),
                    Some(effort.as_str()),
                );
                if bypass && agent == "claude" && cmd.ends_with('\r') {
                    cmd.pop();
                    cmd.push_str(" --dangerously-skip-permissions\r");
                }
                let at = std::time::Instant::now() + std::time::Duration::from_millis(900);
                self.pending_restores.push((sess, cmd, at));
                self.resize_backend(cols, rows);
                self.publish_pty_layout();
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
                true
            }
            Err(e) => {
                eprintln!("[restart_pane_agent] respawn failed: {e:#}");
                false
            }
        }
    }

    /// 계정 전환을 **떠 있는 pane 에까지** 적용한다 — 자동·수동 전환이 같은 꼬리를 탄다.
    ///
    /// 도는 프로세스의 env 는 못 바꾸므로(`restart_pane_agent` doc) 반영 수단은
    /// 재시작뿐이다. 전에는 자동 전환이 「⟳ 재시작」 칩만 띄우고 수동 전환은 그마저
    /// 없어서, 전환해 놓고 pane 안 /status 가 옛 계정인 것을 보고 "바로 안 된다"가
    /// 됐다(사용자 2026-08-15: "재시작칩없이 나도 그렇게 되게해줘"). 이제 쉬는 pane 은
    /// 그 자리에서 대화를 이어 재시작하고, 일하는 중인 pane 은 칩을 단 채 남겼다가
    /// 턴이 끝나면 틱(`run_pending_account_restarts`)이 마저 돌린다 — 일하는 학생을
    /// 중간에 끊으면 진행 중이던 턴이 통째로 죽기 때문이다.
    ///
    /// 대상 판정은 전환 이벤트 추정이 아니라 **pane 별 실측**이다. 그래서 같은 계정을
    /// 다시 눌러도 「어긋난 pane 만」 맞춰 띄운다 — 앱이 못 본 전환으로 이미 어긋나
    /// 있던 pane 도 계정 버튼 한 번으로 수습된다.
    ///
    /// 반환: (전환 전 계정 이름, 새 계정 이름, 즉시 재시작한 수, 끝나길 기다리는 수,
    /// 보는 pane 이 대기 중인가, 작업대 갈아 끼우기 성공 여부).
    /// `character-pick` — 캐릭터 하나를 명단에 넣거나 뺀다.
    ///
    /// 반환은 「반영됐는가」 — 저장 뒤 파일을 다시 읽어 확인한다. 토글이라 「눌렀다」
    /// 만으로는 알 수 없다는 기존 규칙 그대로다.
    pub(crate) fn apply_character_pick(
        &mut self,
        theme: &str,
        name: Option<&str>,
        on: bool,
    ) -> Result<bool, String> {
        let name = name.map(str::trim).unwrap_or_default();
        if theme.is_empty() || name.is_empty() {
            return Err(crate::settings::reject(
                "character_pick_bad_id",
                "어느 테마의 누구인지 알 수 없어요".to_string(),
            ));
        }
        // 켜기는 그 테마에 실재하는 이름만 받는다. 안 막으면 오타 하나가 설정
        // 파일에 그대로 눌러앉는데, 배정 쪽은 유령을 조용히 걸러 내므로 **화면에만
        // 한 명 더 켜진 것처럼 보이고 실제로는 안 나오는** 상태가 된다.
        //
        // 끄기는 검사하지 않는다 — 그래야 이미 들어앉은 유령이나 이름이 바뀐
        // 항목을 화면에서 지울 길이 남는다.
        if on {
            let names = theme_roster_names(theme);
            if names.is_empty() {
                return Err(crate::settings::reject(
                    "theme_roster_missing",
                    "그 테마의 명단을 못 읽었어요".to_string(),
                ));
            }
            if !names.iter().any(|n| n == name) {
                return Err(crate::settings::reject_with_args(
                    "character_pick_unknown",
                    serde_json::json!({ "name": name }),
                    format!("{name} 은(는) 그 테마에 없어요"),
                ));
            }
        }
        let picks = updated_character_picks(kasa_mcp::character::all_picks(), theme, name, on, theme_roster_value)?;
        socket::write_character_picks(&picks);
        self.invalidate_character_view();
        Ok(kasa_mcp::character::picks_of_theme(theme)
            .iter()
            .any(|n| n == name)
            == on)
    }

    /// `theme-pick-all` — 테마 하나를 통째로 켜거나 끈다.
    pub(crate) fn apply_theme_pick_all(&mut self, theme: &str, on: bool) -> Result<bool, String> {
        if theme.is_empty() {
            return Err(crate::settings::reject(
                "character_pick_bad_id",
                "어느 테마인지 알 수 없어요".to_string(),
            ));
        }
        let names = theme_roster_names(theme);
        if names.is_empty() {
            return Err(crate::settings::reject(
                "theme_roster_missing",
                "그 테마의 명단을 못 읽었어요".to_string(),
            ));
        }
        let mut picks = kasa_mcp::character::all_picks();
        picks.retain(|(k, _)| k != theme);
        if on {
            replace_conflicting_character_picks(&mut picks, theme, &names, theme_roster_value);
            picks.push((theme.to_string(), names));
        }
        if !picks.iter().any(|(_, selected)| !selected.is_empty()) {
            return Err("최소 한 명은 선택해 주세요".to_string());
        }
        // 끄기는 키를 빼는 것으로 끝난다 — 빈 배열은 저장 때 어차피 걷힌다.
        socket::write_character_picks(&picks);
        self.invalidate_character_view();
        Ok(kasa_mcp::character::picks_of_theme(theme).is_empty() != on)
    }

    /// 다른 기기에서 학생 테마·명단이 넘어와 settings.json 이 밖에서 바뀌었을 때. 설정 화면이
    /// 쓰는 길과 달리 `write_character_picks` 를 안 거쳤으니 배정·활성 테마 캐시도 여기서 걷는다.
    pub(crate) fn reload_student_choices(&mut self) {
        kasa_mcp::character::invalidate_active_theme();
        kasa_mcp::character::invalidate_character_picks();
        socket::invalidate_theme_rows();
        self.invalidate_character_view();
    }

    /// 명단이 바뀐 뒤 화면 쪽 캐시를 걷는다. 배정 캐시는 `write_character_picks` 가
    /// 이미 비웠다 — 여기는 그림·색·테마 카드 몫이라 **짝으로** 불러야 한다.
    pub(super) fn invalidate_character_view(&mut self) {
        crate::theme::invalidate_roster();
        self.web_visual.invalidate_assets();
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.drop_images_with_prefix("student:");
        }
        crate::render::invalidate_idle_anim();
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
}

/// 학생을 바꿀 때 「다시 띄울까」를 묻는 카드의 상태.
///
/// 이름·얼굴·색은 바로 바뀌지만 **말투는 pane 이 뜰 때 정해져 굳는다** — 도는
/// 프로세스의 시스템 프롬프트는 누구도 못 바꾼다. 그래서 말투까지 지금 맞추려면
/// 다시 띄우는 수밖에 없고, 그건 사용자에게 물어야 하는 조작이다(2026-08-25 지시:
/// 「그럼 새로띄우게해 테마 바꾸면 확인버튼도 만들고」).
pub(crate) struct PendingCharacterSwap {
    pub pane: String,
    pub to: String,
    /// 이어붙일 대화가 있는가. 없으면 다시 띄우기가 지금 내용을 잃으므로 문구와
    /// 버튼 색이 갈린다 — 계정 전환 카드의 `fresh` 와 같은 규칙이다.
    pub resumable: bool,
    /// 그 창의 render 가 매 프레임 채운다(`PendingAccountSwitch::rects` 와 같은 이유).
    pub rects: Vec<(CharacterSwapBtn, (f32, f32, f32, f32))>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CharacterSwapBtn {
    Cancel,
    /// 대화를 이어서 다시 띄운다 — 말투까지 지금 바뀐다.
    Relaunch,
}

/// 받침에 맞춘 「으로/로」. 학생 이름은 사람이 읽는 문장 안에 그대로 들어가므로
/// 조사가 어긋나면 바로 눈에 띈다(2026-08-25 실측 캡처: 「은랑 로 바꿀까요?」).
///
/// ㄹ 받침은 「로」다 — 「서울로」. 한글이 아닌 이름(로마자·기호)도 「로」로 둔다.
pub(super) fn euro_ro(word: &str) -> &'static str {
    match word.chars().last() {
        Some(c) if ('가'..='힣').contains(&c) => {
            let jong = (c as u32 - 0xAC00) % 28;
            if jong == 0 || jong == 8 {
                "로"
            } else {
                "으로"
            }
        }
        _ => "로",
    }
}

/// 학생을 바꿀 때 물을 것인가, 그냥 바꿀 것인가.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SwapPlan {
    /// 확인 없이 지금 바꾼다 — 바뀌는 것이 이름·얼굴·색뿐이라 되돌릴 것이 없다.
    Now,
    /// 다시 띄울지 묻는다. `resumable` 이 false 면 되띄우기가 지금 내용을 버린다.
    Ask { resumable: bool },
}

/// 위 갈림의 순수부. `App` 을 안 들고 다니므로 테스트가 된다.
///
/// 카드를 띄우는 유일한 이유는 **다시 띄워야 하는 것**이다. 말투가 꺼져 있으면
/// 바뀌는 건 껍데기뿐이라 되띄울 이유가 없고(2026-08-25 지시: 「말투 오프돼있으면
/// 그냥 껍데기만바뀌게」), 에이전트가 안 도는 pane 도 되띄울 대화가 없다. 둘 중
/// 하나라도 해당하면 묻지 않고 바로 바꾼다 — 묻지 않아도 되는 것을 묻는 카드는
/// 그 자체가 방해다.
pub(crate) fn plan_character_swap(
    persona_on: bool,
    agent: Option<&str>,
    has_convo: bool,
) -> SwapPlan {
    if !persona_on || agent.is_none() {
        return SwapPlan::Now;
    }
    SwapPlan::Ask {
        resumable: has_convo,
    }
}

/// 카드에 적을 제목과 본문. `App` 을 안 들고 다니므로 테스트가 된다.
pub(crate) fn character_swap_confirm_text(to: &str, resumable: bool) -> (String, Vec<String>) {
    let title = format!("이 자리를 {to}{} 바꿀까요?", euro_ro(to));
    let lines = if resumable {
        vec![
            "다시 띄우면 말투까지 바뀝니다 — 나눈 대화는 이어서 띄우니 그대로예요.".to_string(),
            "이름·얼굴·말투를 함께 바꿉니다. 취소하면 지금 학생을 그대로 유지해요.".to_string(),
        ]
    } else {
        vec![
            "이어붙일 대화가 없어, 다시 띄우면 지금 내용이 사라집니다.".to_string(),
            "취소하면 지금 학생과 대화를 그대로 유지해요.".to_string(),
        ]
    };
    (title, lines)
}

pub(super) fn theme_roster_value(theme: &str) -> Option<serde_json::Value> {
    if theme == kasa_mcp::character::BASE_THEME_KEY {
        kasa_mcp::character::base_characters_json()
    } else {
        kasa_mcp::character::theme_characters_json(theme)
    }
}

pub(super) fn updated_character_picks(
    mut picks: Vec<(String, Vec<String>)>,
    theme: &str,
    name: &str,
    on: bool,
    load: impl Fn(&str) -> Option<serde_json::Value>,
) -> Result<Vec<(String, Vec<String>)>, String> {
    let unrestricted = picks.iter().all(|(_, names)| names.is_empty());
    if on {
        replace_conflicting_character_picks(&mut picks, theme, &[name.to_string()], &load);
    }
    let index = picks.iter().position(|(key, _)| key == theme).unwrap_or_else(|| {
        picks.push((theme.to_string(), Vec::new()));
        picks.len() - 1
    });
    let names = &mut picks[index].1;
    // 처음의 전체 후보에서 한 명을 뺄 때만 나머지를 명시적으로 저장한다.
    if !on && unrestricted {
        *names = load(theme).as_ref().map(kasa_mcp::character::member_names).unwrap_or_default();
    }
    names.retain(|other| other != name);
    if on { names.push(name.to_string()); }
    picks.retain(|(_, names)| !names.is_empty());
    if picks.is_empty() { return Err("최소 한 명은 선택해 주세요".to_string()); }
    Ok(picks)
}

pub(super) fn replace_conflicting_character_picks(
    picks: &mut [(String, Vec<String>)],
    theme: &str,
    names: &[String],
    load: impl Fn(&str) -> Option<serde_json::Value>,
) {
    let slug_of = |roster: &serde_json::Value, name: &str| {
        kasa_mcp::character::member_def(roster, name)
            .and_then(|m| m.get("slug").and_then(|s| s.as_str()).map(String::from))
            .filter(|s| !s.is_empty())
    };
    let slugs: Vec<String> = load(theme).map(|roster| names.iter().filter_map(|name| slug_of(&roster, name)).collect()).unwrap_or_default();
    for (other_theme, selected) in picks {
        if other_theme == theme { continue; }
        let roster = load(other_theme);
        // 얼굴도 슬러그로 찾으므로 동명뿐 아니라 같은 그림 키도 한 테마만 남긴다.
        selected.retain(|name| !names.contains(name) && !roster.as_ref()
            .and_then(|r| slug_of(r, name)).is_some_and(|slug| slugs.contains(&slug)));
    }
}

pub(super) fn theme_roster_names(theme: &str) -> Vec<String> {
    theme_roster_value(theme)
        .as_ref()
        .map(kasa_mcp::character::member_names)
        .unwrap_or_default()
}

/// pane 의 말투·모델·통로를 새 학생 것으로 갈아 둔다 — shim 이 다음 부팅에 읽는
/// override 파일(`repersona-<pane>.*`).
///
/// 도는 프로세스의 시스템 프롬프트는 못 바꾸므로 **지금 대화 중인 상대는 옛 말투
/// 그대로**고, 여기서 바꾸는 것은 다음에 그 pane 에서 claude 가 뜰 때부터다. 그런데
/// 바인딩·마커만 갱신하면 **다시 띄워도 옛 말투로 뜬다**: shim 이 spawn 때 고정된
/// `KASATERM_PERSONA`(옛 학생)로 `--append-system-prompt` 를 붙이고, SessionStart
/// 훅은 그 인자가 보이면 자기 주입을 건너뛰기 때문이다. 이 파일들이 그 사슬을 끊는다.
///
/// `self` 를 안 쓰므로 자유함수다 — 캐릭터를 갈아 끼우는 자리가 GUI(`App`)와 board
/// 빌드(`PtyBackend`) 양쪽에 있고, 한쪽만 갱신하면 그 경로로 바뀐 pane 만 말투가
/// 어긋난 채 남는다.
/// 이 pane 의 학생을 사람이 학생 명령으로 **직접 갈았는가** — override 파일이 그 흔적이다.
/// 자동 배정만 받은 pane 은 파일이 없다(spawn 이 지운다).
pub(crate) fn pane_reassigned(pane: &str) -> bool {
    std::env::var("KASATERM_TMUX_SHIM_DIR").is_ok_and(|shim| {
        std::path::Path::new(&shim)
            .join(format!("repersona-{pane}.character"))
            .is_file()
    })
}

/// bind-transcript 에서 세션의 기존 바인딩과 pane 의 현재 학생이 다를 때, pane 쪽이
/// 정본인가. 사람이 이 pane 을 재배정했거나 바인딩된 학생이 지금 명단에 없을 때만 —
/// 그 밖엔 세션이 이긴다: 갓 자동 배정된 탭에서 `claude --resume` 으로 남의 대화를
/// 이으면 그 pane 은 그 대화의 학생이 돼야지, 우연히 앉은 학생으로 대화를 덮으면
/// 안 된다(2026-09-08 실측: 세이아 대화가 아즈사로 둔갑하고 바인딩까지 덮였다).
pub(crate) fn pane_pick_wins(reassigned: bool, bound_assignable: bool) -> bool {
    reassigned || !bound_assignable
}

pub(crate) fn write_persona_override(pane: &str, character: &str) {
    let Ok(shim) = std::env::var("KASATERM_TMUX_SHIM_DIR") else {
        return;
    };
    let dir = std::path::Path::new(&shim);
    let persona = if socket::read_claude_persona() {
        kasa_mcp::character::characters_json()
            .and_then(|c| kasa_mcp::character::persona_for(&c, character))
            .or_else(|| kasa_mcp::character::persona_for_any(character))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let base = dir.join(format!("repersona-{pane}"));
    let _ = std::fs::write(base.with_extension("persona"), persona);
    let _ = std::fs::write(base.with_extension("character"), character);
    // 이름의 로마자 머리도 새 학생 것으로 — spawn 때 내려간 env 는 옛 학생
    // 것이라, 이 파일이 없으면 얼굴과 말투만 바뀌고 **말 거는 이름은 앞 학생**
    // 으로 남는다.
    let _ = std::fs::write(
        base.with_extension("slug"),
        crate::theme::agent_slug(character),
    );
    // 모델·통로도 새 캐릭터 것으로 — 안 맞추면 이름과 얼굴만 바뀌고 앞 학생의
    // 모델로 계속 돈다(학생 명령이 넷을 함께 쓰는 것과 같은 이유).
    let chars = kasa_mcp::character::characters_json();
    let pick = |f: fn(&serde_json::Value, &str) -> Option<String>| {
        chars
            .as_ref()
            .and_then(|c| f(c, character))
            .unwrap_or_default()
    };
    let _ = std::fs::write(
        base.with_extension("model"),
        pick(kasa_mcp::character::model_for),
    );
    let _ = std::fs::write(
        base.with_extension("backend"),
        pick(kasa_mcp::character::backend_for),
    );
}

/// 학생 교체가 **언제 묻고 언제 그냥 바꾸는가**.
///
/// 2026-08-25 지시: 「그럼 새로띄우게해 테마 바꾸면 확인버튼도 만들고 근데 말투
/// 오프돼있으면 그냥 껍데기만바뀌게」. 말투가 꺼진 채로 카드가 뜨면 되띄울 이유가
/// 없는 재시작을 묻는 셈이고, 켜진 채로 안 뜨면 대화가 말없이 끊긴다.
#[cfg(test)]
mod character_swap_plan_tests {
    use super::{character_swap_confirm_text, plan_character_swap, SwapPlan};

    #[test]
    fn persona_off_swaps_the_shell_without_asking() {
        assert_eq!(
            plan_character_swap(false, Some("claude"), true),
            SwapPlan::Now
        );
    }

    #[test]
    fn a_shell_pane_has_nothing_to_relaunch() {
        assert_eq!(plan_character_swap(true, None, false), SwapPlan::Now);
    }

    #[test]
    fn a_running_agent_with_persona_on_is_asked() {
        assert_eq!(
            plan_character_swap(true, Some("claude"), true),
            SwapPlan::Ask { resumable: true }
        );
    }

    /// 대화가 없어도 **묻기는 한다** — 되띄우면 지금 내용을 잃으므로 오히려 더
    /// 물어야 하는 쪽이다. 카드가 그 사실을 문구와 버튼 색으로 알린다.
    #[test]
    fn no_transcript_still_asks_but_flags_the_loss() {
        assert_eq!(
            plan_character_swap(true, Some("claude"), false),
            SwapPlan::Ask { resumable: false }
        );
        let (_, lines) = character_swap_confirm_text("은랑", false);
        assert!(
            lines.iter().any(|l| l.contains("사라집니다")),
            "대화를 잃는다는 사실이 카드에 안 적힌다"
        );
        // 이어붙일 대화가 있을 때는 반대로 「그대로」임을 말해야 한다.
        let (title, ok) = character_swap_confirm_text("은랑", true);
        assert!(title.contains("은랑으로"), "조사가 어긋난다: {title}");
        // 받침 없는 이름과 ㄹ 받침은 「로」다.
        assert!(character_swap_confirm_text("미도리", true)
            .0
            .contains("미도리로"));
        assert!(character_swap_confirm_text("하치와레", true)
            .0
            .contains("하치와레로"));
        assert!(character_swap_confirm_text("페이몬", true)
            .0
            .contains("페이몬으로"));
        assert!(ok.iter().any(|l| l.contains("그대로")));
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn mixed_character_picks_replace_conflicts_without_changing_other_choices() {
        use super::updated_character_picks;
        fn roster(theme: &str) -> Option<serde_json::Value> {
            Some(if theme == "a" {
                serde_json::json!({"members":[{"name":"Alice","slug":"alice"},{"name":"Shared","slug":"shared"},{"name":"Old art","slug":"same-art"}]})
            } else {
                serde_json::json!({"members":[{"name":"Bob","slug":"bob"},{"name":"Shared","slug":"shared"},{"name":"New art","slug":"same-art"}]})
            })
        }
        let old = vec![("a".into(), vec!["Alice".into(), "Shared".into(), "Old art".into()])];
        let picks = updated_character_picks(old.clone(), "b", "Bob", true, roster).unwrap();
        assert_eq!(picks[0], old[0]);
        let picks = updated_character_picks(picks, "b", "Shared", true, roster).unwrap();
        assert!(!picks[0].1.contains(&"Shared".into()));
        let picks = updated_character_picks(picks, "b", "New art", true, roster).unwrap();
        assert_eq!(picks[0].1, vec!["Alice"]);
        let picks = updated_character_picks(picks, "a", "Alice", false, roster).unwrap();
        assert_eq!(picks.len(), 1);
        assert_eq!(picks[0].0, "b");
        let picks = updated_character_picks(picks, "a", "Alice", false, roster).unwrap();
        assert_eq!(picks.len(), 1, "an unselected collection must not become selected on deselection");
        assert!(updated_character_picks(vec![("b".into(), vec!["Bob".into()])], "b", "Bob", false, roster).is_err());
        let fallback = updated_character_picks(vec![], "a", "Alice", false, roster).unwrap();
        assert_eq!(fallback[0].1, vec!["Shared", "Old art"]);
    }

    #[test]
    fn resume_keeps_the_sessions_student_unless_the_pane_was_reassigned() {
        use super::pane_pick_wins;
        assert!(!pane_pick_wins(false, true), "자동 배정 pane 이 남의 대화를 이으면 대화의 학생이 이긴다");
        assert!(pane_pick_wins(true, true), "사람이 재배정한 pane 은 pane 이 이긴다");
        assert!(pane_pick_wins(false, false), "바인딩된 학생이 명단에 없으면 pane 이 이긴다");
    }
}
