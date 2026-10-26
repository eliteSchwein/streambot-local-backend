use axum::{http::{header, Method, StatusCode}, routing::get, Router};
use std::{net::SocketAddr, path::PathBuf};
use tower_http::{cors::{Any, CorsLayer}, services::{ServeDir, ServeFile}};
use crate::{config::{Config, ServerConfig}, internal::log, status};
use axum::Json;
use serde_json::{json, Value};
use super::response::respond;

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

fn static_router() -> Router {
    let home = home();
    let user_config = home.join(".config/streambot");
    let local_share = home.join(".local/share/streambot");
    let overlay = local_share.join("stream-overlord-overlay");
    let commander = local_share.join("stream-overlord-admin/dist");

    // Serving paths from the original Express server; advanced HTML template expansion,
    // dynamically generated CSS and uploads are NOT implemented yet.
    Router::new()
        .nest_service("/dist", ServeDir::new(overlay.join("dist")))
        .nest_service("/fonts", ServeDir::new(user_config.join("fonts")))
        .nest_service("/compressed", ServeDir::new(user_config.join("compressed_assets")))
        .nest_service("/remote-cache", ServeDir::new(user_config.join("remote-cache")))
        .nest_service("/commander", ServeDir::new(commander.clone())
            .fallback(ServeFile::new(commander.join("index.html"))))
        .fallback_service(ServeDir::new(user_config.join("streambot-overlays"))
            .html_as_default_extension(true)
            .fallback(ServeDir::new(user_config.join("assets"))))
}

async fn status_endpoint() -> impl axum::response::IntoResponse {
    respond(StatusCode::OK, status())
}

// This endpoint intentionally omits authentication secrets.
// Use Config::load() to read the same optional root .env as the backend.
async fn public_config_endpoint() -> Json<Value> {
    // If .env is malformed or inaccessible, avoid publishing internal errors.
    let config = match Config::load() {
        Ok(config) => config,
        Err(error) => {
            log::warn(&format!("Unable to load public config: {error}"));
            return Json(json!({"twitch": {}, "api website": {}}));
        }
    };
    Json(json!({
        "twitch": {
            "test_mode": config.twitch.test_mode,
            "client_id": config.twitch.client_id,
            "channels": config.twitch.channels
        },
        "api website": {
            "url": config.api_website.url,
            "api_slug": config.api_website.api_slug
        }
    }))
}

pub async fn run(config: ServerConfig) -> std::io::Result<()> {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::HEAD, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE]);
    let app = Router::new()
        .route("/api/status", get(status_endpoint))
        .route("/status", get(status_endpoint))
        .route("/config.json", get(public_config_endpoint))
        .merge(static_router())
        .layer(cors);
    let addr = SocketAddr::from(([0,0,0,0], config.restport));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    log::success(&format!("REST/static server listening on http://{addr}"));
    axum::serve(listener, app).await
}
