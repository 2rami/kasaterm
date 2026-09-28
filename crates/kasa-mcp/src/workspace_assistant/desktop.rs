use crate::device_auth::{self, DeviceCred, Stamp};

pub struct DesktopSession {
    credential: DeviceCred,
    stamp: Stamp,
}

impl DesktopSession {
    pub fn capture() -> Option<Self> {
        device_auth::capture().map(|(credential, stamp)| Self { credential, stamp })
    }

    pub fn stamp(&self) -> Stamp {
        self.stamp.clone()
    }

    pub fn is_current(&self) -> bool {
        device_auth::with_current(&self.stamp, || Ok(())).is_ok()
    }

    pub fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
    ) -> Result<(u16, Vec<u8>), &'static str> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "unavailable")?;
        let result = runtime.block_on(exchange(&self.credential, method, path, body, &|| {
            device_auth::with_current(&self.stamp, || Ok(())).map_err(|_| "account_changed")
        }));
        if matches!(result, Ok((401, _))) {
            device_auth::reject(&self.stamp);
        }
        result
    }
}

fn endpoint(
    base: &str,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
) -> Result<String, &'static str> {
    let url = reqwest::Url::parse(base).map_err(|_| "invalid_gateway")?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("invalid_gateway");
    }
    let loopback = url
        .host_str()
        .is_some_and(|host| matches!(host, "127.0.0.1" | "localhost" | "[::1]"));
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err("https_required");
    }
    let allowed = match (method, path) {
        ("GET", "/snapshot") => body.is_none(),
        ("PUT" | "DELETE", "/key")
        | ("POST", "/messages" | "/projects" | "/tasks" | "/notifications/claim") => body.is_some(),
        _ => false,
    };
    if !allowed || body.is_some_and(|bytes| bytes.len() > 64 * 1024) {
        return Err("invalid_request");
    }
    let suffix = if path == "/snapshot" { "" } else { path };
    Ok(format!(
        "{}/relay/workspace-assistant{suffix}",
        base.trim_end_matches('/')
    ))
}

async fn exchange(
    credential: &DeviceCred,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    authorize: &(dyn Fn() -> Result<(), &'static str> + Sync),
) -> Result<(u16, Vec<u8>), &'static str> {
    let url = endpoint(&credential.relay, method, path, body)?;
    authorize()?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(4))
        .timeout(std::time::Duration::from_secs(35))
        .build()
        .map_err(|_| "unavailable")?;
    let mut request = client
        .request(
            reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| "invalid_request")?,
            url,
        )
        .bearer_auth(&credential.token)
        .header(reqwest::header::ACCEPT, "application/json");
    if let Some(body) = body {
        request = request
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_vec());
    }
    let mut response = request.send().await.map_err(|_| "unreachable")?;
    authorize()?;
    let status = response.status().as_u16();
    const LIMIT: usize = 2 * 1024 * 1024;
    if response
        .content_length()
        .is_some_and(|length| length > LIMIT as u64)
    {
        return Err("response_too_large");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "unreachable")? {
        if bytes.len().saturating_add(chunk.len()) > LIMIT {
            return Err("response_too_large");
        }
        bytes.extend_from_slice(&chunk);
    }
    authorize()?;
    Ok((status, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_fixed_account_routes_and_secure_gateway_origins_are_allowed() {
        assert_eq!(
            endpoint("https://gateway.test", "GET", "/snapshot", None).unwrap(),
            "https://gateway.test/relay/workspace-assistant"
        );
        assert!(endpoint("http://127.0.0.1:9999", "PUT", "/key", Some(b"{}")).is_ok());
        for base in [
            "http://gateway.test",
            "file:///tmp/server",
            "https://secret@gateway.test",
            "https://gateway.test?token=secret",
        ] {
            assert!(endpoint(base, "PUT", "/key", Some(b"{}")).is_err());
        }
        for path in [
            "/../logout",
            "//evil.test/key",
            "/snapshot?account=other",
            "/key#fragment",
        ] {
            assert!(endpoint("https://gateway.test", "GET", path, None).is_err());
        }
        assert!(endpoint("https://gateway.test", "GET", "/key", None).is_err());
        assert!(endpoint("https://gateway.test", "PUT", "/key", Some(&vec![0; 65537])).is_err());
    }

    #[test]
    fn isolated_tests_cannot_load_a_real_device_account() {
        assert!(DesktopSession::capture().is_none());
    }
}
