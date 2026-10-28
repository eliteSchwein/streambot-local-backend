use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::Response,
    routing::any,
    Router,
};
use serde_json::{json, Value};
use std::{collections::{HashMap, HashSet}, net::SocketAddr};
use crate::{config::ServerConfig, internal::log, status};
use super::{registry, rpc};

async fn upgrade(ws: WebSocketUpgrade) -> Response { ws.on_upgrade(handle_socket) }

async fn send_json(socket: &mut WebSocket, value: Value) -> bool {
    socket.send(Message::Text(value.to_string().into())).await.is_ok()
}

async fn send_state(socket: &mut WebSocket, method: &str, value: Value, sent: &mut HashMap<String, Value>) -> bool {
    let (payload, partial) = if let Some(previous) = sent.get(method) {
        match registry::delta(previous, &value) { Some(d) => (d, true), None => return true }
    } else { (value.clone(), false) };
    let ok = send_json(socket, registry::notification(method, payload, partial)).await;
    if ok { sent.insert(method.to_owned(), value); }
    ok
}

/// Connection-specific endpoint allowlist and last successfully sent baseline.
async fn handle_socket(mut socket: WebSocket) {
    let mut updates = registry::get().subscribe();
    let mut endpoints = HashSet::<String>::new();
    let mut sent = HashMap::<String, Value>::new();
    if !send_json(&mut socket, rpc::notification("status", status())).await { return; }
    loop {
        tokio::select! {
            packet = socket.recv() => match packet {
                Some(Ok(Message::Text(text))) => {
                    let incoming = serde_json::from_str::<Value>(text.as_str());
                    if let Ok(request) = &incoming {
                        if let Some(method) = request.get("method").and_then(Value::as_str) {
                            if matches!(method, "register_endpoints" | "unregister_endpoints") {
                                let id = request.get("id").cloned();
                                // Accept both legacy positional parameters and named JSON-RPC parameters:
                                //   params: ["notify_*", ...]
                                //   params: {"endpoints": ["notify_*", ...]}
                                let list = request.get("params").and_then(|params| match params {
                                    Value::Array(list) => Some(list),
                                    Value::Object(object) => object.get("endpoints").and_then(Value::as_array),
                                    _ => None,
                                });
                                let valid_request = request.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
                                    && id.as_ref().is_none_or(|id| id.is_null() || id.is_string() || id.is_number());
                                if !valid_request {
                                    if let Some(id) = id {
                                        if !send_json(&mut socket, rpc::error(id, -32600, "Invalid Request")).await { break; }
                                    }
                                    continue;
                                }
                                let Some(names) = list.filter(|list| list.iter().all(Value::is_string)) else {
                                    if let Some(id) = id {
                                        if !send_json(&mut socket, rpc::error(id, -32602, "Invalid params")).await { break; }
                                    }
                                    continue;
                                };
                                let all = names.iter().any(|v| v.as_str() == Some("all"));
                                let names: Vec<String> = if all { registry::get().list() } else {
                                    names.iter().filter_map(Value::as_str).filter(|s| s.starts_with("notify_")).map(str::to_owned).collect()
                                };
                                if method == "register_endpoints" {
                                    for name in &names {
                                        if endpoints.insert(name.clone()) {
                                            sent.remove(name);
                                            if let Some(value) = registry::get().snapshot(name) {
                                                if !send_state(&mut socket, name, value, &mut sent).await { return; }
                                            }
                                        }
                                    }
                                } else {
                                    for name in &names { endpoints.remove(name); sent.remove(name); }
                                }
                                if let Some(id) = id {
                                    if !send_json(&mut socket, rpc::success(id, json!({"endpoints": endpoints.iter().collect::<Vec<_>>() }))).await { break; }
                                }
                                continue;
                            }
                        }
                    }
                    if let Some(reply) = rpc::handle_text_async(text.as_str()).await {
                        if !send_json(&mut socket, reply).await { break; }
                    }
                }
                Some(Ok(Message::Binary(_))) => {
                    if !send_json(&mut socket, rpc::error(Value::Null, -32700, "Parse error")).await { break; }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {},
            },
            update = updates.recv() => match update {
                Ok(event) => {
                    if endpoints.contains(&event.method) && !send_state(&mut socket, &event.method, event.data, &mut sent).await { break; }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // Catch up from the authoritative full snapshots, not stale queued diffs.
                    for method in &endpoints {
                        if let Some(value) = registry::get().snapshot(method) {
                            if !send_state(&mut socket, method, value, &mut sent).await { return; }
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    }
    // Subscriptions and sent-state are dropped with this connection.
}

pub async fn run(config: ServerConfig) -> std::io::Result<()> {
    let app = Router::new().route("/", any(upgrade)).route("/ws", any(upgrade));
    let addr = SocketAddr::from(([0,0,0,0], config.wsport));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    log::success(&format!("JSON-RPC WebSocket server listening on ws://{addr}"));
    axum::serve(listener, app).await
}
