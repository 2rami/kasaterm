//! 새 기기 등록. 새 기기는 일회용 공개키의 해시를 실은 도전문에 폰 Face ID 승인을 받고, 열쇠를 가진 기기는
//! **사람이 데스크톱 화면에서 직접 믿은 폰 열쇠**로 그 서명을 다시 확인한 뒤에만 열쇠를 봉인해 넘긴다.
//! 관문이 끼워 넣어 자동으로 믿게 된 열쇠는 비밀 읽기에는 쓰여도 여기에는 못 쓴다 — 여기서는 열쇠 자체가 관문이 고른
//! 공개키로 나가기 때문이다. 폰에 보이는 글(기기 이름·지문)도 공개키에서 다시 만들어 글자까지 같아야 받는다.

use std::collections::HashMap;

use base64::Engine as _;
use serde_json::{json, Value};

use kasa_identity::approval_text::signature_ok;

use super::crypto::{join_hash, seal_for, JoinSecret, SealedKey, VaultKey};
use super::engine::RemoteError;
use super::server::{Approval, Join};
use crate::op_approval::Trusted;

pub const KIND: &str = "login-vault.join";
pub const REF: &str = "login-vault://join";
/// 도전문의 기기 이름 한도 — 관문 비밀 승인 칸과 같다.
const MAX_LABEL: usize = 64;
/// 도전문 만료의 상한 — 폰 승인은 2분이고, 여유를 1분 둔다. 그보다 먼 만료는 오래 쓰려는 승인이다.
const MAX_EXP_AHEAD_MS: u64 = 3 * 60 * 1000;

/// 관문의 등록 요청 자리.
pub trait Joins {
    fn open_join(&self, label: &str, join_pub: &str) -> Result<String, RemoteError>;
    fn attach_approval(&self, join_id: &str, approval: &Approval) -> Result<(), RemoteError>;
    fn pending_joins(&self) -> Result<Vec<Join>, RemoteError>;
    fn post_sealed(&self, join_id: &str, sealed: &SealedKey) -> Result<(), RemoteError>;
    fn take_sealed(&self, join_id: &str) -> Result<Option<SealedKey>, RemoteError>;
}

/// 폰과 새 기기 화면에 같이 보이는 64비트 지문 — 공개키 해시 앞 16자를 넷씩. 폰은 도전문의 `join` 에서 같은 값을 직접 만든다.
pub fn join_code(join_pub: &[u8]) -> String {
    let hash = join_hash(join_pub).to_uppercase();
    hash.as_bytes()[..16].chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join("-")
}

fn label_of(label: &str) -> String {
    label.chars().take(MAX_LABEL).collect()
}

/// 폰에 보일 명령 글. 열쇠를 가진 기기는 공개키에서 이 글을 다시 만들어 도전문과 글자까지 같아야 받는다.
pub fn command_for(join_pub: &[u8]) -> String {
    format!("이 기기에 로그인 금고 열쇠 주기 · 기기 지문 {}", join_code(join_pub))
}

/// 폰에 보일 도전문. 관문의 비밀 승인 꼴(학생·칸·cwd·명령·참조)을 그대로 따라, 폰은 종류 하나만 더 받으면 같은 화면으로 보인다.
pub fn challenge(account: &str, device: &str, nonce: &str, exp: u64, label: &str, join_pub: &[u8]) -> String {
    json!({
        "v": 1,
        "kind": KIND,
        "nonce": nonce,
        "account": account,
        "device": device,
        "student": label_of(label),
        "pane": "",
        "cwd": "",
        "command": command_for(join_pub),
        "refs": [REF],
        "join": join_hash(join_pub),
        "exp": exp,
    })
    .to_string()
}

/// 쓴 nonce — 같은 승인을 두 번 쓰지 않게. 만료가 지난 것은 걷는다(`op_approval` 의 장부와 같은 규칙).
#[derive(Default)]
pub struct Ledger(HashMap<String, u64>);

impl Ledger {
    fn consume(&mut self, nonce: &str, exp: u64, now: u64) -> bool {
        self.0.retain(|_, until| *until > now);
        self.0.insert(nonce.to_string(), exp).is_none()
    }
}

/// 열쇠를 가진 기기가 등록 요청 하나를 확인한다. 통과해야만 [`serve`] 가 봉인한다.
pub fn verify(join: &Join, account: &str, trusted: &[Trusted], live: &[Value], ledger: &mut Ledger, now_ms: u64) -> Result<(), &'static str> {
    let approval = join.approval.as_ref().ok_or("not_approved")?;
    let v: Value = serde_json::from_str(&approval.challenge).map_err(|_| "bad_challenge")?;
    let engine = base64::engine::general_purpose::STANDARD;
    let join_pub = engine.decode(&join.join_pub).map_err(|_| "bad_join_pub")?;
    if v["v"] != 1 || v["kind"] != KIND || v["refs"] != json!([REF]) || v["pane"] != "" || v["cwd"] != "" {
        return Err("bad_challenge");
    }
    if v["account"].as_str() != Some(account) {
        return Err("other_account");
    }
    if v["device"].as_str() != Some(join.device_id.as_str()) {
        return Err("other_device");
    }
    if v["join"].as_str() != Some(join_hash(&join_pub).as_str()) {
        return Err("join_pub_changed");
    }
    // 폰은 이 글을 그대로 보이니, 공개키에서 다시 만든 글과 다르면 사람이 본 것과 승인한 것이 다르다.
    if v["command"].as_str() != Some(command_for(&join_pub).as_str()) || v["student"].as_str() != Some(label_of(&join.label).as_str()) {
        return Err("shown_text_changed");
    }
    let nonce = v["nonce"].as_str().filter(|n| n.len() == 32 && n.bytes().all(|b| b.is_ascii_hexdigit())).ok_or("bad_challenge")?;
    let exp = v["exp"].as_u64().ok_or("bad_challenge")?;
    if now_ms >= exp {
        return Err("expired");
    }
    if exp > now_ms.saturating_add(MAX_EXP_AHEAD_MS) {
        return Err("bad_exp");
    }
    let t = trusted.iter().find(|t| t.id == approval.key && t.device == approval.by_device).ok_or("key_not_trusted")?;
    if !t.manual {
        return Err("key_not_confirmed");
    }
    let alive = live.iter().any(|k| k["id"] == approval.key.as_str() && k["device"] == approval.by_device.as_str() && k["public"] == t.public.as_str());
    if !alive {
        return Err("key_revoked");
    }
    let (Ok(public), Ok(sig)) = (engine.decode(&t.public), engine.decode(&approval.sig)) else {
        return Err("bad_signature");
    };
    if !signature_ok(&public, &approval.challenge, &sig) {
        return Err("bad_signature");
    }
    if !ledger.consume(nonce, exp, now_ms) {
        return Err("replayed");
    }
    Ok(())
}

/// 열쇠를 가진 기기의 한 바퀴 결과.
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
pub struct Served {
    pub sealed: usize,
    pub refused: usize,
    /// 거절 중 만료 판정 때문인 것 — 이 기기 시계가 관문보다 빠르면 새 기기의 승인이 조용히 만료로 보인다.
    pub clock: usize,
}

/// 열쇠를 가진 기기의 한 바퀴 — 확인된 등록마다 열쇠를 봉인해 올린다.
pub fn serve(joins: &dyn Joins, key: &VaultKey, account: &str, trusted: &[Trusted], live: &[Value], ledger: &mut Ledger, now_ms: u64) -> Served {
    let Ok(pending) = joins.pending_joins() else { return Served::default() };
    let mut served = Served::default();
    for join in pending {
        if let Err(reason) = verify(&join, account, trusted, live, ledger, now_ms) {
            served.refused += 1;
            if matches!(reason, "expired" | "bad_exp") {
                served.clock += 1;
            }
            continue;
        }
        match seal_for(key, account, &join.device_id, &join.id, &join.join_pub) {
            Ok(s) if joins.post_sealed(&join.id, &s).is_ok() => served.sealed += 1,
            _ => served.refused += 1,
        }
    }
    served
}

/// 새 기기 쪽 진행. 폰 승인은 부르는 쪽이 넘긴다(앱은 관문 비밀 승인, 시험은 가짜 폰).
pub struct Enrollment {
    secret: JoinSecret,
    pub join_id: String,
    pub code: String,
}

impl Enrollment {
    pub fn open(joins: &dyn Joins, label: &str) -> anyhow::Result<Self> {
        let secret = JoinSecret::new()?;
        let join_id = joins.open_join(&label_of(label), &secret.public_base64()).map_err(|e| anyhow::anyhow!("{e:?}"))?;
        let code = join_code(secret.public());
        Ok(Self { secret, join_id, code })
    }

    pub fn challenge(&self, account: &str, device: &str, nonce: &str, exp: u64, label: &str) -> String {
        challenge(account, device, nonce, exp, label, self.secret.public())
    }

    pub fn approved(&self, joins: &dyn Joins, approval: &Approval) -> anyhow::Result<()> {
        joins.attach_approval(&self.join_id, approval).map_err(|e| anyhow::anyhow!("{e:?}"))
    }

    /// 봉인이 와 있으면 풀어서 열쇠를 준다. 아직이면 `Ok(Err(self))` 로 돌려줘 다시 기다리게 한다.
    /// 실패하면 이 등록은 끝이다 — 관문은 봉인을 한 번만 내주므로 새로 연다.
    pub fn finish(self, joins: &dyn Joins, account: &str, device: &str, expected_key_id: &str) -> anyhow::Result<Result<VaultKey, Self>> {
        let sealed = match joins.take_sealed(&self.join_id) {
            Ok(Some(sealed)) => sealed,
            Ok(None) => return Ok(Err(self)),
            Err(e) => anyhow::bail!("{e:?}"),
        };
        let key = self.secret.open(account, device, &sealed)?;
        anyhow::ensure!(key.key_id() == expected_key_id, "key_mismatch");
        Ok(Ok(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::login_vault::server::Store;
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};

    const ACCOUNT: &str = "acct";
    const NONCE: &str = "abababababababababababababababab";

    /// 기기 하나의 눈으로 본 관문.
    struct As<'a> {
        store: &'a Store,
        device: &'a str,
        now: u64,
    }

    impl Joins for As<'_> {
        fn open_join(&self, label: &str, join_pub: &str) -> Result<String, RemoteError> {
            self.store.open_join(ACCOUNT, self.device, label, join_pub, self.now).map_err(|_| RemoteError::Failed)
        }
        fn attach_approval(&self, join_id: &str, approval: &Approval) -> Result<(), RemoteError> {
            self.store.attach_approval(ACCOUNT, self.device, join_id, approval.clone(), self.now).map_err(|_| RemoteError::Failed)
        }
        fn pending_joins(&self) -> Result<Vec<Join>, RemoteError> {
            self.store.pending_joins(ACCOUNT, self.device, self.now).map_err(|_| RemoteError::Failed)
        }
        fn post_sealed(&self, join_id: &str, sealed: &SealedKey) -> Result<(), RemoteError> {
            self.store.post_sealed(ACCOUNT, self.device, join_id, sealed.clone(), self.now).map_err(|_| RemoteError::Failed)
        }
        fn take_sealed(&self, join_id: &str) -> Result<Option<SealedKey>, RemoteError> {
            self.store.take_sealed(ACCOUNT, self.device, join_id, self.now).map_err(|_| RemoteError::Failed)
        }
    }

    struct Phone {
        pair: EcdsaKeyPair,
        trusted: Trusted,
    }

    impl Phone {
        fn new(manual: bool) -> Self {
            let rng = ring::rand::SystemRandom::new();
            let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
            let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng).unwrap();
            let public = pair.public_key().as_ref().to_vec();
            let b64 = base64::engine::general_purpose::STANDARD;
            let trusted = Trusted {
                device: "phone".into(),
                label: "폰".into(),
                id: kasa_identity::approval_text::key_id(&public),
                public: b64.encode(&public),
                added: 0,
                manual,
            };
            Self { pair, trusted }
        }

        fn approve(&self, challenge: &str) -> Approval {
            let mut message = kasa_identity::approval_text::SECRET_PREFIX.to_vec();
            message.extend_from_slice(challenge.as_bytes());
            let sig = self.pair.sign(&ring::rand::SystemRandom::new(), &message).unwrap();
            Approval {
                challenge: challenge.into(),
                key: self.trusted.id.clone(),
                sig: base64::engine::general_purpose::STANDARD.encode(sig.as_ref()),
                by_device: self.trusted.device.clone(),
            }
        }

        fn live(&self) -> Vec<Value> {
            vec![json!({"id": self.trusted.id, "device": self.trusted.device, "public": self.trusted.public})]
        }
    }

    fn vault(store: &Store) -> VaultKey {
        let key = VaultKey::generate().unwrap();
        store.init(ACCOUNT, &key.key_id(), &key.sign_pub(), "mac", 0).unwrap();
        key
    }

    #[test]
    fn a_phone_approved_device_receives_the_key_from_a_holder() {
        let store = Store::new(None);
        let key = vault(&store);
        let phone = Phone::new(true);
        let new = As { store: &store, device: "new-pc", now: 1_000 };
        let old = As { store: &store, device: "mac", now: 1_100 };
        let mut ledger = Ledger::default();

        let enrollment = Enrollment::open(&new, "새 PC").unwrap();
        assert_eq!(serve(&old, &key, ACCOUNT, &[phone.trusted.clone()], &phone.live(), &mut ledger, 1_050), Served::default(), "승인 전에는 아무것도 안 넘긴다");
        let text = enrollment.challenge(ACCOUNT, "new-pc", NONCE, 1_000 + 120_000, "새 PC");
        assert!(text.contains(&enrollment.code), "폰에 보이는 지문이 새 기기 화면과 같다");
        assert_eq!(enrollment.code.len(), 19, "64비트, 넷씩 네 묶음");
        enrollment.approved(&new, &phone.approve(&text)).unwrap();
        assert_eq!(serve(&old, &key, ACCOUNT, &[phone.trusted.clone()], &phone.live(), &mut ledger, 1_100).sealed, 1);
        let got = enrollment.finish(&new, ACCOUNT, "new-pc", &key.key_id()).unwrap().ok().unwrap();
        assert_eq!(got.key_id(), key.key_id());
    }

    #[test]
    fn the_new_device_waits_until_sealed_and_rejects_a_foreign_key() {
        let store = Store::new(None);
        let key = vault(&store);
        let phone = Phone::new(true);
        let new = As { store: &store, device: "new-pc", now: 1_000 };
        let enrollment = Enrollment::open(&new, "새 PC").unwrap();
        let enrollment = enrollment.finish(&new, ACCOUNT, "new-pc", &key.key_id()).unwrap().err().unwrap();
        let text = enrollment.challenge(ACCOUNT, "new-pc", NONCE, 121_000, "새 PC");
        enrollment.approved(&new, &phone.approve(&text)).unwrap();
        // 금고 열쇠가 다른 기기의 봉인은 관문이 서명으로 거절한다.
        let foreign = VaultKey::generate().unwrap();
        let old = As { store: &store, device: "mac", now: 1_000 };
        assert_eq!(serve(&old, &foreign, ACCOUNT, &[phone.trusted.clone()], &phone.live(), &mut Ledger::default(), 1_000), Served { sealed: 0, refused: 1, clock: 0 });
        // 이 기기 시계가 빨라 승인이 만료로 보이면 시계 거절로 센다.
        assert_eq!(serve(&old, &key, ACCOUNT, &[phone.trusted.clone()], &phone.live(), &mut Ledger::default(), 200_000), Served { sealed: 0, refused: 1, clock: 1 });
        assert_eq!(serve(&old, &key, ACCOUNT, &[phone.trusted.clone()], &phone.live(), &mut Ledger::default(), 1_000), Served { sealed: 1, refused: 0, clock: 0 });
        assert!(enrollment.finish(&new, ACCOUNT, "new-pc", &key.key_id()).unwrap().is_ok());
    }

    #[test]
    fn the_holder_refuses_swapped_keys_shown_text_strangers_and_auto_trusted_phones() {
        let store = Store::new(None);
        let _key = vault(&store);
        let phone = Phone::new(true);
        let stranger = Phone::new(true);
        let auto = Phone::new(false);
        let new = As { store: &store, device: "new-pc", now: 1_000 };
        let trusted = [phone.trusted.clone()];

        let enrollment = Enrollment::open(&new, "새 PC").unwrap();
        let text = enrollment.challenge(ACCOUNT, "new-pc", NONCE, 121_000, "새 PC");
        let approval = phone.approve(&text);
        enrollment.approved(&new, &approval).unwrap();
        let mut join = store.pending_joins(ACCOUNT, "mac", 1_000).unwrap().pop().unwrap();
        let original = join.join_pub.clone();
        let check = |join: &Join, trusted: &[Trusted], live: &[Value], now: u64| verify(join, ACCOUNT, trusted, live, &mut Ledger::default(), now);
        // 관문이 공개키를 제 것으로 바꿔치기한 요청.
        join.join_pub = JoinSecret::new().unwrap().public_base64();
        assert_eq!(check(&join, &trusted, &phone.live(), 2_000), Err("join_pub_changed"));
        join.join_pub = original.clone();
        assert_eq!(check(&join, &trusted, &phone.live(), 2_000), Ok(()));

        // 진짜 도전문의 글(기기 이름·지문)을 베끼고 join 만 제 공개키로 바꾼 도전문 — 폰에는 같은 글이 보였을 것이다.
        let evil = JoinSecret::new().unwrap();
        let mut forged: Value = serde_json::from_str(&text).unwrap();
        forged["join"] = json!(join_hash(evil.public()));
        let forged_text = forged.to_string();
        let mut swapped = join.clone();
        swapped.join_pub = evil.public_base64();
        swapped.approval = Some(phone.approve(&forged_text));
        assert_eq!(check(&swapped, &trusted, &phone.live(), 2_000), Err("shown_text_changed"));

        assert_eq!(check(&join, &[auto.trusted.clone()], &auto.live(), 2_000), Err("key_not_trusted"));
        let mut by_auto = join.clone();
        by_auto.approval = Some(auto.approve(&text));
        assert_eq!(check(&by_auto, &[auto.trusted.clone()], &auto.live(), 2_000), Err("key_not_confirmed"), "자동으로 믿은 열쇠로는 안 된다");
        assert_eq!(verify(&join, "other", &trusted, &phone.live(), &mut Ledger::default(), 2_000), Err("other_account"));
        assert_eq!(check(&join, &trusted, &[], 2_000), Err("key_revoked"), "관문에서 폐기된 폰");
        assert_eq!(check(&join, &[stranger.trusted.clone()], &stranger.live(), 2_000), Err("key_not_trusted"));
        assert_eq!(check(&join, &trusted, &phone.live(), 121_000), Err("expired"), "만료는 도전문 그대로");
        let mut ledger = Ledger::default();
        assert_eq!(verify(&join, ACCOUNT, &trusted, &phone.live(), &mut ledger, 2_000), Ok(()));
        assert_eq!(verify(&join, ACCOUNT, &trusted, &phone.live(), &mut ledger, 2_000), Err("replayed"));

        let mut huge: Value = serde_json::from_str(&text).unwrap();
        huge["exp"] = json!(u64::MAX);
        let mut overflow = join.clone();
        overflow.approval = Some(phone.approve(&huge.to_string()));
        assert_eq!(check(&overflow, &trusted, &phone.live(), 2_000), Err("bad_exp"), "아주 먼 만료는 넘침 없이 거절한다");

        let mut by_stranger = join.clone();
        by_stranger.approval = Some(Approval { sig: stranger.approve(&text).sig, ..approval });
        assert_eq!(check(&by_stranger, &trusted, &phone.live(), 2_000), Err("bad_signature"), "믿는 폰의 서명이 아니면");
    }
}
