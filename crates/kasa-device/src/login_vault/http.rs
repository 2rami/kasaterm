//! 앱 쪽 관문 클라이언트 — 동기화 스레드가 제 런타임에서 막고 부른다.

use std::time::Duration;

use serde_json::{json, Value};

use super::crypto::SealedKey;
use super::engine::{Remote, RemoteError};
use super::join::Joins;
use super::server::{Approval, Fetched, Join, Put, PutOutcome};
use crate::device_auth::DeviceCred;

pub struct Http {
    client: reqwest::Client,
    runtime: tokio::runtime::Runtime,
    relay: String,
    token: String,
    /// 관문 경로 — 로그인 금고는 `/relay/login-vault`, 북마크·탭 동기화는 `/relay/browser-sync`.
    base: &'static str,
}

impl Http {
    pub fn new(credential: &DeviceCred) -> anyhow::Result<Self> {
        Self::at(credential, "/relay/login-vault")
    }

    pub fn at(credential: &DeviceCred, base: &'static str) -> anyhow::Result<Self> {
        Ok(Self {
            base,
            client: reqwest::Client::builder().timeout(Duration::from_secs(20)).redirect(reqwest::redirect::Policy::none()).build()?,
            runtime: tokio::runtime::Builder::new_current_thread().enable_all().build()?,
            relay: credential.relay.trim_end_matches('/').to_string(),
            token: credential.token.clone(),
        })
    }

    fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value, RemoteError> {
        self.runtime.block_on(async {
            let mut request = self.client.request(method, format!("{}{}{path}", self.relay, self.base)).bearer_auth(&self.token);
            if let Some(body) = body {
                request = request.json(&body);
            }
            let response = request.send().await.map_err(|_| RemoteError::Failed)?;
            let status = response.status().as_u16();
            let value: Value = response.json().await.unwrap_or_default();
            match status {
                200 => Ok(value),
                401 => {
                    crate::device_auth::reject_token(&self.token);
                    Err(RemoteError::Unauthorized)
                }
                403 if value["error"] == "desktop_only" => Err(RemoteError::Unsupported),
                403 if value["error"] == "bad_signature" => Err(RemoteError::BadSignature),
                // 이 창구가 없는 옛 관문은 JSON 없는 404 를 준다.
                404 if value["error"] == "not_found" => Err(RemoteError::NotFound),
                404 => Err(RemoteError::Unsupported),
                405 => Err(RemoteError::Unsupported),
                409 if value["error"] == "key_mismatch" => Err(RemoteError::KeyMismatch),
                409 if value["error"] == "vault_exists" => Err(RemoteError::Exists(value["key_id"].as_str().unwrap_or_default().into())),
                _ => Err(RemoteError::Failed),
            }
        })
    }

    fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, RemoteError> {
        serde_json::from_value(value).map_err(|_| RemoteError::Failed)
    }
}

impl Remote for Http {
    fn init(&self, key_id: &str, sign_pub: &str) -> Result<(), RemoteError> {
        self.call(reqwest::Method::POST, "/init", Some(json!({"key_id": key_id, "sign_pub": sign_pub}))).map(|_| ())
    }

    fn fetch(&self, since: u64) -> Result<Fetched, RemoteError> {
        Self::parse(self.call(reqwest::Method::GET, &format!("?since={since}"), None)?)
    }

    fn put(&self, key_id: &str, puts: &[Put], sig: &str) -> Result<Vec<PutOutcome>, RemoteError> {
        let value = self.call(reqwest::Method::PUT, "/items", Some(json!({"key_id": key_id, "puts": puts, "sig": sig})))?;
        Self::parse(value["outcomes"].clone())
    }
}

impl Joins for Http {
    fn open_join(&self, label: &str, join_pub: &str) -> Result<String, RemoteError> {
        let value = self.call(reqwest::Method::POST, "/joins", Some(json!({"label": label, "join_pub": join_pub})))?;
        value["id"].as_str().map(str::to_string).ok_or(RemoteError::Failed)
    }

    fn attach_approval(&self, join_id: &str, approval: &Approval) -> Result<(), RemoteError> {
        self.call(reqwest::Method::POST, &format!("/joins/{join_id}/approval"), Some(serde_json::to_value(approval).map_err(|_| RemoteError::Failed)?))
            .map(|_| ())
    }

    fn pending_joins(&self) -> Result<Vec<Join>, RemoteError> {
        Self::parse(self.call(reqwest::Method::GET, "/joins", None)?["joins"].clone())
    }

    fn post_sealed(&self, join_id: &str, sealed: &SealedKey) -> Result<(), RemoteError> {
        self.call(reqwest::Method::POST, &format!("/joins/{join_id}/sealed"), Some(serde_json::to_value(sealed).map_err(|_| RemoteError::Failed)?))
            .map(|_| ())
    }

    fn take_sealed(&self, join_id: &str) -> Result<Option<SealedKey>, RemoteError> {
        Self::parse(self.call(reqwest::Method::GET, &format!("/joins/{join_id}"), None)?["sealed"].clone())
    }
}
