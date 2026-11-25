//! The account uplink can operate the six fan spots and the GPU lighting override. Keep its local API and its other controls private.
use axum::{
    extract::Request,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::{sync::OnceLock, time::Duration};

use super::{
    auth::{ViaPhone, ViaUplink},
    MobileAuth,
};

const SPOTS: [&str; 6] = ["top1", "top2", "top3", "left1", "right1", "right2"];
const BASE: &str = "http://127.0.0.1:5000";

fn hex(v: &Value) -> Option<String> {
    let raw = v.as_str()?;
    let s = raw.strip_prefix('#').unwrap_or(raw);
    (s.len() == 6 && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| s.to_ascii_lowercase())
}

fn port(v: &Value) -> Option<usize> {
    match *v.as_str()?.as_bytes() {
        [b'f', n @ b'0'..=b'3', b'0'..=b'2'] => Some(usize::from(n - b'0')),
        _ => None,
    }
}

fn fans(state: &Value) -> Option<Vec<Value>> {
    state["connected"].as_bool()?;
    let mapping = state["parts"]["map"].as_object()?;
    if !SPOTS.iter().all(|spot| mapping.contains_key(*spot)) {
        return None;
    }
    let colors = state["parts"]["colors"].as_object()?;
    let zones = state["zones"].as_array()?;
    let hub = &state["fans"];
    let mut result = Vec::with_capacity(SPOTS.len());
    for spot in SPOTS {
        let index = mapping.get(spot).and_then(port);
        let fixed = colors.get(spot).and_then(hex);
        let ring = |n: usize, suffix: &str| {
            state["fan_colors"][spot]
                .get(n)
                .and_then(hex)
                .or_else(|| colors.get(&format!("{spot}{suffix}")).and_then(hex))
                .or_else(|| fixed.clone())
                .or_else(|| {
                    let zone = (index? * 2 + n) as u64;
                    zones
                        .iter()
                        .find(|z| z["zone"].as_u64() == Some(zone))
                        .and_then(|z| hex(&z["hex"]))
                })
        };
        let rpm = index
            .filter(|_| fresh(hub, "age"))
            .and_then(|i| hub["ports"].get(i)?["rpm"].as_u64());
        result.push(json!({
            "spot": spot, "mapped": index.is_some(), "fixed": fixed,
            "inner": ring(0, "#in"), "outer": ring(1, "#out"), "rpm": rpm
        }));
    }
    Some(result)
}

fn gpu_controller(state: &Value) -> bool {
    state["synced"].as_array().is_some_and(|devices| {
        devices.iter().any(|d| {
            d.as_str()
                .is_some_and(|name| name.eq_ignore_ascii_case("Razer Chroma Addressable RGB Controller"))
        })
    })
}

fn gpu_light(state: &Value) -> Value {
    let settings = &state["state"];
    let asleep =
        settings["off_with_monitor"] == true && state["monitor"]["ok"] == true && state["monitor"]["on"] == false;
    // RGB Control exposes fan-zone samples, but has no optical readback from the card.
    let mut sum = [0u32; 3];
    let mut count = 0;
    for zone in state["zones"].as_array().into_iter().flatten().take(8) {
        if zone["dead"] == true {
            continue;
        }
        if let Some(rgb) = hex(&zone["hex"]).and_then(|c| u32::from_str_radix(&c, 16).ok()) {
            for (channel, shift) in sum.iter_mut().zip([16, 8, 0]) {
                *channel += (rgb >> shift) & 255;
            }
            count += 1;
        }
    }
    let sample = (count > 0).then(|| format!("{:02x}{:02x}{:02x}", sum[0] / count, sum[1] / count, sum[2] / count));
    let effect = settings["mode"]
        .as_str()
        .filter(|s| s.len() <= 24 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)));
    json!({"controller": gpu_controller(state), "sync": settings["sync_all"].as_bool(), "asleep": asleep,
           "effect": effect, "fixed": hex(&state["parts"]["colors"]["gpu"]), "sample": sample,
           "utilization": state["gpu"]["util"].as_u64().filter(|v| *v <= 100)})
}

fn fresh(v: &Value, key: &str) -> bool {
    v["ok"] == true && v[key].as_f64().is_some_and(|n| (0.0..15.0).contains(&n))
}

async fn fetch(client: &reqwest::Client, url: String, body: Option<&Value>, limit: usize) -> Option<Vec<u8>> {
    let request = match body {
        Some(v) => client.post(url).json(v),
        None => client.get(url),
    };
    let mut res = request.send().await.ok()?.error_for_status().ok()?;
    let mut data = Vec::new();
    while let Some(chunk) = res.chunk().await.ok()? {
        if chunk.len() > limit - data.len() {
            return None;
        }
        data.extend_from_slice(&chunk);
    }
    Some(data)
}

async fn dashboard(client: &reqwest::Client, base: &str) -> Result<(Value, Vec<Value>), StatusCode> {
    let err = StatusCode::SERVICE_UNAVAILABLE;
    let data = fetch(client, format!("{base}/api/state"), None, 1 << 20)
        .await
        .ok_or(err)?;
    let state: Value = serde_json::from_slice(&data).map_err(|_| err)?;
    let fans = fans(&state).ok_or(err)?;
    Ok((state, fans))
}

async fn operation(client: &reqwest::Client, base: &str, req: Value) -> Result<Value, StatusCode> {
    let err = StatusCode::SERVICE_UNAVAILABLE;
    match req["op"].as_str() {
        Some("capability" | "snapshot") => {
            let (state, fans) = dashboard(client, base).await?;
            if req["op"] == "capability" {
                return Ok(json!({"v":1,"layout":"y60"}));
            }
            let kr = &state["kraken"];
            let current = fresh(kr, "status_age");
            let lcd = fetch(client, format!("{base}/api/lcd.jpg"), None, 256 << 10)
                .await
                .filter(|b| b.starts_with(&[0xff, 0xd8]))
                .map(|b| STANDARD.encode(b));
            Ok(json!({"connected":state["connected"]==true,"fans":fans,"lcd":lcd,
                "liquid":kr["liquid"].as_f64().filter(|_|current),"pump":kr["pump_rpm"].as_u64().filter(|_|current),"gpu":state["gpu"]["temp"].as_f64(),"gpu_light":gpu_light(&state)}))
        }
        Some("color") => {
            let spot = req["spot"]
                .as_str()
                .filter(|s| *s == "gpu" || SPOTS.contains(s))
                .ok_or(StatusCode::BAD_REQUEST)?;
            let v = req.get("color").ok_or(StatusCode::BAD_REQUEST)?;
            let color = if v.is_null() {
                None
            } else {
                Some(
                    hex(v)
                        .filter(|_| v.as_str().is_some_and(|s| s.len() == 6))
                        .ok_or(StatusCode::BAD_REQUEST)?,
                )
            };
            let (state, _) = dashboard(client, base).await?;
            let available = if spot == "gpu" {
                gpu_controller(&state)
            } else {
                port(&state["parts"]["map"][spot]).is_some()
            };
            if state["connected"] != true || !available {
                return Err(StatusCode::CONFLICT);
            }
            let bytes = fetch(
                client,
                format!("{base}/api/part"),
                Some(&json!({"part":spot,"color":color})),
                1 << 20,
            )
            .await
            .ok_or(err)?;
            let result: Value = serde_json::from_slice(&bytes).map_err(|_| err)?;
            let stored = &result["parts"]["colors"][spot];
            let applied = color
                .as_ref()
                .map_or_else(|| stored.is_null(), |c| hex(stored).as_ref() == Some(c));
            if result["ok"] != true || !result["parts"]["colors"].is_object() || !applied {
                return Err(err);
            }
            Ok(json!({"applied":true}))
        }
        _ => Err(StatusCode::BAD_REQUEST),
    }
}

pub(super) async fn handle(req: Request) -> Response {
    let ext = req.extensions();
    if !crate::uplink::rgb_owner(req.headers())
        || ext.get::<ViaUplink>().is_none()
        || ext.get::<ViaPhone>().is_some()
        || !ext.get::<MobileAuth>().is_some_and(|a| a.0.owner)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !cfg!(windows) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Ok(body) = axum::body::to_bytes(req.into_body(), 1024).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let Ok(value) = serde_json::from_slice(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    static CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    let Some(client) = CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .build()
            .ok()
    }) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match operation(client, BASE, value).await {
        Ok(result) => Json(result).into_response(),
        Err(status) => status.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::{get, post};

    #[tokio::test]
    async fn rgb_reads_and_writes_only_mapped_lights() {
        let color = std::sync::Arc::new(std::sync::Mutex::new(json!({})));
        let read = color.clone();
        let write = color.clone();
        let app=axum::Router::new().route("/api/state",get(move || {
            let fixed=read.lock().unwrap().clone();
            async move {Json(json!({"connected":true,"parts":{"map":{"top1":"f00","top2":"f01","top3":"f02","left1":"f10","right1":"f20","right2":"f30"},"colors":fixed},
                "synced":["Razer Chroma Addressable RGB Controller"],"state":{"sync_all":true,"off_with_monitor":true,"mode":"ice"},"monitor":{"ok":true,"on":false},"fan_colors":{"top1":["112233","445566"]},"zones":[{"zone":6,"hex":"#aabbcc"}],"fans":{"ok":true,"age":1,"ports":[{"rpm":881}]},"private":"not exported"}))}
        })).route("/api/part",post(move |Json(v):Json<Value>| {
            let write=write.clone(); async move {
                let mut colors=write.lock().unwrap();
                colors[v["part"].as_str().unwrap()] = v["color"].clone();
                Json(json!({"ok":true,"parts":{"colors":*colors}}))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::new();
        assert_eq!(
            operation(&client, &base, json!({"op":"capability"})).await.unwrap()["layout"],
            "y60"
        );
        let v = operation(&client, &base, json!({"op":"snapshot"})).await.unwrap();
        assert_eq!(v["fans"][0]["inner"], "112233");
        assert_eq!(v["fans"][0]["rpm"], 881);
        assert_eq!(v["fans"][5]["inner"], "aabbcc");
        assert!(v.get("private").is_none());
        assert_eq!(v["gpu_light"]["sample"], "aabbcc");
        assert_eq!(v["gpu_light"]["asleep"], true);
        assert!(!gpu_controller(&json!({"synced":["Logitech G560 Lightsync Speaker"]})));
        for spot in ["top1", "gpu"] {
            for c in [json!("C0FFEE"), Value::Null] {
                operation(&client, &base, json!({"op":"color","spot":spot,"color":c}))
                    .await
                    .unwrap();
                let got = operation(&client, &base, json!({"op":"snapshot"})).await.unwrap();
                let stored = if spot == "gpu" {
                    &got["gpu_light"]["fixed"]
                } else {
                    &got["fans"][0]["fixed"]
                };
                assert_eq!(*stored, json!(hex(&c)));
            }
        }
        for req in [
            json!({"op":"color","spot":"ram","color":"ff0000"}),
            json!({"op":"color","spot":"top1"}),
            json!({"op":"color","spot":"top1","color":"red"}),
            json!({"op":"color","spot":"top1","color":"#aabbcc"}),
            json!({"op":"unrecognized"}),
        ] {
            assert_eq!(
                operation(&client, &base, req).await.unwrap_err(),
                StatusCode::BAD_REQUEST
            );
        }
        let req = Request::post("/kasadesk/rgb").body(axum::body::Body::empty()).unwrap();
        assert_eq!(handle(req).await.status(), StatusCode::FORBIDDEN);
        server.abort();
    }
}
