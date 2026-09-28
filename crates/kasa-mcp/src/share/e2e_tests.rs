//! 두 기기를 한 프로세스에 띄워 진짜 HTTP 로 주고받는다 — 훑기·목록·조각 받기·충돌·지움.

use super::pull::{pull, Peer};
use super::{serve, Engine, META};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

struct Machine {
    engine: Arc<Engine>,
    root: PathBuf,
    base: String,
    pending: HashMap<String, (u64, u64)>,
}

impl Machine {
    async fn up(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("kasa-share-e2e-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(META).join("tmp")).unwrap();
        let engine = Arc::new(Engine::open(root.clone(), format!("id-{name}"), name.to_string()));
        let router = axum::Router::new()
            .route("/term/share/manifest", axum::routing::get(serve::manifest))
            .route("/term/share/file", axum::routing::get(serve::file))
            .route("/term/share/list", axum::routing::get(serve::list))
            .layer(axum::Extension(engine.clone()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self { engine, root, base, pending: HashMap::new() }
    }

    fn peer(&self) -> Peer {
        Peer { id: self.engine.me.clone(), label: self.engine.label.clone(), base: self.base.clone() }
    }

    fn settle(&mut self) {
        self.engine.scan(&mut self.pending);
        self.engine.scan(&mut self.pending);
    }

    fn write(&self, rel: &str, body: &[u8]) {
        let p = super::join(&self.root, rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn files(&self) -> std::collections::BTreeMap<String, Vec<u8>> {
        let mut found = Vec::new();
        super::walk(&self.root, "", &mut found);
        found.into_iter().map(|(rel, _, _)| (rel.clone(), std::fs::read(super::join(&self.root, &rel)).unwrap())).collect()
    }

    fn seq(&self) -> u64 {
        self.engine.with_index(|i| i.seq)
    }
}

async fn exchange(a: &mut Machine, b: &mut Machine, client: &reqwest::Client) {
    for _ in 0..3 {
        a.settle();
        b.settle();
        let got = pull(&a.engine, client, &b.peer()).await;
        assert!(got.ok, "{:?}", got.error);
        let got = pull(&b.engine, client, &a.peer()).await;
        assert!(got.ok, "{:?}", got.error);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_machines_over_http() {
    let client = reqwest::Client::new();
    let mut a = Machine::up("A").await;
    let mut b = Machine::up("B").await;

    // 9MB — 조각 셋으로 나뉘어 와야 한다.
    let big: Vec<u8> = (0..9 * 1024 * 1024u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    b.write("2026-09-28-시안/big.png", &big);
    b.write("2026-09-28-시안/note.md", b"# hi");
    #[cfg(unix)]
    std::os::unix::fs::symlink("/etc/hosts", b.root.join("2026-09-28-시안/escape")).unwrap();
    exchange(&mut a, &mut b, &client).await;
    assert_eq!(a.files(), b.files());
    assert_eq!(a.files()["2026-09-28-시안/big.png"], big);
    assert!(!a.root.join("2026-09-28-시안/escape").exists(), "a link must not travel");

    // 다 맞춘 뒤 한 바퀴 더 돌려도 아무도 판을 안 올린다(되돌려 보내기 없음).
    let (sa, sb) = (a.seq(), b.seq());
    exchange(&mut a, &mut b, &client).await;
    assert_eq!((sa, sb), (a.seq(), b.seq()));

    // 같은 파일을 양쪽에서 따로 고치면 둘 다 남는다.
    a.write("2026-09-28-시안/note.md", b"from A");
    b.write("2026-09-28-시안/note.md", b"from B");
    exchange(&mut a, &mut b, &client).await;
    let files = a.files();
    assert_eq!(files, b.files());
    let notes: Vec<&Vec<u8>> = files.iter().filter(|(k, _)| k.contains("note")).map(|(_, v)| v).collect();
    assert_eq!(notes.len(), 2, "{:?}", files.keys().collect::<Vec<_>>());
    assert!(notes.contains(&&b"from A".to_vec()) && notes.contains(&&b"from B".to_vec()));

    // 한쪽에서 지우면 다른 쪽은 휴지통으로.
    std::fs::remove_file(b.root.join("2026-09-28-시안/big.png")).unwrap();
    exchange(&mut a, &mut b, &client).await;
    assert!(!a.root.join("2026-09-28-시안/big.png").exists());
    assert_eq!(std::fs::read_dir(a.root.join(META).join("trash")).unwrap().count(), 1);
    assert_eq!(a.files(), b.files());

    // 창구: 폴더 밖·목록에 없는 링크는 못 읽고, html 류는 샌드박스로 나간다.
    let get = |q: &str| client.get(format!("{}/term/share/file?{q}", a.base)).send();
    assert_eq!(get("path=../../etc/hosts").await.unwrap().status(), 400);
    assert_eq!(get("path=2026-09-28-%EC%8B%9C%EC%95%88/escape").await.unwrap().status(), 404);
    let md = get("path=2026-09-28-%EC%8B%9C%EC%95%88/note.md").await.unwrap();
    assert_eq!(md.status(), 200);
    assert_eq!(md.headers()["content-security-policy"], "sandbox");
    assert!(md.headers()["content-type"].to_str().unwrap().starts_with("text/markdown"));
    let stale = get("path=2026-09-28-%EC%8B%9C%EC%95%88/note.md&sha=00").await.unwrap();
    assert_eq!(stale.status(), 409);

    // 폰 목록 모양.
    let list: serde_json::Value = client.get(format!("{}/term/share/list", a.base)).send().await.unwrap().json().await.unwrap();
    assert_eq!(list["name"], "KASA-share");
    assert_eq!(list["folders"][0]["name"], "2026-09-28-시안");
    let first = &list["folders"][0]["files"][0];
    assert_eq!(first["kind"], "markdown");
    assert!(first["path"].as_str().unwrap().starts_with("2026-09-28-시안/"));

    let _ = std::fs::remove_dir_all(&a.root);
    let _ = std::fs::remove_dir_all(&b.root);
}
