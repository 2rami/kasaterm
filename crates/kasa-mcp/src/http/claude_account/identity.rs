use std::collections::HashMap;
use std::future::Future;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const TTL: Duration = Duration::from_secs(300);
const CAPACITY: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedClaudeIdentity {
    pub account_uuid: String,
    pub organization_uuid: String,
    pub email: String,
    pub org: String,
}

impl VerifiedClaudeIdentity {
    fn from_profile(body: &Value) -> Option<Self> {
        let uuid = |path: &str| {
            let raw = body.pointer(path)?.as_str()?;
            if raw.len() != 36 {
                return None;
            }
            let value = uuid::Uuid::parse_str(raw).ok()?;
            (!value.is_nil()).then(|| value.to_string())
        };
        let text = |paths: &[&str], limit: usize| {
            let value = pick(body, paths)?;
            let value = value.trim();
            (!value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control))
                .then(|| value.to_string())
        };
        Some(Self {
            account_uuid: uuid("/account/uuid")?,
            organization_uuid: uuid("/organization/uuid")?,
            email: text(
                &[
                    "/account/email_address",
                    "/account/email",
                    "/email_address",
                    "/email",
                ],
                320,
            )?,
            org: text(
                &[
                    "/organization/name",
                    "/account/organization_name",
                    "/organization_name",
                ],
                1024,
            )?,
        })
    }
}

#[derive(Clone)]
struct Entry {
    at: Instant,
    response: Value,
    proof: Option<VerifiedClaudeIdentity>,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<[u8; 32], Entry>,
}

impl Cache {
    fn get(&mut self, token: &str, now: Instant) -> Option<Entry> {
        self.entries
            .retain(|_, entry| now.saturating_duration_since(entry.at) < TTL);
        self.entries.get(&token_key(token)?).cloned()
    }

    fn insert(&mut self, token: &str, body: &Value, now: Instant) -> Value {
        let response = profile_response(body);
        let Some(key) = token_key(token) else {
            return response;
        };
        if response["ok"] != true {
            return response;
        }
        self.entries
            .retain(|_, entry| now.saturating_duration_since(entry.at) < TTL);
        if !self.entries.contains_key(&key) && self.entries.len() >= CAPACITY {
            if let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.at)
                .map(|(key, _)| *key)
            {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(
            key,
            Entry {
                at: now,
                response: response.clone(),
                proof: VerifiedClaudeIdentity::from_profile(body),
            },
        );
        response
    }
}

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn token_key(token: &str) -> Option<[u8; 32]> {
    if token.is_empty() || token.len() > 16 * 1024 || token.chars().any(char::is_control) {
        return None;
    }
    let mut hash = Sha256::new();
    hash.update(b"kasaterm:verified-claude-profile:v1\0");
    hash.update(token.as_bytes());
    Some(hash.finalize().into())
}

/// A cached display identity cannot authorize a different credential after a slot is reused.
/// This lookup is memory-only; a miss never reads credentials or starts a provider request.
pub fn verified_claude_identity_for_token(token: &str) -> Option<VerifiedClaudeIdentity> {
    cache().lock().ok()?.get(token, Instant::now())?.proof
}

pub(super) fn cached_response(token: &str) -> Option<Value> {
    cache()
        .lock()
        .ok()?
        .get(token, Instant::now())
        .map(|entry| entry.response)
}

pub(super) async fn resolve<F, Fut>(read_current: impl Fn() -> Option<String>, fetch: F) -> Value
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Option<Value>>,
{
    resolve_with(cache(), read_current, fetch).await
}

async fn resolve_with<F, Fut>(
    cache: &Mutex<Cache>,
    read_current: impl Fn() -> Option<String>,
    fetch: F,
) -> Value
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Option<Value>>,
{
    let Some(token) = read_current().filter(|token| token_key(token).is_some()) else {
        return json!({"ok":false,"error":"no token"});
    };
    if let Some(entry) = cache
        .lock()
        .ok()
        .and_then(|mut cache| cache.get(&token, Instant::now()))
    {
        return entry.response;
    }
    let Some(body) = fetch(token.clone()).await else {
        return json!({"ok":false,"error":"profile api unavailable"});
    };
    // A request that started before logout may prove the old token, but must not repaint a new slot.
    if read_current().as_deref() != Some(token.as_str()) {
        return json!({"ok":false,"error":"credentials changed"});
    }
    let Ok(mut cache) = cache.lock() else {
        return json!({"ok":false,"error":"profile cache unavailable"});
    };
    cache.insert(&token, &body, Instant::now())
}

fn pick(body: &Value, paths: &[&str]) -> Option<String> {
    paths
        .iter()
        .filter_map(|path| body.pointer(path).and_then(Value::as_str))
        .find(|value| !value.is_empty())
        .map(str::to_string)
}

fn profile_response(body: &Value) -> Value {
    let email = pick(
        body,
        &[
            "/account/email_address",
            "/account/email",
            "/email_address",
            "/email",
        ],
    );
    let org = pick(
        body,
        &[
            "/organization/name",
            "/account/organization_name",
            "/organization_name",
        ],
    );
    let account = email.is_some().then(|| {
        let get = |path: &str| body.pointer(path).cloned().unwrap_or(Value::Null);
        json!({
            "accountUuid": get("/account/uuid"),
            "emailAddress": email.clone(),
            "displayName": get("/account/display_name"),
            "accountCreatedAt": get("/account/created_at"),
            "organizationUuid": get("/organization/uuid"),
            "organizationName": org.clone(),
            "organizationType": get("/organization/organization_type"),
            "billingType": get("/organization/billing_type"),
            "organizationRateLimitTier": get("/organization/rate_limit_tier"),
            "seatTier": get("/organization/seat_tier"),
            "hasExtraUsageEnabled": get("/organization/has_extra_usage_enabled"),
            "subscriptionCreatedAt": get("/organization/subscription_created_at"),
        })
    });
    json!({"ok":email.is_some(),"email":email,"org":org,"account":account})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    fn profile(account: u128, org: u128) -> Value {
        json!({
            "account":{"uuid":uuid::Uuid::from_u128(account).to_string(),"email_address":"person@example.test","display_name":"Person"},
            "organization":{"uuid":uuid::Uuid::from_u128(org).to_string(),"name":"A shared display name"}
        })
    }

    #[tokio::test]
    async fn a_reused_slot_never_returns_the_previous_tokens_identity() {
        let cache = Mutex::new(Cache::default());
        let store = RefCell::new(Some("fixture-access-a".to_string()));
        let calls = Cell::new(0);
        let read = || store.borrow().clone();
        let fetch = |token: String| {
            calls.set(calls.get() + 1);
            std::future::ready(Some(if token == "fixture-access-a" {
                profile(1, 11)
            } else {
                profile(2, 22)
            }))
        };
        let a = resolve_with(&cache, read, fetch).await;
        assert_eq!(
            a["account"]["accountUuid"],
            uuid::Uuid::from_u128(1).to_string()
        );
        resolve_with(&cache, read, fetch).await;
        assert_eq!(calls.get(), 1);
        *store.borrow_mut() = Some("fixture-access-b".into());
        assert!(cache
            .lock()
            .unwrap()
            .get("fixture-access-b", Instant::now())
            .is_none());
        let b = resolve_with(&cache, read, fetch).await;
        assert_eq!(
            b["account"]["accountUuid"],
            uuid::Uuid::from_u128(2).to_string()
        );
        assert_eq!(calls.get(), 2);
        *store.borrow_mut() = None;
        assert_eq!(resolve_with(&cache, read, fetch).await["ok"], false);
        assert_eq!(calls.get(), 2);
    }

    #[tokio::test]
    async fn changing_credentials_during_profile_lookup_cannot_publish_a_proof() {
        let cache = Mutex::new(Cache::default());
        let store = RefCell::new(Some("fixture-access-a".to_string()));
        let result = resolve_with(
            &cache,
            || store.borrow().clone(),
            |_| {
                *store.borrow_mut() = Some("fixture-access-b".into());
                std::future::ready(Some(profile(1, 11)))
            },
        )
        .await;
        assert_eq!(result["error"], "credentials changed");
        assert!(cache.lock().unwrap().entries.is_empty());
    }

    #[tokio::test]
    async fn failed_provider_and_missing_token_never_create_a_proof() {
        let cache = Mutex::new(Cache::default());
        assert_eq!(
            resolve_with(
                &cache,
                || Some("fixture-access-a".into()),
                |_| std::future::ready(None)
            )
            .await["ok"],
            false
        );
        let result = resolve_with(
            &cache,
            || None,
            |_| async { panic!("provider called without a token") },
        )
        .await;
        assert_eq!(result["error"], "no token");
        assert!(cache.lock().unwrap().entries.is_empty());
    }

    #[test]
    fn display_names_do_not_stand_in_for_uuid_identity() {
        let a = VerifiedClaudeIdentity::from_profile(&profile(1, 11)).unwrap();
        let b = VerifiedClaudeIdentity::from_profile(&profile(1, 22)).unwrap();
        assert_eq!(a.email, b.email);
        assert_eq!(a.org, b.org);
        assert_ne!(a.organization_uuid, b.organization_uuid);
        for path in [
            "/account/uuid",
            "/organization/uuid",
            "/account/email_address",
            "/organization/name",
        ] {
            for invalid in [Value::Null, json!(""), json!("\n")] {
                let mut value = profile(1, 11);
                *value.pointer_mut(path).unwrap() = invalid;
                assert!(VerifiedClaudeIdentity::from_profile(&value).is_none());
            }
        }
        for invalid in ["not-a-uuid", "00000000-0000-0000-0000-000000000000"] {
            let mut value = profile(1, 11);
            value["organization"]["uuid"] = invalid.into();
            assert!(VerifiedClaudeIdentity::from_profile(&value).is_none());
        }
        let mut display_only = profile(1, 11);
        display_only["organization"]["uuid"] = Value::Null;
        let mut cache = Cache::default();
        assert_eq!(
            cache.insert("fixture", &display_only, Instant::now())["ok"],
            true
        );
        assert!(cache
            .get("fixture", Instant::now())
            .unwrap()
            .proof
            .is_none());
    }

    #[test]
    fn proofs_expire_without_sliding_and_the_cache_is_bounded() {
        let start = Instant::now();
        let mut cache = Cache::default();
        cache.insert("fixture", &profile(1, 11), start);
        assert!(cache
            .get("fixture", start + TTL - Duration::from_millis(1))
            .unwrap()
            .proof
            .is_some());
        assert!(cache.get("fixture", start + TTL).is_none());
        for i in 0..=CAPACITY {
            cache.insert(
                &format!("fixture-{i}"),
                &profile(1, 11),
                start + Duration::from_millis(i as u64),
            );
        }
        assert_eq!(cache.entries.len(), CAPACITY);
        assert!(cache
            .get("fixture-0", start + Duration::from_secs(1))
            .is_none());
        assert!(cache
            .get(
                &format!("fixture-{CAPACITY}"),
                start + Duration::from_secs(1)
            )
            .is_some());
    }

    #[test]
    fn the_public_lookup_is_memory_only_and_never_guesses() {
        assert!(verified_claude_identity_for_token("").is_none());
        assert!(verified_claude_identity_for_token("unobserved-fixture-access").is_none());
    }
}
