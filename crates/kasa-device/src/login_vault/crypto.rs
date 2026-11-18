//! 금고 열쇠와 거기서 가른 키들, 항목 봉인, 쓰기 서명, 새 기기에 열쇠를 넘기는 봉인. 관문은 여기서 나온 것만 본다.

use anyhow::{anyhow, bail, ensure};
use base64::Engine as _;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use ring::agreement::{self, EphemeralPrivateKey, UnparsedPublicKey, X25519};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{Ed25519KeyPair, KeyPair as _, ED25519};
use ring::{hkdf, hmac};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use zeroize::{Zeroize, Zeroizing};

const SALT: &[u8] = b"kasa.login-vault.v1";
/// 항목 평문을 이 단위로 채운다 — 봉인 길이로 비밀번호 길이를 짐작하지 못하게.
const PAD: usize = 256;
/// 채운 평문의 한도. 관문의 덩어리 한도(4KB) 안에 base64 로 들어가는 크기다.
pub const MAX_PLAIN: usize = 2048;
const FINGERPRINT_ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn random<const N: usize>() -> anyhow::Result<[u8; N]> {
    let mut out = [0u8; N];
    SystemRandom::new().fill(&mut out).map_err(|_| anyhow!("random"))?;
    Ok(out)
}

fn hkdf32(ikm: &[u8], info: &[u8]) -> Zeroizing<[u8; 32]> {
    let mut out = Zeroizing::new([0u8; 32]);
    hkdf::Salt::new(hkdf::HKDF_SHA256, SALT)
        .extract(ikm)
        .expand(&[info], hkdf::HKDF_SHA256)
        .and_then(|okm| okm.fill(out.as_mut()))
        .expect("32 bytes is a valid HKDF-SHA256 length");
    out
}

fn aead_seal(key: &[u8; 32], aad: &str, plain: &[u8]) -> anyhow::Result<Vec<u8>> {
    let key = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).map_err(|_| anyhow!("key"))?);
    let nonce = random::<NONCE_LEN>()?;
    // 제자리에서 봉인해 이 버퍼에는 봉인된 것만 남는다.
    let mut buf = plain.to_vec();
    key.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::from(aad.as_bytes()), &mut buf)
        .map_err(|_| anyhow!("seal"))?;
    let mut out = nonce.to_vec();
    out.extend_from_slice(&buf);
    Ok(out)
}

/// 제자리에서 풀고 그 버퍼를 그대로 돌려준다 — 사본을 만들지 않아 놓을 때 평문이 지워진다.
fn aead_open(key: &[u8; 32], aad: &str, sealed: &[u8]) -> anyhow::Result<Zeroizing<Vec<u8>>> {
    ensure!(sealed.len() > NONCE_LEN, "short");
    let key = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).map_err(|_| anyhow!("key"))?);
    let (nonce, body) = sealed.split_at(NONCE_LEN);
    let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| anyhow!("nonce"))?;
    let mut buf = Zeroizing::new(body.to_vec());
    let len = key.open_in_place(nonce, Aad::from(aad.as_bytes()), &mut buf).map_err(|_| anyhow!("open"))?.len();
    buf.truncate(len);
    Ok(buf)
}

/// 금고 열쇠. 기기 키체인에만 두고, 관문에는 [`VaultKey::key_id`] 와 쓰기 서명 공개키만 간다.
pub struct VaultKey(Zeroizing<[u8; 32]>);

impl std::fmt::Debug for VaultKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VaultKey([redacted])")
    }
}

/// 사람이 두 화면에서 맞춰 보는 80비트 지문 — `XXXX-XXXX-XXXX-XXXX`. 2분 안에 같은 앞자리를 찾을 수 없는 길이다.
pub fn fingerprint_of(bytes: &[u8]) -> String {
    let mut bits: u128 = 0;
    for b in &bytes[..10] {
        bits = (bits << 8) | *b as u128;
    }
    let chars: Vec<char> = (0..16).map(|i| FINGERPRINT_ALPHABET[((bits >> (75 - i * 5)) & 31) as usize] as char).collect();
    chars.chunks(4).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>().join("-")
}

impl VaultKey {
    pub fn generate() -> anyhow::Result<Self> {
        Ok(Self(Zeroizing::new(random::<32>()?)))
    }

    /// 키체인에 둘 꼴.
    pub fn to_base64(&self) -> Zeroizing<String> {
        Zeroizing::new(b64().encode(*self.0))
    }

    pub fn from_base64(text: &str) -> anyhow::Result<Self> {
        let mut raw = Zeroizing::new(b64().decode(text.trim()).map_err(|_| anyhow!("bad_key"))?);
        let bytes: [u8; 32] = raw.as_slice().try_into().map_err(|_| anyhow!("bad_key"))?;
        raw.zeroize();
        Ok(Self(Zeroizing::new(bytes)))
    }

    fn derive(&self, label: &str) -> Zeroizing<[u8; 32]> {
        hkdf32(self.0.as_ref(), label.as_bytes())
    }

    /// 같은 금고 열쇠에서 다른 쓰임(`browser-sync` 등)의 열쇠를 가른다. 그 열쇠의 표식·서명·봉인은 이 열쇠의 것과 섞이지
    /// 않아, 한 쓰임의 덩어리나 서명을 다른 쓰임에 끼워 넣지 못한다. 기기에 금고 열쇠가 있으면 따로 등록하지 않는다.
    pub fn subkey(&self, purpose: &str) -> VaultKey {
        VaultKey(self.derive(&format!("subkey:{purpose}")))
    }

    /// 관문에 두는 열쇠 표식 — 잘못된 열쇠를 항목을 풀어 보기 전에 가린다. 열쇠는 여기서 거꾸로 못 구한다.
    pub fn key_id(&self) -> String {
        hex(&self.derive("key-id")[..16])
    }

    pub fn fingerprint(&self) -> String {
        fingerprint_of(self.derive("fingerprint").as_ref())
    }

    fn signer(&self) -> Ed25519KeyPair {
        Ed25519KeyPair::from_seed_unchecked(self.derive("sign").as_ref()).expect("32-byte Ed25519 seed")
    }

    /// 쓰기 서명 공개키. 관문은 금고를 만들 때 이것을 받아, 이 서명이 맞는 쓰기만 받는다 — 열쇠 없는 기기는 못 쓴다.
    pub fn sign_pub(&self) -> String {
        b64().encode(self.signer().public_key().as_ref())
    }

    pub fn sign(&self, message: &[u8]) -> String {
        b64().encode(self.signer().sign(message).as_ref())
    }

    /// 항목 이름. 아이디는 브라우저처럼 대소문자를 가리지 않아 소문자로 넣는다. 관문은 사이트·아이디를 모른다.
    pub fn item_id(&self, host: &str, username: &str) -> String {
        let key = hmac::Key::new(hmac::HMAC_SHA256, self.derive("item-id").as_ref());
        hex(hmac::sign(&key, format!("web:{host}\n{}", username.to_lowercase()).as_bytes()).as_ref())
    }

    /// 이름 하나의 항목 이름 — [`Self::item_id`] 와 같은 이름 키로, 쓰임이 정한 글을 그대로 넣는다.
    pub fn name_id(&self, name: &str) -> String {
        let key = hmac::Key::new(hmac::HMAC_SHA256, self.derive("item-id").as_ref());
        hex(hmac::sign(&key, name.as_bytes()).as_ref())
    }

    /// 로컬 상태 파일에 남기는 값 지문. 열쇠 없는 해시면 그 파일만으로 비밀번호를 대입해 맞혀 볼 수 있다.
    pub fn value_mac(&self, value: &[u8]) -> String {
        let key = hmac::Key::new(hmac::HMAC_SHA256, self.derive("state").as_ref());
        hex(hmac::sign(&key, value).as_ref())
    }

    fn item_aad(account: &str, id: &str) -> String {
        format!("kasa.login-vault.v1:{account}:{id}")
    }

    /// 항목 하나를 봉인한다. AAD 에 계정·항목 이름을 넣어 관문이 덩어리를 다른 항목 자리로 옮기면 풀리지 않는다.
    pub fn seal(&self, account: &str, id: &str, plain: &[u8]) -> anyhow::Result<String> {
        self.seal_padded(account, id, plain, PAD, MAX_PLAIN)
    }

    /// [`Self::seal`] 을 다른 채움 단위·한도로. 열린 탭 목록처럼 큰 평문은 큰 단위로 채운다.
    pub fn seal_padded(&self, account: &str, id: &str, plain: &[u8], pad: usize, max: usize) -> anyhow::Result<String> {
        let padded = (4 + plain.len()).div_ceil(pad) * pad;
        ensure!(padded <= max, "too_large");
        let mut buf = Zeroizing::new(Vec::with_capacity(padded));
        buf.extend_from_slice(&(plain.len() as u32).to_le_bytes());
        buf.extend_from_slice(plain);
        buf.resize(padded, 0);
        Ok(b64().encode(aead_seal(&self.derive("enc"), &Self::item_aad(account, id), &buf)?))
    }

    pub fn open(&self, account: &str, id: &str, blob: &str) -> anyhow::Result<Zeroizing<Vec<u8>>> {
        self.open_padded(account, id, blob, PAD)
    }

    /// [`Self::open`] 을 [`Self::seal_padded`] 의 채움 단위로. 채운 길이가 그 단위의 배수가 아니면 받지 않는다.
    pub fn open_padded(&self, account: &str, id: &str, blob: &str, pad: usize) -> anyhow::Result<Zeroizing<Vec<u8>>> {
        let sealed = b64().decode(blob).map_err(|_| anyhow!("bad_blob"))?;
        let padded = aead_open(&self.derive("enc"), &Self::item_aad(account, id), &sealed)?;
        ensure!(padded.len() >= 4 && padded.len() % pad == 0, "bad_padding");
        let len = u32::from_le_bytes(padded[..4].try_into().expect("4 bytes")) as usize;
        ensure!(len <= padded.len() - 4, "bad_padding");
        Ok(Zeroizing::new(padded[4..4 + len].to_vec()))
    }
}

/// 관문이 쓰기 서명을 확인한다.
pub fn signature_ok(sign_pub: &str, message: &[u8], sig: &str) -> bool {
    let (Ok(public), Ok(sig)) = (b64().decode(sign_pub), b64().decode(sig)) else { return false };
    ring::signature::UnparsedPublicKey::new(&ED25519, public).verify(message, &sig).is_ok()
}

/// 새 기기가 등록하는 동안 쥐는 일회용 X25519 열쇠. 봉인을 한 번 풀면 사라진다.
pub struct JoinSecret {
    private: EphemeralPrivateKey,
    public: [u8; 32],
}

/// 열쇠를 가진 기기가 새 기기의 공개키로 봉인한 금고 열쇠.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedKey {
    pub eph_pub: String,
    pub blob: String,
    /// 금고 쓰기 서명 — 관문은 열쇠를 가진 기기의 봉인만 받는다.
    #[serde(default)]
    pub sig: String,
}

fn join_aad(account: &str, device_id: &str) -> String {
    format!("kasa.login-vault.join.v1:{account}:{device_id}")
}

fn join_key(shared: &[u8], eph_pub: &[u8], join_pub: &[u8]) -> Zeroizing<[u8; 32]> {
    let mut info = b"join".to_vec();
    info.extend_from_slice(eph_pub);
    info.extend_from_slice(join_pub);
    hkdf32(shared, &info)
}

/// 폰 도전문에 싣는 새 기기 공개키 해시. 관문이 공개키를 바꿔치기하면 폰 서명이 이것과 맞지 않는다.
pub fn join_hash(join_pub: &[u8]) -> String {
    hex(&sha2::Sha256::digest(join_pub))
}

impl JoinSecret {
    pub fn new() -> anyhow::Result<Self> {
        let private = EphemeralPrivateKey::generate(&X25519, &SystemRandom::new()).map_err(|_| anyhow!("random"))?;
        let public: [u8; 32] = private
            .compute_public_key()
            .map_err(|_| anyhow!("public"))?
            .as_ref()
            .try_into()
            .map_err(|_| anyhow!("public"))?;
        Ok(Self { private, public })
    }

    pub fn public_base64(&self) -> String {
        b64().encode(self.public)
    }

    pub fn public(&self) -> &[u8; 32] {
        &self.public
    }

    pub fn open(self, account: &str, device_id: &str, sealed: &SealedKey) -> anyhow::Result<VaultKey> {
        let eph = b64().decode(&sealed.eph_pub).map_err(|_| anyhow!("bad_sealed"))?;
        let blob = b64().decode(&sealed.blob).map_err(|_| anyhow!("bad_sealed"))?;
        let public = self.public;
        let key = agreement::agree_ephemeral(self.private, &UnparsedPublicKey::new(&X25519, &eph), |shared| {
            join_key(shared, &eph, &public)
        })
        .map_err(|_| anyhow!("bad_sealed"))?;
        let raw = aead_open(&key, &join_aad(account, device_id), &blob)?;
        let bytes: [u8; 32] = raw.as_slice().try_into().map_err(|_| anyhow!("bad_sealed"))?;
        Ok(VaultKey(Zeroizing::new(bytes)))
    }
}

/// 봉인 서명이 덮는 글.
pub fn seal_message(account: &str, join_id: &str, eph_pub: &str, blob: &str) -> Vec<u8> {
    format!("kasa.login-vault.seal.v1\n{account}\n{join_id}\n{eph_pub}\n{blob}\n").into_bytes()
}

/// 열쇠를 가진 기기 쪽 — 폰 서명을 확인한 뒤에만 부른다(`join::serve`).
pub fn seal_for(key: &VaultKey, account: &str, device_id: &str, join_id: &str, join_pub_b64: &str) -> anyhow::Result<SealedKey> {
    let join_pub = b64().decode(join_pub_b64).map_err(|_| anyhow!("bad_join_pub"))?;
    if join_pub.len() != 32 {
        bail!("bad_join_pub");
    }
    let eph = EphemeralPrivateKey::generate(&X25519, &SystemRandom::new()).map_err(|_| anyhow!("random"))?;
    let eph_pub = eph.compute_public_key().map_err(|_| anyhow!("public"))?.as_ref().to_vec();
    let wrap = agreement::agree_ephemeral(eph, &UnparsedPublicKey::new(&X25519, &join_pub), |shared| {
        join_key(shared, &eph_pub, &join_pub)
    })
    .map_err(|_| anyhow!("bad_join_pub"))?;
    let blob = b64().encode(aead_seal(&wrap, &join_aad(account, device_id), key.0.as_ref())?);
    let eph_pub = b64().encode(eph_pub);
    let sig = key.sign(&seal_message(account, join_id, &eph_pub, &blob));
    Ok(SealedKey { eph_pub, blob, sig })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_round_trip_hides_length_and_binds_the_slot() {
        let key = VaultKey::generate().unwrap();
        let id = key.item_id("example.com", "Alice");
        assert_eq!(id, key.item_id("example.com", "alice"), "아이디는 대소문자를 가리지 않는다");
        assert_ne!(id, key.item_id("example.org", "alice"));
        let short = key.seal("acct", &id, b"a").unwrap();
        let long = key.seal("acct", &id, &[b'x'; 200]).unwrap();
        assert_eq!(short.len(), long.len(), "같은 채움 단위면 길이가 같다");
        assert_eq!(key.open("acct", &id, &long).unwrap().as_slice(), &[b'x'; 200][..]);
        let other = key.item_id("example.com", "bob");
        assert!(key.open("acct", &other, &short).is_err(), "다른 자리로 옮긴 덩어리는 풀리지 않는다");
        assert!(key.open("other", &id, &short).is_err(), "다른 계정으로 옮긴 덩어리도");
        assert!(VaultKey::generate().unwrap().open("acct", &id, &short).is_err());
        assert!(key.seal("acct", &id, &[0u8; MAX_PLAIN]).is_err());
    }

    #[test]
    fn derived_values_are_stable_per_key_and_reveal_nothing_without_it() {
        let key = VaultKey::generate().unwrap();
        let again = VaultKey::from_base64(&key.to_base64()).unwrap();
        assert_eq!(key.key_id(), again.key_id());
        assert_eq!(key.key_id().len(), 32);
        assert_eq!(key.fingerprint(), again.fingerprint());
        assert_eq!(key.fingerprint().len(), 19, "4자씩 4묶음, 80비트");
        assert_eq!(key.value_mac(b"pw"), again.value_mac(b"pw"));
        let other = VaultKey::generate().unwrap();
        assert_ne!(key.value_mac(b"pw"), other.value_mac(b"pw"), "값 지문은 열쇠마다 달라 대입으로 못 맞힌다");
        assert_ne!(key.key_id(), other.key_id());
        assert!(VaultKey::from_base64("c2hvcnQ=").is_err());
    }

    #[test]
    fn a_subkey_shares_nothing_with_its_vault_key() {
        let key = VaultKey::generate().unwrap();
        let sub = key.subkey("browser-sync");
        assert_eq!(sub.key_id(), VaultKey::from_base64(&key.to_base64()).unwrap().subkey("browser-sync").key_id(), "같은 열쇠면 같은 갈래");
        assert_ne!(sub.key_id(), key.key_id());
        assert_ne!(sub.sign_pub(), key.sign_pub());
        assert_ne!(sub.key_id(), key.subkey("other").key_id());
        let id = sub.name_id("mark
c
m1");
        let blob = sub.seal_padded("acct", &id, &[b'x'; 3000], 4096, 8192).unwrap();
        assert_eq!(sub.open_padded("acct", &id, &blob, 4096).unwrap().len(), 3000);
        assert!(sub.open_padded("acct", &id, &blob, 8192).is_err(), "채움 단위가 다르면 받지 않는다");
        assert!(key.open("acct", &id, &blob).is_err(), "금고 열쇠로는 갈래의 덩어리가 안 풀린다");
        assert!(!signature_ok(&key.sign_pub(), b"m", &sub.sign(b"m")), "갈래의 서명은 금고 쓰기로 못 쓴다");
        assert!(sub.seal_padded("acct", &id, &[0u8; 9000], 4096, 8192).is_err());
    }

    #[test]
    fn writes_are_signed_by_the_vault_key_only() {
        let key = VaultKey::generate().unwrap();
        let sig = key.sign(b"message");
        assert!(signature_ok(&key.sign_pub(), b"message", &sig));
        assert!(!signature_ok(&key.sign_pub(), b"other", &sig));
        assert!(!signature_ok(&VaultKey::generate().unwrap().sign_pub(), b"message", &sig), "다른 열쇠의 서명은 아니다");
        assert_eq!(VaultKey::from_base64(&key.to_base64()).unwrap().sign_pub(), key.sign_pub());
    }

    #[test]
    fn join_hands_the_key_only_to_the_matching_secret() {
        let key = VaultKey::generate().unwrap();
        let secret = JoinSecret::new().unwrap();
        let sealed = seal_for(&key, "acct", "dev-new", "j1", &secret.public_base64()).unwrap();
        assert!(signature_ok(&key.sign_pub(), &seal_message("acct", "j1", &sealed.eph_pub, &sealed.blob), &sealed.sig));
        let got = secret.open("acct", "dev-new", &sealed).unwrap();
        assert_eq!(got.key_id(), key.key_id());

        let wrong_device = JoinSecret::new().unwrap();
        let sealed = seal_for(&key, "acct", "dev-new", "j1", &wrong_device.public_base64()).unwrap();
        assert!(wrong_device.open("acct", "dev-other", &sealed).is_err(), "AAD 의 기기 id 가 다르면 풀리지 않는다");

        let intended = JoinSecret::new().unwrap();
        let intruder = JoinSecret::new().unwrap();
        let sealed = seal_for(&key, "acct", "dev-new", "j1", &intended.public_base64()).unwrap();
        assert!(intruder.open("acct", "dev-new", &sealed).is_err(), "다른 일회용 열쇠로는 못 푼다");
        assert_eq!(join_hash(intended.public()).len(), 64);
        assert!(seal_for(&key, "acct", "dev-new", "j1", "c2hvcnQ=").is_err());
    }
}
