//! 인계·완료 소식을 **펫이 끌어간다** (사용자 2026-09-22 「카사텀 펫에도 갈 수 있는 거 아니야?」).
//!
//! 전에는 나쵸가 펫에 소식을 **밀어 넣었다**(`kasaterm-cli pet-say` → 모델 폴더의 파일).
//! 그 길은 나쵸가 도는 기계에서만 된다 — 다른 바탕화면의 펫에는 못 닿고, 넣은 쪽은 그 줄이
//! 사람 눈에 닿았는지 알 방법이 없다. 그래서 방향을 뒤집었다: 펫이 나쵸를 부르는 길
//! (`ask.rs` 의 `/api/ask`)은 이미 모든 기계에서 도니, 그 위에 **끌어가기**를 얹는다.
//!
//! 지키는 선
//! - **말한 것만 받았다고 한다.** 서버는 ACK 를 받고서야 그 줄을 지운다 — 받아 들고 오다
//!   끊기거나 펫이 죽으면 다음에 다시 온다. 그래서 인계 소식이 조용히 사라지지 않는다.
//! - **두 번 말하지 않는다** — 다만 **정확히 한 번은 아니다.** 아래 「어디까지 보장하나」를 봐라.
//!   ACK 가 유실되면 같은 줄이 **같은 id** 로 다시 오므로 id 로 막는다(이미 말한 것·아직 안
//!   꺼낸 것 양쪽 모두). ⚠️그 기억이 **메모리에만 있으면 펫을 껐다 켠 뒤에 다시 말한다** —
//!   ACK 가 유실된 채로 재기동하면 서버는 그 줄을 아직 들고 있고 펫은 처음 보는 것으로 읽는다
//!   (2026-09-22 검수 지적). 그래서 말한 id 를 펫 폴더에 남긴다(`remember_at`).
//!
//! ■ 어디까지 보장하나 — **at-least-once 에 가깝고 exactly-once 가 아니다**
//!   ·소식이 **사라지지는 않는다**: ACK 전까지 서버가 들고 있고, 만료(하루) 전에 다시 온다.
//!   ·같은 줄을 두 번 말하는 창이 **남아 있다**. 셋 다 좁지만 없다고 하면 거짓말이다:
//!     ①`remember_at` 으로 자리를 못 받은 판(펫 폴더를 모르는 검증 실행)은 메모리만 쓴다.
//!     ②기억 파일 쓰기가 실패하면(디스크 참, 권한) 그 판에서는 **조용히 넘어간다** — 여기서
//!       멈추면 인계가 통째로 막히기 때문이다. 다음 재기동에 그 줄을 다시 말할 수 있다.
//!     ③기억을 남긴 **직후·말풍선에 올리기 전**에 죽으면, 그 줄은 「말했다」로 기록됐지만
//!       사람은 못 봤다. 그래도 이 순서인 것은 반대가 더 나쁘기 때문이다 — 말한 뒤에 남기면
//!       흔한 종료마다 되풀이가 생긴다. 드물게 **한 줄을 놓치는** 쪽을 골랐다.
//!   ·그래서 「받았다」의 정본은 펫이 아니라 **나쵸의 영수증**이다(`petbox.delivered`).
//! - **끼어들지 않는다.** 여기는 줄을 받아 쌓아만 두고, 말할지 말지는 부르는 쪽이 정한다
//!   (사람이 읽는 답 위에 덮지 않으려고 — 무조건 팝업·포커스 뺏기는 안 한다).
//! - **못 닿으면 조용히 물러난다.** 뒤로 갈수록 뜸하게 다시 걸고, 화면에 실패를 안 띄운다.
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// 우편함에서 꺼내 온 한 줄.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub id: String,
    pub text: String,
    /// `hop` 인계 · `watch` 학생 소식 · 그 밖.
    pub kind: String,
}

/// 이 바탕화면이 **지금 이어받고 있는 일**. 바에 한 줄로 걸린다.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub step: String,
    pub paused: bool,
}

impl Task {
    /// 바에 거는 한 줄. 단계가 있으면 어디까지 왔는지까지.
    pub fn line(&self) -> String {
        let head = if self.paused { "멈춤" } else { "맡은 일" };
        if self.step.is_empty() {
            format!("{head} · {}", self.title)
        } else {
            format!("{head} · {} — {}", self.title, self.step)
        }
    }
}

struct Fetched {
    machine: String,
    lines: Vec<Line>,
    task: Option<Task>,
}

/// 한 번에 붙잡고 기다리는 시간. 새 소식이 들어오면 서버가 바로 깨워 주므로, 이 값은
/// 「조용할 때 얼마나 뜸하게 다시 거나」이지 소식이 늦게 오는 시간이 아니다.
const WAIT_SEC: u64 = 20;
/// 읽기 제한은 그보다 넉넉히 — 여기서 먼저 끊으면 서버는 준 줄을 못 받은 것으로 남긴다.
const READ_SEC: u64 = WAIT_SEC + 12;
/// 못 닿을 때 다시 걸기까지. 뒤로 갈수록 뜸해진다(펫은 종일 떠 있다).
const BACKOFF: [u64; 4] = [3, 10, 30, 60];
/// 성공했더라도 이만큼은 쉬었다 다시 건다.
///
/// ⚠️이게 없으면 **붙잡아 주지 않는 서버를 만났을 때 폭주한다.** 빈손으로 곧장 돌아오는
/// 창구(`wait` 를 모르는 옛 대리인, 빈 목록을 바로 주는 판)에는 다시 걸기까지가 0 이라,
/// 2026-09-22 실측에서 25초에 **434번** 두드렸다. 롱폴이 제대로 도는 정상 경로는 한 번에
/// 20초를 기다리므로 이 문은 열리지 않는다 — 어긋난 서버만 여기 걸린다.
const MIN_GAP: Duration = Duration::from_secs(3);
const QUEUE_CAP: usize = 16;
const SPOKEN_CAP: usize = 200;

#[derive(Default)]
pub struct Client {
    pending: Option<Receiver<Result<Fetched, ()>>>,
    /// 이 바탕화면 이름. 서버가 알려 준 것을 그대로 들고 다시 낸다 — 펫은 자기가 어느
    /// 기계인지 모르고, 그 이름이 곧 나쵸 쪽 대화 자리다.
    machine: String,
    /// 말한 줄들 — 다음에 들를 때 돌려준다(그제야 서버가 지운다).
    acks: Vec<String>,
    /// 이미 말한 id. ACK 가 유실돼 같은 줄이 다시 와도 두 번 말하지 않게 한다.
    spoken: VecDeque<String>,
    queue: VecDeque<Line>,
    task: Option<Task>,
    /// 말한 id 를 남겨 두는 자리. 없으면(펫 폴더를 모르는 검증 실행) 메모리만 쓴다.
    memo: Option<std::path::PathBuf>,
    next_at: Option<Instant>,
    /// 지금 도는 왕복이 언제 떠났나 — 너무 빨리 돌아온 성공에 쉬는 시간을 주려고 든다.
    started: Option<Instant>,
    fails: usize,
}

impl Client {
    /// 말한 id 를 남길 자리를 준다. 펫이 켜질 때 한 번 — 여기서 지난 판의 기억을 읽어 온다.
    ///
    /// **기계마다 파일이 갈린다**(펫 폴더가 그 바탕화면의 것이다). 한 기계의 기억이 다른
    /// 기계의 재전달을 막으면 그쪽 인계가 조용히 사라진다.
    pub fn remember_at(&mut self, path: std::path::PathBuf) {
        if self.memo.as_deref() == Some(path.as_path()) {
            return;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(&text) {
                self.spoken = rows.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.chars().take(40).collect::<String>())
                    .filter(|s| !s.is_empty())
                    .rev().take(SPOKEN_CAP).collect::<Vec<_>>()
                    .into_iter().rev().collect();
            }
        }
        self.memo = Some(path);
    }

    /// 말한 id 를 자리에 남긴다. **실패해도 멈추지 않는다.**
    ///
    /// 여기서 막아서면 디스크가 찼다는 이유로 인계가 통째로 안 뜬다 — 그건 되풀이보다 나쁘다.
    /// 대신 그 판에서는 되풀이를 못 막는다는 것을 모듈 머리글의 「어디까지 보장하나」에 적어
    /// 뒀다. 조용히 넘어가되 없는 보장을 있는 척하지는 않는다.
    fn remember(&self) {
        let Some(path) = self.memo.as_ref() else { return };
        let rows: Vec<&String> = self.spoken.iter().collect();
        let Ok(text) = serde_json::to_string(&rows) else { return };
        // 반쯤 쓰다 죽으면 기억이 통째로 깨진다 — 옆에 쓰고 갈아 끼운다.
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    /// 필요하면 한 번 더 들른다. 매 프레임 불러도 된다 — 도는 중이거나 쉴 때는 아무 일도 안 한다.
    pub fn pump(&mut self, service: &Path) {
        self.collect();
        if self.pending.is_some() {
            return;
        }
        if self.next_at.is_some_and(|at| Instant::now() < at) {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.pending = Some(receiver);
        self.started = Some(Instant::now());
        let (path, machine, acks) = (service.to_path_buf(), self.machine.clone(), std::mem::take(&mut self.acks));
        std::thread::spawn(move || {
            let _ = sender.send(fetch(&path, &machine, &acks));
        });
    }

    fn collect(&mut self) {
        let got = match self.pending.as_ref().map(Receiver::try_recv) {
            Some(Ok(got)) => got,
            Some(Err(TryRecvError::Disconnected)) => Err(()),
            Some(Err(TryRecvError::Empty)) | None => return,
        };
        self.pending = None;
        let took = self.started.take().map(|at| at.elapsed()).unwrap_or(MIN_GAP);
        let Ok(got) = got else {
            // 못 닿았다 — 화면에는 아무 말도 안 한다. 펫이 종일 「나쵸에 못 닿았어요」를
            // 되풀이하면 그건 알림이 아니라 소음이다.
            self.next_at = Some(Instant::now() + Duration::from_secs(BACKOFF[self.fails.min(BACKOFF.len() - 1)]));
            self.fails += 1;
            return;
        };
        self.fails = 0;
        // 붙잡아 주지 않는 창구에 초당 수십 번 두드리지 않는다.
        self.next_at = (took < MIN_GAP).then(|| Instant::now() + (MIN_GAP - took));
        if !got.machine.is_empty() {
            self.machine = got.machine;
        }
        self.task = got.task;
        for line in got.lines {
            // 이미 말한 것은 **다시 말하지 않고 다시 ACK 만** 한다(ACK 가 유실된 재전달).
            if self.spoken.contains(&line.id) {
                self.acks.push(line.id);
                continue;
            }
            // ★아직 안 꺼낸 줄도 겹치지 않는다. 서버는 ACK 전까지 같은 줄을 계속 주는데,
            // 사람이 글자를 치는 동안에는 아무도 안 꺼낸다 — 그 사이 폴링이 도는 만큼 같은
            // id 가 쌓이고 나중에 그만큼 되풀이해 말하게 된다(2026-09-22 검수). 여기서는
            // **ACK 도 안 한다**: 아직 안 말했는데 ACK 하면 서버가 지워 그 줄이 사라진다.
            if self.queue.iter().any(|row| row.id == line.id) {
                continue;
            }
            if self.queue.len() >= QUEUE_CAP {
                self.queue.pop_front();
            }
            self.queue.push_back(line);
        }
    }

    /// 말할 줄 하나. 꺼내는 순간 「받았다」로 치고 다음에 들를 때 서버에 알린다.
    pub fn take(&mut self) -> Option<Line> {
        let line = self.queue.pop_front()?;
        if self.spoken.len() >= SPOKEN_CAP {
            self.spoken.pop_front();
        }
        self.spoken.push_back(line.id.clone());
        self.acks.push(line.id.clone());
        // ★**말하기 전에** 남긴다. 말한 뒤에 남기면 그 사이에 죽었을 때 같은 줄을 또 말한다.
        self.remember();
        Some(line)
    }

    pub fn waiting(&self) -> bool {
        !self.queue.is_empty()
    }

    /// 지금 이어받고 있는 일. 바에 걸고, 묻는 말에 함께 실어 보낸다.
    pub fn task(&self) -> Option<&Task> {
        self.task.as_ref()
    }

    pub fn task_id(&self) -> String {
        self.task.as_ref().map(|t| t.id.clone()).unwrap_or_default()
    }
}

fn fetch(service: &Path, machine: &str, acks: &[String]) -> Result<Fetched, ()> {
    let port = crate::journal::ask_service(service)?;
    let body = json!({ "machine": machine, "ack": acks, "wait": WAIT_SEC });
    let value = crate::journal::request_within(
        port,
        "POST",
        "/api/pet/poll",
        Some(&body),
        Duration::from_secs(READ_SEC),
    )?;
    Ok(parse(&value))
}

fn parse(value: &Value) -> Fetched {
    let lines = value["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let id = row["id"].as_str()?.trim();
            let text = row["text"].as_str().unwrap_or("").trim();
            if id.is_empty() || text.is_empty() {
                return None;
            }
            Some(Line {
                id: id.chars().take(40).collect(),
                text: text.chars().take(1200).collect(),
                kind: row["kind"].as_str().unwrap_or("say").chars().take(20).collect(),
            })
        })
        .take(8)
        .collect();
    let task = value["task"].as_object().and_then(|row| {
        let id = row.get("id")?.as_str()?.trim();
        if id.is_empty() {
            return None;
        }
        let pick = |key: &str, cap: usize| -> String {
            row.get(key).and_then(Value::as_str).unwrap_or("").trim().chars().take(cap).collect()
        };
        Some(Task {
            id: id.chars().take(40).collect(),
            title: pick("title", 80),
            step: pick("step", 60),
            paused: row.get("paused").and_then(Value::as_bool).unwrap_or(false),
        })
    });
    Fetched {
        machine: value["machine"].as_str().unwrap_or("").chars().take(40).collect(),
        lines,
        task,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fed(client: &mut Client, value: &Value) {
        fed_after(client, value, MIN_GAP);
    }

    /// `took` 은 이 왕복이 걸린 시간 — 붙잡아 주는 창구(정상)와 즉답하는 창구를 가른다.
    fn fed_after(client: &mut Client, value: &Value, took: Duration) {
        let (tx, rx) = mpsc::channel();
        tx.send(Ok(parse(value))).unwrap();
        client.pending = Some(rx);
        client.started = Some(Instant::now() - took);
        client.collect();
    }

    #[test]
    fn messages_queue_up_and_are_spoken_once() {
        let mut client = Client::default();
        fed(&mut client, &json!({"machine": "미니", "messages": [
            {"id": "a", "text": "디스코드에서 하던 일 여기서 이어받을게.", "kind": "hop"},
            {"id": "b", "text": "아즈사가 다 했대.", "kind": "watch"}
        ]}));
        assert!(client.waiting());
        let first = client.take().unwrap();
        assert_eq!(first.text, "디스코드에서 하던 일 여기서 이어받을게.");
        assert_eq!(first.kind, "hop");
        assert_eq!(client.take().unwrap().id, "b");
        assert!(client.take().is_none());
        // 말한 것은 다음에 들를 때 돌려준다 — 그제야 서버가 지운다.
        assert_eq!(client.acks, vec!["a".to_string(), "b".to_string()]);
    }

    /// ACK 가 오다 끊기면 서버는 **같은 id** 로 다시 준다. 그때 두 번 말하면 사람은 같은
    /// 인계를 두 번 받은 줄 안다.
    #[test]
    fn a_redelivered_line_is_acked_again_but_never_spoken_twice() {
        let mut client = Client::default();
        fed(&mut client, &json!({"messages": [{"id": "a", "text": "한 번만 할 말"}]}));
        assert_eq!(client.take().unwrap().text, "한 번만 할 말");
        client.acks.clear();
        fed(&mut client, &json!({"messages": [{"id": "a", "text": "한 번만 할 말"}]}));
        assert!(!client.waiting(), "다시 말하지 않는다");
        assert_eq!(client.acks, vec!["a".to_string()], "대신 다시 ACK 한다");
    }

    #[test]
    fn the_machine_name_the_server_gave_is_carried_back() {
        let mut client = Client::default();
        assert_eq!(client.machine, "");
        fed(&mut client, &json!({"machine": "건호의 MacBook Pro", "messages": []}));
        assert_eq!(client.machine, "건호의 MacBook Pro");
        // 서버가 이름을 안 주면 들고 있던 것을 버리지 않는다(빈 이름으로 다시 물으면
        // 서버가 자리를 못 정해 503 이 된다).
        fed(&mut client, &json!({"messages": []}));
        assert_eq!(client.machine, "건호의 MacBook Pro");
    }

    #[test]
    fn the_task_card_rides_along_and_clears_when_the_work_closes() {
        let mut client = Client::default();
        fed(&mut client, &json!({"task": {"id": "w1", "title": "빌드 고치기", "step": "검사 돌리기"}}));
        assert_eq!(client.task_id(), "w1");
        assert_eq!(client.task().unwrap().line(), "맡은 일 · 빌드 고치기 — 검사 돌리기");
        fed(&mut client, &json!({"task": null}));
        assert!(client.task().is_none() && client.task_id().is_empty());
    }

    #[test]
    fn a_paused_task_says_so() {
        let task = Task { id: "w1".into(), title: "빌드 고치기".into(), step: String::new(), paused: true };
        assert_eq!(task.line(), "멈춤 · 빌드 고치기");
    }

    /// 빈 줄·이름 없는 줄은 버린다 — 말풍선에 빈 칸이 뜨면 사람은 못 읽은 줄 알고 기다린다.
    #[test]
    fn empty_rows_are_dropped() {
        let mut client = Client::default();
        fed(&mut client, &json!({"messages": [
            {"id": "", "text": "이름이 없다"}, {"id": "b", "text": "  "}, {"id": "c", "text": "이건 산다"}
        ]}));
        assert_eq!(client.take().unwrap().id, "c");
        assert!(client.take().is_none());
    }

    /// 못 닿으면 **조용히** 물러나고 뒤로 갈수록 뜸하게 다시 건다.
    #[test]
    fn failures_are_quiet_and_back_off() {
        let mut client = Client::default();
        let (tx, rx) = mpsc::channel();
        tx.send(Err(())).unwrap();
        client.pending = Some(rx);
        client.collect();
        assert!(!client.waiting() && client.task().is_none());
        assert_eq!(client.fails, 1);
        let first = client.next_at.expect("다시 걸 때를 잡아 둔다");
        for _ in 0..5 {
            let (tx, rx) = mpsc::channel();
            tx.send(Err(())).unwrap();
            client.pending = Some(rx);
            client.collect();
        }
        assert!(client.next_at.unwrap() > first, "점점 뜸해진다");
        // 한 번 닿으면 곧바로 제자리로 — 잠깐 끊긴 뒤에 1분씩 기다리면 인계가 늦는다.
        fed_after(&mut client, &json!({"messages": []}), Duration::from_secs(20));
        assert_eq!(client.fails, 0);
        assert!(client.next_at.is_none());
    }

    /// 붙잡아 주지 않는 창구(빈손으로 곧장 돌아오는 판)에 초당 수십 번 두드리지 않는다.
    /// 2026-09-22 실측: 이 문이 없을 때 25초에 434번 두드렸다.
    #[test]
    fn a_server_that_answers_instantly_does_not_get_hammered() {
        let mut client = Client::default();
        fed_after(&mut client, &json!({"messages": []}), Duration::from_millis(5));
        let rest = client.next_at.expect("곧장 다시 걸지 않는다");
        assert!(rest > Instant::now() + Duration::from_secs(2));
        // 제대로 붙잡아 준 왕복(롱폴)은 쉬지 않고 바로 다시 건다 — 소식이 늦으면 안 된다.
        fed_after(&mut client, &json!({"messages": []}), Duration::from_secs(20));
        assert!(client.next_at.is_none());
    }

    /// ★**안 꺼낸 채로 다시 받아도 줄이 겹치지 않는다.** 서버는 ACK 전까지 같은 줄을 계속
    /// 주는데, 사람이 글자를 치는 동안에는 `take` 를 안 한다(끼어들지 않으려고). 그 사이
    /// 폴링이 여러 번 돌면 **같은 id 가 큐에 여러 개 쌓이고**, 나중에 그만큼 되풀이해 말하게
    /// 된다(2026-09-22 검수 지적).
    #[test]
    fn the_same_line_never_piles_up_while_the_person_is_typing() {
        let mut client = Client::default();
        let same = json!({"messages": [{"id": "a", "text": "인계 한 줄"}]});
        for _ in 0..5 {
            fed(&mut client, &same);        // 사람이 치는 동안 아무도 안 꺼낸다
        }
        assert_eq!(client.queue.len(), 1, "같은 줄은 한 자리만 차지한다");
        assert_eq!(client.take().unwrap().text, "인계 한 줄");
        assert!(client.take().is_none(), "되풀이해 말하지 않는다");
        // 아직 안 말한 줄을 ACK 하면 서버가 지워 버린다 — 겹친 것은 조용히 버린다.
        assert_eq!(client.acks, vec!["a".to_string()]);
    }

    /// ★**펫을 껐다 켠 뒤에도 두 번 말하지 않는다.** ACK 가 유실된 채로 재기동하면 서버는
    /// 그 줄을 아직 들고 있고, 기억이 메모리에만 있으면 펫이 처음 보는 것으로 읽는다
    /// (2026-09-22 검수 지적 — 그 전까지 이 보장은 한 프로세스 안에서만 참이었다).
    #[test]
    fn what_was_spoken_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("kasapet-postbox-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let memo = dir.join("spoken.json");
        let _ = std::fs::remove_file(&memo);

        let mut client = Client::default();
        client.remember_at(memo.clone());
        fed(&mut client, &json!({"messages": [{"id": "a", "text": "한 번만 할 말"}]}));
        assert_eq!(client.take().unwrap().text, "한 번만 할 말");

        // 껐다 켠다 — 새 Client 다. ACK 는 유실돼 서버가 같은 줄을 다시 준다.
        let mut reborn = Client::default();
        reborn.remember_at(memo.clone());
        fed(&mut reborn, &json!({"messages": [{"id": "a", "text": "한 번만 할 말"}]}));
        assert!(!reborn.waiting(), "재기동해도 다시 말하지 않는다");
        assert_eq!(reborn.acks, vec!["a".to_string()], "대신 다시 ACK 한다");

        // 자리를 안 준 판(펫 폴더를 모르는 검증 실행)은 기억이 없다 — 그 판에서는 다시 말한다.
        let mut bare = Client::default();
        fed(&mut bare, &json!({"messages": [{"id": "a", "text": "한 번만 할 말"}]}));
        assert!(bare.waiting(), "자리를 안 주면 이 보장이 없다 — 문서와 같아야 한다");

        std::fs::remove_file(&memo).unwrap();
    }

    /// 기억은 **바탕화면마다 갈린다.** 한 기계의 기억이 다른 기계의 재전달을 막으면 그쪽
    /// 인계가 조용히 사라진다.
    #[test]
    fn two_desktops_keep_separate_memories() {
        let dir = std::env::temp_dir().join(format!("kasapet-postbox-two-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (mini, book) = (dir.join("mini.json"), dir.join("book.json"));
        for p in [&mini, &book] {
            let _ = std::fs::remove_file(p);
        }

        let mut on_mini = Client::default();
        on_mini.remember_at(mini.clone());
        fed(&mut on_mini, &json!({"machine": "미니", "messages": [{"id": "m1", "text": "미니 인계"}]}));
        assert_eq!(on_mini.take().unwrap().text, "미니 인계");

        // 같은 id 가 **다른 바탕화면**에 왔다. 그쪽은 처음 보는 것이므로 말해야 한다.
        let mut on_book = Client::default();
        on_book.remember_at(book.clone());
        fed(&mut on_book, &json!({"machine": "맥북", "messages": [{"id": "m1", "text": "맥북 인계"}]}));
        assert_eq!(on_book.take().unwrap().text, "맥북 인계");

        for p in [&mini, &book] {
            std::fs::remove_file(p).unwrap();
        }
    }

    /// 기억을 **못 남기는** 자리라도 인계는 뜬다 — 되풀이를 못 막을 뿐이다(머리글 ②).
    /// 여기서 막아서면 디스크가 찼다는 이유로 소식이 통째로 안 뜬다.
    #[test]
    fn a_memo_that_cannot_be_written_still_lets_the_line_through() {
        let dir = std::env::temp_dir().join(format!("kasapet-postbox-ro-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 파일 자리에 **폴더**를 두면 쓰기가 반드시 실패한다.
        let memo = dir.join("spoken.json");
        let _ = std::fs::remove_file(&memo);
        std::fs::create_dir_all(&memo).unwrap();

        let mut client = Client::default();
        client.remember_at(memo.clone());
        fed(&mut client, &json!({"messages": [{"id": "a", "text": "그래도 말한다"}]}));
        assert_eq!(client.take().unwrap().text, "그래도 말한다");
        // 기억은 못 남았다 — 재기동하면 다시 말하게 된다(그 한계가 문서에 적혀 있다).
        let mut reborn = Client::default();
        reborn.remember_at(memo.clone());
        fed(&mut reborn, &json!({"messages": [{"id": "a", "text": "그래도 말한다"}]}));
        assert!(reborn.waiting(), "기억을 못 남기면 되풀이를 못 막는다 — 문서와 같아야 한다");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 기억 파일이 깨졌거나 없으면 **조용히 빈손으로 시작한다** — 여기서 터지면 펫이 안 뜬다.
    #[test]
    fn a_broken_memo_does_not_stop_the_pet() {
        let dir = std::env::temp_dir().join(format!("kasapet-postbox-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let memo = dir.join("bad.json");
        std::fs::write(&memo, "이건 JSON 이 아니다").unwrap();
        let mut client = Client::default();
        client.remember_at(memo.clone());
        assert!(client.spoken.is_empty());
        fed(&mut client, &json!({"messages": [{"id": "a", "text": "말한다"}]}));
        assert!(client.waiting());
        std::fs::remove_file(&memo).unwrap();
    }

    /// 쌓이는 줄에 상한이 있다. 며칠 자리를 비운 뒤 수십 줄이 한꺼번에 오면 펫이 그것만
    /// 읊다 끝난다 — 오래된 것부터 버린다.
    #[test]
    fn the_queue_has_a_ceiling() {
        let mut client = Client::default();
        let rows: Vec<Value> = (0..QUEUE_CAP + 5)
            .map(|i| json!({"id": format!("m{i}"), "text": format!("줄 {i}")}))
            .collect();
        for row in rows {
            fed(&mut client, &json!({"messages": [row]}));
        }
        assert_eq!(client.queue.len(), QUEUE_CAP);
        assert_eq!(client.take().unwrap().text, format!("줄 {}", 5));
    }
}
