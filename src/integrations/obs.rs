//! OBS WebSocket v5 integration. Each configured OBS instance owns an OS thread/runtime.
use std::{collections::BTreeMap, io, sync::{Arc, Mutex}};
use serde_json::{json, Value};
use tokio::sync::oneshot;
use crate::{internal::log, server::registry};
use super::Worker;

#[derive(Clone, Debug)]
struct ObsConfig { host: String, port: u16, password: String }

#[derive(Default)]
struct Snapshot { scenes: BTreeMap<String, Value>, audio: BTreeMap<String, Value>, connections: BTreeMap<String, Value> }

pub struct ObsService { workers: Vec<Worker> }

fn load_configs() -> Vec<(String, ObsConfig)> {
    let raw = super::read_integrations();
    let Some(entries) = raw.get("obs").and_then(Value::as_object) else { return Vec::new(); };
    entries.iter().filter_map(|(name, item)| {
        let host = item.get("ip")?.as_str()?.trim();
        if host.is_empty() { return None; }
        let port = item.get("port").and_then(Value::as_u64).and_then(|v| u16::try_from(v).ok()).unwrap_or(4455);
        let password = item.get("password").and_then(Value::as_str).unwrap_or("").to_owned();
        Some((name.clone(), ObsConfig { host: host.to_owned(), port, password }))
    }).collect()
}

fn publish(state: &Arc<Mutex<Snapshot>>, name: &str, connected: bool, scenes: Option<Value>, audio: Option<Value>) {
    let mut state = state.lock().unwrap();
    state.connections.insert(name.to_owned(), json!({"enabled":true,"connected":connected,"state": if connected {"connected"} else {"disconnected"}}));
    if let Some(v) = scenes { state.scenes.insert(name.to_owned(), v); }
    if let Some(v) = audio { state.audio.insert(name.to_owned(), v); }
    let connected_state = state.connections.clone();
    let scene_state = state.scenes.clone();
    let audio_state = state.audio.clone();
    drop(state);
    registry::notify("notify_connection_update", json!({"obs":connected_state}));
    // One authoritative aggregate per method: the registry preserves the last snapshot for new subscribers.
    registry::notify("notify_obs_scene_update_named", json!({"connections":scene_state}));
    registry::notify("notify_obs_audio_update_named", json!({"connections":audio_state}));
    if name == "default" {
        if let Some(data) = scene_state.get("default") { registry::notify("notify_obs_scene_update", data.clone()); }
        if let Some(data) = audio_state.get("default") { registry::notify("notify_obs_audio_update", data.clone()); }
    }
}

async fn update_data(client: &obws::Client, state: &Arc<Mutex<Snapshot>>, name: &str) -> anyhow::Result<()> {
    // Avoid depending on custom serialization of obws response types; display remains inspectable.
    let scene_list = client.scenes().list().await?;
    let scenes = json!({"connection":name, "scenes":format!("{:#?}",scene_list), "current":client.scenes().current_program_scene().await.ok()});
    let inputs = client.inputs().list(None).await?;
    let mut audio = serde_json::Map::new();
    for input in inputs {
        let input_name = input.id.name;
        let mute = client.inputs().muted(obws::requests::inputs::InputId::Name(&input_name)).await.ok();
        let volume = client.inputs().volume(obws::requests::inputs::InputId::Name(&input_name)).await.ok();
        if mute.is_none() && volume.is_none() { continue; }
        audio.insert(input_name, json!({"muted":mute, "volume":format!("{volume:?}")}));
    }
    publish(state, name, true, Some(scenes), Some(Value::Object(audio)));
    Ok(())
}

async fn run_instance(name: String, cfg: ObsConfig, state: Arc<Mutex<Snapshot>>, mut stop: oneshot::Receiver<()>) {
    let mut retry = tokio::time::interval(std::time::Duration::from_secs(5));
    retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! { _ = &mut stop => break, _ = retry.tick() => {} }
        let password = (!cfg.password.is_empty()).then_some(cfg.password.as_str());
        let connect = obws::Client::connect(&cfg.host, cfg.port, password);
        let mut client = match tokio::select! { _ = &mut stop => break, result = tokio::time::timeout(std::time::Duration::from_secs(5), connect) => result } {
            Ok(Ok(client)) => client,
            other => {
                log::warn(&format!("OBS {name} connection failed: {}", match other { Ok(Err(err)) => err.to_string(), Err(err) => err.to_string(), Ok(Ok(_)) => "unknown".to_owned() }));
                publish(&state, &name, false, None, None);
                continue;
            }
        };
        log::success(&format!("OBS {name} connected: {}:{}",cfg.host,cfg.port));
        publish(&state, &name, true, None, None);
        let mut poll = tokio::time::interval(std::time::Duration::from_secs(3));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = &mut stop => { client.disconnect().await; publish(&state,&name,false,None,None); return; }
                _ = poll.tick() => {
                    if let Err(error) = update_data(&client,&state,&name).await {
                        log::warn(&format!("OBS {name} disconnected / snapshot failed: {error:#}"));
                        break;
                    }
                }
            }
        }
        client.disconnect().await;
        publish(&state, &name, false, None, None);
    }
    publish(&state,&name,false,None,None);
}

pub fn init() -> io::Result<ObsService> {
    let entries = load_configs();
    let state = Arc::new(Mutex::new(Snapshot::default()));
    let mut workers = Vec::new();
    for (name, cfg) in entries {
        let own_name = name.clone();
        let own_state = state.clone();
        let worker = Worker::start("obs", move |stop| run_instance(own_name, cfg, own_state, stop))?;
        workers.push(worker);
    }
    log::info(&format!("Started {} OBS worker thread(s)", workers.len()));
    Ok(ObsService { workers })
}

impl ObsService {
    pub async fn shutdown(&mut self) -> io::Result<()> {
        for worker in &mut self.workers { worker.shutdown().await?; }
        Ok(())
    }
}
