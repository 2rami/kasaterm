//! rust-analyzer 를 stdio 로 붙여 **진단만** 받아 오는 최소 LSP 클라이언트.
//!
//! `async-lsp` 를 쓰지 않는다. 이 레포엔 async 런타임이 아예 없고(tokio·futures
//! 없음, 전부 스레드+채널 관용구), 진단만 받는 프로토콜 표면은 initialize ·
//! didOpen · didChange · publishDiagnostics 넷뿐이다. 그 넷을 직접 말하면
//! 의존성 트리가 하나도 안 늘고, 서버가 죽었을 때의 처리도 우리가 다 본다.
//!
//! 서버는 **프로젝트 루트 하나당 하나**만 띄운다. rust-analyzer 는 첫 인덱싱에
//! 수십 초와 수 GB 를 쓰는 무거운 프로세스라, 파일마다 띄우면 맥이 멈춘다.
//!
//! ⚠️ 헤드리스 검증 함정: `~/.cargo/bin/rust-analyzer` 는 rustup **프록시**라
//! `HOME` 을 스크래치로 격리하면 "could not choose a version of rust-analyzer"
//! 로 죽는다(stderr 를 버리고 있으면 그냥 조용해서, 우리 프로토콜이 틀린 것처럼
//! 보인다). 격리 실행에는 `RUSTUP_HOME`·`CARGO_HOME` 을 실제 경로로 함께 줘야
//! 한다. 진행이 안 보일 때는 `KASATERM_LSP_DEBUG=1` 로 서버 stderr 를 흘려라.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// 진단 하나. 좌표는 **char 인덱스**로 이미 변환된 상태다(LSP 는 UTF-16
/// 코드유닛으로 세므로 그대로 쓰면 이모지가 섞인 줄에서 밑줄이 밀린다).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diag {
    pub line: usize,
    pub col: usize,
    pub end_line: usize,
    pub end_col: usize,
    /// 1=error 2=warning 3=information 4=hint. LSP 값 그대로.
    pub severity: u8,
    pub message: String,
}

/// 서버가 보낸 진단 — 파일 경로별. 렌더 스레드가 매 프레임 읽으므로 잠금은
/// 짧게 잡는다.
pub type DiagMap = Arc<Mutex<HashMap<PathBuf, Vec<Diag>>>>;

/// LSP 의 UTF-16 열 오프셋을 char 인덱스로 옮긴다.
///
/// 한글은 UTF-16 한 유닛이라 값이 같지만 이모지·희귀 한자는 두 유닛이다. 그걸
/// 무시하면 그런 글자가 앞에 있는 줄에서 밑줄이 오른쪽으로 밀린다.
pub fn utf16_col_to_char(line: &str, utf16: usize) -> usize {
    let mut u = 0usize;
    for (i, ch) in line.chars().enumerate() {
        if u >= utf16 {
            return i;
        }
        u += ch.len_utf16();
    }
    line.chars().count()
}

/// `Content-Length` 프레임 하나를 읽어 본문을 돌려준다. 스트림이 끝나면 `None`.
///
/// 헤더는 CRLF 로 끝나고 빈 줄 뒤에 본문이 온다. `Content-Type` 같은 다른
/// 헤더도 올 수 있으므로 길이만 골라 읽는다.
fn read_frame(r: &mut impl BufRead) -> Option<Vec<u8>> {
    let mut len = 0usize;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let t = line.trim_end();
        if t.is_empty() {
            break;
        }
        if let Some(v) = t.strip_prefix("Content-Length:") {
            len = v.trim().parse().ok()?;
        }
    }
    if len == 0 {
        return Some(Vec::new());
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).ok()?;
    Some(buf)
}

/// 한 프레임을 써 보낸다.
fn write_frame(w: &mut impl Write, body: &str) -> std::io::Result<()> {
    write!(w, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    w.flush()
}

/// 서버가 보낸 `publishDiagnostics` 를 우리 표현으로 옮긴다. 진단이 아닌
/// 메시지면 `None` — 그 밖의 알림·응답은 이 클라이언트가 쓰지 않는다.
///
/// 줄 텍스트를 넘기는 이유는 UTF-16 열을 char 로 바꾸려면 그 줄이 필요해서다.
/// 파일을 다시 읽지 않고 열려 있는 버퍼를 쓰도록 호출자가 준다.
pub fn parse_diagnostics(
    msg: &serde_json::Value,
    line_text: impl Fn(&Path, usize) -> Option<String>,
) -> Option<(PathBuf, Vec<Diag>)> {
    if msg.get("method")?.as_str()? != "textDocument/publishDiagnostics" {
        return None;
    }
    let p = msg.get("params")?;
    let uri = p.get("uri")?.as_str()?;
    let path = uri_to_path(uri)?;
    let mut out = Vec::new();
    for d in p.get("diagnostics")?.as_array()? {
        let r = d.get("range")?;
        let (s, e) = (r.get("start")?, r.get("end")?);
        let (sl, el) = (s.get("line")?.as_u64()? as usize, e.get("line")?.as_u64()? as usize);
        let conv = |li: usize, u16col: usize| match line_text(&path, li) {
            Some(t) => utf16_col_to_char(&t, u16col),
            // 그 줄을 못 읽으면 UTF-16 값을 그대로 쓴다 — ASCII 줄에선 같고,
            // 아니면 조금 밀리지만 진단을 버리는 것보다 낫다.
            None => u16col,
        };
        out.push(Diag {
            line: sl,
            col: conv(sl, s.get("character")?.as_u64()? as usize),
            end_line: el,
            end_col: conv(el, e.get("character")?.as_u64()? as usize),
            severity: d.get("severity").and_then(|v| v.as_u64()).unwrap_or(1) as u8,
            message: d.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        });
    }
    Some((path, out))
}

/// `file://` URI 를 경로로. 퍼센트 인코딩을 되돌린다 — 경로에 공백이나 한글이
/// 있으면 서버가 `%20`·`%ED%95%9C` 로 보내오고, 그대로 쓰면 파일이 안 맞는다.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let raw = uri.strip_prefix("file://")?;
    let b = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok()?;
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    Some(PathBuf::from(String::from_utf8(out).ok()?))
}

/// 경로를 `file://` URI 로. 퍼센트 인코딩이 필요한 바이트만 감싼다.
pub fn path_to_uri(p: &Path) -> String {
    let s = p.to_string_lossy();
    let mut out = String::from("file://");
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 살아 있는 rust-analyzer 하나.
pub struct LspClient {
    /// 보낼 프레임을 **큐에 넣기만** 한다. 실제 write 는 전용 스레드가 한다.
    ///
    /// 예전엔 GUI 스레드가 서버 stdin 에 직접 썼는데, 파이프는 상대가 읽어야
    /// 비므로 rust-analyzer 가 무거운 작업에 붙들리면 그 write 가 블록하고
    /// **창 전체가 멈춘다**(편집 직후 한 번 실제로 굳었다). 편집기는 LSP 가
    /// 어떤 상태든 멈추면 안 되므로, 전송은 절대 GUI 를 기다리게 하지 않는다.
    ///
    /// `Option` 인 이유는 `Drop` 에서 명시적으로 닫아 쓰기 스레드를 끝내기
    /// 위해서다(필드 드롭 순서에 기대면 조용히 어긋난다).
    tx: Option<std::sync::mpsc::Sender<String>>,
    root: PathBuf,
    next_id: i64,
    /// 파일별 문서 버전 — didChange 마다 올려야 서버가 순서를 안다.
    version: HashMap<PathBuf, i64>,
    /// 이미 didOpen 한 파일. 두 번 열면 서버가 프로토콜 위반으로 본다.
    opened: HashMap<PathBuf, ()>,
    /// 서버에 보낸 줄들. 읽기 스레드가 UTF-16 열을 char 로 바꿀 때 **그 줄**이
    /// 필요한데, 진단이 올 때 디스크를 다시 읽으면 편집 중인 버퍼와 어긋난다.
    /// 그래서 보낸 내용을 그대로 들고 있다.
    texts: Arc<Mutex<HashMap<PathBuf, Vec<String>>>>,
    pub diags: DiagMap,
    /// `initialize` 응답이 왔는가. LSP 는 그 응답 **뒤에** 다른 메시지를 보내라고
    /// 정해 두었고, 어긴 채 보낸 didOpen 은 rust-analyzer 가 조용히 버린다
    /// (진단이 영원히 0 이던 원인이 정확히 이것이었다).
    ready: Arc<AtomicBool>,
    /// 재전송 디바운스 상태. App 이 아니라 여기 두는 이유는 서버가 죽어 새로
    /// 붙을 때 이 기억도 같이 사라져야 하기 때문이다 — 남아 있으면 새 서버가
    /// 아직 못 본 버퍼를 "이미 보냈다"고 건너뛴다.
    sync: HashMap<PathBuf, SyncState>,
    child: Child,
}

/// rust-analyzer 는 didChange 마다 재분석을 건다. 타이핑 중 매 키를 보내면
/// 서버가 취소·재시작만 반복하다 진단이 영영 안 온다 — 손이 멎을 때까지 미룬다.
const QUIET: std::time::Duration = std::time::Duration::from_millis(400);

/// 파일 하나의 재전송 판정용 상태.
struct SyncState {
    /// 마지막으로 본 버퍼 세대.
    seen: u64,
    /// 그 세대를 **처음** 본 시각. 세대가 또 바뀌면 여기서 다시 잰다.
    at: std::time::Instant,
    /// 마지막으로 서버에 보낸 세대.
    sent: u64,
}

impl SyncState {
    /// 지금 보내야 하나. 시각을 인자로 받는 이유는 테스트가 400ms 를 실제로
    /// 기다리지 않고 판정만 확인할 수 있어야 하기 때문이다.
    fn due(&mut self, gen: u64, now: std::time::Instant, quiet: std::time::Duration) -> bool {
        // 세대가 막 바뀌었으면 아직 타이핑 중이다 — 시계를 여기서 다시 잰다.
        if self.seen != gen {
            self.seen = gen;
            self.at = now;
            return false;
        }
        self.sent != gen && now.duration_since(self.at) >= quiet
    }
}

impl LspClient {
    /// 서버를 띄우고 `initialize` 까지 마친다. rust-analyzer 가 없거나 못 뜨면
    /// `None` — 편집기는 LSP 없이도 온전히 동작해야 하므로 실패는 조용히 넘긴다.
    pub fn spawn(root: &Path) -> Option<Self> {
        // `KASATERM_LSP_DEBUG=1` 이면 서버 stderr 와 우리 판단을 그대로 흘린다.
        // 기본은 버린다 — rust-analyzer 는 인덱싱 로그를 쉼 없이 쏟는다.
        let debug = std::env::var_os("KASATERM_LSP_DEBUG").is_some();
        let err = if debug { Stdio::inherit() } else { Stdio::null() };
        let spawned = Command::new("rust-analyzer")
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(err)
            .spawn();
        if debug {
            eprintln!("[lsp] spawn root={root:?} ok={}", spawned.is_ok());
        }
        let mut child = spawned.ok()?;
        let mut stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        // 쓰기 전용 스레드 — 여기서만 파이프를 만진다. 서버가 느려 write 가
        // 막혀도 막히는 건 이 스레드뿐이고 GUI 는 계속 돈다.
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            while let Ok(msg) = rx.recv() {
                if write_frame(&mut stdin, &msg).is_err() {
                    break;
                }
            }
        });
        let diags: DiagMap = Arc::new(Mutex::new(HashMap::new()));
        // 읽기는 전용 스레드에서. 서버는 인덱싱 진행 알림을 쉼 없이 보내므로
        // GUI 스레드에서 폴링하면 그 프레임이 통째로 막힌다.
        let sink = Arc::clone(&diags);
        let ready = Arc::new(AtomicBool::new(false));
        let rflag = Arc::clone(&ready);
        let rtx = tx.clone();
        let texts: Arc<Mutex<HashMap<PathBuf, Vec<String>>>> = Arc::new(Mutex::new(HashMap::new()));
        let tsink = Arc::clone(&texts);
        std::thread::spawn(move || {
            let mut r = BufReader::new(stdout);
            while let Some(body) = read_frame(&mut r) {
                let Ok(v) = serde_json::from_slice::<serde_json::Value>(&body) else {
                    continue;
                };
                let line_of = |p: &Path, li: usize| -> Option<String> {
                    tsink.lock().ok()?.get(p)?.get(li).cloned()
                };
                if debug {
                    let what = v
                        .get("method")
                        .and_then(|m| m.as_str())
                        .map(|m| m.to_string())
                        .unwrap_or_else(|| format!("response id={:?}", v.get("id")));
                    eprintln!("[lsp] <- {what}");
                }
                // initialize 응답(id 1)을 본 자리에서 initialized 를 보내고 문을 연다.
                if !rflag.load(Ordering::Relaxed)
                    && v.get("id").and_then(|i| i.as_i64()) == Some(1)
                    && v.get("result").is_some()
                {
                    let note =
                        serde_json::json!({"jsonrpc":"2.0","method":"initialized","params":{}});
                    let _ = rtx.send(note.to_string());
                    rflag.store(true, Ordering::Relaxed);
                    if debug {
                        eprintln!("[lsp] initialized — 이제 didOpen 을 받는다");
                    }
                }
                if let Some((path, ds)) = parse_diagnostics(&v, line_of) {
                    if debug {
                        eprintln!("[lsp] 진단 {} 개 {path:?}", ds.len());
                    }
                    if let Ok(mut m) = sink.lock() {
                        m.insert(path, ds);
                    }
                }
            }
        });
        let mut me = Self {
            tx: Some(tx),
            root: root.to_path_buf(),
            next_id: 1,
            version: HashMap::new(),
            opened: HashMap::new(),
            texts,
            diags,
            ready,
            sync: HashMap::new(),
            child,
        };
        me.send_initialize()?;
        Some(me)
    }

    fn send_initialize(&mut self) -> Option<()> {
        let id = self.next_id;
        self.next_id += 1;
        let req = serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "initialize",
            "params": {
                "processId": std::process::id(),
                "rootUri": path_to_uri(&self.root),
                // 진단만 받는다 — 다른 기능을 선언하면 서버가 그쪽 작업까지
                // 준비하느라 첫 인덱싱이 더 느려진다.
                "capabilities": {
                    "textDocument": {
                        "publishDiagnostics": { "relatedInformation": false },
                        "synchronization": { "didSave": false }
                    }
                },
                "clientInfo": { "name": "kasaterm" }
            }
        });
        self.send(req.to_string())
    }

    /// 프레임 하나를 전송 큐에 넣는다. 서버가 죽었거나 종료 중이면 조용히
    /// 버린다 — 진단은 있으면 좋은 것이지, 없다고 편집을 막을 이유가 없다.
    fn send(&self, msg: String) -> Option<()> {
        self.tx.as_ref()?.send(msg).ok()
    }

    /// 이 파일을 이미 서버에 알렸는가. 호출자가 버퍼를 문자열로 합치기 **전에**
    /// 물어볼 수 있어야 한다 — 틱마다 5천 줄을 join 하면 그게 곧 프레임 예산이다.
    pub fn is_open(&self, p: &Path) -> bool {
        self.opened.contains_key(p)
    }

    /// 파일을 서버에 처음 알린다. 이미 열었으면 아무 일도 안 한다.
    pub fn did_open(&mut self, path: &Path, text: &str) {
        // 아직 initialize 응답을 못 봤으면 보내지 않는다. 호출자(틱)가 계속 다시
        // 부르므로 문이 열리는 순간 저절로 나간다.
        if !self.ready.load(Ordering::Relaxed) || self.opened.contains_key(path) {
            return;
        }
        self.opened.insert(path.to_path_buf(), ());
        self.version.insert(path.to_path_buf(), 1);
        self.remember(path, text);
        let note = serde_json::json!({
            "jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": path_to_uri(path), "languageId": "rust", "version": 1, "text": text
            }}
        });
        let _ = self.send(note.to_string());
    }

    /// 서버에 보낸 줄을 기록해 둔다 — 읽기 스레드가 UTF-16 열을 char 로 바꿀 때
    /// 쓴다. 안 채우면 변환이 늘 폴백으로 떨어져 이모지가 섞인 줄에서 밑줄이
    /// 밀린다(처음 짤 때 실제로 빠뜨렸다).
    fn remember(&self, path: &Path, text: &str) {
        if let Ok(mut m) = self.texts.lock() {
            m.insert(path.to_path_buf(), text.split('\n').map(String::from).collect());
        }
    }

    /// 이 세대의 버퍼를 지금 보내야 하는가. 세대가 새로 바뀌면 시계를 다시
    /// 재고, 같은 세대가 `QUIET` 만큼 유지되면 그때 true 를 돌려준다.
    ///
    /// 호출자가 버퍼를 이어 붙이기 **전에** 물어보라고 세대(정수)만 받는다 —
    /// 매 틱 5천 줄을 join 해서 비교하면 그 자체가 프레임 예산이다.
    pub fn change_due(&mut self, path: &Path, gen: u64) -> bool {
        let now = std::time::Instant::now();
        self.sync
            .entry(path.to_path_buf())
            .or_insert(SyncState { seen: gen, at: now, sent: gen })
            .due(gen, now, QUIET)
    }

    /// 버퍼 전체를 다시 보낸다. `gen` 은 이 본문의 세대 — 보낸 세대를 기록해
    /// 같은 내용을 두 번 보내지 않는다.
    ///
    /// 증분 동기화도 프로토콜에 있지만 쓰지 않는다 — 우리는 이 호출을 타이핑이
    /// **조용해진 뒤에만** 하므로(tree-sitter 재파싱과 같은 판정) 한 번의 전체
    /// 전송이 증분 추적 코드를 유지할 값어치보다 싸다.
    pub fn did_change(&mut self, path: &Path, text: &str, gen: u64) {
        if let Some(st) = self.sync.get_mut(path) {
            st.sent = gen;
        }
        if !self.opened.contains_key(path) {
            self.did_open(path, text);
            return;
        }
        self.remember(path, text);
        let v = self.version.entry(path.to_path_buf()).or_insert(1);
        *v += 1;
        let note = serde_json::json!({
            "jsonrpc": "2.0", "method": "textDocument/didChange",
            "params": {
                "textDocument": { "uri": path_to_uri(path), "version": *v },
                "contentChanges": [ { "text": text } ]
            }
        });
        let _ = self.send(note.to_string());
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        // 예의를 지켜 shutdown 을 보내지만 응답을 기다리지 않는다 — 종료 경로에서
        // 서버가 인덱싱에 붙들려 있으면 창이 안 닫힌다.
        let _ = self.send(serde_json::json!({"jsonrpc":"2.0","id":0,"method":"shutdown"}).to_string());
        // 보내는 쪽을 명시적으로 닫아 쓰기 스레드를 끝낸다. 이걸 필드 드롭
        // 순서에 맡기면 스레드가 살아남아 죽은 파이프를 붙들고 있게 된다.
        self.tx = None;
        let _ = self.child.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_debounce_waits_for_typing_to_stop() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let q = Duration::from_millis(400);
        // 막 연 파일 — 이미 보낸 세대라 보낼 것이 없다.
        let mut st = SyncState { seen: 1, at: t0, sent: 1 };
        assert!(!st.due(1, t0 + Duration::from_secs(9), q));
        // 한 글자 쳤다 → 세대가 오른다. 그 순간엔 아직 안 보낸다.
        assert!(!st.due(2, t0, q));
        // 400ms 안에 또 쳤다 → 시계가 다시 돈다. 연타 중엔 절대 안 나간다.
        assert!(!st.due(3, t0 + Duration::from_millis(200), q));
        assert!(!st.due(3, t0 + Duration::from_millis(500), q));
        // 손이 멎고 400ms 가 지나야 나간다.
        assert!(st.due(3, t0 + Duration::from_millis(601), q));
        // 보냈다고 기록하면 같은 세대는 다시 안 나간다 — 틱마다 물어보는 자리라
        // 이걸 빠뜨리면 같은 본문을 초당 수십 번 보낸다.
        st.sent = 3;
        assert!(!st.due(3, t0 + Duration::from_secs(9), q));
    }

    #[test]
    fn utf16_col_maps_through_wide_and_astral_chars() {
        // 한글은 UTF-16 한 유닛 — 값이 그대로다.
        assert_eq!(utf16_col_to_char("가나다", 2), 2);
        // 이모지는 두 유닛이라 char 인덱스가 절반씩 밀린다.
        let s = "🙂🙂ab";
        assert_eq!(utf16_col_to_char(s, 0), 0);
        assert_eq!(utf16_col_to_char(s, 2), 1);
        assert_eq!(utf16_col_to_char(s, 4), 2);
        assert_eq!(utf16_col_to_char(s, 5), 3);
        // 줄 끝을 넘어가면 줄 끝에서 멈춘다.
        assert_eq!(utf16_col_to_char("ab", 99), 2);
    }

    #[test]
    fn uri_round_trip_survives_spaces_and_hangul() {
        for p in ["/tmp/a b/c.rs", "/Users/kasa/내 드라이브/x.rs", "/plain/path.rs"] {
            let uri = path_to_uri(Path::new(p));
            assert!(!uri.contains(' '), "URI 에 날 공백이 남으면 서버가 파일을 못 찾는다");
            assert_eq!(uri_to_path(&uri).unwrap(), PathBuf::from(p));
        }
    }

    #[test]
    fn frame_reader_takes_the_body_after_the_blank_line() {
        let raw = b"Content-Length: 7\r\nContent-Type: x\r\n\r\n{\"a\":1}";
        let mut r = std::io::BufReader::new(&raw[..]);
        assert_eq!(read_frame(&mut r).unwrap(), b"{\"a\":1}");
        // 스트림이 끝나면 None — 서버가 죽었을 때 읽기 스레드가 조용히 끝난다.
        assert!(read_frame(&mut r).is_none());
    }

    #[test]
    fn parse_diagnostics_converts_columns_and_skips_other_messages() {
        let msg = serde_json::json!({
            "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///tmp/x.rs",
                "diagnostics": [{
                    "range": {"start": {"line": 0, "character": 4},
                              "end": {"line": 0, "character": 6}},
                    "severity": 2, "message": "unused"
                }]
            }
        });
        let (p, ds) = parse_diagnostics(&msg, |_, _| Some("🙂🙂abcd".to_string())).unwrap();
        assert_eq!(p, PathBuf::from("/tmp/x.rs"));
        assert_eq!(ds.len(), 1);
        // UTF-16 4·6 → char 2·4 (이모지 둘이 4 유닛).
        assert_eq!((ds[0].col, ds[0].end_col), (2, 4));
        assert_eq!(ds[0].severity, 2);
        // 진단이 아닌 메시지는 무시.
        let other = serde_json::json!({"jsonrpc":"2.0","method":"$/progress","params":{}});
        assert!(parse_diagnostics(&other, |_, _| None).is_none());
    }
}
