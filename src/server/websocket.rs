use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::Response,
    routing::any,
    Router,
};
use std::net::SocketAddr;
use crate::{config::ServerConfig, internal::log, status};
use super::rpc;

async fn upgrade(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(handle_socket)
}

async fn handle_socket(mut socket: WebSocket) {
    // Server push is a JSON-RPC notification: it must not include an id.
    let initial = rpc::notification("status", status());
    if socket.send(Message::Text(initial.to_string().into())).await.is_err() {
        return;
    }

    while let Some(message) = socket.recv().await {
        match message {
            Ok(Message::Text(text)) => {
                if let Some(reply) = rpc::handle_text(text.as_str()) {
                    if socket.send(Message::Text(reply.to_string().into())).await.is_err() {
                        break;
                    }
                }
            }
            Ok(Message::Binary(_)) => {
                // This endpoint accepts JSON text messages only.
                let reply = rpc::error(serde_json::Value::Null, -32700, "Parse error");
                if socket.send(Message::Text(reply.to_string().into())).await.is_err() {
                    break;
                }
            }
            Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => {
                // Axum handles WebSocket control frames.
            }
            Ok(Message::Close(_)) | Err(_) => break,
        }
    }
}

pub async fn run(config: ServerConfig) -> std::io::Result<()> {
    let app = Router::new().route("/", any(upgrade)).route("/ws", any(upgrade));
    let addr = SocketAddr::from(([0, 0, 0, 0], config.wsport));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    log::success(&format!("JSON-RPC WebSocket server listening on ws://{addr}"));
    axum::serve(listener, app).await
}
