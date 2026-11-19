//! 앱 쪽 관문 클라이언트 — 동기화 스레드가 제 런타임에서 막고 부른다.

use std::time::Duration;

use serde_json::{json, Value};

use super::crypto::VaultKey;
use super::engine::{Remote, RemoteError};
use super::server::{Fetched, Put, PutOutcome};
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

impl Http {
    /// 이 기기의 금고 열쇠를 관문에 맡긴다. 관문은 열쇠가 금고의 것일 때만 받는다(`key_mismatch`).
    pub fn put_escrow(&self, key: &VaultKey) -> Result<(), RemoteError> {
        self.call(reqwest::Method::PUT, "/escrow", Some(json!({"key": key.to_base64().as_str()}))).map(|_| ())
    }

    /// 맡긴 금고 열쇠. 아직 아무도 맡기지 않았으면 None.
    pub fn escrow(&self) -> Result<Option<VaultKey>, RemoteError> {
        match self.call(reqwest::Method::GET, "/escrow", None) {
            Ok(value) => {
                let key = value["key"].as_str().ok_or(RemoteError::Failed)?;
                VaultKey::from_base64(key).map(Some).map_err(|_| RemoteError::Failed)
            }
            Err(RemoteError::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }
}
