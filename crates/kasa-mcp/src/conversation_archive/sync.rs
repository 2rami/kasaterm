use super::{id_for, Append, Entry, MAX_APPEND};
use crate::device_auth::{DeviceCred, Stamp};
use kasa_socket::backend::Backend;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Deserialize)]
struct List { enabled: bool, conversations: Vec<Entry> }
#[derive(Deserialize)]
struct Written { conversation: Entry }

pub fn spawn(backend: Arc<dyn Backend>) {
    if cfg!(test) { return; }
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::AcqRel) { return; }
    let _ = std::thread::Builder::new().name("conversation-sync".into()).spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
        runtime.block_on(async move {
            let Ok(client) = reqwest::Client::builder().timeout(Duration::from_secs(20))
                .redirect(reqwest::redirect::Policy::none()).build() else { return };
            loop {
                let mut pending = false;
                if let (Some((credential, stamp)), Some(machine)) = (crate::device_auth::capture(), crate::mobile::machine_identity()) {
                    pending = synchronize(backend.as_ref(), &client, &credential, &machine, &|| current(&stamp)).await.unwrap_or(false);
                }
                tokio::time::sleep(Duration::from_secs(if pending { 1 } else { 15 })).await;
            }
        });
    });
}

fn current(stamp: &Stamp) -> bool { crate::device_auth::with_current(stamp, || Ok(())).is_ok() }

async fn json<T: serde::de::DeserializeOwned>(mut response: reqwest::Response) -> Result<T, ()> {
    if !response.status().is_success() { return Err(()); }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
        if bytes.len() + chunk.len() > 32 << 20 { return Err(()); }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| ())
}

fn clipped(value: &str, max: usize) -> String {
    let mut value: String = value.chars().filter(|c| !c.is_control()).collect();
    let mut end = value.len().min(max);
    while !value.is_char_boundary(end) { end -= 1; }
    value.truncate(end);
    value
}

pub(crate) async fn synchronize(backend: &dyn Backend, client: &reqwest::Client, credential: &DeviceCred, machine: &str, authorized: &dyn Fn() -> bool) -> Result<bool, ()> {
    if !authorized() { return Err(()); }
    let url = format!("{}/relay/conversations", credential.relay.trim_end_matches('/'));
    let listed: List = json(client.get(&url).bearer_auth(&credential.token).send().await.map_err(|_| ())?).await?;
    if !listed.enabled { return Ok(false); }
    let mut known: HashMap<_, _> = listed.conversations.into_iter().filter(|e| e.machine_id == machine).map(|e| (e.id.clone(), e)).collect();
    let board = backend.collab_board().unwrap_or_default();
    let mut ids: Vec<_> = backend.pane_windows().into_iter().map(|(id, _)| id)
        .chain(board.iter().map(|row| row.surface_id.clone())).collect();
    ids.sort(); ids.dedup();
    let mut pending = false;
    for id in ids {
        if !authorized() { return Err(()); }
        if crate::remote::is_remote_pane(&id) { continue; }
        let Ok(identity) = backend.collab_pane_identity(&id) else { continue };
        let Some(key) = identity["surface_key"].as_str() else { continue };
        let candidate = known.values().filter(|e| e.surface_key == key).max_by_key(|e| e.updated_at);
        let mut from = candidate.map_or(0, |e| e.cursor);
        let Ok(mut chunk) = backend.transcript_archive(&id, from) else { continue };
        if chunk.source.is_empty() || chunk.raw.len() > MAX_APPEND { continue; }
        let archive_id = id_for(machine, &chunk.source);
        let previous = known.get(&archive_id);
        if previous.is_some_and(|e| e.deleted) { continue; }
        let wanted = previous.map_or(0, |e| e.cursor);
        if from != wanted {
            let source = chunk.source.clone();
            from = wanted;
            let Ok(fresh) = backend.transcript_archive(&id, from) else { continue };
            if fresh.source != source { continue; }
            chunk = fresh;
        }
        if chunk.raw.len() > MAX_APPEND || !authorized() { continue; }
        let row = board.iter().find(|row| row.surface_id == id);
        let harness = row.and_then(|row| row.harness.as_deref())
            .unwrap_or(if chunk.source.starts_with("rollout-") { "codex" } else { "claude" });
        if !matches!(harness, "claude" | "codex") { continue; }
        let title = clipped(row.map(|r| r.title.as_str()).unwrap_or(""), 512);
        let name = clipped(row.and_then(|r| r.character.as_deref()).unwrap_or(""), 128);
        let cwd = clipped(row.map(|r| r.cwd.as_str()).unwrap_or(""), 4096);
        let caught_up = chunk.offset == from && chunk.raw.is_empty();
        if previous.is_some_and(|e| e.ready && caught_up && e.surface_key == key && e.title == title && e.name == name && e.cwd == cwd) {
            continue;
        }
        let update = Append {
            source: chunk.source, surface_key: key.into(), title, name, harness: harness.into(), cwd,
            expected_revision: previous.map_or(0, |e| e.revision), from: if chunk.reset { 0 } else { from }, next: chunk.offset,
            reset: chunk.reset || previous.is_none(), caught_up, raw: chunk.raw,
        };
        if !authorized() { return Err(()); }
        let response = client.post(&url).bearer_auth(&credential.token).json(&update).send().await.map_err(|_| ())?;
        if response.status() == reqwest::StatusCode::CONFLICT { continue; }
        let saved: Written = match json(response).await { Ok(saved) => saved, Err(_) => continue };
        if !authorized() { return Err(()); }
        if saved.conversation.id != archive_id || saved.conversation.machine_id != machine { return Err(()); }
        known.insert(archive_id, saved.conversation);
        pending |= !caught_up;
    }
    Ok(pending)
}
