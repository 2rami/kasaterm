//! 사이드바·git col·파일트리 토글·패널·줌/폰트·toast 등 chrome UI 메서드.
use super::*;

/// 기둥 하나가 지금 폭에서 **무엇까지 보여 줄 수 있나**. 폭을 줄이는 것만으로는
/// 반응형이 안 된다 — 넓을 때 쓰던 글자가 좁은 칼럼에 그대로 남으면 잘리거나
/// 겹쳐서, 좁아진 게 아니라 고장난 것으로 보인다. 각 렌더가 이 단계를 물어보고
/// 자기 내용을 그 폭에 맞게 덜어낸다.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Density {
    /// 넓다 — 원래 설계대로 전부.
    Full,
    /// 좁다 — 부차적인 것(경로·부제·여백)을 덜고 핵심만.
    Compact,
    /// 아주 좁다 — 글자를 포기하고 아이콘·기호로만.
    Icon,
}

impl Density {
    /// 폭과 두 문턱으로 단계를 고른다. 문턱은 «이 폭이면 이만큼은 읽힌다» 는
    /// 기준이라 기둥마다 다르다(글자 크기·들여쓰기·아이콘 자리가 달라서).
    pub(crate) fn of(w: f32, full: f32, compact: f32) -> Self {
        if w >= full {
            Density::Full
        } else if w >= compact {
            Density::Compact
        } else {
            Density::Icon
        }
    }
    pub(crate) fn is_icon(self) -> bool {
        matches!(self, Density::Icon)
    }
    pub(crate) fn at_least_compact(self) -> bool {
        !self.is_icon()
    }
}

/// How long a completion notification pulses a pane header / sidebar done-dot.
const NOTIFY_FLASH_MS: u128 = 1800;

/// 계정이 바뀐 순간 계정 칩 둘레가 반짝이는 시간. 짧게 두는 이유는 이것이 「무슨
/// 일이 있었다」를 알리는 것이지 읽을 정보가 아니어서다 — 눈이 그리로 한 번 가면
/// 목적은 끝난다(거노 2026-08-25 "바뀐지도 잘모르겠는데").
const ACCOUNT_FLASH_MS: u128 = 900;

/// 위 진행도의 자유함수판. 렌더가 `g`(=`self.gpu`)를 대여한 채로 불러야 해서
/// `&self` 메서드로는 안 된다 — 필드 하나만 받으면 disjoint borrow 로 통과한다.
pub(crate) fn account_flash_k(at: Option<std::time::Instant>) -> Option<f32> {
    at.and_then(|t| {
        let age = t.elapsed().as_millis();
        (age < ACCOUNT_FLASH_MS).then(|| 1.0 - age as f32 / ACCOUNT_FLASH_MS as f32)
    })
}

/// 권한 대기 토스트 — 완료 토스트와 같은 원칙(캐릭터 고정값 + hook reason,
/// 미현존이면 reason 만). Notification hook 경로.
fn format_attention_toast(character: Option<&str>, reason: &str) -> String {
    let reason = reason.trim();
    match (character, reason.is_empty()) {
        (Some(c), true) => format!("⚠ {c} — 권한 대기중"),
        (Some(c), false) => format!("⚠ {c} — {reason}"),
        (None, true) => "⚠ 권한 대기중".to_string(),
        (None, false) => format!("⚠ 권한 대기중 — {reason}"),
    }
}

/// 오토메모리 볼트 폴더. `KASATERM_MEMORY_DIR` 이 이기고, 없으면 알려진 자리를
/// 차례로 본다 — 구글 드라이브 마운트는 시스템 언어에 따라 이름이 갈린다(한국어
/// 「내 드라이브」/영어 「My Drive」). 어느 것도 없으면 None 이고, 그러면 빠른 파일에
/// 그 줄이 아예 안 선다(없는 파일을 걸어 두면 눌렀을 때 빈 편집기가 열린다).
fn memory_vault_dir() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("KASATERM_MEMORY_DIR") {
        let p = std::path::PathBuf::from(p);
        if p.is_dir() {
            return Some(p);
        }
    }
    let home = kasa_socket::home_dir()?;
    ["내 드라이브/MEMORY", "My Drive/MEMORY", "MEMORY"]
        .iter()
        .map(|s| home.join(s))
        .find(|p| p.is_dir())
}

impl App {
    /// pane 의 표시용 학생 — "터미널은 파싱만"(거노): claude sessionId 바인딩이 정본,
    /// agents/attach 뷰 pane 은 파싱 전 스폰 랜덤(ws.pane_character)을 보여주지 않는다
    /// (거노: 세션 진입 직후 다른 학생으로 보임 — 뷰 pane 의 로컬 배정은 무의미한 잔재
    /// 라 None 으로 두면 학생 시각 요소가 중립으로 남는다). 일반 pane 은 스폰 배정
    /// 폴백 유지(첫 프레임부터 학생 표시). render 의 프사·타이틀바·테두리가 공유한다.
    ///
    /// **탭 접기는 여기서 한다** — 부르는 쪽은 BSP leaf(outer)를 들고 있는데 학생 상태는
    /// 탭 pid 로 기록된다. 접지 않으면 탭으로 띄운 학생이 화면에 아예 안 나온다.
    ///
    /// **「에이전트가 실제로 도는가」 관문은 여기서 진다**(2026-08-22). 예전엔 이
    /// 함수가 날것을 돌려주고 **그리는 쪽이 각자 막았다** — 배정은 spawn 때 *모든*
    /// pane 에 되므로(`assign_character_env`) 안 막은 자리는 셸에도 학생이 붙었고,
    /// 그 병을 호출부에서 한 곳씩 고쳐 왔다(`5c761f6` 이 헤더·되살리기·알림 셋).
    /// 그런데 **새 호출부가 생길 때마다 같은 것을 또 밟는다** — 전수로 세어 보니
    /// 아홉 자리 중 여섯이 각자 관문을 걸고 있었고, 안 건 둘은 실제로 새고 있었다
    /// (별도창 헤더는 **바로 옆에서 `pane_accent`(관문 有)를 부르면서** 이름만
    /// 관문 없이 불러, 같은 pane 이 이름은 있고 색은 없었다). 기본값이 반대였다.
    ///
    /// 그래서 관문을 이 안으로 들였다. 이제 **덜 안전한 쪽을 쓰려면 그렇게 쓰겠다고
    /// 적어야 한다** — 잊어서 새는 일이 안 생긴다.
    ///
    /// ⚠️PTY 가 **아예 없는** 자리는 통과시킨다. 「돌지 않는다」와 「알 수 없다」는
    /// 다르고, 후자까지 막으면 되살리기 목록처럼 PTY 가 이미 사라진 화면에서 이름이
    /// 통째로 없어진다.
    pub(crate) fn display_pane_char(&self, ws: &Workspace, id: &str) -> Option<String> {
        self.display_tab_char(ws, &ws.active_tab_pid(id))
    }

    /// 그 **탭 하나**의 학생. 위가 활성 탭으로 접어 부르는 겉옷이고 이쪽이 알맹이다.
    /// 카드 덱이 장마다 다른 사람을 말하려면 접기 **전** 값이 필요하다(2026-08-24
    /// 지시: 탭 겹친 pane 에서 「어떤 학생인지 모른다」). 관문은 같다 — claude 가
    /// 실제로 도는 탭만 학생을 갖는다.
    pub(crate) fn display_tab_char(&self, ws: &Workspace, tab: &str) -> Option<String> {
        if let Some(p) = self.pty.get(tab) {
            p.active_agent()?;
        }
        // claude agents 목록 뷰는 학생을 안 그린다(옛 or_else 폴백의 view 가드).
        if self.pty.get(tab).map(|p| p.is_claude_agents()).unwrap_or(false) {
            return None;
        }
        // 현재 배정(`ws.pane_character`)이 정본이다 — 미니맵·목록이 읽는 값과 같다.
        // 예전엔 `session_character(sid)` 를 우선했는데, 재배정·테마전환은 pane_character
        // 만 갱신하고 `session_characters.json` 의 옛 claude-stem 바인딩은 안 지워서,
        // 그걸 우선하면 재배정 전 캐릭터(옛 테마)가 얼굴·이름으로 되살아났다(거노 실측:
        // 배정은 히후미인데 info 는 고블린). pane 이 살아 있는 한 pane_character 를 믿고,
        // 그것이 빈(복원 직후 아직 미배정) 순간에만 세션 바인딩으로 되짚는다.
        ws.pane_character
            .get(tab)
            .cloned()
            .filter(|c| !c.is_empty())
            .or_else(|| {
                self.pane_claude_sid
                    .get(tab)
                    .and_then(|sid| kasa_mcp::character::session_character(sid))
            })
    }

    /// pane 에 **학생색을 입힐지**의 정본. 이름(`display_pane_char`)과 달리
    /// 「지금 에이전트가 도는가」 관문을 지난다 — 순수 셸 pane 에 남의 학생색이
    /// 둘리면 「저기 누가 있다」로 잘못 읽히기 때문이고, 메인 그리드의 pane 테두리가
    /// 이미 그 규칙이다(`render.rs` 의 `claude_panes` 필터).
    ///
    /// 별도창(터미널·방)이 이걸 안 쓰고 `ws.pane_character` 를 날로 읽던 동안,
    /// 같은 pane 이 창마다 다른 대접을 받았다 — 셸 pane 이 별도창에선 학생색·이름을
    /// 달고 메인에선 무채색이라, 되돌리면 「학생 테마가 깨졌다」로 보였다(거노).
    /// 관문은 이름과 같은 키(**탭 pid**)로 본다 — 탭에서 도는 학생을 놓치지 않게.
    pub(crate) fn pane_accent(&self, ws: &Workspace, id: &str) -> Option<[u8; 4]> {
        let tab = ws.active_tab_pid(id);
        self.pty.get(tab.as_str()).and_then(|p| p.active_agent())?;
        let name = self.display_pane_char(ws, id)?;
        crate::theme::character_accent_n(&name, crate::theme::character_ordinal(&ws.pane_character, id))
    }

    /// 헤더 탭에 실을 제목들. **메인 그리드와 별도창이 같은 이 함수를 지난다** —
    /// 두 벌로 두면 별도창 탭만 OSC 날제목(`✳ Claude Code`)으로 돌아간다.
    pub(crate) fn pane_tab_labels(
        &self,
        ws: &Workspace,
        id: &str,
        pane: &PaneState,
    ) -> Vec<String> {
        // 단일 탭 + 배정된 학생이면 탭 제목을 비운다 — render 의 tab_list
        // 폴백(h.tabs.is_empty → h.label)이 character label("미도리 · 작업명")
        // 을 헤더에 그리게(거노: 탭 제목이 학생 이름을 덮어쓰던 버그). 멀티탭/
        // 비배정 pane 은 기존대로 탭별 제목.
        if pane.tabs.len() <= 1
            && pane.character.as_deref().is_some_and(|c| !c.is_empty())
        {
            Vec::new()
        } else {
            pane.tabs
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    // 탭 이름도 헤더와 같은 규칙으로 짓는다 — 여기만
                    // OSC 제목을 **날것 그대로** 실어, claude 탭이
                    // `✳ Claude Code` 로 떴다(거노 2026-08-21: "탭 안에
                    // 있을 때 claude code 랑 무슨 유니코드 이모지 나오는데
                    // 그것도 이쁘게"). 헤더는 진작 학생 이름을 쓰고 있어서
                    // **같은 pane 인데 헤더와 탭이 서로 다른 것을 부르는**
                    // 상태이기도 했다.
                    let sess =
                        t.pid.as_deref().and_then(|p| self.pty.get(p));
                    // 작업명 — 스피너·별표 접두를 벗긴다. 벗기는 일은
                    // `strip_activity_prefix` 계약대로 **접두만**이고,
                    // 「Claude Code」 같은 기본 제목을 따로 거르지는 않는다:
                    // 그건 claude 쪽 문구라 바뀌면 조용히 어긋난다.
                    let task = t
                        .title
                        .as_deref()
                        .map(|s| crate::strip_activity_prefix(s).trim().to_string())
                        .filter(|s| !s.is_empty());
                    // 학생 관문은 헤더(`true_char` + `runs_claude`)와 같은
                    // 것을 **탭 pid 로** 묻는다. pane 이 아니라 탭마다
                    // 물어야 하는 건, 한 pane 의 탭들이 각각 다른 학생일
                    // 수 있어서다.
                    let student = sess
                        .filter(|s| s.active_agent().is_some())
                        .and(t.pid.as_deref())
                        .and_then(|p| self.display_pane_char(ws, p));
                    let name = match (student, task) {
                        // pane id 는 안 붙인다 — 헤더가 이미 들고 있고,
                        // 한 pane 의 탭끼리는 그 값이 전부 같아 구분에
                        // 보탬이 안 되면서 좁은 자리만 먹는다.
                        (Some(c), Some(t)) => format!("{c} · {t}"),
                        (Some(c), None) => c,
                        (None, Some(t)) => t,
                        // 각 탭의 pid로 스마트 라벨(셸=cwd, 명령=프로세스).
                        (None, None) => sess
                            .and_then(|s| Self::smart_pane_label(s))
                            .unwrap_or_else(|| {
                                if i == 0 {
                                    id.to_string()
                                } else {
                                    format!("탭 {}", i + 1)
                                }
                            }),
                    };
                    // 탭별 ● 미저장 도트 — 멀티탭 pane 에서 어느
                    // 파일이 저장 안 됐는지 탭 단위로 보이게.
                    if t.markdown().map_or(false, |m| m.modified) {
                        format!("● {name}")
                    } else {
                        name
                    }
                })
                .collect()
        }
    }

    /// A pane's claude finished (Stop hook → `kasaterm-cli notify` → socket →
    /// `UserEvent::Notify`). Flash the pane's header and, unless the user is
    /// already looking at that exact pane (our window focused + it's the
    /// active pane), raise a desktop alert. cmux-style suppression keeps the
    /// alert for the cases that actually need attention (background window or
    /// a sibling pane).
    pub(crate) fn handle_notify(&mut self, surface_id: &str, title: &str, body: &str) {
        let now = std::time::Instant::now();
        let is_active_pane = self
            .ws
            .lock()
            .unwrap()
            .active_pane
            .as_deref()
            == Some(surface_id);
        // claude's Stop hook fired → this pane's turn is DONE. Trust this push
        // over the glyph heuristic: force the pane idle right now so the working
        // bar can't linger on a stale "✻ Churned for 42s" line, and drop the
        // busy-grace timer. The glyph working→idle path in
        // `refresh_pane_activity` then sees the pane is already idle.
        //
        // 완료 화면 토스트는 제거(거노 2026-07-27) — 학생이 많아 턴마다 떠서 시야를
        // 가린다. 완료 신호는 탭 펄스·dock 배지·백그라운드 데스크톱 알림(아래)으로
        // 전달된다.
        self.pane_last_busy.remove(surface_id);
        self.pane_activity
            .entry(surface_id.to_string())
            .and_modify(|a| a.status = "idle".to_string())
            .or_insert_with(|| crate::stream::PaneStatusView {
                status: "idle".to_string(),
                ..Default::default()
            });
        self.notify_flash.insert(surface_id.to_string(), now);
        // 턴이 끝났다 = 사용량이 방금 움직였다. 폴러를 깨워 남은 잠을 건너뛰게 한다
        // (`usage_poke`) — 그러지 않으면 방금 쓴 몫이 최대 1분 뒤에야 표에 뜬다.
        crate::handler::usage_poke().store(true, std::sync::atomic::Ordering::Relaxed);
        // A pane in a *background* window finished — pulse that window's sidebar
        // tab until the user switches to it (switch_window clears the entry).
        if let Some(wi) = self.window_of_pane(surface_id) {
            if wi != self.active_window {
                self.window_alert.insert(wi);
            }
        }
        self.chrome_dirty = true;
        // 읽음 처리(=dock 배지)만 지금 보고 있는 pane 을 뺀다. 데스크톱 알림 자체는
        // 그 pane 을 보고 있어도 쏜다 — 거노 2026-08-11 "pane별로 그냥 다오게하자".
        // 학생이 여럿이면 어느 창을 보고 있든 나머지가 끝난 걸 놓치는 쪽이 손해다.
        if !(self.window_focused && is_active_pane) {
            self.unread_panes.insert(surface_id.to_string());
        }
        let who = self.pane_character_if_known(surface_id);
        // 누구 알림인지는 프사(오른쪽 썸네일)와 제목 둘 다로 말한다 — 캐릭터가 없는
        // 순정 pane 은 프사가 안 붙으므로 제목만 남는다.
        let titled = match who.as_deref() {
            Some(c) => format!("{c} · {title}"),
            None => title.to_string(),
        };
        // 완료는 열쇠를 안 준다 — 턴마다 정당하게 떠야 하고, 학생이 여럿이면 서로
        // 다른 pane 의 완료가 같은 창 안에 겹치는 게 정상이다.
        let sid = self.pane_claude_sid.get(surface_id).cloned();
        notify_desktop(
            &titled,
            body,
            who.as_deref(),
            None,
            Some((surface_id, sid.as_deref())),
        );
        // 배너는 `notify_desktop` 이 줄 세운다 — 한때 여기서 따로 push 했는데,
        // 그러면 **배너가 서는 자리와 안 서는 자리가 갈린다.** 실제로 승인 대기·
        // 계정 한도는 `notify_desktop` 만 부르고 있어서, OS 알림을 끄는 순간 그
        // 둘은 알림이 통째로 사라질 참이었다(2026-08-21 실측). 발사구를 하나로
        // 두면 중복 방지 열쇠(`dedup`)도 배너에 그대로 걸린다.
    }

    /// A pane's claude is blocked on a permission / input prompt (its
    /// `Notification` hook → `kasaterm-cli attention` → `UserEvent::Attention`).
    /// Toast + pulse the pane, and unless the user is already looking at that
    /// exact pane (our window focused + it's the active pane), raise a desktop
    /// alert. Same suppression as `handle_notify`, but this is the *attention*
    /// case — the agent isn't done, it's stuck waiting on you. The board's
    /// `waiting` flag is set separately in `collab_board` (socket thread, off
    /// the shared attention map); here we only own the GUI-side surfacing.
    pub(crate) fn handle_attention(&mut self, surface_id: &str, reason: &str) {
        let now = std::time::Instant::now();
        let is_active_pane = self.ws.lock().unwrap().active_pane.as_deref() == Some(surface_id);
        // 캐릭터명(pane 고정) + hook reason(완료 순간). OSC 작업명은 안 쓴다.
        let character = self.pane_character_if_known(surface_id);
        let reason = reason.trim();
        self.notify_flash.insert(surface_id.to_string(), now);
        // Attention raised in a background window — pulse its sidebar tab too.
        if let Some(wi) = self.window_of_pane(surface_id) {
            if wi != self.active_window {
                self.window_alert.insert(wi);
            }
        }
        self.chrome_dirty = true;
        // 이미 sticky 승인 토스트(칩 포함)가 이 pane 으로 떠 있으면 hook 의
        // 중복 알림으로 텍스트를 덮지 않는다.
        if self.collab.toast_action.as_deref() != Some(surface_id) {
            self.collab.toast =
                Some((format_attention_toast(character.as_deref(), reason), now));
            self.collab.toast_rect = None;
        }
        if !(self.window_focused && is_active_pane) {
            self.unread_panes.insert(surface_id.to_string());
        }
        // 완료 알림과 같은 이유로 억제하지 않는다 — 막혀 선 학생은 더더욱 놓치면
        // 안 되는 쪽이다(그 pane 을 보고 있었다면 어차피 화면에도 토스트가 떠 있다).
        let who = character.as_deref().unwrap_or("pane");
        let body = if reason.is_empty() {
            who.to_string()
        } else {
            format!("{who} — {reason}")
        };
        // 화면 감지 경로(`input.rs` 의 `⚠ 승인 필요`)와 **같은 열쇠**를 쓴다 — 승인
        // 프롬프트 하나에 배너가 둘 나가던 것을 여기서 하나로 만든다. 훅이 먼저 오면
        // reason 이 실린 이쪽이 이기고, 화면 감지가 뒤따라 와도 조용히 접힌다.
        let sid = self.pane_claude_sid.get(surface_id).cloned();
        notify_desktop(
            "⚠ 권한 필요",
            &body,
            character.as_deref(),
            Some(&format!("approval:{surface_id}")),
            Some((surface_id, sid.as_deref())),
        );
    }

    /// pane 이 현존하고 캐릭터가 배정됐으면 그 이름(고정값) — 토스트 "누가" 소스.
    /// 미현존(resume/재사용으로 surface_id 가 stale)이거나 순정 pane 이면 None →
    /// 호출부는 hook 정보만으로 폴백(토스트를 드롭하지 않는다).
    pub(crate) fn pane_character_if_known(&self, id: &str) -> Option<String> {
        let ws = self.ws.lock().unwrap();
        let key = ws.active_tab_pid(id);
        // 현존만으로는 모자란다 — 캐릭터는 spawn 때 **모든** pane 에 배정되므로
        // (`assign_character_env`) 셸 pane 도 이름을 물고 나온다. 위 주석의 「순정
        // pane 이면 None」 이 실제로 성립하려면 에이전트 관문이 함께 있어야 한다.
        let known = (ws.panes.contains_key(id) || self.pty.contains_key(id))
            && (self.pane_claude_sid.contains_key(key.as_str())
                || self.pty.get(key.as_str()).and_then(|p| p.active_agent()).is_some());
        known.then(|| ws.pane_character.get(&key).cloned()).flatten()
    }

    /// claude 가 떠 있는 pane 을 기억해 둔다 — 얼굴을 내보일 자격.
    ///
    /// pane 이 사라지면 같이 잊는다. surface id 는 재사용되므로, 안 지우면 새로 난
    /// 셸 pane 이 남의 자격을 물려받아 켜지도 않은 학생을 달고 뜬다.
    pub(crate) fn note_claude_panes(&mut self) {
        // 판정은 **OSC 제목** 이 먼저다. claude 는 뜨자마자 「✳ Claude Code」를
        // 보내는데, 프로세스 이름 쪽은 셸의 직계 자식을 500ms 캐시로 훑는 경로라
        // 헤드리스 실측에서 claude 가 떠 있는데도 계속 `zsh` 를 돌려줬다.
        let seen: Vec<String> = self
            .pty
            .iter()
            .filter(|(_, p)| {
                p.osc_title()
                    .is_some_and(|t| t.contains("Claude") || t.contains("Codex"))
                    || p.active_agent().is_some()
            })
            .map(|(id, _)| id.clone())
            .collect();
        self.pane_claude_seen.extend(seen);
        self.pane_claude_seen.retain(|id| self.pty.contains_key(id));
    }

    /// 밖에 나간 방을 메인으로 되돌린다 — 사이드바가 부르는 쪽 입구.
    ///
    /// `dock_window_room` 은 **aux 창 인덱스**를 받는데 사이드바는 방 인덱스로만
    /// 말한다. 그 둘을 그대로 넘기면 엉뚱한 창이 닫히므로 여기서 한 번 옮긴다.
    pub(crate) fn dock_room_back(&mut self, win: usize) {
        let Some(aux) =
            self.aux_windows.iter().position(|a| a.room_window() == Some(win))
        else {
            return;
        };
        self.dock_window_room(aux);
        self.chrome_dirty = true;
    }

    /// 이 pane 에 학생 얼굴을 내보여도 되나 — claude 를 한 번이라도 띄웠는가.
    ///
    /// **탭을 접는다.** 자격은 `note_claude_panes` 가 `self.pty` 를 훑어 넣으므로
    /// 키가 **PTY id(=pid)** 인데, 부르는 쪽(사이드바 줄·미니맵 칸)은 **BSP leaf** 를
    /// 든다. 탭으로 띄운 학생은 그 둘이 달라 조회가 영영 빗나갔고, 그래서 탭 안의
    /// 학생은 미니맵에 얼굴이 아예 안 떴다(거노 2026-08-20 「탭 안에 소환돼서 꺼내면
    /// 미니맵에 학생 표시 없는 버그」). 같은 병을 `display_pane_char`·`pane_accent`
    /// 는 이미 접어서 피하고 있었다 — 접는 자리와 안 접는 자리가 갈려 한 pane 이
    /// 화면 자리마다 다른 얼굴을 갖던 계열의 마지막 하나다.
    ///
    /// leaf 를 **먼저** 보는 건 탭이 없는 보통 pane 에서 ws 락을 아예 안 잡기
    /// 위해서다(매 프레임 pane 수만큼 불린다).
    pub(crate) fn pane_claude_ready(&self, id: &str) -> bool {
        if self.pane_claude_seen.contains(id) {
            return true;
        }
        let tab = self.ws.lock().unwrap().active_tab_pid(id);
        tab != id && self.pane_claude_seen.contains(tab.as_str())
    }

    /// Drop the focused pane from the unread set (the user is now looking at
    /// it) and push the count to the Dock badge when it changes. Called every
    /// tick from `about_to_wait` — cheap unless the count actually moves.
    pub(crate) fn sync_dock_badge(&mut self) {
        if self.window_focused {
            if let Some(active) = self.ws.lock().unwrap().active_pane.clone() {
                self.unread_panes.remove(&active);
            }
        }
        let n = self.unread_panes.len();
        if n != self.dock_badge_n {
            self.dock_badge_n = n;
            set_dock_badge(n);
        }
    }

    /// Flash strength (1.0 → 0.0) for `id`'s completion pulse, or `None` when
    /// it isn't flashing. Drives the header pulse and the sidebar done-dot;
    /// both fade over `NOTIFY_FLASH_MS`.
    /// pane 하나의 상태를 색 하나로. 사이드바가 방을 열지 않고도 "누가 나를
    /// 기다리는지"를 말하는 근거다 — 대기가 먼저다: 작업 중은 놔두면 끝나지만
    /// 대기는 내가 손대야 풀리므로, 둘이 겹치면 급한 쪽을 보여야 한다.
    pub(crate) fn pane_state_color(&self, id: &str) -> [u8; 4] {
        let st = self.pane_activity.get(id);
        // 끊김이 가장 먼저다 — 멈춘 pane 은 스피너가 없어 idle 로 보이므로, 아래
        // 어느 갈래에도 안 걸려 「조용한 정상」과 똑같이 회색으로 앉는다. 그게 이
        // 표시를 만든 이유다(2026-08-26 지시).
        if st.is_some_and(|a| a.stalled.is_some()) {
            theme::danger()
        } else if st.is_some_and(|a| status_needs_you(&a.status)) {
            theme::attention()
        } else if self.notify_flash_factor(id).is_some() {
            theme::success()
        } else if st.is_some_and(|a| a.status != "idle" && !a.status.is_empty())
            // 파란 점은 **지금 보고 있는 pane** 에만 준다. 작업 중인 pane 이 여럿이면
            // 목록이 온통 파래져 정작 손이 필요한 빨강·끝난 초록이 묻혔다 — 남의
            // 진행은 배너 바가 이미 말하고 있으니 여기서 한 번 더 외칠 자리가 아니다.
            && self.ws.lock().unwrap().active_pane.as_deref() == Some(id)
        {
            theme::accent()
        } else {
            theme::with_alpha(theme::text_mute(), 0x66)
        }
    }
    /// 그 pane 이 **도는 중**인가 — 헤더 진행 바와 사이드바 걷기가 같이 쓴다.
    ///
    /// 기다리는 중(`waiting`·`blocked`)은 도는 게 아니다. 사람 답을 기다리는데 바가
    /// 계속 차오르면 "일하는 줄" 알고 지나치게 된다(거노 2026-08-11: "프로세스바
    /// 제대로 안되는거"). 사이드바는 이미 그걸 갈라 놨는데 헤더만 안 갈려 있었다 —
    /// 같은 판정이 두 벌이면 한쪽만 고쳐진다.
    pub(crate) fn pane_is_busy(&self, id: &str) -> bool {
        self.pane_activity
            .get(id)
            .is_some_and(|a| !a.status.is_empty() && a.status != "idle" && !status_needs_you(&a.status))
    }

    /// 그 pane 이 **내 손을 기다리는 중**인가 — 승인 프롬프트든 질문이든.
    pub(crate) fn pane_needs_you(&self, id: &str) -> bool {
        self.pane_activity
            .get(id)
            .is_some_and(|a| status_needs_you(&a.status))
    }

    /// 계정 전환 반짝임의 진행도 `1.0 → 0.0`. 끝났으면 `None` — 호출부가 그걸로
    /// 그리기와 프레임 펌프를 함께 끊는다.
    pub(crate) fn account_flash_factor(&self) -> Option<f32> {
        account_flash_k(self.account_flash)
    }

    pub(crate) fn notify_flash_factor(&self, id: &str) -> Option<f32> {
        self.notify_flash.get(id).and_then(|t| {
            let age = t.elapsed().as_millis();
            (age < NOTIFY_FLASH_MS).then(|| 1.0 - age as f32 / NOTIFY_FLASH_MS as f32)
        })
    }

    /// Whether any pane is mid-flash — `about_to_wait` pumps ~30fps frames
    /// while this is true so the pulse fades smoothly instead of freezing on
    /// the last painted frame.
    pub(crate) fn any_notify_flash(&self) -> bool {
        self.notify_flash
            .values()
            .any(|t| t.elapsed().as_millis() < NOTIFY_FLASH_MS)
    }

    /// Sidebar width that layout math should actually use: the full
    /// `SIDEBAR_W` when the strip is shown, 0 when collapsed. Every
    /// origin_x / window_cells / hit-test calc routes through here so a
    /// single `sidebar_visible` flip reflows the whole grid.
    pub(crate) fn effective_sidebar_w(&self) -> f32 {
        self.tab_strip_w() + self.file_tree_col_w()
    }

    /// 세 기둥이 **원하는** 폭(사용자가 드래그로 정한 값). 접혀 있으면 0.
    /// `chrome_widths` 만 이걸 쓴다 — 바깥에서 이 값을 그리기에 쓰면 창이 좁을 때
    /// 예산을 건너뛰고 겹쳐 그리게 된다.
    fn chrome_wants(&self) -> (f32, f32, f32) {
        (
            if self.sidebar_visible && !self.tabs_on_top { self.sidebar_w_logical } else { 0.0 },
            if self.file_tree.visible { self.file_tree.w_logical } else { 0.0 },
            if self.git.col_visible { self.git.col_w_logical } else { 0.0 },
        )
    }

    /// 이번 프레임에 세 기둥이 **실제로** 차지할 폭 `(탭 스트립, 파일트리, 우측 칼럼)`.
    ///
    /// 원하는 폭의 합이 창에 들어가면 그대로 준다. 넘치면 터미널 몫(`GRID_KEEP_COLS`)을
    /// 먼저 떼어 두고, 남는 것을 **우선순위 역순**(우측 칼럼 → 파일트리 → 탭 스트립)으로
    /// 하한까지 깎는다. 그래도 넘치는 창이면 같은 순서로 접는다.
    ///
    /// 깎는 것은 **여기서 돌려주는 값뿐이고 `*_w_logical` 은 안 건드린다** — 그래서 창을
    /// 도로 넓히면 사용자가 정한 폭이 그대로 돌아온다. 예산을 필드에 써 버리면 창을 한 번
    /// 줄였다 늘린 사람은 자기가 맞춰 둔 폭을 영영 잃는다.
    ///
    /// 순서가 우측 칼럼부터인 이유: 셋 중 가장 넓고(420) 참조용이라, 같은 픽셀을 내놓을 때
    /// 잃는 것이 가장 적다. 탭 스트립이 마지막인 건 그게 방을 오가는 유일한 손잡이여서다.
    pub(crate) fn chrome_widths(&self) -> (f32, f32, f32) {
        let (mut tab, mut tree, mut git) = self.chrome_wants();
        let Some(win) = self.window.as_ref() else {
            return (tab, tree, git);
        };
        let win_w = win.inner_size().width as f32 / self.effective_scale();
        if win_w <= 1.0 {
            return (tab, tree, git);
        }
        let keep = GRID_KEEP_COLS * self.cell.w + 2.0 * WINDOW_PADDING;
        let mut over = tab + tree + git - (win_w - keep);
        if over <= 0.0 {
            return (tab, tree, git);
        }
        for (w, floor) in [
            (&mut git, GIT_COL_W_AUTO_MIN),
            (&mut tree, FILE_TREE_W_AUTO_MIN),
            (&mut tab, SIDEBAR_W_AUTO_MIN),
        ] {
            if over <= 0.0 {
                break;
            }
            if *w <= 0.0 {
                continue;
            }
            let give = (*w - floor).max(0.0).min(over);
            *w -= give;
            over -= give;
        }
        // 하한까지 밀고도 안 들어가는 창 — 여기서는 뭔가를 지워야 터미널이 성립한다.
        // 하한 폭의 기둥을 억지로 남기면 셋 다 읽을 수 없는 채로 그리드만 죽는다.
        for w in [&mut git, &mut tree, &mut tab] {
            if over <= 0.0 {
                break;
            }
            if *w <= 0.0 {
                continue;
            }
            over -= *w;
            *w = 0.0;
        }
        (tab, tree, git)
    }

    /// Width of the session-tab strip alone (0 when collapsed). With top tabs
    /// the strip never opens — the tabs live in the title bar instead.
    pub(crate) fn tab_strip_w(&self) -> f32 {
        self.chrome_widths().0
    }

    /// pane 헤더 탭의 × 클릭. 맞혔으면 true.
    ///
    /// 메서드로 둔 건 하네스가 **같은 좌표 판정을 지나게** 하기 위함이다
    /// (`sidebar_row_right_click` 과 같은 이유). 이 동작은 "같은 좌표를 연달아
    /// 눌렀을 때 매번 다음 탭이 닫히는가" 가 전부인데, 상태를 손으로 세우는 검증은
    /// 정작 어긋나는 자리(히트렉트 재계산)를 못 본다.
    pub(crate) fn pane_tab_close_click(&mut self, cx: f32, cy: f32) -> bool {
        let Some((pid, idx)) = self
            .pane_tab_close_rects
            .iter()
            .find(|(_, _, r)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3)
            .map(|(id, i, _)| (id.clone(), *i))
        else {
            return false;
        };
        // 알약 자리를 닫기 **전에** 떠 둔다. 이 띠는 폭이 라벨 길이를 따라가서, 하나
        // 닫으면 남은 탭이 넓어지며 × 가 딴 자리로 간다 — 세로 목록과 달리 여기만
        // 기하가 실제로 어긋난다. 남은 탭이 이 슬롯을 앞에서부터 채워 × 가 방금 누른
        // 자리에 온다.
        let slots: Vec<(f32, f32)> = self
            .pane_tab_rects
            .iter()
            .filter(|(p, _, _)| *p == pid)
            .map(|(_, _, r)| (r.0, r.2))
            .collect();
        if slots.len() > 1 {
            self.freeze_closing(crate::CloseFreezeKind::Tabs(pid.clone(), slots));
        }
        // Same tab-vs-pane + "job running?" logic as Cmd+W.
        self.confirm_or_close_tab(&pid, idx);
        true
    }

    /// 닫기 동결을 한 프레임 재검사한다 — 시한이 지났거나 커서가 그 목록을 떠났으면
    /// 녹인다. 녹는 그 프레임에 목록이 정상 배치로 돌아간다(= 재정렬).
    ///
    /// 커서 판정을 렌더 직전에 두는 건 `CursorLeft` 를 안 받기 때문이다. 이벤트로
    /// 하면 창 밖으로 나간 것을 못 잡아 영영 굳는다 — `pump_info` 가 시한을 둔 것과
    /// 같은 이유이고, 여기서도 시한이 마지막 방벽이다.
    pub(crate) fn tick_close_freeze(&mut self) {
        if self.close_freeze.since.is_none() {
            return;
        }
        if !self.close_freeze.live() {
            self.close_freeze.thaw();
            return;
        }
        let (cx, cy) = self.cursor_px;
        let still_there = if self.close_freeze.sidebar_scroll.is_some() {
            let w = self.tab_strip_w();
            w > 0.0 && cx < w && cy > TITLE_HEIGHT
        } else if self.close_freeze.info_content.is_some() {
            self.info.panel_rect.is_some_and(|(x, y, w, h)| {
                cx >= x && cx < x + w && cy >= y && cy < y + h
            })
        } else if let Some((pane, _)) = self.close_freeze.tab_slots.as_ref() {
            // 탭 알약 사이 틈에서 녹지 않게 좌우로 조금 넓혀 본다. 그 pane 의 탭이
            // 하나도 안 남았으면 자연히 false 라 그때 녹는다.
            self.pane_tab_rects.iter().any(|(p, _, (x, y, w, h))| {
                p == pane && cx >= x - 10.0 && cx <= x + w + 10.0 && cy >= *y && cy <= y + h
            })
        } else {
            false
        };
        if !still_there {
            self.close_freeze.thaw();
        }
    }

    /// 닫기 동결을 건다. 어느 목록이든 한 번에 하나만 언다 — 두 목록을 동시에
    /// 닫을 일이 없고, 하나만 두면 해제 판정도 갈래가 하나다.
    pub(crate) fn freeze_closing(&mut self, what: CloseFreezeKind) {
        self.close_freeze.thaw();
        match what {
            CloseFreezeKind::Sidebar(px) => self.close_freeze.sidebar_scroll = Some(px),
            CloseFreezeKind::Info(content) => self.close_freeze.info_content = Some(content),
            CloseFreezeKind::Tabs(pane, slots) => {
                self.close_freeze.tab_slots = Some((pane, slots))
            }
        }
        self.close_freeze.since = Some(Instant::now());
    }

    /// Shift the windowed tab strip so window `idx` is visible. Called on
    /// window switch/create — a keyboard- or click-driven switch must never
    /// land on a tab scrolled out of the strip. Free wheel-scrolling is left
    /// alone otherwise (sidebar_layout only clamps, never follows).
    pub(crate) fn win_tab_reveal(&mut self, idx: usize) {
        if self.tabs_on_top {
            let vis = self.win_tab_vis.max(1);
            if idx < self.win_tab_first {
                self.win_tab_first = idx;
            } else if idx >= self.win_tab_first.saturating_add(vis) {
                self.win_tab_first = idx + 1 - vis;
            }
            return;
        }
        // 세로 사이드바는 픽셀로 흐르므로 그 방 카드의 위·아래 변을 직접 칸 안으로
        // 들인다. 이미 온전히 보이는 카드면 아무것도 안 움직인다 — 자유 굴림을
        // 되돌리지 않는 것이 이 함수의 규약이다.
        let Some(win_h) = self
            .window
            .as_ref()
            .map(|w| w.inner_size().height as f32 / self.effective_scale())
        else {
            return;
        };
        let heights = self.sidebar_card_heights();
        let Some(h) = heights.get(idx).copied() else {
            return;
        };
        let card_top = heights[..idx].iter().sum::<f32>() + SIDEBAR_TAB_GAP * idx as f32;
        let avail = self.sidebar_avail_h(win_h);
        let mut px = self.sidebar_scroll_px;
        if card_top < px {
            px = card_top;
        } else if card_top + h > px + avail {
            px = card_top + h - avail;
        }
        self.sidebar_scroll_px = px.clamp(0.0, self.sidebar_max_scroll(win_h));
    }
    /// File-tree column width (0 when hidden). Independent of the tab strip.
    pub(crate) fn file_tree_col_w(&self) -> f32 {
        self.chrome_widths().1
    }
    /// Left edge (logical x) of the file-tree column — right after the tab
    /// strip. The column sits between the tabs and the cell grid.
    pub(crate) fn file_tree_col_x(&self) -> f32 {
        self.tab_strip_w()
    }
    /// Right-hand chrome width (the git column), mirroring `effective_sidebar_w`
    /// on the left. Folded into `window_cells` so the cell grid reflows and no
    /// pane ever overlaps the column.
    pub(crate) fn effective_right_chrome_w(&self) -> f32 {
        self.git_col_w()
    }
    /// 페르소나 탭이 지금 화면에 있어야 하나 — 우측 패널이 열려 있고 그 탭이 선택된 때.
    pub(crate) fn persona_active(&self) -> bool {
        self.git.col_visible && self.info.tab == state::SideTab::Persona
    }
    /// 웹뷰를 렌더가 적어 준 본문 사각형에 맞추고, 탭이 바뀌었으면 보이기/숨기기를
    /// 뒤집는다. 드롭하지 않고 숨기는 이유는 대화 이력이 그 페이지에 있어서다 —
    /// 탭을 한 번 오갔다고 하던 얘기를 잊으면 말상대가 아니다.
    pub(crate) fn sync_persona_view(&mut self) {
        let active = self.persona_active();
        if active && self.persona.webview.is_none() {
            self.open_persona_view();
            return;
        }
        let Some(wv) = self.persona.webview.as_ref() else { return };
        if active != self.persona.shown {
            let _ = wv.set_visible(active);
            // 숨은 채로 board 를 계속 긁으면 아무도 안 보는 화면 때문에 토큰이 샌다.
            let _ = wv.evaluate_script(&format!("window.__paused = {}", !active));
            self.persona.shown = active;
        }
        if !active {
            return;
        }
        let Some(rect) = self.persona.body_rect else { return };
        if self.persona.last_rect == Some(rect) {
            return;
        }
        let _ = wv.set_bounds(wry::Rect {
            position: wry::dpi::LogicalPosition::new(rect.0 as f64, rect.1 as f64).into(),
            size: wry::dpi::LogicalSize::new(rect.2 as f64, rect.3 as f64).into(),
        });
        self.persona.last_rect = Some(rect);
    }
    /// 우측 패널의 페르소나 탭 본문을 세운다. 메인 창의 **자식** 웹뷰라 별도 OS 창이
    /// 뜨지 않는다 — 참조 배치(패널 안에 늘 서 있는 한 칸)가 그래야 성립한다.
    pub(crate) fn open_persona_view(&mut self) {
        if self.persona.webview.is_some() {
            return;
        }
        let Some(window) = self.window.clone() else { return };
        // 렌더가 아직 자리를 안 적었으면 다음 프레임에 다시 온다 — 폭이 0 인 웹뷰를
        // 세워 두면 보이지도 않고 bounds 를 다시 밀 때까지 죽은 칸이 된다.
        let Some(rect) = self.persona.body_rect else { return };
        if rect.2 <= 1.0 || rect.3 <= 1.0 {
            return;
        }
        let port = mcp_panel_port();
        // launch 별 캐시버스트 — WKWebView 가 옛 persona.html 을 물고 있으면 고친 것이
        // 화면에 안 온다(설정 웹뷰가 같은 이유로 붙인다).
        let cb = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let webview = match wry::WebViewBuilder::new()
            .with_url(format!("http://127.0.0.1:{port}/arona-ui/persona.html?v={cb}"))
            .with_background_color((16, 22, 42, 255))
            .with_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(rect.0 as f64, rect.1 as f64).into(),
                size: wry::dpi::LogicalSize::new(rect.2 as f64, rect.3 as f64).into(),
            })
            .build_as_child(window.as_ref())
        {
            Ok(wv) => wv,
            Err(e) => {
                eprintln!("[persona] webview build failed: {e}");
                return;
            }
        };
        self.persona.webview = Some(webview);
        self.persona.last_rect = Some(rect);
        self.persona.shown = true;
        window.request_redraw();
    }
    /// 이사 탭이 지금 화면에 있어야 하나 — persona_active 와 같은 판정.
    /// (본문은 웹뷰가 아니라 네이티브 렌더 — machinescol.rs.)
    pub(crate) fn machines_tab_active(&self) -> bool {
        self.git.col_visible && self.info.tab == state::SideTab::Machines
    }
    /// Git-column width (0 when hidden).
    pub(crate) fn git_col_w(&self) -> f32 {
        self.chrome_widths().2
    }
    /// Left edge (logical x) of the git column — flush against the window's
    /// right edge. 0 before the window exists (no paint yet).
    pub(crate) fn git_col_x(&self) -> f32 {
        let w = self.git_col_w();
        self.window.as_ref().map_or(0.0, |win| {
            let scale = self.effective_scale();
            win.inner_size().width as f32 / scale - w
        })
    }
    /// Whether `id`'s per-pane status bar is shown. The global default
    /// (`set_footer_default`) decides it unless this pane sits in an exception
    /// set: `shown` forces it on, `hidden` forces it off.
    pub(crate) fn statusbar_visible(&self, id: &str) -> bool {
        if self.statusbar.shown.contains(id) {
            true
        } else if self.statusbar.hidden.contains(id) {
            false
        } else {
            self.set_footer_default
        }
    }
    /// Logical-px footer band `id` reserves for its status bar — `self.pane_footer_h()`
    /// when shown, 0 when collapsed. Mirrors the header band in `resize_backend`
    /// and the render clip so the PTY grid stops exactly above the bar.
    pub(crate) fn statusbar_px(&self, id: &str) -> f32 {
        if self.statusbar_visible(id) {
            self.pane_footer_h()
        } else {
            0.0
        }
    }
    /// 헤더 띠 높이(logical px) — image/md pane만 30, 그 외 0. resize_backend
    /// 처럼 ws 미잠금 지점에서 id로 조회한다(PaneState::header_px 위임).
    pub(crate) fn pane_header_px(&self, id: &str) -> f32 {
        self.ws
            .lock()
            .ok()
            .and_then(|w| w.panes.get(id).map(|p| p.header_px()))
            .unwrap_or(0.0)
    }
    /// Git-column-toggle button rect, parked at the right end of the title
    /// strip (mirrors the file-tree toggle on the left). Needs the window
    /// width, so it returns `None` before the first paint.
    pub(crate) fn git_col_toggle_rect(&self) -> Option<(f32, f32, f32, f32)> {
        let w = 26.0;
        let h = 22.0;
        let win_w = self.window.as_ref().map(|win| {
            let scale = self.effective_scale();
            win.inner_size().width as f32 / scale
        })?;
        #[cfg(not(windows))]
        let x = win_w - w - 8.0;
        // Windows paints min/max/close at the right edge; keep this toggle left
        // of that cluster so render (render.rs) and this hit-test agree.
        #[cfg(windows)]
        let x = Self::win_control_rects(win_w)[0].0 - 2.0 - w;
        let y = (TITLE_HEIGHT - h) / 2.0;
        Some((x, y, w, h))
    }
    /// Show/hide the git column. Same reflow path as `toggle_sidebar`: flip the
    /// flag, resize the PTYs to the new usable cols, repaint. Publishes the
    /// active cwd so the poller has something to refresh the moment it opens.
    pub(crate) fn toggle_git_col(&mut self) {
        self.git.col_visible = !self.git.col_visible;
        if self.git.col_visible {
            self.publish_git_col_cwd();
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Show/hide pane `id`'s status bar. Collapsing returns the footer rows to
    /// the cell grid, so the PTY is reshaped; an open dropdown on that bar is
    /// dismissed. `resize_backend` reads `statusbar_px` per leaf, so the toggle
    /// is all the state it needs.
    pub(crate) fn toggle_statusbar(&mut self, id: &str) {
        // Record this pane as an exception to the global default — drop any
        // stale membership first, then file it under the set opposite to its
        // new state (off → hidden, on → shown).
        let was_visible = self.statusbar_visible(id);
        self.statusbar.hidden.remove(id);
        self.statusbar.shown.remove(id);
        if was_visible {
            self.statusbar.hidden.insert(id.to_string());
            if self.statusbar.menu.as_ref().map(|(p, _)| p == id).unwrap_or(false) {
                self.statusbar.menu = None;
            }
        } else {
            self.statusbar.shown.insert(id.to_string());
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
    }
    /// Flip the pane's top bar (header band), pinning the choice so it stops
    /// following the automatic rule (tabs>1 / image / md).
    ///
    /// Must resize the backend: the band eats a row off the cell grid, and
    /// render / hit-test / PTY all derive that from `header_px()`. Skip this and
    /// the PTY keeps its old row count while the renderer draws the new one —
    /// clicks land a row off, which is the same class of bug as the zoom
    /// mapping. `chrome_dirty` alone would repaint but not re-measure.
    pub(crate) fn toggle_pane_header(&mut self, id: &str) {
        {
            let mut ws = self.ws.lock().unwrap();
            let Some(pane) = ws.panes.get_mut(id) else { return };
            let now = pane.has_header();
            pane.header_override = Some(!now);
            pane.dirty = true;
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
    }
    /// Open (or toggle) a status-bar dropdown for pane `id`. The list is
    /// snapshotted now — `read_dir` / `git_branches` block, so they can't run on
    /// the render path. A second click on the same chip closes the menu.
    pub(crate) fn open_statusbar_menu(&mut self, id: &str, kind: StatusbarMenu) {
        if self.statusbar.menu.as_ref() == Some(&(id.to_string(), kind)) {
            self.statusbar.menu = None;
            self.chrome_dirty = true;
            return;
        }
        let cwd = self.pane_cwd_cache.get(id).cloned();
        self.statusbar.menu_dirs.clear();
        self.statusbar.menu_branches.clear();
        self.statusbar.menu_scroll = 0.0;
        self.statusbar.menu_search.clear();
        match kind {
            StatusbarMenu::Path => {
                if let Some(cwd) = cwd.as_ref() {
                    // `..` first, then child entries (folders before files, each
                    // alpha-sorted) — a quick-nav picker, so files show too, not
                    // just directories. Dotfiles (and `.git`) stay hidden here.
                    if let Some(parent) = cwd.parent() {
                        self.statusbar.menu_dirs.push(parent.to_path_buf());
                    }
                    if let Ok(rd) = std::fs::read_dir(cwd) {
                        let mut entries: Vec<(bool, std::path::PathBuf)> = rd
                            .filter_map(|e| e.ok())
                            .filter(|e| {
                                e.file_name()
                                    .to_str()
                                    .map(|s| !s.starts_with('.'))
                                    .unwrap_or(false)
                            })
                            .map(|e| {
                                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                                (is_dir, e.path())
                            })
                            .collect();
                        entries.sort_by(|a, b| {
                            b.0.cmp(&a.0).then_with(|| {
                                a.1.file_name()
                                    .map(|s| s.to_ascii_lowercase())
                                    .cmp(&b.1.file_name().map(|s| s.to_ascii_lowercase()))
                            })
                        });
                        self.statusbar.menu_dirs.extend(entries.into_iter().map(|(_, p)| p));
                    }
                }
            }
            StatusbarMenu::Branch => {
                if let Some(cwd) = cwd.as_ref() {
                    self.statusbar.menu_branches = kasa_mcp::git::git_branches(cwd);
                }
            }
        }
        self.statusbar.menu = Some((id.to_string(), kind));
        self.chrome_dirty = true;
    }
    /// Indices into `statusbar_menu_dirs` that survive the live search query
    /// (case-insensitive substring on the entry name; the `..` parent row at
    /// index 0 always shows). Drives both the dropdown render and Enter-to-open.
    pub(crate) fn statusbar_menu_filtered(&self) -> Vec<usize> {
        let q = self.statusbar.menu_search.to_lowercase();
        self.statusbar.menu_dirs
            .iter()
            .enumerate()
            .filter(|(i, p)| {
                if q.is_empty() || *i == 0 {
                    return true;
                }
                p.file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| nfc_hangul(s).to_lowercase().contains(&q))
                    .unwrap_or(false)
            })
            .map(|(i, _)| i)
            .collect()
    }
    /// Enter on the path dropdown: open the first real match (folder → cd, file
    /// → preview pane). With an active query the `..` parent is skipped so Enter
    /// commits to a searched entry, not the parent.
    pub(crate) fn statusbar_menu_activate_first(&mut self) {
        let Some((pid, _)) = self.statusbar.menu.clone() else { return };
        let idxs = self.statusbar_menu_filtered();
        let target = if self.statusbar.menu_search.is_empty() {
            idxs.first().copied()
        } else {
            idxs.iter().find(|&&i| i != 0).or_else(|| idxs.first()).copied()
        };
        if let Some(path) = target.and_then(|i| self.statusbar.menu_dirs.get(i).cloned()) {
            if path.is_dir() {
                self.statusbar_cd(&pid, &path);
            } else {
                self.statusbar.menu = None;
                self.open_file_split(path);
            }
        }
    }
    /// `cd` pane `id`'s shell into `dir` (status-bar path picker). Sent straight
    /// to that pane's PTY — single-quoted so spaces survive — and the dropdown
    /// closes. The cwd sniffer repaints the bar once the shell reports the move.
    pub(crate) fn statusbar_cd(&mut self, id: &str, dir: &std::path::Path) {
        self.statusbar.menu = None;
        // 현재 추적 중인 cwd의 부모로 가는 경우엔 셸 상대경로 `cd ..` 를 쓴다.
        // (PowerShell 등에서 kasaterm이 잡은 절대경로가 부정확해도 한 칸 위로는 항상 정확.)
        let is_parent = self.pane_cwd_cache.get(id).and_then(|c| c.parent()) == Some(dir);
        let cmd = if is_parent {
            "cd ..\r".to_string()
        } else {
            let q = dir.to_string_lossy().replace('\'', "'\\''");
            format!("cd '{q}'\r")
        };
        if let Some(pty) = self.pty.get(id) {
            let _ = pty.send_bytes(cmd.as_bytes());
        }
        self.chrome_dirty = true;
    }
    /// Check out `branch` in pane `id`'s repo (status-bar branch switcher). Runs
    /// inline so the result can become a toast: a dirty tree makes git refuse
    /// (the silent failure that read as "branch switch doesn't work"), so we
    /// surface its message instead of dropping it. We don't stash/force — same
    /// no-surprises stance as the git column.
    pub(crate) fn statusbar_checkout(&mut self, id: &str, branch: String) {
        self.statusbar.menu = None;
        self.chrome_dirty = true;
        let Some(cwd) = self.pane_cwd_cache.get(id).cloned() else { return };
        let res = kasa_mcp::git::git_checkout(&cwd, &branch);
        let ok = res.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
        let msg = if ok {
            format!("{branch} 브랜치로 전환")
        } else {
            let out = res.get("output").and_then(|v| v.as_str()).unwrap_or("");
            if out.contains("would be overwritten") {
                "변경사항 때문에 전환 불가 — 커밋하거나 stash 먼저".to_string()
            } else if out.is_empty() {
                "브랜치 전환 실패".to_string()
            } else {
                format!("전환 실패: {}", out.lines().next().unwrap_or(""))
            }
        };
        self.collab.toast = Some((msg, std::time::Instant::now()));
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Surface a transient top-right toast (reuses the collab toast slot).
    pub(crate) fn set_toast(&mut self, msg: String) {
        self.collab.toast = Some((msg, std::time::Instant::now()));
        self.collab.toast_rect = None;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Run a file-tree right-click menu item. The target is the primary
    /// selection the right-click pinned. New*/Rename open an inline input row;
    /// CopyPath/Reveal/Delete act immediately.
    pub(crate) fn run_ft_menu_action(&mut self, action: crate::FtMenuAction) {
        use crate::FtMenuAction as A;
        let target = self.file_tree.selected.clone();
        match action {
            A::NewFile | A::NewFolder => {
                // Folder → inside it; file → its parent; nothing → tree root.
                let parent = target.as_ref().and_then(|p| {
                    if p.is_dir() {
                        Some(p.clone())
                    } else {
                        p.parent().map(|x| x.to_path_buf())
                    }
                });
                if let Some(par) = parent.clone() {
                    self.file_tree.expanded.insert(par);
                    self.rebuild_file_tree_nodes();
                }
                self.file_tree.new_parent = parent;
                self.file_tree.new = Some((matches!(action, A::NewFolder), String::new()));
                self.file_tree.rename = None;
                self.file_tree.search_active = false;
                self.file_tree.scroll = 0.0;
            }
            A::Rename => {
                if let Some(p) = target {
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    // 이름을 싣고 여는 유일한 칸이라 커서를 여기서 끝에 찍는다 —
                    // 0 으로 두면 고치려던 확장자 앞이 아니라 이름 맨 앞에 선다.
                    self.file_tree.edit_cursor = name.chars().count();
                    self.file_tree.rename = Some((p, name));
                    self.file_tree.new = None;
                    self.file_tree.search_active = false;
                }
            }
            A::CopyPath => {
                if let Some(p) = target {
                    let s = p.to_string_lossy().to_string();
                    match arboard::Clipboard::new() {
                        Ok(mut cb) => {
                            if cb.set_text(s).is_ok() {
                                self.set_toast("경로 복사됨".to_string());
                            }
                        }
                        Err(e) => eprintln!("[kasaterm] clipboard open failed: {e}"),
                    }
                }
            }
            A::Reveal => {
                if let Some(p) = target {
                    self.reveal_in_file_manager(&p);
                }
            }
            A::OpenWith(i) => {
                // 인덱스는 이 프레임에 메뉴를 그린 목록에서 왔고 그 목록은
                // 프로세스당 한 번만 만들어지므로 어긋날 수 없다. 그래도 get 으로
                // 받는 건, 목록이 비었을 때 패닉 대신 아무 일도 안 일어나게.
                if let (Some(p), Some((_, target))) =
                    (target, crate::proc::open_with_apps().get(i))
                {
                    crate::proc::open_path_with(target, &p);
                }
            }
            A::OpenDefault => {
                if let Some(p) = target {
                    crate::proc::open_path_default(&p);
                }
            }
            A::Delete => self.delete_tree_selection(),
        }
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Reveal a path in the OS file manager (macOS Finder reveal · Windows
    /// Explorer select · Linux opens the parent folder via xdg-open).
    pub(crate) fn reveal_in_file_manager(&self, path: &std::path::Path) {
        #[cfg(target_os = "macos")]
        {
            let _ = crate::proc::command("open").arg("-R").arg(path).spawn();
        }
        #[cfg(target_os = "windows")]
        {
            let _ = crate::proc::command("explorer")
                .arg(format!("/select,{}", path.display()))
                .spawn();
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let dir = if path.is_dir() {
                path
            } else {
                path.parent().unwrap_or(path)
            };
            let _ = crate::proc::command("xdg-open").arg(dir).spawn();
        }
    }
    /// Info 패널의 포트 칩 클릭 — 기본 브라우저로 `http://localhost:<port>`.
    /// https 를 시도하지 않는 건 로컬 dev 서버가 거의 평문이기 때문이다(TLS 인
    /// 서버는 브라우저가 리다이렉트해준다).
    /// 창 맨 아래 상태줄 높이(logical px). 설정에서 바뀌므로 상수를 직접 읽지 마라.
    pub(crate) fn status_h(&self) -> f32 {
        self.set_status_h
    }

    /// pane 하단바(경로·브랜치·diff 칩) 높이(logical px).
    pub(crate) fn pane_footer_h(&self) -> f32 {
        self.set_pane_footer_h
    }

    pub(crate) fn open_localhost(&self, port: u16) {
        self.open_url(&format!("http://localhost:{port}"));
    }

    /// 기본 브라우저로 연다. 세 갈래 분기가 여기 한 곳에만 있어야 새 진입점을
    /// 더할 때 macOS 전용 `open` 이 다시 박히지 않는다.
    pub(crate) fn open_url(&self, url: &str) {
        #[cfg(target_os = "macos")]
        let _ = crate::proc::command("open").arg(url).spawn();
        #[cfg(target_os = "windows")]
        let _ = crate::proc::command("cmd").args(["/C", "start", "", url]).spawn();
        #[cfg(all(unix, not(target_os = "macos")))]
        let _ = crate::proc::command("xdg-open").arg(url).spawn();
    }

    /// Expand/collapse a file's inline unified diff in the git panel. The diff
    /// is parsed once on first expand and cached; `git diff` for a single file
    /// is cheap but not render-loop cheap, so it must not run per frame.
    pub(crate) fn toggle_git_diff(&mut self, staged: bool, path: String) {
        let key = (staged, path.clone());
        if self.git.col_expanded.remove(&key) {
            self.chrome_dirty = true;
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
            return;
        }
        if !self.git.col_diff_cache.contains_key(&key) {
            if let Some(cwd) = self.git.col_data.lock().ok().and_then(|g| g.cwd.clone()) {
                let rows = kasa_mcp::git::git_file_diff(&cwd, &path, staged);
                self.git.col_diff_cache.insert(key.clone(), rows);
            }
        }
        self.git.col_expanded.insert(key);
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Double-click on a recent-commit row: expand/collapse its changed-file
    /// list inline (only one commit open at a time). On expand the file list is
    /// fetched once and cached.
    pub(crate) fn toggle_git_commit(&mut self, hash: String) {
        if self.git.col_commit_expanded.as_deref() == Some(hash.as_str()) {
            self.git.col_commit_expanded = None;
        } else {
            if !self.git.col_commit_files_cache.contains_key(&hash) {
                if let Some(cwd) = self.git.col_data.lock().ok().and_then(|g| g.cwd.clone()) {
                    let files = kasa_mcp::git::git_commit_files(&cwd, &hash);
                    self.git.col_commit_files_cache.insert(hash.clone(), files);
                }
            }
            self.git.col_commit_expanded = Some(hash);
        }
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Click a file row inside an expanded commit: expand/collapse that file's
    /// diff. The diff is fetched once and cached, like `toggle_git_diff`.
    pub(crate) fn toggle_git_commit_file(&mut self, hash: String, path: String) {
        let key = (hash.clone(), path.clone());
        if self.git.col_commit_file_expanded.remove(&key) {
            self.chrome_dirty = true;
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
            return;
        }
        if !self.git.col_commit_diff_cache.contains_key(&key) {
            if let Some(cwd) = self.git.col_data.lock().ok().and_then(|g| g.cwd.clone()) {
                let rows = kasa_mcp::git::git_commit_file_diff(&cwd, &hash, &path);
                self.git.col_commit_diff_cache.insert(key.clone(), rows);
            }
        }
        self.git.col_commit_file_expanded.insert(key);
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Collapse + drop cached diffs after a stage/unstage/commit changes the
    /// tree — the cached rows (and which side a file lives on) are now stale, so
    /// closing them is the no-surprise reset; the user re-expands for fresh diff.
    pub(crate) fn invalidate_git_diffs(&mut self) {
        self.git.col_expanded.clear();
        self.git.col_diff_cache.clear();
        // 편집기 거터도 같은 HEAD 를 기준으로 삼는다 — 여기서 안 버리면 방금
        // 커밋한 변경이 거터에 그대로 남아, 고치지도 않은 줄이 파랗게 보인다.
        self.invalidate_editor_diffs();
    }
    /// Open the git column for pane `id`'s repo (status-bar diff chip click).
    /// Focuses that pane so the column follows it (auto-track), then opens the
    /// column if it's hidden. A second click on an already-open column for the
    /// same pane closes it (toggle).
    pub(crate) fn open_git_panel_for(&mut self, id: &str) {
        let already = self.git.col_visible
            && self
                .ws
                .lock()
                .ok()
                .and_then(|w| w.active_pane.clone())
                .as_deref()
                == Some(id);
        if already {
            self.toggle_git_col();
            return;
        }
        if let Ok(mut w) = self.ws.lock() {
            w.active_pane = Some(id.to_string());
        }
        self.git.col_pinned_cwd = None;
        if self.git.col_visible {
            self.publish_git_col_cwd();
            self.chrome_dirty = true;
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        } else {
            self.toggle_git_col();
        }
    }
    /// Feed every pane's cwd into the badge poller (`git_poll_cwds`) so each
    /// pane's status bar shows its own repo's branch/diff, not just the active
    /// one. The poller dedups + rate-limits, so a flat overwrite each frame is
    /// fine. Skipped entirely when no pane shows a bar (nothing to refresh).
    pub(crate) fn publish_pane_git_cwds(&self) {
        // Always feed every pane's cwd. Besides the native status bars, the BA
        // GUI — now opened in an external browser tab — consumes per-pane badges
        // through `/layout`, and the GUI can't tell whether that tab is open. The
        // poller dedups by cwd and only wakes on a change, so an idle feed is one
        // cheap git call per distinct repo per interval (no repaint).
        let cwds: Vec<std::path::PathBuf> = self.pane_cwd_cache.values().cloned().collect();
        if let Ok(mut guard) = self.git_poll_cwds.lock() {
            *guard = cwds;
        }
    }
    /// Publish each pane's cwd + git badge into `pane_status_pub` so the socket
    /// thread's `/layout` can stamp them onto every `PaneRect` — the BA GUI draws
    /// a Warp-style cwd/branch/diff bar on plain (non-claude) terminal tiles from
    /// it. Reads only the already-resolved `pane_cwd_cache` + `window_git` caches,
    /// so nothing here touches the lsof/git hot path.
    pub(crate) fn publish_pane_status(&self) {
        let badges = self.window_git.lock().ok();
        let mut map: HashMap<String, PaneStatus> = HashMap::new();
        for (id, cwd) in &self.pane_cwd_cache {
            let badge = badges.as_ref().and_then(|g| g.get(cwd).cloned());
            // Share the PTY's OSC 133 block store (cheap Arc clone) so the
            // socket `/blocks` can read it without reaching into App.pty.
            let blocks = self.pty.get(id).map(|p| p.blocks_arc());
            map.insert(
                id.clone(),
                PaneStatus {
                    cwd: cwd.clone(),
                    badge,
                    blocks,
                },
            );
        }
        if let Ok(mut guard) = self.pane_status_pub.lock() {
            *guard = map;
        }
    }
    /// Push the active pane's cwd into the shared `git_col_cwd` so the git
    /// poller refreshes the right repo. Cheap string clone; called from the
    /// render right before the column paints (mirrors `git_poll_cwds`).
    pub(crate) fn publish_git_col_cwd(&self) {
        if !self.git.col_visible {
            return;
        }
        // A user-pinned repo (picked from the path dropdown) overrides the
        // active-pane follow — the column stays on that repo until unpinned.
        if let Some(pinned) = self.git.col_pinned_cwd.clone() {
            if let Ok(mut guard) = self.git.col_cwd.lock() {
                *guard = Some(pinned);
            }
            return;
        }
        let active = self.ws.lock().ok().and_then(|w| w.active_pane.clone());
        let resolved = active
            .as_ref()
            .and_then(|id| self.pane_cwd_cache.get(id).cloned());
        if let Ok(mut guard) = self.git.col_cwd.lock() {
            match resolved {
                // A confidently-resolved pane cwd always wins.
                Some(cwd) => *guard = Some(cwd),
                // Cache miss (e.g. right after a pane switch, before the cwd
                // sniffer catches up): keep the last good cwd instead of
                // flashing the launch dir — which is often a non-repo and
                // would read as "not a repo". Seed from current_dir only on
                // the very first frame, when nothing is known yet.
                None if guard.is_none() => *guard = std::env::current_dir().ok(),
                None => {}
            }
        }
    }
    /// Run a git-column button off a worker thread so the UI never blocks on
    /// git/network. Pull/Push sync the branch; Commit
    /// commits the STAGED changes with the panel's message (VSCode model). All
    /// read the column's repo from the poller's snapshot so the action always
    /// targets what the user sees.
    pub(crate) fn run_git_col_action(&mut self, btn: GitColBtn) {
        let cwd = self.git.col_data.lock().ok().and_then(|g| g.cwd.clone());
        let Some(cwd) = cwd else { return };
        match btn {
            GitColBtn::Pull => {
                self.git.op = Some("Pulling");
                let proxy = self.proxy.clone();
                std::thread::spawn(move || {
                    let _ = kasa_mcp::git::git_pull(&cwd);
                    // GitOpDone clears the spinner; the poller's next tick
                    // repaints ahead/behind.
                    let _ = proxy.send_event(UserEvent::GitOpDone);
                });
            }
            GitColBtn::Push => {
                self.git.op = Some("Pushing");
                let proxy = self.proxy.clone();
                std::thread::spawn(move || {
                    let _ = kasa_mcp::git::git_push(&cwd);
                    let _ = proxy.send_event(UserEvent::GitOpDone);
                });
            }
            GitColBtn::Commit => {
                // Commit the STAGED changes with the panel's message (VSCode
                // model — commit -m, no add). Empty message → focus the input
                // instead of a silent no-op, so the user sees where to type.
                let msg = self.git.commit_msg.trim().to_string();
                if msg.is_empty() {
                    self.git.commit_focused = true;
                    self.chrome_dirty = true;
                    return;
                }
                self.git.op = Some("Committing");
                let proxy = self.proxy.clone();
                std::thread::spawn(move || {
                    let _ = kasa_mcp::git::git_commit_staged(&cwd, &msg);
                    let _ = proxy.send_event(UserEvent::GitOpDone);
                });
                self.git.commit_msg.clear();
                self.git.commit_cursor = 0;
                self.git.commit_focused = false;
                self.chrome_dirty = true;
            }
        }
    }
    /// Open the cursor-style Commit modal: pre-fill nothing, focus the message
    /// box, default to including unstaged changes (the toggle in the modal).
    pub(crate) fn open_commit_modal(&mut self) {
        self.git.commit_menu_open = false;
        self.git.commit_modal_open = true;
        self.git.commit_focused = true;
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    pub(crate) fn close_commit_modal(&mut self) {
        self.git.commit_modal_open = false;
        self.git.commit_focused = false;
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Run the modal's commit. `push` also pushes after. Honors the
    /// include-unstaged toggle: when on, stage everything first (`git add -A`),
    /// else commit only what's already staged. Empty message is a no-op.
    pub(crate) fn run_commit_modal(&mut self, push: bool) {
        let msg = self.git.commit_msg.trim().to_string();
        if msg.is_empty() {
            self.git.commit_focused = true;
            self.chrome_dirty = true;
            return;
        }
        let Some(cwd) = self.git.col_data.lock().ok().and_then(|g| g.cwd.clone()) else { return };
        let include = self.git.commit_modal_include_unstaged;
        self.git.op = Some(if push { "Pushing" } else { "Committing" });
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            if include {
                // Stage everything, then commit all staged.
                let _ = kasa_mcp::git::git_commit_all(&cwd, &msg);
            } else {
                let _ = kasa_mcp::git::git_commit_staged(&cwd, &msg);
            }
            if push {
                let _ = kasa_mcp::git::git_push(&cwd);
            }
            let _ = proxy.send_event(UserEvent::GitOpDone);
        });
        self.git.commit_msg.clear();
        self.git.commit_cursor = 0;
        self.git.commit_focused = false;
        self.git.commit_modal_open = false;
        self.invalidate_git_diffs();
        self.chrome_dirty = true;
    }
    /// `gh pr create --web` for the column's repo (Commit-menu → Create PR).
    pub(crate) fn create_git_pr(&mut self) {
        self.git.commit_menu_open = false;
        let Some(cwd) = self.git.col_data.lock().ok().and_then(|g| g.cwd.clone()) else { return };
        std::thread::spawn(move || {
            let _ = crate::proc::command("gh")
                .args(["pr", "create", "--web"])
                .current_dir(&cwd)
                .spawn();
        });
        self.collab.toast = Some(("gh pr create --web 실행".to_string(), std::time::Instant::now()));
        self.chrome_dirty = true;
    }
    /// Expand/restore the git column width (header ⤢ button). Toggles between a
    /// wide reading width and the normal sidebar width; reshapes the PTYs.
    pub(crate) fn toggle_git_col_expand(&mut self) {
        let wide = 620.0_f32;
        let normal = 340.0_f32;
        self.git.col_w_logical = if self.git.col_w_logical >= wide - 1.0 { normal } else { wide };
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
    }
    /// 칼럼 발치 「최근 커밋」 구역의 크기조절 — 누르기·끌기·놓기 세 짝.
    ///
    /// handler 의 마우스 분기에서 뽑아 둔 건 헤드리스 하네스가 **같은 코드**를 타게
    /// 하려고다. 손잡이 판정과 높이 계산이 두 벌로 갈리면 검증이 통과해도 실제 클릭은
    /// 빗나갈 수 있고, 그 어긋남은 스크린샷에 안 찍힌다.
    ///
    /// 손잡이를 눌렀으면 참. 지금 그려져 있는 높이를 드래그 출발점으로 삼는다 —
    /// 0 에서 출발하면 첫 픽셀에 구역이 접혔다 펴진다.
    pub(crate) fn commits_grip_press(&mut self, cx: f32, cy: f32) -> bool {
        let Some(gr) = self.git.col_commits_grip else { return false };
        if !(cx >= gr.0 && cx <= gr.0 + gr.2 && cy >= gr.1 && cy <= gr.1 + gr.3) {
            return false;
        }
        let cur = self.git.col_commits_h.unwrap_or_else(|| {
            let n = self
                .git.col_data
                .lock()
                .map(|g| g.recent_commits.len())
                .unwrap_or(crate::GIT_RECENT_COMMITS_DEFAULT);
            24.0 + n as f32 * 20.0
        });
        self.git.col_commits_resize = Some((cy, cur));
        true
    }
    /// 끄는 중이면 `Some(값이 움직였나)`. 손잡이가 구역 **머리**라 위로 끌수록 커진다.
    /// 델타를 누적하지 않고 시작점에서 재는 건 폭 드래그와 같은 이유다 — 누적하면
    /// clamp 에 걸린 뒤 커서를 되돌려도 값이 안 따라온다.
    pub(crate) fn commits_grip_drag(&mut self, cy: f32) -> Option<bool> {
        let (start_y, start_h) = self.git.col_commits_resize?;
        let new_h = (start_h - (cy - start_y)).clamp(crate::GIT_COMMITS_H_MIN, crate::GIT_COMMITS_H_MAX);
        let moved = self.git.col_commits_h.map_or(true, |h| (new_h - h).abs() > 0.5);
        if moved {
            self.git.col_commits_h = Some(new_h);
            self.chrome_dirty = true;
        }
        Some(moved)
    }
    /// 끄는 중이었으면 참. 늘어난 자리를 폴러 tick(1.2초)까지 빈칸으로 두면 「늘려도
    /// 안 늘어난다」로 읽히므로 여기서 한 번 바로 읽어 온다.
    pub(crate) fn commits_grip_release(&mut self) -> bool {
        if self.git.col_commits_resize.take().is_none() {
            return false;
        }
        let cwd = self.git.col_data.lock().ok().and_then(|g| g.cwd.clone());
        if let Some(cwd) = cwd {
            let proxy = self.proxy.clone();
            let data = self.git.col_data.clone();
            let want = self.git.col_commit_want.load(std::sync::atomic::Ordering::Relaxed);
            std::thread::spawn(move || {
                if let Some(view) = crate::handler::fetch_git_col_view(&cwd, want) {
                    if let Ok(mut g) = data.lock() {
                        *g = view;
                    }
                }
                let _ = proxy.send_event(crate::UserEvent::Redraw);
            });
        }
        true
    }
    /// Check out `branch` in the column's repo (off-thread). A dirty tree makes
    /// git refuse with a clear message — we don't stash/force, just let the
    /// poller repaint whatever git did. Closes the branch dropdown.
    pub(crate) fn run_git_checkout(&mut self, branch: String) {
        self.git.branch_menu_open = false;
        let cwd = self.git.col_data.lock().ok().and_then(|g| g.cwd.clone());
        let Some(cwd) = cwd else { return };
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let _ = kasa_mcp::git::git_checkout(&cwd, &branch);
            let _ = proxy.send_event(UserEvent::Redraw);
        });
    }
    /// Persist the current window frame (logical size + physical position).
    /// Called from `exiting` and from the Moved/Resized debounce in
    /// `about_to_wait` — the debounce keeps the frame safe across a crash.
    pub(crate) fn save_window_frame(&self) {
        // **검증 실행은 저장하지 않는다.** 위치·크기를 env 로 강제했다는 건 그 창이
        // 사람이 쓰던 창이 아니라 하네스가 띄운 창이라는 뜻인데, 설정 파일은 인스턴스
        // 사이에 공유돼서 그 값이 그대로 거노 앱의 다음 크기가 된다(실사고 2026-08-06:
        // 좁은 화면 재현으로 430x700 를 띄웠더니 `window.json` 이 그 값으로 덮여,
        // 재시작하면 앱이 구석에 손바닥만 하게 뜰 뻔했다).
        if crate::verification_run() {
            return;
        }
        let Some(win) = self.window.as_ref() else { return };
        let scale = win.scale_factor().max(0.5);
        let sz = win.inner_size();
        let pos = win
            .outer_position()
            .ok()
            .map(|p| (p.x as f64, p.y as f64));
        crate::socket::write_window_frame(
            sz.width as f64 / scale,
            sz.height as f64 / scale,
            pos,
        );
    }

    /// Hit-test a press against the window-tab strip controls, in paint order:
    /// close-× (sits on top of a tab) → tab → "+" new-window button. Shared by
    /// the side sidebar strip and the top-tabs title strip — the top strip
    /// previously had no click gate at all, so its tabs painted but never
    /// switched/closed. Returns true when the click was handled.
    /// 그 방의 목록이 얼마나 펴져 있나 — 0(접힘)..1(다 폄).
    ///
    /// 애니메이션이 없으면 0 이나 1 이고, 도는 중이면 그 사이다. 목록 높이·행
    /// 투명도가 이 하나를 같이 보므로 밀림과 나타남이 어긋나지 않는다.
    pub(crate) fn expand_progress(&self, idx: usize) -> f32 {
        let target = if self.expanded_windows.contains(&idx) { 1.0 } else { 0.0 };
        let Some((ai, opening, at)) = self.expand_anim else { return target };
        if ai != idx {
            return target;
        }
        let t = (at.elapsed().as_secs_f32() / EXPAND_ANIM_SECS).clamp(0.0, 1.0);
        // ease-out — 손을 뗀 직후가 가장 빠르고 끝에서 가라앉는다. 선형은 멈추는
        // 순간이 툭 끊겨 목록이 "튄" 것처럼 보인다.
        let e = 1.0 - (1.0 - t).powi(3);
        if opening { e } else { 1.0 - e }
    }

    /// 사이드바 pane 행 우클릭 → 메뉴 장전. 그 줄을 맞혔으면 true.
    ///
    /// 판정을 메서드로 둔 건 하네스가 **같은 좌표 판정을 지나게** 하기 위함이다
    /// (`window_strip_click` 과 같은 이유). 메뉴가 뜨는 자리는 미니맵 칸과 목록 줄이
    /// 한 벡터에 섞여 있어, 상태를 손으로 세우는 검증은 정작 어긋나는 자리를 못 본다.
    pub(crate) fn sidebar_row_right_click(&mut self, cx: f32, cy: f32) -> bool {
        let inside =
            |r: &(f32, f32, f32, f32)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3;
        let Some((wi, pane)) = self
            .sidebar_row_rects
            .iter()
            .find(|(_, _, r)| inside(r))
            .map(|(i, p, _)| (*i, p.clone()))
        else {
            return false;
        };
        self.sidebar_menu = Some((cx, cy, wi, pane));
        self.chrome_dirty = true;
        true
    }

    /// 떠 있는 사이드바 메뉴에 좌클릭. 메뉴가 떠 있었으면 true — 항목을 맞혔든
    /// 빈 곳을 눌렀든 클릭을 삼키고 닫는다.
    pub(crate) fn sidebar_menu_click(&mut self, cx: f32, cy: f32) -> bool {
        let Some((_, _, wi, pane)) = self.sidebar_menu.clone() else { return false };
        let inside =
            |r: &(f32, f32, f32, f32)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3;
        let hit = self.sidebar_menu_rects.iter().find(|(_, r)| inside(r)).map(|(a, _)| *a);
        self.sidebar_menu = None;
        self.sidebar_menu_rects.clear();
        if let Some(a) = hit {
            self.run_sidebar_menu_action(a, wi, &pane);
        }
        self.chrome_dirty = true;
        true
    }

    /// 사이드바 pane 행 우클릭 메뉴 실행.
    pub(crate) fn run_sidebar_menu_action(
        &mut self,
        action: SidebarMenuAction,
        wi: usize,
        pane: &str,
    ) {
        match action {
            SidebarMenuAction::Hide => {
                // ★ 그 방을 **먼저 활성으로** 만든다. 사이드바는 모든 방의 pane 을
                // 보여주는데, 다른 방 pane 에 `stash_pane` 을 걸면 활성 트리에서 못 찾아
                // `remove_pane`(죽이는 경로)으로 샌다 — 「숨겼는데 학생이 사라졌다」가
                // 되는 자리다.
                if wi != self.active_window {
                    self.switch_window(wi);
                }
                // 밖에 나간 방은 `switch_window` 가 그 창을 앞으로 보낼 뿐 활성은 안
                // 바뀐다. 그때는 손대지 않는다 — 그 창에서 닫으면 된다.
                if wi == self.active_window {
                    self.stash_pane(pane);
                }
            }
            SidebarMenuAction::Unhide => {
                if let Some(i) = self.closed_pane_index(pane) {
                    self.reopen_closed_pane_at(i);
                }
            }
        }
    }

    /// 방을 펴거나 접는다 — 상태와 애니메이션을 같이 세우는 유일한 입구.
    pub(crate) fn toggle_window_expand(&mut self, idx: usize) {
        let opening = !self.expanded_windows.contains(&idx);
        if opening {
            self.expanded_windows.insert(idx);
        } else {
            self.expanded_windows.remove(&idx);
        }
        self.expand_anim = Some((idx, opening, std::time::Instant::now()));
        self.chrome_dirty = true;
    }

    /// 방 탭 카드 안 **펼치기 버튼**의 사각 — 상태 점 왼쪽의 삼각형 자리.
    /// `tab` 은 그 방 카드의 사각.
    ///
    /// 렌더와 클릭 판정이 이 하나를 같이 본다. 예전엔 클릭 쪽이 "아랫줄 오른쪽
    /// 100px" 이라는 자기 공식을 따로 갖고 있어서, 눈에는 삼각형 하나만 보이는데
    /// 그 옆 점들까지 눌러도 방 전환이 안 됐다 — 버튼이 어디까지인지 화면이
    /// 말해 주지 않는 상태였다(거노: "접기 버튼이 따로 있어야, 누르면 전환은
    /// 되고"). pane 이 하나뿐인 방은 펼쳐도 그 하나뿐이라 버튼을 두지 않는다.
    pub(crate) fn window_expand_rect(
        &self,
        idx: usize,
        tab: (f32, f32, f32, f32),
    ) -> Option<(f32, f32, f32, f32)> {
        let n = self.window_leaves(idx).len();
        // pane 이 하나뿐인 방도 편다. 예전엔 `n < 2` 로 막았는데 — 한 줄짜리 목록은
        // 펼 값어치가 없다는 판단이었다 — 그 한 줄이 **누가 거기 있고 무슨 상태인지**
        // 다. 학생 하나를 방 하나에 두고 쓰면 사이드바에서 그 학생을 볼 길이 통째로
        // 사라졌다(거노: "방하나에 학생하나면 펼치기가 없어서 학생목록이 안보이네").
        if n == 0 {
            return None;
        }
        // 삼각형 하나짜리 18px 칩은 눌러 보기에 너무 작았다(거노). pane 개수를
        // 같이 담아 pill 로 키우면 타깃이 두 배 넘게 커지고, 방을 펴지 않고도
        // 몇 개짜리 방인지 읽힌다 — 커진 자리에 정보가 같이 들어온 셈이다.
        let w = if n >= 10 { 44.0 } else { 37.0 };
        Some((tab.0 + tab.2 - 8.0 - w, tab.1 + 26.0, w, 20.0))
    }

    pub(crate) fn window_strip_click(&mut self, cx: f32, cy: f32) -> bool {
        let inside =
            |r: &(f32, f32, f32, f32)| cx >= r.0 && cx <= r.0 + r.2 && cy >= r.1 && cy <= r.1 + r.3;
        // 밖에 나간 방의 되돌리기 버튼이 가장 먼저다. × 히트렉트는 그려지지 않는
        // 탭에도 남아 있고 자리가 정확히 겹쳐서, 뒤에 두면 되돌리려던 클릭이
        // 「이 방 닫을까요」로 새어 나갔다(실측: 방이 통째로 사라짐).
        if let Some(idx) = self
            .window_dock_rects
            .iter()
            .find(|(_, r)| inside(r))
            .map(|(i, _)| *i)
        {
            self.dock_room_back(idx);
            return true;
        }
        if let Some(idx) = self
            .window_tab_close_rects
            .iter()
            .find(|(_, r)| inside(r))
            .map(|(i, _)| *i)
        {
            // 세로 사이드바에서만 자리를 얼린다. 상단 strip 은 폭 기준이라 기하가
            // 다르고, 무엇보다 이 × 는 두 배치가 공용이다.
            if !self.tabs_on_top {
                self.freeze_closing(crate::CloseFreezeKind::Sidebar(self.sidebar_scroll_px));
            }
            // close_window 를 직접 부르면 그 창의 claude 가 돌고 있어도 말없이
            // 죽는다. 같은 동작의 가운데 클릭은 confirm_or_close_session 으로
            // 물어보는데, 이 ×(사이드바/상단 strip 공용)만 확인을 건너뛰고 있었다.
            self.confirm_or_close_session(idx);
            return true;
        }
        // 펼쳐 둔 pane 줄이 먼저다 — 줄은 탭 카드 **안에** 그려지므로, 탭을 먼저
        // 검사하면 줄을 눌러도 방 전환만 되고 학생에게는 영영 못 간다.
        if let Some((wi, pane)) = self
            .sidebar_row_rects
            .iter()
            .find(|(_, _, r)| inside(r))
            .map(|(i, p, _)| (*i, p.clone()))
        {
            if wi != self.active_window {
                self.switch_window(wi);
            }
            self.focus_pane(&pane);
            // 포커스는 누르는 즉시(목록에서 pane 을 고르는 게 이 줄의 본업이다),
            // 옮기기는 여기서 장전만. 문턱을 못 넘으면 release 가 그냥 버린다.
            self.sidebar_row_drag = Some(crate::SidebarRowDrag {
                pane,
                start: (cx, cy),
                active: false,
                target: None,
            });
            self.chrome_dirty = true;
            return true;
        }
        if let Some(idx) = self
            .window_tab_rects
            .iter()
            .find(|(_, r)| inside(r))
            .map(|(i, _)| *i)
        {
            // 펼치기 버튼만 전환의 예외다 — 그 배지 크기.
            let tab = self.window_tab_rects.iter().find(|(i, _)| *i == idx).map(|(_, r)| *r);
            if let Some(r) = tab.and_then(|t| self.window_expand_rect(idx, t)) {
                if inside(&r) {
                    self.toggle_window_expand(idx);
                    return true;
                }
            }
            // 이미 열려 있는 방을 **천천히** 다시 누르면 이름 편집(Finder 규칙).
            // 전환보다 먼저 본다 — 전환은 같은 방이면 어차피 무동작이다.
            let now = std::time::Instant::now();
            if starts_room_rename(self.room_rename.last_click, idx, self.active_window, now) {
                // 손으로 붙인 이름이 없으면 **지금 화면에 보이는 라벨**로 시작한다.
                // 빈칸으로 열면 cwd 에서 파생된 이름이 눈앞에서 사라져, 고치려던
                // 사람이 이름을 통째로 다시 쳐야 한다(Finder 는 현 이름을 채워 준다).
                let cur = self
                    .window_name_override
                    .get(&idx)
                    .cloned()
                    .or_else(|| self.window_labels.get(idx).map(|(n, _)| n.clone()))
                    .unwrap_or_default();
                self.room_rename.cursor = cur.chars().count();
                self.room_rename.editing = Some((idx, cur));
                self.room_rename.last_click = None;
                let _ = self.hangul.flush();
                self.mark_room_label_dirty();
                return true;
            }
            self.room_rename.last_click = Some((idx, now));
            // 다른 방을 누르면 편집은 확정하고 넘어간다(바깥 클릭 = 확정).
            self.commit_room_rename();
            self.switch_window(idx);
            // 전환은 누르는 즉시(브라우저 탭과 같다 — 방 전환은 트리 스왑이라
            // 싸다), 재배치는 여기서 장전만. 문턱을 못 넘으면 release 가 그냥
            // 버리므로 평범한 클릭의 감각은 그대로다.
            self.win_tab_drag = Some(WinTabDrag {
                from: idx,
                start: (cx, cy),
                active: false,
                target: idx,
            });
            return true;
        }
        if self.new_window_btn_rect.map(|r| inside(&r)).unwrap_or(false) {
            // 피커 항목은 Windows 설치 셸뿐 — macOS/Linux 는 목록이 비므로
            // 메뉴 대신 즉시 기본 셸 새 윈도우("Claude 학생" 항목은 폐기 —
            // split+claude 수동 부팅으로 충분, 거노).
            if crate::available_shells().is_empty() {
                self.new_window();
            } else {
                self.shell_menu_open = !self.shell_menu_open;
            }
            self.chrome_dirty = true;
            return true;
        }
        false
    }

    /// Preview a changed file from the git column, resolved against the
    /// column's repo cwd. `open_file` does its own extension branching
    /// (image viewer / md render / raw code editor) and focuses an existing
    /// pane instead of duplicating — same path as a file-tree double-click.
    /// A native diff view is still phase 2; opening the file is the useful v1.
    pub(crate) fn open_git_file(&mut self, rel: &str) {
        let cwd = self.git.col_data.lock().ok().and_then(|g| g.cwd.clone());
        let Some(cwd) = cwd else { return };
        self.open_file(cwd.join(rel), None, false);
    }
    /// 창 아래쪽이 pane 그리드에서 먹는 높이 — **접힘 dock + 상태줄**.
    ///
    /// 예약과 그리기가 서로 다른 조건을 보면 바가 마지막 셀 줄 위에 겹치거나
    /// 빈 띠만 남는다 — 판단은 여기 한 곳에서만 한다.
    ///
    /// 상태줄(`self.status_h()`)은 **조건 없이 항상** 들어간다. dock 과 달리 늘 있는
    /// 띠라, 여기서 안 빼면 마지막 셀 줄 위에 그대로 덮여 그려진다 — 넘친 것이
    /// 잘리지 않고 **멀쩡해 보이는 채로 겹친다**. 시저가 생겼어도 이건 안 바뀐다:
    /// 클립은 chrome 인스턴스 버퍼를 구간으로 갈라 거는 것이라 **터미널 셀 패스에는
    /// 안 걸린다**. 셀이 차지할 높이는 여기서 미리 빼 두는 수밖에 없다.
    ///
    /// 닫은 pane 은 여기 안 센다. 되살리기는 Info 의 「되살리기」 섹션이 맡는다 —
    /// dock 에 두면 pane 을 하나 닫을 때마다 그리드가 40px 줄면서 화면 전체가
    /// 재배치되고, 그 띠가 포커스 테두리 아랫변까지 덮었다(거노).
    ///
    /// 접어 둔 별도창은 **센다**. 그건 사용자가 그 순간 직접 접은 것이라 띠가 생기는
    /// 게 결과로 읽히고, 무엇보다 되살릴 손잡이가 여기 말고는 없다.
    pub(crate) fn bottom_reserve_h(&self) -> f32 {
        self.dock_reserve_h() + self.status_h()
    }

    /// 접힘 dock 만의 높이(0 이면 dock 자체가 없다). 상태줄은 안 센다 — dock 을
    /// 그리는 자리는 상태줄 **위**에 놓여야 해서 둘을 갈라 쓴다.
    pub(crate) fn dock_reserve_h(&self) -> f32 {
        if self.docked.is_empty() && self.zoomed_pane.is_none() && self.hidden_aux.is_empty() {
            0.0
        } else {
            DOCK_HEIGHT
        }
    }

    /// 사이드바 하단에 붙박인 트레이 — 새 세션(`+`)과 앱 전역 버튼(피드백·설정).
    /// 반환은 `(구분선 y, +, 피드백, 설정)`, 세로 사이드바가 없으면 `None`.
    ///
    /// 셋 다 원래는 세션 목록 *뒤에* 줄줄이 붙어 있었다. 그러면 세션이 늘 때마다
    /// 아래로 밀려서, 늘 같은 버튼을 누르는데 자리가 매번 달라진다. 트레이는 목록
    /// 길이와 무관하게 바닥에 고정이라 근육기억이 선다.
    pub(crate) fn sidebar_tray_rects(
        &self,
        win_h: f32,
    ) -> Option<(f32, (f32, f32, f32, f32), (f32, f32, f32, f32), (f32, f32, f32, f32))> {
        if self.tabs_on_top || !self.sidebar_visible {
            return None;
        }
        // 사이드바는 pane 그리드를 안 지나므로 `window_cells` 의 예약이 여기까지
        // 오지 않는다 — 상태줄을 직접 빼야 트레이가 그 밑에 깔리지 않는다.
        let bottom_h =
            if self.docked.is_empty() { 0.0 } else { DOCK_HEIGHT } + self.status_h();
        let line_y = (win_h - bottom_h - SIDEBAR_TRAY_H).max(TITLE_HEIGHT);
        let strip = self.tab_strip_w();
        let left = SIDEBAR_TAB_INSET + 4.0;
        let gap = 4.0_f32;
        let avail = (strip - left * 2.0).max(0.0);
        // 셋이 「왼쪽 하나 · 오른쪽 둘」로 갈라서려면 28×3 에 여백까지 120px 은 있어야
        // 한다. 그보다 좁으면 오른쪽 기준으로 잡던 가운데 버튼의 x 가 **음수**가 되어
        // 세 아이콘이 왼쪽 구석에 포개졌다(64px 실측). 폭이 모자랄 때는 자리 배분을
        // 포기하고 크기를 줄여 균등하게 늘어놓는다 — 뭉쳐 있으면 셋 다 못 누른다.
        let roomy = avail >= 28.0 * 3.0 + gap * 2.0 + 12.0;
        let b = if roomy { 28.0 } else { ((avail - gap * 2.0) / 3.0).clamp(16.0, 28.0) };
        let y = line_y + (SIDEBAR_TRAY_H - b) / 2.0;
        if roomy {
            let right = (strip - SIDEBAR_TAB_INSET - 4.0 - b).max(left);
            return Some((line_y, (left, y, b, b), (right - 4.0 - b, y, b, b), (right, y, b, b)));
        }
        Some((
            line_y,
            (left, y, b, b),
            (left + b + gap, y, b, b),
            (left + (b + gap) * 2.0, y, b, b),
        ))
    }
    /// 사이드바 토글 버튼 rect(논리 px).
    ///
    /// 자리가 상태에 따라 갈린다. 접혀 있으면 신호등 오른쪽 — 열 것이 아직 없으니
    /// 창의 버튼이다. 펴져 있으면 사이드바 자신의 오른쪽 위 — 닫는 버튼은 닫힐
    /// 판 위에 있어야 무엇을 닫는지가 자리로 설명된다. 사이드바를 좁게 끌면
    /// 신호등을 침범하니 거기서 멈춘다.
    pub(crate) fn sidebar_toggle_rect(&self) -> (f32, f32, f32, f32) {
        let w = 26.0;
        let h = 22.0;
        #[cfg(not(windows))]
        let home = TRAFFIC_LIGHT_WIDTH + 6.0;
        // Windows is frameless with no traffic-light cluster to clear — start
        // the toggles at the left edge instead of reserving the macOS width.
        #[cfg(windows)]
        let home = 10.0;
        let y = (TITLE_HEIGHT - h) / 2.0;
        if self.tabs_on_top || !self.sidebar_visible {
            return (home, y, w, h);
        }
        ((self.tab_strip_w() - SIDEBAR_TAB_INSET - w).max(home), y, w, h)
    }
    /// 파일트리 토글 rect. 사이드바 토글을 따라다닌다 — 접혀 있으면 그 오른쪽,
    /// 펴져 있으면 사이드바 밖(본문 쪽 첫 자리)이다. 토글이 판 안으로 들어간
    /// 마당에 트리 버튼까지 넣으면 세션 목록 머리가 버튼 줄이 된다.
    pub(crate) fn file_tree_toggle_rect(&self) -> (f32, f32, f32, f32) {
        let (sx, sy, sw, sh) = self.sidebar_toggle_rect();
        if self.tabs_on_top {
            return (sx, sy, sw, sh);
        }
        if self.sidebar_visible {
            return (self.tab_strip_w() + 10.0, sy, sw, sh);
        }
        (sx + sw + 2.0, sy, sw, sh)
    }
    /// Windows-only frameless window controls (minimize / maximize / close),
    /// parked at the right end of the title strip. macOS keeps the native
    /// traffic lights, so this exists only where we drop OS decorations.
    /// Returns `[minimize, maximize, close]` left→right; close is the
    /// right-most so it lands where Windows users reach for it. Same chip
    /// size as the sidebar toggle to read as one button family.
    #[cfg(windows)]
    pub(crate) fn win_control_rects(win_w_logical: f32) -> [(f32, f32, f32, f32); 3] {
        let w = 26.0;
        let h = 22.0;
        let gap = 2.0;
        let right_pad = 8.0;
        let y = (TITLE_HEIGHT - h) / 2.0;
        let close_x = win_w_logical - right_pad - w;
        let max_x = close_x - gap - w;
        let min_x = max_x - gap - w;
        [(min_x, y, w, h), (max_x, y, w, h), (close_x, y, w, h)]
    }
    /// Show/hide the left window-tab sidebar. The cell grid reflows to the
    /// new usable width (every layout calc reads `effective_sidebar_w()`),
    /// so we just flip the flag, resize the PTYs to the new cols/rows, and
    /// repaint.
    /// 편집 중이면 버퍼를 방 이름으로 확정한다. **빈 문자열이면 override 를 지워**
    /// 기본 라벨(캐릭터 이름)로 되돌린다 — 빈 이름을 저장하면 방이 무명이 된다.
    pub(crate) fn commit_room_rename(&mut self) {
        let Some((idx, mut buf)) = self.room_rename.editing.take() else { return };
        // 조합 중이던 마지막 글자도 이름의 일부다 — 안 흘리면 "가나다" 를 치고
        // Enter 를 눌렀을 때 "가나" 만 남는다.
        if let Some(tail) = self.hangul.flush() {
            buf.push_str(&tail);
        }
        self.end_room_rename_ime();
        let name = buf.trim().to_string();
        if name.is_empty() {
            self.window_name_override.remove(&idx);
        } else {
            self.window_name_override.insert(idx, name);
        }
        self.window_labels_at = None;
        self.mark_room_label_dirty();
    }

    /// 편집을 버린다(Esc).
    pub(crate) fn cancel_room_rename(&mut self) {
        if self.room_rename.editing.take().is_some() {
            let _ = self.hangul.flush();
            self.end_room_rename_ime();
            self.window_labels_at = None;
            self.mark_room_label_dirty();
        }
    }

    /// 편집이 끝났으니 조합 상태를 걷는다. `ime_focus` 를 비워 두지 않으면 다음에
    /// pane 으로 치는 한글이 `ime_retarget` 에서 사라진 편집칸으로 흘러간다.
    fn end_room_rename_ime(&mut self) {
        if matches!(self.ime_focus, Some(crate::ImeFocus::RoomRename(_))) {
            self.ime_focus = None;
        }
        self.preedit.clear();
        self.in_preedit = false;
    }

    /// 조합이 끝난 글자를 커서 자리에 넣는다(`ime_retarget` 도 여기로 흘린다).
    pub(crate) fn room_rename_insert(&mut self, text: &str) {
        let cursor = &mut self.room_rename.cursor;
        if let Some((_, buf)) = self.room_rename.editing.as_mut() {
            crate::lineedit::insert(buf, cursor, text);
            self.chrome_dirty = true;
        }
    }

    /// 편집 중인 방에 키를 넣는다. 처리했으면 true — 호출부가 그 키를 pane 으로
    /// 흘리지 않게 한다(편집 중 타이핑이 셸에 새면 안 된다).
    ///
    /// **한글은 자체 조합기(`self.hangul`)를 태운다.** macOS 는 OS IME 를 꺼 두고
    /// (`set_ime_allowed(false)`) 자모를 `KeyboardInput.text` 로 직접 받으므로, 여기서
    /// 조합하지 않으면 "안녕"이 "ㅇㅏㄴㄴㅕㅇ"으로 박힌다 — 거노: "이름 바꾸는 거
    /// 이상한데". git 커밋 칸(`git_commit_input`)이 같은 이유로 같은 경로를 탄다.
    pub(crate) fn room_rename_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        use winit::keyboard::{Key, NamedKey};
        let Some(idx) = self.room_rename.editing.as_ref().map(|(i, _)| *i) else { return false };
        if crate::input::is_modifier_key(event) {
            return true;
        }
        // Cmd/Ctrl 조합은 삼키되 버퍼엔 안 넣는다. 앞단에서 안 잡힌 조합(Cmd+C 등)이
        // 여기 오면 글자만 박히고, 흘려보내면 편집 중인데 셸이 그 키를 먹는다.
        if self.modifiers.super_key() || self.modifiers.control_key() {
            return true;
        }
        self.ime_retarget(crate::ImeFocus::RoomRename(idx));
        #[cfg(target_os = "macos")]
        if let Some(t) = &event.text {
            if let Some(c) = t.chars().next().filter(|_| t.chars().count() == 1) {
                if (0x3130..=0x318F).contains(&(c as u32)) {
                    if let Some(done) = self.hangul.feed(c) {
                        self.room_rename_insert(&done);
                    }
                    self.preedit = self.hangul.preedit().unwrap_or_default();
                    self.in_preedit = !self.preedit.is_empty();
                    self.mark_room_label_dirty();
                    return true;
                }
            }
        }
        // 조합 중이던 자모를 지우는 백스페이스가 먼저다 — 완성 글자를 지우기 전에
        // 조합기 안의 것부터 물린다.
        if matches!(event.logical_key, Key::Named(NamedKey::Backspace)) && self.hangul.backspace() {
            self.preedit = self.hangul.preedit().unwrap_or_default();
            self.in_preedit = !self.preedit.is_empty();
            self.mark_room_label_dirty();
            return true;
        }
        if let Some(flushed) = self.hangul.flush() {
            self.room_rename_insert(&flushed);
        }
        self.preedit.clear();
        self.in_preedit = false;
        let cursor = &mut self.room_rename.cursor;
        let act = match self.room_rename.editing.as_mut() {
            Some((_, buf)) => crate::lineedit::key(buf, cursor, &event.logical_key),
            None => crate::lineedit::LineEditAction::Ignored,
        };
        match act {
            crate::lineedit::LineEditAction::Submit => self.commit_room_rename(),
            crate::lineedit::LineEditAction::Cancel => self.cancel_room_rename(),
            _ => {}
        }
        self.mark_room_label_dirty();
        true
    }

    /// 방 라벨을 이번 프레임에 다시 짓게 한다. `refresh_window_labels` 는 1초 캐시라
    /// 이걸 안 깨면 **타이핑이 1초씩 뭉쳐 나온다**(거노: "버벅여"). 편집 중인 방의
    /// 라벨은 캐시 밖에서 버퍼로 덮으므로 재계산 자체는 안 돌지만, 편집을 끝낸 뒤
    /// 원래 이름으로 돌아가려면 캐시를 한 번 비워야 한다.
    fn mark_room_label_dirty(&mut self) {
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    pub(crate) fn toggle_sidebar(&mut self) {
        self.sidebar_visible = !self.sidebar_visible;
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Show/hide the file-tree column. Same reflow path as `toggle_sidebar`.
    pub(crate) fn toggle_file_tree(&mut self) {
        self.file_tree.visible = !self.file_tree.visible;
        if self.file_tree.visible {
            self.refresh_file_tree();
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Drop a word off the end of the input buffer (for Ctrl-W /
    /// Alt-Backspace): eat trailing spaces, then non-spaces.
    pub(crate) fn buf_pop_word(&mut self) {
        while self.input_buf.ends_with(' ') {
            self.input_buf.pop();
        }
        while let Some(c) = self.input_buf.chars().last() {
            if c == ' ' {
                break;
            }
            self.input_buf.pop();
        }
    }
    /// Recompute the inline suggestion against the live grid. Called once
    /// per frame from the render path (so the grid reflects the latest
    /// shell echo). Only runs at a shell prompt (active pane, not
    /// alt-screen).
    ///
    /// Two ways to find the editable command line:
    ///   1. **OSC 133 mark** (primary) — the shell's precmd hook emits a
    ///      `B` mark at prompt end; pty-backend tags the cursor there. We
    ///      read the grid from that column to the cursor, which is the
    ///      ground truth: it survives Tab-completion, paste, RPROMPT and
    ///      wide (CJK) chars that the typed-buffer heuristic can't see.
    ///   2. **typed buffer** (fallback) — when there's no usable mark yet
    ///      (tmux backend, pre-first-prompt, or a scrolled-away mark), we
    ///      trust `input_buf` but only if it's still the tail of the
    ///      cursor row, which auto-suppresses on edits we can't track.
    pub(crate) fn update_suggestion(&mut self) {
        if !self.autosuggest.enabled() || !self.preedit.is_empty() {
            self.current_suggestion = None;
            return;
        }
        let line: Option<String> = {
            let ws = self.ws.lock().unwrap();
            match ws.active().and_then(|p| p.term()) {
                Some(t) if !t.alt_screen => {
                    let crow = t.cursor_row as usize;
                    let ccol = t.cursor_col as usize;
                    let row_cells = t.cells.get(crow);
                    let cell_str = |r: &[GridCell], from: usize, to: usize| -> String {
                        r.iter()
                            .take(to)
                            .skip(from)
                            .map(|c| if c.ch == '\0' { ' ' } else { c.ch })
                            .collect()
                    };
                    // Primary: OSC 133 mark still on the cursor's row.
                    let from_mark = match t.prompt_end {
                        Some((pr, pc))
                            if pr as usize == crow && (pc as usize) <= ccol =>
                        {
                            row_cells.map(|r| cell_str(r, pc as usize, ccol))
                        }
                        _ => None,
                    };
                    if from_mark.is_some() {
                        from_mark
                    } else if !self.input_buf.is_empty() {
                        let synced = row_cells
                            .map(|r| cell_str(r, 0, ccol).ends_with(&self.input_buf))
                            .unwrap_or(false);
                        synced.then(|| self.input_buf.clone())
                    } else {
                        None
                    }
                }
                _ => None,
            }
        };
        let Some(line) = line else {
            self.current_suggestion = None;
            return;
        };
        // Nothing to complete from an empty / whitespace-only line.
        if line.trim().is_empty() {
            self.current_suggestion = None;
            return;
        }
        self.autosuggest.maybe_refresh();
        self.current_suggestion = self.autosuggest.suggest(&line);
    }
    /// Build banner shown bottom-right on launch: `v<pkg>·<git rev>`
    /// (rev carries a trailing '+' when built dirty). Stamped at compile
    /// time by build.rs.
    pub(crate) fn version_label() -> String {
        format!(
            "v{}·{}",
            env!("CARGO_PKG_VERSION"),
            env!("KASATERM_GIT_REV")
        )
    }
    /// 0.0..1.0 opacity for the launch banner: solid through
    /// VERSION_HOLD_MS, then a linear fade across VERSION_FADE_MS, then
    /// gone. Also the single source of truth for "is the banner still
    /// animating" (alpha > 0).
    pub(crate) fn version_alpha(&self) -> f32 {
        let e = self.version_anim_start.elapsed().as_millis();
        if e < VERSION_HOLD_MS {
            1.0
        } else if e < VERSION_HOLD_MS + VERSION_FADE_MS {
            1.0 - (e - VERSION_HOLD_MS) as f32 / VERSION_FADE_MS as f32
        } else {
            0.0
        }
    }
    /// 0.0..1.0 opacity for the "복사됨" copy toast: solid for a brief hold
    /// after a block copy, then a quick fade. Mirrors `version_alpha`.
    pub(crate) fn copy_toast_alpha(&self) -> f32 {
        const HOLD: u128 = 900;
        const FADE: u128 = 500;
        let Some(at) = self.copy_toast_at else { return 0.0 };
        let e = at.elapsed().as_millis();
        if e < HOLD {
            1.0
        } else if e < HOLD + FADE {
            1.0 - (e - HOLD) as f32 / FADE as f32
        } else {
            0.0
        }
    }
    /// 0.0..1.0 opacity for a collab completion toast: a longer hold than the
    /// copy toast (a sibling finishing is worth a real glance) then a fade.
    /// Returns 0 with no active toast, so callers gate paint + frame-loop wake.
    pub(crate) fn collab_toast_alpha(&self) -> f32 {
        const HOLD: u128 = 2400;
        const FADE: u128 = 600;
        let Some((_, at)) = self.collab.toast.as_ref() else { return 0.0 };
        // 승인 토스트(칩 포함)는 사용자가 응답하거나 프롬프트가 풀릴 때까지
        // 고정 — 시간 페이드 없음. (해제는 route_approval_prompts/클릭 핸들러)
        if self.collab.toast_action.is_some() {
            return 1.0;
        }
        let e = at.elapsed().as_millis();
        if e < HOLD {
            1.0
        } else if e < HOLD + FADE {
            1.0 - (e - HOLD) as f32 / FADE as f32
        } else {
            0.0
        }
    }
    /// Copy a detected code block's text to the clipboard and arm the
    /// toast. Reuses arboard like `copy_selection`. Best-effort: a
    /// clipboard failure just logs (the toast still fires on success).
    pub(crate) fn copy_block_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        match arboard::Clipboard::new() {
            Ok(mut cb) => {
                if let Err(e) = cb.set_text(text.to_string()) {
                    eprintln!("[kasaterm] clipboard write failed: {e}");
                    return;
                }
            }
            Err(e) => {
                eprintln!("[kasaterm] clipboard open failed: {e}");
                return;
            }
        }
        self.copy_toast_at = Some(Instant::now());
    }
    /// Open the session panel in its own OS window. Mirrors open_git_panel:
    /// the page polls `/sessions` on the MCP server. Best-effort — any failure
    /// just logs and leaves the terminal untouched.
    pub(crate) fn open_session_panel(&mut self, event_loop: &ActiveEventLoop) {
        if self.session_panel_window.is_some() {
            return;
        }
        let port = mcp_panel_port();
        let attrs = WindowAttributes::default()
            .with_title("sessions")
            .with_theme(Some(Theme::Dark))
            .with_inner_size(LogicalSize::new(260.0, 360.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[session-panel] window create failed: {e}");
                return;
            }
        };
        let html = SESSION_PANEL_HTML
            .replace("__PORT__", &port)
            .replace("__TOKEN__", kasa_mcp::session_token());
        // build_as_child for the same use-after-free reason as the git panel.
        let webview = match wry::WebViewBuilder::new()
            .with_html(html)
            .with_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
                size: wry::dpi::LogicalSize::new(260.0, 360.0).into(),
            })
            .build_as_child(window.as_ref())
        {
            Ok(wv) => wv,
            Err(e) => {
                eprintln!("[session-panel] webview build failed: {e}");
                return;
            }
        };
        eprintln!("[session-panel] open; polling 127.0.0.1:{port}/sessions");
        self.session_panel_window = Some(window);
        self.session_panel_webview = Some(webview);
    }
    /// Toggle the session panel from the menu: close if open, open if not.
    pub(crate) fn toggle_session_panel(&mut self, event_loop: &ActiveEventLoop) {
        if self.session_panel_window.is_some() {
            // Drop the webview before the window it borrows from.
            self.session_panel_webview = None;
            self.session_panel_window = None;
        } else {
            self.open_session_panel(event_loop);
        }
    }
    /// Open the board panel in its own OS window. Mirrors open_session_panel:
    /// the page polls `/board` on the MCP server. Best-effort — any failure
    /// just logs and leaves the terminal untouched.
    pub(crate) fn open_board_panel(&mut self, event_loop: &ActiveEventLoop) {
        if self.board_panel_window.is_some() {
            return;
        }
        let port = mcp_panel_port();
        let attrs = WindowAttributes::default()
            .with_title("board")
            .with_theme(Some(Theme::Dark))
            .with_inner_size(LogicalSize::new(320.0, 440.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[board-panel] window create failed: {e}");
                return;
            }
        };
        let html = BOARD_PANEL_HTML
            .replace("__PORT__", &port)
            .replace("__TOKEN__", kasa_mcp::session_token());
        // build_as_child for the same use-after-free reason as the git panel.
        let webview = match wry::WebViewBuilder::new()
            .with_html(html)
            .with_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
                size: wry::dpi::LogicalSize::new(320.0, 440.0).into(),
            })
            .build_as_child(window.as_ref())
        {
            Ok(wv) => wv,
            Err(e) => {
                eprintln!("[board-panel] webview build failed: {e}");
                return;
            }
        };
        eprintln!("[board-panel] open; polling 127.0.0.1:{port}/board");
        self.board_panel_window = Some(window);
        self.board_panel_webview = Some(webview);
    }
    /// Toggle the board panel from the menu: close if open, open if not.
    pub(crate) fn toggle_board_panel(&mut self, event_loop: &ActiveEventLoop) {
        if self.board_panel_window.is_some() {
            // Drop the webview before the window it borrows from.
            self.board_panel_webview = None;
            self.board_panel_window = None;
        } else {
            self.open_board_panel(event_loop);
        }
    }
    /// Open the arona full UI in its own OS window. Unlike the
    /// HTML-string panels this loads the arona-ui dist over the MCP HTTP
    /// server (`/arona-ui/`) — same-origin with the API the page fetches, and
    /// the in-window wry embed is off the table anyway (Metal layer conflict).
    pub(crate) fn open_arona_panel(&mut self, event_loop: &ActiveEventLoop) {
        // shim OFF(순정 모드)면 아로나 GUI 자체를 안 띄운다 — board/미러 hook 이 전무해
        // 빈 웹뷰가 뜨는 어색함 방지(arona_btn_rect 도 None 이라 버튼부터 숨겨진다).
        // 이건 전역 shim 축 — 아래 방 모드 게이트 부재와는 다른 결이다.
        if !crate::socket::read_shim_inject() {
            return;
        }
        if self.arona_panel_window.is_some() {
            return;
        }
        // (방) 모드 게이트 없음: solo/미설정 방에서도 창은 연다. 모드 안내·전환은
        // 웹 쪽 ModePicker 담당(GET /mode 로 분기) — 네이티브가 차단하면
        // ModePicker 에 도달 자체가 불가한 설계 모순이었다(거노 실측).
        let port = mcp_panel_port();
        // 처음부터 보이게 띄운다 — 배경을 교실 다크톤으로 칠해(아래 with_background_color)
        // 흰 플래시가 없고, 무엇보다 webview 로드가 실패해도(포트 stale 등) 창이 영영
        // 숨겨지는 단일 실패점을 없앤다. 옛 "Finished 후에만 set_visible(true)"는 로드가
        // 안 끝나면 "버튼 눌러도 안 열림"이 됐다(멀티 인스턴스 포트 race). 완료 시 focus 만.
        let attrs = WindowAttributes::default()
            .with_title("아로나 — 샬레 교실")
            .with_theme(Some(Theme::Dark))
            .with_visible(true)
            .with_inner_size(LogicalSize::new(1100.0, 720.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[arona-panel] window create failed: {e}");
                return;
            }
        };
        // launch 별 캐시버스트 — webview 가 옛 index.html 을 캐시해도 새 URL 이라
        // 무조건 새로 받는다(서버 no-store 와 이중 방어). relaunch 마다 값이 바뀜.
        let cb = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        // 로드 끝나면 포커스를 주는 핸들러(창은 이미 보임). webview2 가 UI(메인)
        // 스레드에서 콜백하므로 winit 호출이 안전하다.
        let win_show = window.clone();
        // 이미지 드롭은 HTML5 onDrop(입력창/패널)이 처리한다 — dragover preventDefault 로
        // WKWebView 가 드롭을 웹콘텐츠에 넘겨 ondrop+files 가 뜬다. wry 네이티브
        // drag_drop_handler 를 설치하면 그게 드롭을 가로채(active_pty 로 오배송) HTML 경로를
        // 막아 첨부가 안 됐다(거노 실측) → 설치 안 함.
        // build_as_child for the same use-after-free reason as the git panel.
        let webview = match wry::WebViewBuilder::new()
            .with_url(format!("http://127.0.0.1:{port}/arona-ui/?v={cb}"))
            // 로딩 중 노출되는 빈 배경을 교실 다크톤으로 — 흰 플래시 제거.
            .with_background_color((20, 22, 28, 255))
            .with_on_page_load_handler(move |event, _url| {
                if matches!(event, wry::PageLoadEvent::Finished) {
                    win_show.focus_window();
                }
            })
            .with_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
                size: wry::dpi::LogicalSize::new(1100.0, 720.0).into(),
            })
            .build_as_child(window.as_ref())
        {
            Ok(wv) => Some(wv),
            Err(e) => {
                // webview 생성 실패해도 window 는 살려둔다(아래 항상 저장) — 옛 `return`
                // 은 로컬 window 를 drop 해 "검은창 떴다 사라짐"을 냈다. 실패 원인이
                // 보이게 창 제목을 에러로 바꾸고 webview 는 None 으로 둔다.
                eprintln!("[arona-panel] webview build failed: {e}");
                window.set_title(&format!("아로나 — 웹뷰 로드 실패: {e}"));
                None
            }
        };
        eprintln!("[arona-panel] open; http://127.0.0.1:{port}/arona-ui/");
        self.arona_panel_window = Some(window);
        self.arona_panel_webview = webview;
        // BA GUI 는 세션을 건드리지 않는다(거노 06-17 방향전환: "시각 레이어"). 자동 통솔
        // 자체가 폐기됐다(솔로 확정 06-18) — 아로나/SCHALE OS 는 관찰·시각 레이어일 뿐
        // 세션을 통솔하지 않는다(활성 pane 승격 호출 제거).
        // 제품 동작: 교실(BA UI)과 터미널을 둘 다 띄워 나란히 연동한다 — BA UI 의
        // 포커스/입력/상태가 메인 터미널 창과 양방향으로 묶인다. 옛 "교실이 화면을
        // 인수(터미널 숨김)"는 KASATERM_ARONA_SOLO_VIEW 몰입 옵션으로 강등.
        if std::env::var_os("KASATERM_ARONA_SOLO_VIEW").is_some() {
            if let Some(w) = &self.window {
                w.set_visible(false);
            }
        }
    }
    /// Close the arona window and bring the hidden main terminal back. The
    /// single close path — menu toggle, the window's X button, and
    /// `POST /arona-close` (ModePicker "터미널로") all route here so none of
    /// them can forget the reveal and strand the terminal hidden. No-op when
    /// the window isn't open.
    pub(crate) fn close_arona_panel(&mut self) {
        if self.arona_panel_window.is_none() {
            return;
        }
        // Drop the webview before the window it borrows from.
        self.arona_panel_webview = None;
        self.arona_panel_window = None;
        // 교실에서 나옴 — 숨겨둔 메인 터미널 창 복귀(+숨김 동안 못 받은
        // redraw 직접 청구).
        if let Some(w) = &self.window {
            w.set_visible(true);
            w.focus_window();
            w.request_redraw();
        }
        eprintln!("[arona-panel] closed; terminal revealed");
    }
    /// 설정 화면의 웹뷰 판. 별도 OS 창 + `/arona-ui/settings.html` 을 MCP HTTP 로
    /// 로드한다 — same-origin 이라 페이지의 fetch 가 CORS 없이 붙고, POST 는
    /// `origin_guard_mw`(Router::layer)가 새 라우트까지 자동으로 보호한다.
    /// 이미 열려 있으면 포커스만 주고 `true`.
    ///
    /// **`set_ime_allowed` 를 부르지 않는다.** 네이티브 설정창은 macOS 에서 OS IME
    /// 를 끄고 in-process 조합기를 쓰지만(auxwin.rs `spawn_aux_settings`), 그건 GPU
    /// 폼의 텍스트 편집용이다. 웹뷰에 그걸 걸면 WKWebView 가 제 IME 로 받아야 할
    /// 한글 조합을 끊어 이행의 목적을 정확히 무효화한다 — 아로나 창도 같은 이유로
    /// 안 부르고, 거기서 한글 입력이 이미 프로덕션으로 돌고 있다.
    /// 학생 세부설정 창 — `/arona-ui/settings.html?student=<slug>&theme=<id>`.
    ///
    /// 설정 본체와 **따로** 뜬다. 본체가 앱 안으로 들어가면 세부는 밖에 있어야 하고,
    /// 그때 이 창이 그 자리를 맡는다(거노 2026-08-25 「세부설정을 별도창으로」).
    ///
    /// 창은 하나만 유지한다 — 이미 떠 있으면 주소만 갈아 끼운다. 학생마다 창을 열면
    /// 같은 로스터를 고치는 창이 여럿 떠서 어느 쪽이 정본인지 알 수 없게 된다.
    pub(crate) fn open_student_web_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        slug: &str,
        theme: &str,
    ) -> bool {
        if slug.trim().is_empty() {
            return false;
        }
        let port = mcp_panel_port();
        let cb = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let url = format!(
            "http://127.0.0.1:{port}/arona-ui/settings.html?v={cb}&student={}&theme={}",
            urlencode(slug),
            urlencode(theme)
        );
        // 이미 떠 있으면 주소만 바꾸고 앞으로 가져온다.
        if let (Some(w), Some(wv)) = (
            self.student_web_window.as_ref(),
            self.student_web_webview.as_ref(),
        ) {
            let _ = wv.load_url(&url);
            w.focus_window();
            return true;
        }
        if !settings_web_reachable(&port) {
            eprintln!("[student-web] 페이지에 못 닿는다");
            return false;
        }
        let attrs = WindowAttributes::default()
            .with_title("학생 설정")
            .with_theme(Some(Theme::Dark))
            .with_visible(true)
            // 본체(920×720)보다 좁다 — 한 학생의 폼과 그림만 서므로 가로가 덜 든다.
            .with_inner_size(LogicalSize::new(560.0, 720.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[student-web] window create failed: {e}");
                return false;
            }
        };
        let win_show = window.clone();
        let webview = match wry::WebViewBuilder::new()
            .with_url(&url)
            .with_background_color((27, 37, 65, 255))
            .with_on_page_load_handler(move |event, _url| {
                if matches!(event, wry::PageLoadEvent::Finished) {
                    win_show.focus_window();
                }
            })
            .with_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
                size: wry::dpi::LogicalSize::new(560.0, 720.0).into(),
            })
            // 아로나 패널과 같은 use-after-free 사유로 build_as_child.
            .build_as_child(window.as_ref())
        {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[student-web] webview build failed: {e}");
                return false;
            }
        };
        self.student_web_window = Some(window);
        self.student_web_webview = Some(webview);
        eprintln!("[student-web] open; student={slug} theme={theme}");
        true
    }

    pub(crate) fn open_settings_web_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        cat: Option<crate::SettingsCat>,
    ) -> bool {
        if let Some(w) = self.settings_web_window.as_ref() {
            w.focus_window();
            // 이미 떠 있으면 URL 을 다시 못 쓴다 — 다시 로드하면 입력 중이던 값이
            // 날아간다. 열려 있는 화면을 그 자리에서 돌린다.
            if let (Some(c), Some(wv)) = (cat, self.settings_web_webview.as_ref()) {
                let _ = wv.evaluate_script(&format!(
                    "window.__ktSetCat && window.__ktSetCat('{}')",
                    c.web_key()
                ));
            }
            return true;
        }
        // 페이지가 실제로 서빙되는지 **창을 만들기 전에** 묻는다. 웹뷰는 404 를 받아도
        // 빌드에 성공하므로, 빌드 성공을 「열렸다」로 읽으면 `web/arona-ui/dist` 가 없는
        // 체크아웃에서 **오류 페이지가 뜬 창**이 설정 자리를 차지한다 — 그러면 설정을
        // 아예 못 연다. 여기서 false 를 주면 부른 쪽이 네이티브 화면으로 떨어진다.
        if !settings_web_reachable(&mcp_panel_port()) {
            eprintln!("[settings-web] 페이지에 못 닿는다 — 네이티브 설정으로 간다");
            return false;
        }
        // 포트가 **확실하지 않을 때만** 제목에 박는다. `mcp_panel_port` 은 8765 폴백을
        // 가지고 있어 멀티 인스턴스에서 남의 프로세스를 가리킬 수 있고, 설정은 파일을
        // 쓰므로 그때는 어디에 말하는지 보여야 한다. 다만 늘 띄우면 평소에 지저분하고
        // (거노 2026-08-25 「그거 주소안나오게해봐」) 정작 위험한 순간에도 늘 있던
        // 글자라 눈에 안 띈다 — 경고는 드물어야 경고다.
        let (port, certain) = crate::mcp_panel_port_certain();
        let title = if certain {
            "설정".to_string()
        } else {
            format!("설정 — 127.0.0.1:{port} (포트 불확실)")
        };
        let attrs = WindowAttributes::default()
            .with_title(title)
            .with_theme(Some(Theme::Dark))
            .with_visible(true)
            // 네이티브 설정창과 같은 치수 — 나란히 스샷 비교(Step 4)가 목적이다.
            .with_inner_size(LogicalSize::new(920.0, 720.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("[settings-web] window create failed: {e}");
                return false;
            }
        };
        // launch 별 캐시버스트 — WKWebView 가 옛 settings.html+JS 를 캐시해도 새
        // URL 이라 무조건 새로 받는다(서버 no-store 와 이중 방어).
        let cb = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let win_show = window.clone();
        let drop_proxy = self.proxy.clone();
        let webview = match wry::WebViewBuilder::new()
            .with_url(format!(
                "http://127.0.0.1:{port}/arona-ui/settings.html?v={cb}{}",
                cat.map(|c| format!("&cat={}", c.web_key())).unwrap_or_default()
            ))
            .with_background_color((27, 37, 65, 255))
            // 테마 zip 을 이 창에 떨어뜨리면 받는다. 웹뷰가 창을 덮고 있으면 winit 의
            // `DroppedFile` 이 창까지 오지 않아(네이티브 설정 창 경로와 갈린다) 여기서
            // 잡아야 한다. 핸들러는 `Fn(..) -> bool` 이라 `&mut self` 에 못 닿으므로
            // 소켓 스레드와 같은 방식으로 GUI 에 위임한다.
            //
            // zip 이 아니면 false — 설정 페이지가 제 드롭을 쓰게 될 때 여기서 먹어
            // 버리지 않도록. zip 은 이 앱에서 테마 말고 쓸 데가 없어 모호하지 않다.
            .with_drag_drop_handler(move |e| match e {
                wry::DragDropEvent::Drop { paths, .. } => {
                    let mut took = false;
                    for p in paths {
                        if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("zip")) {
                            let _ = drop_proxy.send_event(UserEvent::ImportTheme(p));
                            took = true;
                        }
                    }
                    took
                }
                _ => false,
            })
            .with_on_page_load_handler(move |event, _url| {
                if matches!(event, wry::PageLoadEvent::Finished) {
                    win_show.focus_window();
                }
            })
            .with_bounds(wry::Rect {
                position: wry::dpi::LogicalPosition::new(0.0, 0.0).into(),
                size: wry::dpi::LogicalSize::new(920.0, 720.0).into(),
            })
            // 아로나 패널과 같은 use-after-free 사유로 build_as_child.
            .build_as_child(window.as_ref())
        {
            Ok(wv) => wv,
            Err(e) => {
                // 창을 접고 false 를 준다 — 부른 쪽이 네이티브 설정을 연다.
                //
                // 예전엔 빈 창을 살려 두고 실패 원인을 제목에 띄운 뒤 `true` 를 줬다.
                // 웹뷰가 기본 OFF 이던 시절엔 그게 나았다(일부러 켠 사람에게 원인을
                // 보여주는 것이 목적이고, 창이 떴다 사라지면 그마저 못 본다). 기본이
                // ON 이 된 지금은 정반대다 — 그 빈 창이 설정 자리를 차지해 **설정을
                // 아예 못 열게** 만든다. 창이 한 번 깜빡이는 대가로 옛 화면이 뜬다.
                eprintln!("[settings-web] webview build failed: {e} — 네이티브 설정으로 간다");
                drop(window);
                return false;
            }
        };
        eprintln!("[settings-web] open; http://127.0.0.1:{port}/arona-ui/settings.html");
        self.settings_web_window = Some(window);
        self.settings_web_webview = Some(webview);
        true
    }

    /// 설정 웹뷰 창의 단일 닫기 경로 — 창 X 버튼(`window_event` 가드)과 메뉴/토글이
    /// 모두 여기로 온다. webview 를 그것이 빌린 window 보다 먼저 drop 한다.
    pub(crate) fn close_settings_web_window(&mut self) {
        if self.settings_web_window.is_none() {
            return;
        }
        self.settings_web_webview = None;
        self.settings_web_window = None;
        eprintln!("[settings-web] closed");
    }

    /// 거노: "터미널 보기"를 누르면 화면을 2분할 — 터미널(왼쪽)·아로나 교실(오른쪽).
    /// 두 네이티브 창을 현재 모니터 작업영역의 좌/우 절반에 타일링한다. 둘 다 떠
    /// 있을 때만 의미가 있어, 아로나 창이 없으면(순수 터미널) no-op.
    pub(crate) fn tile_terminal_arona_split(&self) {
        let (Some(term), Some(arona)) = (self.window.as_ref(), self.arona_panel_window.as_ref())
        else {
            return;
        };
        // 사용자가 보고 있는 화면 기준 — 떠 있는 아로나 창의 모니터.
        let Some(monitor) = arona.current_monitor().or_else(|| term.current_monitor()) else {
            return;
        };
        let mpos = monitor.position(); // 가상 데스크톱 물리좌표(멀티모니터 오프셋)
        let msize = monitor.size(); // 모니터 해상도(물리 px)
        // macOS 상단 메뉴바를 가리지 않게 인셋. 다른 OS는 0.
        let top_inset: i32 = if cfg!(target_os = "macos") {
            (28.0 * monitor.scale_factor()) as i32
        } else {
            0
        };
        let half_w = (msize.width / 2) as i32;
        let usable_h = ((msize.height as i32 - top_inset).max(200)) as u32;
        let y = mpos.y + top_inset;
        // 왼쪽: 터미널(frameless 라 inner≈outer).
        term.set_outer_position(winit::dpi::PhysicalPosition::new(mpos.x, y));
        let _ = term.request_inner_size(winit::dpi::PhysicalSize::new(half_w as u32, usable_h));
        term.set_visible(true);
        term.request_redraw();
        // 오른쪽: 아로나 교실(타이틀바 높이만큼 아래로 밀려도 허용).
        arona.set_outer_position(winit::dpi::PhysicalPosition::new(mpos.x + half_w, y));
        let _ = arona.request_inner_size(winit::dpi::PhysicalSize::new(
            msize.width - half_w as u32,
            usable_h,
        ));
    }
    /// BA GUI 버튼: 없으면 열고, 뒤/최소화 상태면 앞으로 가져오고, 이미 맨 앞이면 닫는다.
    /// 순수 토글이던 시절엔 창이 다른 창 뒤로 내려가 있어도 버튼이 "있음→닫기"라 두 번
    /// 눌러야 다시 떴다(거노: 내려간 창 다시 누르면 꺼져 불편). has_focus 로 분기.
    pub(crate) fn toggle_arona_panel(&mut self, event_loop: &ActiveEventLoop) {
        // 기본: arona-ui 를 기본 웹브라우저 탭으로 연다(거노 06-26). wry 임베드(별도 OS
        // 창)는 Metal layer 충돌을 피하려던 우회였고 지금은 비활성 — KASATERM_ARONA_WRY=1
        // 로만 복귀한다. 버튼·메뉴·키보드 3경로가 다 이 함수를 거치므로 여기서만 분기.
        if std::env::var_os("KASATERM_ARONA_WRY").is_none() {
            self.open_arona_in_browser();
            return;
        }
        if self.arona_panel_window.is_none() {
            self.open_arona_panel(event_loop);
            return;
        }
        // 떠 있음: 맨 앞이면 닫고, 뒤/숨김/최소화면 앞으로(raise+focus). borrow 를 블록에
        // 가둬 self.close_arona_panel(&mut self) 와 충돌하지 않게 focus 여부만 빼낸다.
        let focused = {
            let w = self.arona_panel_window.as_ref().unwrap();
            let f = w.has_focus();
            if !f {
                w.set_minimized(false);
                w.set_visible(true);
                w.focus_window();
            }
            f
        };
        if focused {
            self.close_arona_panel();
        }
    }

    /// arona-ui 를 기본 브라우저 탭으로 연다. MCP HTTP 서버가 같은 포트로 `/arona-ui/`
    /// 와 API 를 동일 origin 서빙하므로 페이지 fetch 가 그대로 동작한다. 캐시버스트(`?v=`)는
    /// 안 붙인다 — 같은 URL 이면 브라우저가 기존 탭을 재사용할 수 있다(중복 탭 방지).
    pub(crate) fn open_arona_in_browser(&self) {
        let port = mcp_panel_port();
        let url = format!("http://127.0.0.1:{port}/arona-ui/");
        open_url_in_browser(&url);
        eprintln!("[arona-browser] open {url}");
    }

    /// 거노: 새 방(윈도우) + 첫 pane 캐릭터 지정. 방별 collab 격리로 room slug 를
    /// 셸 env(KASATERM_ROOM)로 주입하고(spawn_session_pane 이 pending_room 을 읽음),
    /// 첫 pane 캐릭터를 지정값으로 강제한다(pending_character). 사용자가 그 pane 에서
    /// claude 를 치면 shim 이 persona·session-id 를 입히고, 추가 split pane 은 랜덤 배정.
    pub(crate) fn new_room_with_character(&mut self, character: &str) {
        let room = format!("room-{}", self.next_room_seq);
        self.next_room_seq += 1;
        self.pending_room = Some(room);
        self.pending_character = Some(character.to_string()); // 첫 pane = 지정 캐릭터
        self.new_window();
        // 좌측 방 라벨 = 선택 캐릭터 이름(방 구분 시각 라벨).
        self.window_name_override
            .insert(self.active_window, format!("● {character}"));
    }
    /// Effective render scale = DPI scale × whole-UI zoom. Everything that
    /// converts logical↔physical (cell metrics, chrome coords, cursor px,
    /// window→cols) routes through this so a single `ui_zoom` change scales
    /// the entire UI uniformly.
    pub(crate) fn effective_scale(&self) -> f32 {
        let dpi = self
            .window
            .as_ref()
            .map(|w| w.scale_factor() as f32)
            .unwrap_or(1.0);
        dpi * self.ui_zoom
    }
    /// "빠른 파일" 고정 섹션 목록: (라벨, 경로, 아이콘 이름). ① 개인 CLAUDE.md
    /// (~/.claude/CLAUDE.md) 는 항상, ② 프로젝트 CLAUDE.md(트리 root/CLAUDE.md)·
    /// ③ 프로젝트 메모리(root/.memory/MEMORY.md, symlink 허용→exists) 는 있을 때만.
    /// codex 짝(개인 ~/.codex/AGENTS.md · 프로젝트 root/AGENTS.md)도 있을 때만 넣는다 —
    /// codex pane 도 claude 처럼 자기 지시 파일을 한 번에 열게.
    /// ⚠️ 아이콘 "codex" 는 codex.svg 가 아직 없으면 gpu.rs match 에서 None 으로
    /// 빠져 아이콘만 안 뜬다(빌드는 안 깨진다). svg 들어오면 gpu.rs 에 arm 추가 필요.
    pub(crate) fn quick_files(&self) -> Vec<(&'static str, std::path::PathBuf, &'static str)> {
        let mut out: Vec<(&'static str, std::path::PathBuf, &'static str)> = Vec::new();
        if let Some(home) = kasa_socket::home_dir() {
            out.push(("개인 CLAUDE.md", home.join(".claude/CLAUDE.md"), "claude"));
            let agents = home.join(".codex/AGENTS.md");
            if agents.exists() {
                out.push(("개인 AGENTS.md", agents, "codex"));
            }
        }
        if let Some(root) = self.file_tree.root.as_ref() {
            let proj = root.join("CLAUDE.md");
            if proj.exists() {
                out.push(("프로젝트 CLAUDE.md", proj, "claude"));
            }
            let agents = root.join("AGENTS.md");
            if agents.exists() {
                out.push(("프로젝트 AGENTS.md", agents, "codex"));
            }
            let mem = root.join(".memory/MEMORY.md");
            if mem.exists() {
                out.push(("프로젝트 메모리", mem, "braces"));
            }
        }
        // 오토메모리 볼트 — 세션마다 자동으로 실리는 **루트 인덱스**와, 지금 열어 둔
        // 프로젝트의 **폴더 인덱스**. 루트는 상한이 있어 폴더 목록만 담고 토픽 훅은
        // 폴더 인덱스에 있으므로, 둘을 같이 걸어야 실제로 읽을 것이 손에 닿는다.
        // 폴더 이름은 프로젝트 폴더 이름과 같을 때만 맞춘다 — 추측해서 엉뚱한 폴더를
        // 걸면 「내 메모리가 아닌 것」이 열려 더 나쁘다.
        if let Some(vault) = memory_vault_dir() {
            let root_idx = vault.join("MEMORY.md");
            if root_idx.exists() {
                out.push(("오토메모리", root_idx, "braces"));
            }
            if let Some(name) = self
                .file_tree
                .root
                .as_ref()
                .and_then(|r| r.file_name())
                .and_then(|s| s.to_str())
            {
                let folder_idx = vault.join(name).join(format!("{name}.md"));
                if folder_idx.exists() {
                    out.push(("오토메모리 · 이 프로젝트", folder_idx, "braces"));
                }
            }
        }
        out
    }
    /// Adjust the whole-UI zoom by `delta` (additive on the multiplier).
    /// Clamped to a sane range; chrome + sidebar + every pane scale together.
    pub(crate) fn change_ui_zoom(&mut self, delta: f32) {
        let new = (self.ui_zoom + delta).clamp(0.5, 3.0);
        if (new - self.ui_zoom).abs() < 0.01 {
            return;
        }
        self.ui_zoom = new;
        self.persist_ui_zoom();
        self.apply_effective_scale();
    }
    /// Reset whole-UI zoom to native (1.0).
    ///
    /// 되돌리기도 **저장한다.** 안 그러면 100% 가 "고른 값"이 아니라 "값 없음"과
    /// 구별되지 않아, 넓은 모니터에서 일부러 100% 로 되돌린 다음 실행에서
    /// 자동 추정이 다시 키워 버린다.
    pub(crate) fn reset_ui_zoom(&mut self) {
        if (self.ui_zoom - 1.0).abs() < 0.01 && !self.ui_zoom_unset {
            return;
        }
        self.ui_zoom = 1.0;
        self.persist_ui_zoom();
        self.apply_effective_scale();
    }
    /// 지금 배율을 `settings.json` 에 적고, 자동 추정 자격을 내린다.
    /// 사람이 한 번이라도 고른 뒤에는 앱이 배율을 넘겨짚지 않는다.
    pub(crate) fn persist_ui_zoom(&mut self) {
        self.ui_zoom_unset = false;
        // 검증 실행은 설정 파일을 공유하므로 쓰지 않는다 — `save_window_frame`
        // 과 같은 이유다(하네스가 띄운 창 값이 거노 앱의 다음 배율이 되면 안 된다).
        if crate::verification_run() {
            return;
        }
        crate::socket::write_setting("ui_zoom", serde_json::json!(self.ui_zoom));
    }
    /// Push the current effective scale into the GPU renderer and reflow the
    /// cell grid + PTY size. Shared by zoom changes and (future) DPI
    /// scale-factor changes when the window moves between monitors.
    pub(crate) fn apply_effective_scale(&mut self) {
        let eff = self.effective_scale();
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.set_scale(eff);
            let (cw, ch) = gpu.set_font_size(self.font_size);
            self.cell = CellGeom { w: cw, h: ch, baseline: 0.0 };
        }
        if self.window.is_some() {
            let (cols, rows) = self.window_cells();
            self.resize_backend(cols, rows);
            self.chrome_dirty = true;
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
    }
    /// Rebuild everything the renderer derives from the display, without
    /// touching a single PTY. Cmd+Shift+R and the pane ⋮ menu's rotate icon.
    ///
    /// This exists because a terminal is the one app you cannot just restart
    /// to fix — every pane in it is live work. The automatic invalidation
    /// (`set_scale` → oversample + repack, `maintain_atlas` on a full atlas)
    /// should make this unnecessary; it is here for the display state we
    /// failed to notice, so a wrong-looking window is never a dead end.
    ///
    /// Order matters: re-seat the NSView onto the window's content rect,
    /// reconfigure the swapchain to the size we are actually at, re-derive
    /// scale/font metrics/PTY grid from the *current* monitor, and queue the
    /// atlas repack last so it lands at the next frame boundary with the new
    /// scale in place.
    ///
    /// The view step is the one that matters for a window that came back wrong
    /// from another monitor — see `gpu::ensure_view_fills_window`. It must run
    /// first: everything below reads `inner_size()`, which is derived from the
    /// view, so a shrunken view would quietly poison all of it. The swapchain
    /// must then be re-jammed with the size read *now*, not with the stored
    /// config — a refresh is needed precisely when that config is what drifted.
    pub(crate) fn refresh_renderer(&mut self) {
        if let Some(w) = self.window.as_ref() {
            gpu::ensure_view_fills_window(w);
            // 뷰가 멀쩡한데도 화면이 어긋나 새로고침을 누르는 경우가 있다 —
            // 레이어 backing scale 이 옛 모니터에 남은 상태. 아래 resize 로
            // drawable 을 다시 잡기 전에 짝부터 맞춰 둔다.
            gpu::ensure_layer_scale_matches(w);
            let size = w.inner_size();
            if let Some(gpu) = self.gpu.as_mut() {
                gpu.resize(size.width, size.height);
            }
        }
        self.apply_effective_scale();
        if let Some(gpu) = self.gpu.as_mut() {
            gpu.force_atlas_reset();
        }
        self.chrome_dirty = true;
        if let Ok(mut ws) = self.ws.lock() {
            for pane in ws.panes.values_mut() {
                pane.dirty = true;
            }
        }
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
        self.set_toast("화면 새로고침".to_string());
    }
    /// Adjust the focused pane's font multiplier (pane-local zoom). Only that
    /// pane's glyphs + PTY grid change; the BSP layout and other panes stay
    /// put. Delta is additive on the multiplier; clamped to a sane range.
    pub(crate) fn change_pane_font(&mut self, delta: f32) {
        let Some(active) = self.target_pane() else { return };
        let cur = self.pane_font_scales.get(&active).copied().unwrap_or(1.0);
        let new = (cur + delta).clamp(0.5, 3.0);
        if (new - cur).abs() < 0.01 {
            return;
        }
        self.pane_font_scales.insert(active, new);
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// Reset the focused pane's font multiplier to match the rest of the UI.
    pub(crate) fn reset_pane_font(&mut self) {
        let Some(active) = self.target_pane() else { return };
        if self.pane_font_scales.remove(&active).is_none() {
            return;
        }
        let (cols, rows) = self.window_cells();
        self.resize_backend(cols, rows);
        self.chrome_dirty = true;
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }
    /// True when the cursor block should be visible this frame.
    /// Solid for `BLINK_PAUSE_AFTER_INPUT_MS` after any input event, then
    /// toggles every `BLINK_HALF_PERIOD_MS`.
    pub(crate) fn cursor_blink_on(&self, now: Instant) -> bool {
        // Debug: KASATERM_NOBLINK=1 keeps the cursor solid so a
        // screenshot can verify cursor position/visibility without
        // racing the blink phase.
        if std::env::var_os("KASATERM_NOBLINK").is_some() {
            return true;
        }
        let since_input = now.saturating_duration_since(self.last_input_at);
        if since_input.as_millis() < BLINK_PAUSE_AFTER_INPUT_MS as u128 {
            return true;
        }
        let elapsed = since_input.as_millis() - BLINK_PAUSE_AFTER_INPUT_MS as u128;
        (elapsed / BLINK_HALF_PERIOD_MS as u128) % 2 == 0
    }
    /// "Host modifier" chord that opens the kasaterm shortcut layer
    /// (split / close / focus / copy-paste). macOS conventions reserve
    /// Cmd for this; Windows and Linux terminals overwhelmingly use
    /// Ctrl+Shift instead so Ctrl+letter stays free to deliver control
    /// bytes to the shell.
    pub(crate) fn host_mod(&self) -> bool {
        if cfg!(target_os = "macos") {
            self.modifiers.super_key()
        } else {
            self.modifiers.control_key() && self.modifiers.shift_key()
        }
    }
    /// Secondary modifier that flips a host shortcut into its alternate
    /// behavior (e.g. `Cmd+Shift+D` = stacked split on macOS). The host
    /// chord on Windows/Linux already owns Shift, so Alt fills the same
    /// role there.
    pub(crate) fn host_mod_alt(&self) -> bool {
        if cfg!(target_os = "macos") {
            self.modifiers.shift_key()
        } else {
            self.modifiers.alt_key()
        }
    }
    /// The foreground job name of `pid`'s session if it's something other than
    /// a plain login shell — what makes closing it worth a confirmation.
    /// `None` when the pane is idle at a shell prompt or has no session.
    pub(crate) fn pid_busy(&self, pid: &str) -> Option<String> {
        let name = self.pty.get(pid)?.active_process_name()?;
        if name.is_empty() || is_shell_name(&name) {
            None
        } else {
            Some(decorate_process_name(&name))
        }
    }

    /// 그 pane 을 **통째로** 닫을 때 걸리는 작업 — 탭 하나가 아니라 전부를 본다.
    ///
    /// `pid_busy` 를 탭 하나에만 물으면 두 가지를 놓친다. ①옆 탭에서 도는 claude
    /// ②`pid` 가 비어 있는 탭 — 이미지·마크다운 탭의 `pid` 는 `None` 이고, 호출부가
    /// `and_then` 이라 그러면 바쁨 검사가 통째로 건너뛰어져 **확인 없이 닫힌다**.
    /// 2026-08-18 에 `ws.panes` 항목 자체가 없는 케이스는 막았는데, 항목은 있고
    /// `pid` 만 비는 이 케이스가 같은 구멍으로 남아 있었다.
    ///
    /// **leaf id 자체가 그 pane 의 primary pid** 라 탭 목록이 비어도 판정이 선다
    /// (`confirm_or_close_tab` 의 `None` 분기·layout.rs `leaf_cells` 와 같은 근거).
    pub(crate) fn pane_busy(&self, pane: &str) -> Option<String> {
        let mut pids: Vec<String> = vec![pane.to_string()];
        {
            let ws = self.ws.lock().unwrap();
            if let Some(p) = ws.panes.get(pane) {
                pids.extend(p.tabs.iter().filter_map(|t| t.pid.clone()));
            }
        }
        pids.iter().find_map(|p| self.pid_busy(p))
    }
    /// First running job across every pane/tab — drives the window-close
    /// confirmation ("close the whole app while claude is mid-run?").
    fn any_pane_busy(&self) -> Option<String> {
        let pids: Vec<String> = {
            let ws = self.ws.lock().unwrap();
            ws.panes
                .values()
                .flat_map(|p| p.tabs.iter().filter_map(|t| t.pid.clone()))
                .collect()
        };
        pids.iter().find_map(|p| self.pid_busy(p))
    }
    /// First running job in sidebar session `idx` only — drives the per-session
    /// close confirm (sidebar tab ×). Mirrors `close_window`'s layout pick: the
    /// active window's tree lives in `pty_layout`, the rest in `windows[idx]`.
    fn window_busy(&self, idx: usize) -> Option<String> {
        let layout = if idx == self.active_window {
            self.pty_layout.as_ref()
        } else {
            self.windows.get(idx).and_then(|w| w.as_ref())
        }?;
        let pids: Vec<String> = {
            let ws = self.ws.lock().unwrap();
            layout
                .leaves()
                .iter()
                .filter_map(|leaf| ws.panes.get(*leaf))
                .flat_map(|p| p.tabs.iter().filter_map(|t| t.pid.clone()))
                .collect()
        };
        pids.iter().find_map(|p| self.pid_busy(p))
    }
    /// Cmd+W / header ×: close tab `idx` of `pane`. A multi-tab pane drops just
    /// that tab; the last tab drops the pane (no-op on a single-pane window, so
    /// we skip it there and leave the OS close button to quit). If the tab is
    /// running a real job, raise the confirm modal instead of closing now.
    pub(crate) fn confirm_or_close_tab(&mut self, pane: &str, idx: usize) {
        let (tabs_len, pid) = {
            let ws = self.ws.lock().unwrap();
            match ws.panes.get(pane) {
                Some(p) => (p.tabs.len(), p.tabs.get(idx).and_then(|t| t.pid.clone())),
                // PaneState 가 **없는 게 정상**인 pane 이 있다 — split leaf 는 보조 탭이
                // 생길 때까지 `ws.panes` 에 안 들어간다(main.rs `pane_font_scales` 주석이
                // 같은 사실을 말한다). 여기서 return 하면 그런 pane 은 Cmd+W 가 통째로
                // 죽는다(거노: "커맨드 W 해도 무반응"). 항목이 없다 = 탭 하나짜리 pane.
                //
                // ⚠️ pid 를 `None` 으로 두면 안 된다 — 아래 바쁨 검사가 `and_then` 이라
                // 통째로 건너뛰어져 **claude 가 도는 pane 이 확인 없이 닫힌다**
                // (2026-08-18 "pane닫기도 클로드켜져있는데 그냥닫혀버려"). 그리고 학생
                // pane 은 대개 split 으로 만들어 첫 출력 전까지 여기 없으므로, 하필
                // 가장 자주 닫는 pane 들이 전부 무방비였다.
                //
                // **leaf id 자체가 그 pane 의 primary pid 다**(layout.rs `leaf_cells`
                // 주석과 같은 근거) — ws.panes 를 안 거쳐도 알 수 있다. 같은 함정을
                // resize 가 먼저 밟아 leaf_cells 기반으로 고쳤는데, 닫기 쪽엔 그대로
                // 남아 있었다.
                None => (1, Some(pane.to_string())),
            }
        };
        let action = if tabs_len > 1 {
            PendingClose::Tab { pane: pane.to_string(), idx }
        } else {
            let leaves = self.pty_layout.as_ref().map_or(0, |t| t.leaves().len());
            if leaves <= 1 {
                // 이 방의 마지막 pane. 방이 여럿이면 **방을 닫는 것**으로 잇는다 —
                // 전에는 여기서 그냥 return 이라 Cmd+W 가 죽은 키였다(거노).
                // 방이 하나뿐이면 그건 앱 종료라 OS 닫기 버튼에 맡기고 no-op.
                let idx = self.active_window;
                if self.windows.len() <= 1 {
                    return;
                }
                let action = PendingClose::Session(idx);
                // 바쁨·미저장이 있으면 그쪽 대화가 무엇을 잃는지까지 말해 주므로 먼저다.
                if self.guard_dirty(&action) {
                    return;
                }
                match self.window_busy(idx) {
                    Some(proc) => self.open_confirm_close(proc, action),
                    None => self.raise_confirm(ConfirmClose { why: CloseWhy::LastPane, action }),
                }
                return;
            }
            PendingClose::Pane { pane: pane.to_string() }
        };
        if self.guard_dirty(&action) {
            return;
        }
        // 탭 하나를 닫는 건 그 탭만 걸리지만, pane 으로 승격됐으면 그 안의 탭이
        // 전부 걸린다 — 그걸 탭 하나로 판정하면 옆 탭에서 도는 claude 를 놓친다.
        let busy = match &action {
            PendingClose::Pane { pane } => self.pane_busy(pane),
            _ => pid.as_deref().and_then(|p| self.pid_busy(p)),
        };
        match busy {
            Some(proc) => self.open_confirm_close(proc, action),
            None => self.do_close(action),
        }
    }

    /// ⋮ 메뉴의 × — **pane 을 통째로** 닫는다. ⌘W(`confirm_or_close_tab`)와 달리
    /// 탭 단위가 아니라서 승격 로직 없이 바로 `PendingClose::Pane` 이고, 바쁨 판정도
    /// pane 전체(`pane_busy`)다.
    ///
    /// 이 경로는 `close_pane` 직행이라 **확인이 통째로 없었다**(거노 2026-08-20
    /// 「pane 종료할 때도 바로 꺼지네, 안 물어보고. 클로드 도는데」). 헤더 우측
    /// 클러스터엔 × 가 없어서 **헤더 없는 split pane 의 유일한 닫기 버튼이 이
    /// 무방비 경로**였고, 학생 pane 은 대개 헤더 없는 split 이라 하필 가장 자주 쓰는
    /// 닫기가 무방비였다. ⌘W 로 시험하면 멀쩡히 물어봐서 여태 안 드러났다.
    pub(crate) fn confirm_or_close_pane(&mut self, pane: &str) {
        let action = PendingClose::Pane { pane: pane.to_string() };
        if self.guard_dirty(&action) {
            return;
        }
        match self.pane_busy(pane) {
            Some(proc) => self.open_confirm_close(proc, action),
            None => self.do_close(action),
        }
    }
    /// CloseRequested (red light / Cmd+Q): returns true when a job is running
    /// and the confirm modal was raised — the caller must NOT exit yet. Returns
    /// false when nothing's running, so the caller exits immediately.
    pub(crate) fn confirm_or_close_window(&mut self) -> bool {
        if self.guard_dirty(&PendingClose::Window) {
            return true;
        }
        match self.any_pane_busy() {
            Some(proc) => {
                self.open_confirm_close(proc, PendingClose::Window);
                true
            }
            None => false,
        }
    }
    /// Sidebar session (window `idx`) close: raise the confirm modal if any pane
    /// in that session is running a job, else close it now. The app stays open —
    /// this is the per-session path, distinct from the whole-app quit above.
    pub(crate) fn confirm_or_close_session(&mut self, idx: usize) {
        if self.guard_dirty(&PendingClose::Session(idx)) {
            return;
        }
        match self.window_busy(idx) {
            Some(proc) => self.open_confirm_close(proc, PendingClose::Session(idx)),
            None => self.do_close(PendingClose::Session(idx)),
        }
    }
    fn open_confirm_close(&mut self, proc: String, action: PendingClose) {
        self.raise_confirm(ConfirmClose { why: CloseWhy::Busy(proc), action });
    }
    fn raise_confirm(&mut self, dlg: ConfirmClose) {
        self.confirm_close = Some(dlg);
        self.chrome_dirty = true;
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Every unsaved editor that `action` would destroy, with the file name to
    /// show. Empty means nothing would be lost.
    fn dirty_docs(&self, action: &PendingClose) -> Vec<(DirtyDoc, String)> {
        // 별도창 편집기는 pane 트리 밖에 산다 — 앱 종료와 그 창 자체를 닫을
        // 때만 걸린다.
        let aux = |want: Option<winit::window::WindowId>| -> Vec<(DirtyDoc, String)> {
            self.aux_windows
                .iter()
                .filter(|a| want.is_none_or(|w| a.window.id() == w))
                .filter_map(|a| {
                    let m = a.editor().filter(|m| m.modified)?;
                    Some((DirtyDoc::Aux(a.window.id()), doc_name(&m.doc.path)))
                })
                .collect()
        };
        let panes: Vec<String> = match action {
            PendingClose::Tab { pane, idx } => {
                let ws = self.ws.lock().unwrap();
                return ws
                    .panes
                    .get(pane)
                    .and_then(|p| p.tabs.get(*idx))
                    .and_then(|t| t.markdown().filter(|m| m.modified))
                    .map(|m| {
                        vec![(
                            DirtyDoc::Tab { pane: pane.clone(), tab: *idx },
                            doc_name(&m.doc.path),
                        )]
                    })
                    .unwrap_or_default();
            }
            PendingClose::Pane { pane } => vec![pane.clone()],
            PendingClose::Session(i) => {
                let layout = if *i == self.active_window {
                    self.pty_layout.as_ref()
                } else {
                    self.windows.get(*i).and_then(|w| w.as_ref())
                };
                layout.map_or_else(Vec::new, |t| t.leaves().iter().map(|l| l.to_string()).collect())
            }
            PendingClose::AuxEditor(id) => return aux(Some(*id)),
            // 앱 종료는 세션 전부 + 별도창 전부.
            PendingClose::Window => {
                let mut all: Vec<String> = self
                    .windows
                    .iter()
                    .flatten()
                    .chain(self.pty_layout.as_ref())
                    .flat_map(|t| t.leaves().into_iter().map(|l| l.to_string()))
                    .collect();
                all.sort();
                all.dedup();
                all
            }
        };
        let ws = self.ws.lock().unwrap();
        let mut out: Vec<(DirtyDoc, String)> = panes
            .iter()
            .filter_map(|id| Some((id, ws.panes.get(id)?)))
            .flat_map(|(id, p)| {
                p.tabs.iter().enumerate().filter_map(move |(t, tab)| {
                    let m = tab.markdown().filter(|m| m.modified)?;
                    Some((DirtyDoc::Tab { pane: id.clone(), tab: t }, doc_name(&m.doc.path)))
                })
            })
            .collect();
        drop(ws);
        if matches!(action, PendingClose::Window) {
            out.extend(aux(None));
        }
        out
    }

    /// Raise the unsaved-changes dialog if `action` would throw work away.
    /// Returns true when the caller must stop and wait for the answer.
    pub(crate) fn guard_dirty(&mut self, action: &PendingClose) -> bool {
        let docs = self.dirty_docs(action);
        if docs.is_empty() {
            return false;
        }
        // 별도창을 닫으려는데 확인은 메인 창에 뜬다 — 안 띄우면 사용자는 창이
        // 그냥 안 닫히는 것으로 본다.
        if matches!(action, PendingClose::AuxEditor(_)) {
            if let Some(w) = &self.window {
                w.focus_window();
            }
        }
        self.raise_confirm(ConfirmClose { why: CloseWhy::Dirty(docs), action: action.clone() });
        true
    }

    /// Save every listed editor. False means at least one write failed — the
    /// caller must abort the close rather than lose those edits.
    pub(crate) fn save_dirty_docs(&mut self, docs: &[(DirtyDoc, String)]) -> bool {
        let mut ok = true;
        for (doc, name) in docs {
            let Some((text, path)) = self.doc_text(doc) else { continue };
            if let Err(e) = crate::markdown::write_atomic(&path, &text) {
                eprintln!("[editor] 저장 실패 {path}: {e}");
                self.set_toast(format!("⚠ {name} 저장 실패: {e}"));
                ok = false;
                continue;
            }
            self.mark_doc_clean(doc);
        }
        ok
    }

    /// Drop the listed editors' changes. Clearing `modified` is what makes the
    /// close go through — the re-entered guard then sees nothing to lose.
    pub(crate) fn discard_dirty_docs(&mut self, docs: &[(DirtyDoc, String)]) {
        for (doc, _) in docs {
            self.mark_doc_clean(doc);
        }
    }

    /// Write every editor whose typing has gone quiet for the autosave delay,
    /// and report when the next one comes due so the caller can park a timer
    /// on it (the loop sleeps completely when idle — without a deadline the
    /// last edit would sit unwritten until something else woke us).
    ///
    /// Silent by design: no toast, and a failure only logs. Autosave the user
    /// didn't ask for shouldn't interrupt them; the unsaved dot stays up and
    /// the close guard still catches it, which is the honest signal.
    pub(crate) fn run_editor_autosave(&mut self) -> Option<Instant> {
        let Some(delay) = self.set_autosave else { return None };
        // "저장 / 저장 안 함" 을 묻는 중에 몰래 쓰면 '저장 안 함' 이 거짓말이 된다.
        // 대화창이 닫힐 때까지 미룬다(취소하면 그때 정상 만기로 다시 걸린다).
        if matches!(
            self.confirm_close.as_ref().map(|c| &c.why),
            Some(CloseWhy::Dirty(_))
        ) {
            return None;
        }
        let now = Instant::now();
        let mut next: Option<Instant> = None;
        // (문서 위치, 마지막 타자 시각) 을 먼저 모은다 — 저장은 ws 락 밖에서.
        let mut ready: Vec<DirtyDoc> = Vec::new();
        {
            let ws = self.ws.lock().unwrap();
            for (id, pane) in ws.panes.iter() {
                for (t, tab) in pane.tabs.iter().enumerate() {
                    let Some(at) = tab.markdown().and_then(|m| m.edited_at) else { continue };
                    if now.duration_since(at) >= delay {
                        ready.push(DirtyDoc::Tab { pane: id.clone(), tab: t });
                    } else {
                        let due = at + delay;
                        next = Some(next.map_or(due, |n: Instant| n.min(due)));
                    }
                }
            }
        }
        for a in self.aux_windows.iter() {
            let Some(at) = a.editor().and_then(|m| m.edited_at) else { continue };
            if now.duration_since(at) >= delay {
                ready.push(DirtyDoc::Aux(a.window.id()));
            } else {
                let due = at + delay;
                next = Some(next.map_or(due, |n: Instant| n.min(due)));
            }
        }
        if !ready.is_empty() {
            self.save_dirty_docs_quiet(&ready);
            self.chrome_dirty = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        next
    }

    /// `save_dirty_docs` without the failure toast — see `run_editor_autosave`.
    fn save_dirty_docs_quiet(&mut self, docs: &[DirtyDoc]) {
        for doc in docs {
            let job = self.doc_text(doc);
            let Some((text, path)) = job else { continue };
            match crate::markdown::write_atomic(&path, &text) {
                Ok(()) => self.mark_doc_clean(doc),
                Err(e) => eprintln!("[editor] 자동 저장 실패 {path}: {e}"),
            }
        }
    }

    fn doc_text(&self, doc: &DirtyDoc) -> Option<(String, String)> {
        match doc {
            DirtyDoc::Tab { pane, tab } => {
                let ws = self.ws.lock().unwrap();
                ws.panes
                    .get(pane)
                    .and_then(|p| p.tabs.get(*tab))
                    .and_then(|t| t.markdown())
                    .map(|m| (m.edit_lines.join("\n"), m.doc.path.clone()))
            }
            DirtyDoc::Aux(id) => self
                .aux_windows
                .iter()
                .find(|a| a.window.id() == *id)
                .and_then(|a| a.editor())
                .map(|m| (m.edit_lines.join("\n"), m.doc.path.clone())),
        }
    }

    fn mark_doc_clean(&mut self, doc: &DirtyDoc) {
        match doc {
            DirtyDoc::Tab { pane, tab } => {
                let mut ws = self.ws.lock().unwrap();
                let Some(p) = ws.panes.get_mut(pane) else { return };
                if let Some(m) = p.tabs.get_mut(*tab).and_then(|t| t.markdown_mut()) {
                    m.mark_saved();
                    // 미저장 점이 사라지려면 이 pane 이 다시 그려져야 한다.
                    p.dirty = true;
                }
            }
            DirtyDoc::Aux(id) => {
                let Some(a) = self.aux_windows.iter_mut().find(|a| a.window.id() == *id)
                else {
                    return;
                };
                if let Some(m) = a.editor_mut() {
                    m.mark_saved();
                    a.window.request_redraw();
                }
            }
        }
    }

    /// Run a non-window close action immediately. `Window` is left to the
    /// caller (it needs the event loop to exit).
    pub(crate) fn do_close(&mut self, action: PendingClose) {
        match action {
            PendingClose::Tab { pane, idx } => {
                self.close_tab(&pane, idx);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            PendingClose::Pane { pane } => self.close_pane(&pane),
            PendingClose::Session(idx) => {
                if let Err(e) = self.close_window(idx) {
                    eprintln!("[window] close failed: {e:#}");
                }
            }
            PendingClose::AuxEditor(id) => {
                if let Some(i) = self.aux_windows.iter().position(|a| a.window.id() == id) {
                    self.close_aux_window(i);
                }
            }
            PendingClose::Window => {}
        }
    }
}

/// File name for the dialog — the full path would blow the card's width.
fn doc_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string()
}

/// 그 상태가 **사람 손을 기다리는 중**인가.
///
/// 어휘가 둘로 갈려 있다: 화면 감지 경로는 `blocked` 을, 훅·board 경로는 `waiting` 을
/// 쓴다. 표시하는 쪽에서 둘을 가릴 이유가 없는데 **여섯 자리가 전부 `waiting` 만 보고
/// 있었고**, 정작 실제로 들어오는 값은 `blocked` 뿐이라(`route_approval_prompts` 가
/// `faces_user` 를 true 로 고정) 승인 대기가 화면 어디에도 안 그려졌다 — 핑크 테두리도
/// 사이드바 깜빡임도 방 탭 점도 전부 죽어 있었다(2026-08-11 조사).
///
/// 판정을 여기 한 벌로 둔다. 같은 조건을 여섯 군데 적어 두면 한쪽만 고쳐진다.
pub(crate) fn status_needs_you(status: &str) -> bool {
    status == "waiting" || status == "blocked"
}

/// 같은 열쇠의 알림은 이 창 안에서 한 번만 나간다.
const NOTIFY_DEDUP_WINDOW: std::time::Duration = std::time::Duration::from_secs(8);

/// 이 열쇠로 지금 알려도 되나 — 처음이면 true 를 주고 시각을 적어 둔다.
///
/// 승인 프롬프트 하나에 배너가 **두 번** 나가고 있었다: 훅 경로(`⚠ 권한 필요`)와 화면
/// 감지 경로(`⚠ 승인 필요`)가 서로를 모른 채 각자 쏜다. 두 경로를 합치는 대신 발사구에
/// 게이트를 두면 앞으로 경로가 늘어도 자동으로 걸린다. pane 이 여섯이면 시끄러움은
/// 배수로 늘고, 시끄러우면 사람은 알림을 꺼 버려 앞의 모든 표시가 같이 죽는다.
///
/// 열쇠는 **호출부가 정한다** — 완료 알림처럼 매번 떠야 하는 것은 열쇠를 안 준다.
/// 상태는 함수-로컬 static 이다(`struct App` 필드는 다른 pane 작업과 충돌한다).
fn notify_dedup_passes(key: &str) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;
    static SEEN: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    let Ok(mut g) = SEEN.get_or_init(Mutex::default).lock() else {
        return true; // 잠금이 깨졌으면 막지 않는다 — 알림을 삼키는 쪽이 더 나쁘다
    };
    let now = Instant::now();
    // 지나간 것은 그때그때 치운다 — 맵이 살아 있는 pane 수만큼만 남는다.
    g.retain(|_, t| now.duration_since(*t) < NOTIFY_DEDUP_WINDOW);
    if g.contains_key(key) {
        return false;
    }
    g.insert(key.to_string(), now);
    true
}

/// Raise a macOS desktop notification. Inside the signed `.app` bundle we use
/// `UNUserNotificationCenter` so the alert carries kasaterm's own app icon (and
/// gets the native sound/click affordances). The bare `cargo run` binary has no
/// bundle identifier and can't obtain notification authorization, so there we
/// fall back to `osascript` — which shows the Script Editor icon (dev-only).
///
/// `dedup` 을 주면 그 열쇠로 8초 게이트를 탄다(같은 일에 두 경로가 쏘는 경우).
///
/// 플랫폼 분기는 **안쪽**에 둔다 — 게이트를 바깥에 한 벌로 두려면 함수가 하나여야
/// 하고, 두 벌로 나누면 한쪽에만 게이트가 붙는 그 함정으로 곧장 돌아간다.
/// `route` 는 배너를 눌렀을 때 갈 자리 — `(pane id, 그때의 claude 세션 id)`. 세션까지
/// 싣는 이유는 surface id 가 재사용되기 때문이다(`macos_notify` 참조).
/// 배너를 만들 `ActiveEventLoop` 가 없는 자리에서 줄을 서는 곳.
///
/// `notify_desktop` 은 자유 함수라 `App` 에 직접 못 닿는다. 그렇다고 호출부마다
/// 배너를 따로 세우면 **서는 자리와 안 서는 자리가 갈린다** — 그게 실제로 이
/// 앱에서 벌어지고 있던 일이다(완료만 배너가 서고 승인 대기·계정 한도는 OS
/// 알림뿐이었다). 그래서 알림은 전부 이 큐 하나로 모으고, `handler` 가 매 틱
/// `ActiveEventLoop` 를 쥔 자리에서 걷어 창으로 만든다.
pub(crate) fn banner_inbox() -> &'static std::sync::Mutex<Vec<crate::notify_banner::BannerReq>> {
    static Q: std::sync::OnceLock<std::sync::Mutex<Vec<crate::notify_banner::BannerReq>>> =
        std::sync::OnceLock::new();
    Q.get_or_init(Default::default)
}

/// OS 알림(macOS 알림 센터)을 함께 쏠지 — **기본은 끔**이다.
///
/// 자체 서명 번들이라 `UNUserNotificationCenter` 등록이 거절되고
/// (`Notifications are not allowed for this application`), 그래서 osascript 로
/// 떨어지면 배너에 **스크립트 편집기 아이콘**이 붙는다. 우리 코드가 원인이
/// 아니라는 것까지 배제 실측으로 확인했다(`98b6502`: 53KB 최소 ObjC 앱도 같은
/// 오류). 거노 2026-08-21 「그럼 기본 알림은 꺼줘」 — 자체 배너가 같은 자리에서
/// 뜨므로 OS 알림은 중복이고, 게다가 남의 아이콘을 달고 뜬다.
///
/// **지우지 않고 끈 이유**: 지금 못 고치는 것이지 영영 아닌 게 아니다. 애플
/// 개발자 인증서가 생기면 아이콘·알림센터 누적·클릭 라우팅이 전부 제대로 서고,
/// 그때는 되살리는 것이 맞다. `KASATERM_OS_NOTIFY=1` 로 그 자리에서 켠다.
///
/// **끄면서 잃는 것**: 알림센터에 안 쌓이고 방해금지 연동이 없다. 그 자리는
/// 이미 다른 것들이 메운다 — `unread_panes`(못 본 완료)·Dock 배지·사이드바
/// 숨쉬기, 그리고 자체 배너. 넷 다 `handle_notify` 한 자리에서 함께 선다.
fn os_notify_enabled() -> bool {
    std::env::var("KASATERM_OS_NOTIFY").is_ok_and(|v| v == "1" || v == "true")
}

pub(crate) fn notify_desktop(
    title: &str,
    body: &str,
    character: Option<&str>,
    dedup: Option<&str>,
    route: Option<(&str, Option<&str>)>,
) {
    if dedup.is_some_and(|k| !notify_dedup_passes(k)) {
        return;
    }
    // 자체 배너가 정본이다. 여기가 모든 알림이 지나는 한 자리라, 이 줄 하나로
    // 완료·승인 대기·계정 한도가 전부 같은 모양으로 뜬다.
    banner_inbox().lock().unwrap().push((
        title.to_string(),
        body.to_string(),
        character.map(str::to_string),
        route.map(|(p, s)| (p.to_string(), s.map(str::to_string))),
    ));
    if !os_notify_enabled() {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        if is_bundled() {
            notify_native(title, body, character, route);
        } else {
            notify_osascript(title, body);
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (title, body, character, route);
    }
}

/// 알림에 붙일 그 학생의 프사 파일.
///
/// 이미지는 `include_bytes!` 로 바이너리에 박혀 있어 경로가 없는데, 첨부가 받는
/// 것은 **파일 URL 뿐**이다. 그래서 슬러그마다 한 번씩 임시 파일로 떨궈 두고 그
/// 경로를 재사용한다. 로스터에 없는 커스텀 캐릭터는 슬러그가 없어 None 이다.
#[cfg(target_os = "macos")]
fn student_profile_file(character: &str) -> Option<std::path::PathBuf> {
    let slug = crate::theme::character_slug(character)?;
    let path = std::env::temp_dir()
        .join("kasaterm-notify-icons")
        .join(format!("{slug}.png"));
    if path.exists() {
        return Some(path);
    }
    let png = crate::render::student_profile_png(slug)?;
    std::fs::create_dir_all(path.parent()?).ok()?;
    std::fs::write(&path, png).ok()?;
    Some(path)
}

/// 알림 권한 판정 — 0=아직 답 없음 · 1=허용 · 2=거부.
///
/// 전에는 `requestAuthorization` 의 콜백이 **빈 블록**이라 거부돼도 아무도 몰랐다:
/// 요청은 그대로 native 로 나가고 시스템이 조용히 버려, 화면에는 "알림이 안 온다"
/// 만 남았다. 답을 여기 남겨 두면 다음 알림부터 osascript 로 돌릴 수 있다.
#[cfg(target_os = "macos")]
static NOTIFY_AUTH: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// True when running from a `.app` bundle (has a `CFBundleIdentifier`). Native
/// `UNUserNotificationCenter` requires this; the bare binary returns `None`.
#[cfg(target_os = "macos")]
pub(crate) fn is_bundled() -> bool {
    objc2_foundation::NSBundle::mainBundle()
        .bundleIdentifier()
        .is_some()
}

/// Request alert/sound authorization once per process. The system shows the
/// permission prompt on first call; the grant persists across launches.
#[cfg(target_os = "macos")]
pub(crate) fn ensure_notification_authorization() {
    use objc2_user_notifications::{UNAuthorizationOptions, UNUserNotificationCenter};
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if !is_bundled() {
            return;
        }
        let handler = block2::RcBlock::new(
            |granted: objc2::runtime::Bool, err: *mut objc2_foundation::NSError| {
                let ok = granted.as_bool();
                NOTIFY_AUTH.store(
                    if ok { 1 } else { 2 },
                    std::sync::atomic::Ordering::Relaxed,
                );
                if !ok {
                    let why = unsafe { err.as_ref() }
                        .map(|e| e.localizedDescription().to_string())
                        .unwrap_or_else(|| "사유 없음".to_string());
                    eprintln!("[notify] 데스크톱 알림 권한 없음 — osascript 로 돌린다: {why}");
                }
            },
        );
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let opts = UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound;
        center.requestAuthorizationWithOptions_completionHandler(opts, &handler);
    });
}

#[cfg(target_os = "macos")]
fn notify_native(
    title: &str,
    body: &str,
    character: Option<&str>,
    route: Option<(&str, Option<&str>)>,
) {
    use objc2_foundation::{NSArray, NSString, NSURL};
    use objc2_user_notifications::{
        UNMutableNotificationContent, UNNotificationAttachment, UNNotificationRequest,
        UNNotificationSound, UNUserNotificationCenter,
    };
    ensure_notification_authorization();
    // Unique id per request so rapid completions don't replace each other.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // 눌렀을 때 갈 자리를 **identifier 에 실어** 보낸다. `userInfo`(NSDictionary)를
    // 쓰려면 objc2 의 키 타입 제약(`NSCopying`)에 맞춰 딕셔너리를 세워야 하는데, 여기
    // 필요한 건 짧은 문자열 둘뿐이라 그 무게를 질 이유가 없다.
    // 형식: `kasaterm-notify-{seq}|{pane}|{sid}` — pane id(`%116`)도 uuid 도 `|` 를
    // 안 쓴다. 받는 쪽은 `macos_notify::route_from_identifier`(UN·구식 공용).
    let route_ident = match route {
        Some((pane, sid)) => format!("kasaterm-notify-{seq}|{pane}|{}", sid.unwrap_or("")),
        None => format!("kasaterm-notify-{seq}"),
    };
    // 거부·미등록이 확정났으면 native 는 요청을 받아 놓고 버린다 — 그 자리에서
    // osascript 로 돌린다. 자체 서명 'kasaterm-dev' 번들은 등록 요청이
    // "Notifications are not allowed for this application" 으로 거절되므로
    // (2026-08-17 실측) 배포 실물에서는 사실상 늘 이 길이고, 그래서 배너에
    // 스크립트 편집기 아이콘이 붙는다. 구식 센터(NSUserNotification)로 앱
    // 아이콘을 실어 보려 했지만 **같은 검문에 조용히 버려졌다**(배달 예외도
    // 오류도 없이 알림 DB 에 기록이 안 남는 것을 격리 인스턴스 + 알림 DB 로
    // 실측) — 조용한 유실은 아이콘보다 나쁘므로 걷어냈다. 앱 아이콘을 실으려면
    // 애플 발급 인증서로 서명하거나 자체 배너 창을 그려야 한다.
    //
    // ★2026-08-21 재조사(같은 지적이 세 번째라 판정을 다시 쟀다). **원인이
    // 우리 쪽에 없다는 것까지 확인했다** — 다음 다섯을 하나씩 배제했으므로
    // 네 번째 조사는 이 목록을 지나 곧장 「서명」으로 가면 된다:
    //   ①실행 방식 — 번들 실행파일 직접 exec 도, `open` 으로 LaunchServices 를
    //     제대로 거친 것도 같은 오류(직접 exec 는 프로세스가 앱으로 등록되지
    //     않아 흔히 의심되는 자리인데, 여기선 범인이 아니다)
    //   ②번들 id 오염 — 같은 `com.kasa.kasaterm` 이 여섯 경로에 등록돼 있어
    //     유력해 보였지만, 번들 id 만 바꾼 사본도 똑같이 거절당했다
    //   ③서명 무결성 — `codesign --verify --deep --strict` 가 중첩
    //     Sparkle.framework·XPC·kasaterm-cli 까지 전부 통과한다(valid on disk,
    //     satisfies its Designated Requirement). 깨진 중첩 서명이 이 오류의
    //     흔한 원인이라 재 봤다
    //   ④요청 시점 — `resumed`(=applicationDidFinishLaunching)라 이미 정석이다
    //   ⑤TCC 잔재 — 한 번도 등록된 적이 없다(`ncprefs` 91개 중 kasaterm 없음).
    //     새 번들 id 는 기록 자체가 없는데도 같은 오류다
    // 결정타: **53KB 짜리 순수 ObjC 최소 앱**(NSApplication + delegate +
    // requestAuthorization 뿐, 같은 자체 서명, 새 id, `open` 실행)도 글자 그대로
    // 같은 오류를 받는다. 남은 변수는 TeamIdentifier(애플 발급 인증서) 하나뿐이다.
    // ⇒ 코드로 넘을 수 있는 벽이 아니다. 길은 둘: 애플 개발자 인증서로 서명하거나
    // (알림센터 누적·클릭 라우팅·아이콘을 통째로 되찾는다), 자체 배너 창을 그린다
    // (아이콘은 자유지만 알림센터에 안 쌓이고 방해금지 같은 OS 통합을 잃는다).
    if NOTIFY_AUTH.load(std::sync::atomic::Ordering::Relaxed) == 2 {
        notify_osascript(title, body);
        return;
    }
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(body));
    // 소리를 붙이지 않으면 **무음으로 배달된다**. 권한은 처음부터 `Alert | Sound` 로
    // 받아 두고 정작 콘텐츠에 안 달아, 창을 뒤로 물린 동안 학생이 끝나도 알 길이
    // dock 배지뿐이었다 — 그건 "봐야 보이는" 신호다(2026-08-11 조사).
    content.setSound(Some(&UNNotificationSound::defaultSound()));
    // 학생 프사를 오른쪽 썸네일로 — 알림이 여럿 겹쳐도 누구 것인지 그림으로 갈린다.
    // **왼쪽 작은 아이콘은 번들 아이콘 고정**이라 여기서 못 바꾼다(그건 앱 아이콘).
    if let Some(p) = character.and_then(student_profile_file) {
        let url = NSURL::fileURLWithPath(&NSString::from_str(&p.to_string_lossy()));
        let aid = NSString::from_str(&format!("kasaterm-icon-{seq}"));
        if let Ok(att) = unsafe {
            UNNotificationAttachment::attachmentWithIdentifier_URL_options_error(&aid, &url, None)
        } {
            content.setAttachments(&NSArray::from_retained_slice(&[att]));
        }
    }
    let ident = NSString::from_str(&route_ident);
    let request =
        UNNotificationRequest::requestWithIdentifier_content_trigger(&ident, &content, None);
    let center = UNUserNotificationCenter::currentNotificationCenter();
    // 배달이 실패하면(권한 회수·첨부 거부 등) 그 자리에서 osascript 로 돌린다.
    // 실패를 삼키면 "알림이 안 온다" 만 남고 이유는 어디에도 안 남는다.
    let (t, b) = (title.to_string(), body.to_string());
    let done = block2::RcBlock::new(move |err: *mut objc2_foundation::NSError| {
        if let Some(e) = unsafe { err.as_ref() } {
            eprintln!(
                "[notify] native 배달 실패 — osascript 로 돌린다: {}",
                e.localizedDescription()
            );
            notify_osascript(&t, &b);
        }
    });
    center.addNotificationRequest_withCompletionHandler(&request, Some(&done));
}

#[cfg(target_os = "macos")]
fn notify_osascript(title: &str, body: &str) {
    let script = format!(
        "display notification {} with title {}",
        applescript_quote(body),
        applescript_quote(title),
    );
    let _ = crate::proc::command("osascript")
        .arg("-e")
        .arg(script)
        .spawn();
}

/// Set (or clear, when 0) the Dock tile badge to the unread-notification count.
#[cfg(target_os = "macos")]
fn set_dock_badge(count: usize) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    use objc2_foundation::NSString;
    let Some(mtm) = MainThreadMarker::new() else { return };
    let app = NSApplication::sharedApplication(mtm);
    let label = (count > 0).then(|| NSString::from_str(&count.to_string()));
    app.dockTile().setBadgeLabel(label.as_deref());
}
#[cfg(not(target_os = "macos"))]
fn set_dock_badge(_count: usize) {}

#[cfg(not(target_os = "macos"))]
pub(crate) fn ensure_notification_authorization() {}

/// 설정 웹뷰 페이지가 지금 실제로 서빙되나 — 루프백에 300ms.
///
/// 웹뷰는 404 를 받아도 **빌드에 성공한다.** 그래서 빌드 성공만 보고 「열렸다」로
/// 판단하면 `web/arona-ui/dist` 를 안 만든 체크아웃에서 오류 페이지가 뜬 창이 설정
/// 자리를 차지한다. 그건 설정을 못 여는 것과 같다 — 그래서 창을 만들기 전에 묻는다.
///
/// HTTP 클라이언트를 새로 들이지 않고 std 로만 한다. 같은 프로세스 안의 루프백이라
/// 정상 경로는 1ms 도 안 걸리고, 서버가 죽어 있으면 connect 가 곧바로 거절된다.
/// 타임아웃은 그 둘 다 아닌 경우(포트를 다른 프로세스가 물고 응답을 안 함) 대비다.
/// 쿼리 값 인코딩. slug 는 대개 ASCII 지만 **커스텀 테마 폴더 이름에는 한글·공백이
/// 들어간다** — 그대로 실으면 주소가 깨져 창이 빈 화면으로 뜬다.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn settings_web_reachable(port: &str) -> bool {
    use std::io::{Read, Write};
    let Ok(addr) = format!("127.0.0.1:{port}").parse::<std::net::SocketAddr>() else {
        return false;
    };
    let to = std::time::Duration::from_millis(300);
    let Ok(mut s) = std::net::TcpStream::connect_timeout(&addr, to) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(to));
    let _ = s.set_write_timeout(Some(to));
    // HTTP/1.0 이라 서버가 응답 뒤 알아서 닫는다 — keep-alive 를 안 다뤄도 된다.
    if s.write_all(b"GET /arona-ui/settings.html HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")
        .is_err()
    {
        return false;
    }
    // 상태줄만 본다. "HTTP/1.1 200 ..." 의 9..12 가 코드다.
    let mut head = [0u8; 16];
    let mut n = 0;
    while n < head.len() {
        match s.read(&mut head[n..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => n += k,
        }
    }
    n >= 12 && head.starts_with(b"HTTP/1.") && &head[9..12] == b"200"
}

/// Wrap `s` in an AppleScript string literal, escaping `"` and `\` so a pane
/// title with quotes can't break out of the `display notification` command.
#[cfg(target_os = "macos")]
fn applescript_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// 기본 브라우저로 URL 열기 — macOS `open`, Windows `cmd /C start`, 그 외 `xdg-open`.
/// BA GUI 버튼이 arona-ui 를 외부 탭으로 띄울 때 쓴다(wry 임베드 비활성 대체).
pub(crate) fn open_url_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let _ = crate::proc::command("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let _ = crate::proc::command("cmd")
        .args(["/C", "start", "", url])
        .spawn();
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let _ = crate::proc::command("xdg-open").arg(url).spawn();
}

#[cfg(test)]
mod toast_tests {
    use super::*;

    // 완료 토스트 = 캐릭터(pane 고정) + hook title(완료 순간). title 앞 "✓ " 중복 제거.
    #[test]
    fn attention_toast_combines_character_and_reason() {
        assert_eq!(
            format_attention_toast(Some("미도리"), "권한 요청"),
            "⚠ 미도리 — 권한 요청"
        );
    }

    // 미현존/미배정 pane → 캐릭터 없이 hook title 만(정보 보존, 드롭 안 함).
    #[test]
    fn attention_toast_falls_back_when_no_character() {
        assert_eq!(
            format_attention_toast(None, "권한 요청"),
            "⚠ 권한 대기중 — 권한 요청"
        );
        assert_eq!(format_attention_toast(None, ""), "⚠ 권한 대기중");
    }

    // 권한 대기 토스트 — 캐릭터·reason 유무 4갈래.
    #[test]
    fn attention_toast_variants() {
        assert_eq!(
            format_attention_toast(Some("아루"), "Bash 실행 권한"),
            "⚠ 아루 — Bash 실행 권한"
        );
        assert_eq!(format_attention_toast(Some("아루"), ""), "⚠ 아루 — 권한 대기중");
        assert_eq!(
            format_attention_toast(None, "Bash 실행 권한"),
            "⚠ 권한 대기중 — Bash 실행 권한"
        );
        assert_eq!(format_attention_toast(None, ""), "⚠ 권한 대기중");
    }
}


/// 이 간격 안에 다시 누르면 더블클릭이지 이름 편집이 아니다. 헤드리스 하네스도
/// 이 값을 봐야 하므로(문턱을 두 벌로 두면 하네스가 조용히 어긋난다) 밖에 둔다.
pub(crate) const ROOM_RENAME_DOUBLE_CLICK_MS: u128 = 500;

/// 「느린 더블클릭」인가 — **이미 열려 있는 방**의 줄을, **더블클릭 문턱보다 늦게**
/// 다시 누른 경우.
///
/// 셋을 다 봐야 한다: ①같은 줄 ②그 방이 지금 활성(=첫 클릭이 전환이 아니라 선택이었다)
/// ③직전 클릭에서 문턱 초과. ③이 없으면 진짜 더블클릭이 편집을 열고, ②가 없으면
/// 다른 방으로 전환하려던 두 번째 클릭이 편집을 연다.
pub(crate) fn starts_room_rename(
    last: Option<(usize, std::time::Instant)>,
    idx: usize,
    active: usize,
    now: std::time::Instant,
) -> bool {
    // 너무 오래 지난 클릭은 "다시 누른 것"이 아니라 새 클릭이다 — 몇 분 전 클릭이
    // 편집을 열면 사용자는 이유를 못 찾는다.
    const STALE_MS: u128 = 5_000;
    let Some((prev_idx, at)) = last else { return false };
    let ms = now.duration_since(at).as_millis();
    prev_idx == idx && idx == active && ms > ROOM_RENAME_DOUBLE_CLICK_MS && ms <= STALE_MS
}

#[cfg(test)]
mod room_rename_tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn at(ms: u64) -> (Option<(usize, Instant)>, Instant) {
        let t0 = Instant::now();
        (Some((1, t0)), t0 + Duration::from_millis(ms))
    }

    #[test]
    fn 느리게_다시_누르면_편집이다() {
        let (last, now) = at(700);
        assert!(starts_room_rename(last, 1, 1, now));
    }

    #[test]
    fn 진짜_더블클릭은_편집이_아니다() {
        // 문턱 안(300ms) — 여기서 열면 더블클릭 동작과 겹쳐 둘 다 오작동한다.
        let (last, now) = at(300);
        assert!(!starts_room_rename(last, 1, 1, now));
    }

    #[test]
    fn 다른_방으로_전환하는_두번째_클릭은_편집이_아니다() {
        // 누른 줄(2)이 활성(1)이 아니다 = 첫 클릭이 선택이 아니라 전환이었다.
        let (last, now) = at(700);
        assert!(!starts_room_rename(last, 2, 1, now));
    }

    #[test]
    fn 한참_뒤_클릭은_새_클릭이다() {
        let (last, now) = at(9_000);
        assert!(!starts_room_rename(last, 1, 1, now));
    }

    #[test]
    fn 직전_클릭이_없으면_편집이_아니다() {
        assert!(!starts_room_rename(None, 1, 1, Instant::now()));
    }

    /// 대기 어휘가 둘(`waiting`/`blocked`)인데 표시 여섯 자리가 앞의 것만 보고 있었고,
    /// 정작 화면 감지는 뒤의 것만 쓴다 — 그래서 승인 대기가 아무 데도 안 그려졌다.
    #[test]
    fn 대기_판정은_두_어휘를_모두_받는다() {
        assert!(status_needs_you("waiting"));
        assert!(status_needs_you("blocked"));
        assert!(!status_needs_you("working"));
        assert!(!status_needs_you("idle"));
        assert!(!status_needs_you(""));
    }

    /// 같은 승인에 훅과 화면 감지가 각각 쏘던 것을 발사구에서 막는다.
    #[test]
    fn 같은_열쇠는_한_번만_통과한다() {
        // 열쇠는 테스트마다 고유해야 한다 — 게이트 상태가 프로세스 전역이라.
        assert!(notify_dedup_passes("test:dedup-alpha"));
        assert!(!notify_dedup_passes("test:dedup-alpha"));
        // 다른 열쇠는 서로를 막지 않는다.
        assert!(notify_dedup_passes("test:dedup-beta"));
    }

    /// 설정 웹뷰의 폴백은 이 판정에 달려 있다 — 여기가 무조건 true 를 내면 페이지가
    /// 없는 체크아웃에서 **오류 페이지가 뜬 창이 설정 자리를 차지한다**(설정을 아예
    /// 못 여는 것과 같다). 그래서 200 만 통과시키는지, 그리고 서버가 아예 없을 때
    /// 실패하는지를 못 박는다.
    #[test]
    fn 설정_페이지_판정은_200만_통과시킨다() {
        use std::io::{Read, Write};
        // 200 을 주는 서버 → 통과.
        let ok = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let ok_port = ok.local_addr().unwrap().port().to_string();
        let h = std::thread::spawn(move || {
            for status in ["HTTP/1.1 200 OK", "HTTP/1.1 404 Not Found"] {
                let Ok((mut s, _)) = ok.accept() else { return };
                let mut buf = [0u8; 256];
                let _ = s.read(&mut buf);
                let _ = s.write_all(format!("{status}\r\nContent-Length: 0\r\n\r\n").as_bytes());
            }
        });
        assert!(settings_web_reachable(&ok_port), "200 은 통과해야 한다");
        // 같은 서버의 두 번째 응답은 404 — 페이지가 없는 체크아웃이 이 모양이다.
        assert!(!settings_web_reachable(&ok_port), "404 는 막아야 한다");
        let _ = h.join();

        // 아무도 안 듣는 포트 → 실패. 방금 닫은 리스너의 포트를 재사용해 「누가
        // 쓰고 있을지도 모르는 번호」를 찍지 않는다.
        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_port = dead.local_addr().unwrap().port().to_string();
        drop(dead);
        assert!(!settings_web_reachable(&dead_port), "서버가 없으면 실패해야 한다");
        assert!(!settings_web_reachable("포트아님"), "숫자가 아니면 실패해야 한다");
    }
}
