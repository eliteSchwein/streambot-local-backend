//! YoloBox LAN discovery and per-connection OS threads.
//! Mirrors the TypeScript NetworkHelper port-8887 scan, with bounded concurrency.
use super::{Worker, publish_connection, read_integrations};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{collections::{BTreeMap, BTreeSet}, io, net::Ipv4Addr, process::Command, time::Duration};
use tokio::{net::TcpStream, sync::{mpsc, oneshot, Semaphore}, task::JoinSet, time};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use std::sync::Arc;

const PORT: u16 = 8887;
const RETRY: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const SCAN_CONCURRENCY: usize = 128;
const MAX_HOSTS_PER_INTERFACE: u32 = 4096;
const DATA: &[(&str, &str)] = &[
    ("DirectorList", "/remote/controller/getDirectorList"),
    ("MaterialList", "/remote/controller/getMaterialList"),
    ("MixerList", "/remote/controller/getMixerList"),
    ("LiveStatus", "/remote/controller/getLiveStatus"),
];
const AUTH_PATH: &str = "/remote/controller/authenticate";

pub struct YoloboxService { manager: Option<Worker> }

pub fn init() -> io::Result<YoloboxService> {
    let cfg = read_integrations();
    if !cfg["yolobox"]["enabled"].as_bool().unwrap_or(false) {
        publish_connection("yolobox", false, "disconnected", false);
        return Ok(YoloboxService { manager: None });
    }
    let additional = cfg["yolobox"]["additionalDevices"]
        .as_array().map(|items| items.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default();
    let manager = Worker::start("yolobox-discovery", move |stop| discovery_manager(additional, stop))?;
    Ok(YoloboxService { manager: Some(manager) })
}
impl YoloboxService {
    pub async fn shutdown(&mut self) -> io::Result<()> {
        if let Some(worker) = &mut self.manager { worker.shutdown().await?; }
        self.manager = None;
        Ok(())
    }
}

// `ip` is already available on supported Linux deployments. Skip loopback,
// Docker and VMware VMnet just like the original TypeScript NetworkHelper.
// Unlike its /24-specific loop, CIDR subnets are handled correctly.
fn local_scan_targets() -> io::Result<Vec<String>> {
    let output = Command::new("ip").args(["-o", "-4", "addr", "show"]).output()?;
    if !output.status.success() { return Err(io::Error::other("ip -o -4 addr show failed")); }
    let mut targets = BTreeSet::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 { continue; }
        let iface = fields[1].trim_end_matches(':').split('@').next().unwrap_or("");
        if iface == "lo" || iface.starts_with("docker") || iface.starts_with("vmnet") { continue; }
        let Some(inet_index) = fields.iter().position(|field| *field == "inet") else { continue };
        let Some(cidr) = fields.get(inet_index + 1) else { continue };
        let Some((address, prefix)) = cidr.split_once('/') else { continue };
        let (Ok(ip), Ok(bits)) = (address.parse::<Ipv4Addr>(), prefix.parse::<u32>()) else { continue };
        if bits > 32 { continue; }
        let host = u32::from(ip);
        let mask = if bits == 0 { 0 } else { u32::MAX << (32 - bits) };
        let network = host & mask;
        let broadcast = network | !mask;
        let host_count = (broadcast as u64) - (network as u64) + 1;
        // Never attempt to probe huge routed networks in one discovery pass.
        if host_count > MAX_HOSTS_PER_INTERFACE as u64 {
            crate::internal::log::warn(&format!("YoloBox: skipping large subnet {cidr} ({host_count} IPs); specify additionalDevices"));
            continue;
        }
        let (first, last) = if host_count >= 4 { (network + 1, broadcast - 1) } else { (network, broadcast) };
        for candidate in first..=last {
            if candidate != host { targets.insert(Ipv4Addr::from(candidate).to_string()); }
        }
    }
    Ok(targets.into_iter().collect())
}

async fn scan_devices(additional: &[String]) -> Vec<String> {
    let mut targets: BTreeSet<String> = match local_scan_targets() {
        Ok(targets) => targets.into_iter().collect(),
        Err(error) => { crate::internal::log::warn(&format!("YoloBox discovery: cannot enumerate interfaces: {error}")); BTreeSet::new() }
    };
    targets.extend(additional.iter().filter(|host| !host.trim().is_empty()).cloned());
    let permits = Arc::new(Semaphore::new(SCAN_CONCURRENCY));
    let mut tasks = JoinSet::new();
    for device in targets {
        let semaphore = permits.clone();
        tasks.spawn(async move {
            let _permit = semaphore.acquire_owned().await.ok()?;
            let open = time::timeout(CONNECT_TIMEOUT, TcpStream::connect((device.as_str(), PORT))).await;
            matches!(open, Ok(Ok(_))).then_some(device)
        });
    }
    let mut found = Vec::new();
    while let Some(result) = tasks.join_next().await {
        if let Ok(Some(host)) = result { found.push(host); }
    }
    // Deterministic order, with user-selected devices preferred.
    found.sort_by_key(|host| (if additional.contains(host) { 0 } else { 1 }, host.clone()));
    found
}

async fn verify_device(device: &str) -> bool {
    let url = format!("ws://{device}:{PORT}{AUTH_PATH}");
    matches!(time::timeout(Duration::from_secs(2), connect_async(url)).await, Ok(Ok(_)))
}

async fn discovery_manager(additional: Vec<String>, mut stop: oneshot::Receiver<()>) {
    loop {
        publish_connection("yolobox", true, "discovering", false);
        let found = tokio::select! { _ = &mut stop => break, result = scan_devices(&additional) => result };
        let mut selected = None;
        for device in found {
            let verified = tokio::select! { _ = &mut stop => return, result = verify_device(&device) => result };
            if verified { selected = Some(device); break; }
        }
        if let Some(device) = selected {
            crate::internal::log::success(&format!("YoloBox discovered at {device}:{PORT}"));
            if let Err(error) = run_device(device, &mut stop).await {
                crate::internal::log::warn(&format!("YoloBox workers stopped: {error}"));
            }
        } else {
            crate::internal::log::info("YoloBox not found; retrying discovery in 5 seconds");
        }
        tokio::select! { _ = &mut stop => break, _ = time::sleep(RETRY) => {} }
    }
    publish_connection("yolobox", true, "disconnected", false);
}

async fn run_device(device: String, stop: &mut oneshot::Receiver<()>) -> io::Result<()> {
    let (events_tx, events_rx) = mpsc::unbounded_channel::<(String, Value)>();
    let aggregate_device = device.clone();
    let mut workers = vec![Worker::start("yolobox-aggregate", move |done| aggregate(aggregate_device, events_rx, done))?];
    for (name, path) in DATA.iter().copied().chain([
        ("Authenticate", AUTH_PATH),
        ("Heartbeat", "/remote/controller/heartbeat"),
        ("Command", "/remote/controller/postOrder"),
    ]) {
        let device = device.clone(); let tx = events_tx.clone();
        let label = name.to_string(); let path = path.to_string();
        let thread_name = match name {
            "DirectorList" => "yolobox-director", "MaterialList" => "yolobox-material",
            "MixerList" => "yolobox-mixer", "LiveStatus" => "yolobox-live",
            "Authenticate" => "yolobox-auth", "Heartbeat" => "yolobox-heartbeat", _ => "yolobox-command",
        };
        workers.push(Worker::start(thread_name, move |done| socket_worker(device, path, label, tx, done))?);
    }
    drop(events_tx);
    publish_connection("yolobox", true, "connecting", false);
    // Monitor the actual port. On a lost YoloBox, close all seven WS workers
    // and scan again, allowing a replacement device to be selected.
    loop {
        tokio::select! {
            _ = &mut *stop => break,
            _ = time::sleep(RETRY) => {
                if !matches!(time::timeout(CONNECT_TIMEOUT, TcpStream::connect((device.as_str(), PORT))).await, Ok(Ok(_))) {
                    crate::internal::log::warn(&format!("YoloBox disconnected: {device}"));
                    break;
                }
            }
        }
    }
    for worker in &mut workers { worker.shutdown().await?; }
    publish_connection("yolobox", true, "disconnected", false);
    Ok(())
}

async fn aggregate(device: String, mut rx: mpsc::UnboundedReceiver<(String, Value)>, mut stop: oneshot::Receiver<()>) {
    let mut data = BTreeMap::<String, Value>::new();
    data.insert("http".into(), json!(format!("http://{device}:8080")));
    data.insert("ws".into(), json!(format!("ws://{device}:8080")));
    loop {
        tokio::select! {
            _ = &mut stop => break,
            msg = rx.recv() => match msg {
                Some((key, payload)) => {
                    data.insert(key, payload);
                    crate::server::registry::notify("notify_yolobox_update", json!(data));
                    publish_connection("yolobox", true, "connected", true);
                }
                None => break,
            }
        }
    }
}

async fn socket_worker(device: String, path: String, label: String, tx: mpsc::UnboundedSender<(String, Value)>, mut stop: oneshot::Receiver<()>) {
    let url = format!("ws://{device}:{PORT}{path}");
    loop {
        let opened = tokio::select! { _ = &mut stop => return, result = connect_async(&url) => result };
        match opened {
            Ok((mut ws, _)) => {
                let mut heartbeat = time::interval(Duration::from_secs(10));
                loop {
                    tokio::select! {
                        _ = &mut stop => { let _ = ws.close(None).await; return; }
                        _ = heartbeat.tick(), if label == "Heartbeat" => {
                            if ws.send(Message::Text(r#"{"alive":true}"#.into())).await.is_err() { break; }
                        }
                        frame = ws.next() => match frame {
                            Some(Ok(Message::Text(text))) => {
                                if let Ok(v) = serde_json::from_str::<Value>(&text) {
                                    if v["code"].as_i64() == Some(200) {
                                        let result = v["data"]["result"].clone();
                                        if !result.is_null() { let _ = tx.send((label.clone(), result)); }
                                    }
                                }
                            }
                            Some(Ok(Message::Ping(p))) => { if ws.send(Message::Pong(p)).await.is_err() { break; } }
                            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                            _ => {}
                        }
                    }
                }
            }
            Err(err) => crate::internal::log::warn(&format!("YoloBox {label}: {err}")),
        }
        tokio::select! { _ = &mut stop => return, _ = time::sleep(RETRY) => {} }
    }
}
