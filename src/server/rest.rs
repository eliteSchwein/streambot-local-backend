use axum::{extract::Query, http::{header, Method, StatusCode}, response::{IntoResponse, Redirect}, routing::{get, post}, Router};
use serde::Deserialize;
use crate::twitch::auth::{AuthKind, TwitchAuth};
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

#[derive(Deserialize)]
struct TwitchCallbackParams {
    code: Option<String>, exchange_code: Option<String>, auth_code: Option<String>,
    #[serde(rename="type")] auth_type: Option<String>,
    cloud_auth: Option<String>, status: Option<String>, error: Option<String>,
}

async fn twitch_auth_control() -> impl IntoResponse { cloud_auth_redirect(AuthKind::Control) }
async fn twitch_auth_message() -> impl IntoResponse { cloud_auth_redirect(AuthKind::Message) }
fn cloud_auth_redirect(kind: AuthKind) -> axum::response::Response {
    let config = match Config::load() {
        Ok(v) => v, Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response()
    };
    let auth = TwitchAuth::new(&config.twitch, &config.cloud);
    // Relative callback URL is intentionally NOT constructed from an untrusted Host header.
    // Set STREAMDING_PUBLIC_URL to the externally reachable backend base URL (http://localhost:8105 by default).
    let origin = std::env::var("STREAMDING_PUBLIC_URL")
        .unwrap_or_else(|_| format!("http://localhost:{}", config.server.restport));
    let callback = format!("{}/auth/twitch/callback?type={}", origin.trim_end_matches('/'), kind.as_str());
    match auth.cloud_auth_url(kind, &callback) {
        Ok(url) => Redirect::temporary(&url).into_response(),
        Err(err) => { log::warn(&format!("Twitch cloud authorization URL failed: {err}")); StatusCode::INTERNAL_SERVER_ERROR.into_response() }
    }
}

async fn twitch_auth_callback(Query(params): Query<TwitchCallbackParams>) -> impl IntoResponse {
    if params.status.as_deref().is_some_and(|s| s != "success") || params.error.is_some() {
        return (StatusCode::BAD_REQUEST, "Twitch cloud authentication failed").into_response();
    }
    let Some(code) = params.code.or(params.exchange_code).or(params.auth_code) else {
        return (StatusCode::BAD_REQUEST, "Missing cloud Twitch exchange code").into_response();
    };
    let kind = if params.auth_type.as_deref() == Some("message") || matches!(params.cloud_auth.as_deref(), Some("message" | "message-bot")) {
        AuthKind::Message
    } else { AuthKind::Control };
    let config = match Config::load() {
        Ok(v) => v, Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response()
    };
    let mut auth = TwitchAuth::new(&config.twitch, &config.cloud);
    if let Err(err) = auth.load().await {
        log::warn(&format!("Failed reading Twitch integration: {err}"));
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    match auth.exchange_code(kind, &code).await {
        Ok(_) => {
            log::success(&format!("Cloud Twitch {} authorization saved", kind.as_str()));
            // TODO: Notify/reconnect the running Twitch client without restarting the backend.
            (StatusCode::OK, format!("Twitch {} authentication successful. Restart the backend to connect.", kind.as_str())).into_response()
        }
        Err(err) => {
            log::warn(&format!("Cloud Twitch exchange failed: {err}"));
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

async fn category_list() -> impl IntoResponse { category_call("category_library_list", json!({})) }
async fn category_get(Json(data): Json<Value>) -> impl IntoResponse { category_call("category_library_get", data) }
async fn category_save(Json(data): Json<Value>) -> impl IntoResponse { category_call("category_library_save", data) }
async fn category_delete(Json(data): Json<Value>) -> impl IntoResponse { category_call("category_library_delete", data) }
async fn category_activate(Json(data): Json<Value>) -> impl IntoResponse { category_call("category_library_activate", data) }
async fn category_refresh(Json(data): Json<Value>) -> impl IntoResponse { category_call("category_library_refresh", data) }
async fn category_import(Json(data): Json<Value>) -> impl IntoResponse { category_call("category_library_import_website", data) }
fn category_call(method: &str, params: Value) -> impl IntoResponse {
    match crate::category_library::dispatch(method, params) {
        Ok(data) => respond(StatusCode::OK, data).into_response(),
        Err(message) => respond(StatusCode::BAD_REQUEST, json!({"error":message})).into_response()
    }
}

pub async fn run(config: ServerConfig) -> std::io::Result<()> {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::HEAD, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE]);
    let app = Router::new()
        .route("/api/status", get(status_endpoint))
        .route("/api/category-library/list", get(category_list).post(category_list))
        .route("/api/category-library/get", post(category_get))
        .route("/api/category-library/save", post(category_save))
        .route("/api/category-library/delete", post(category_delete))
        .route("/api/category-library/activate", post(category_activate))
        .route("/api/category-library/refresh", post(category_refresh))
        .route("/api/category-library/import-website", post(category_import))
        .route("/status", get(status_endpoint))
        .route("/config.json", get(public_config_endpoint))
        .route("/auth/twitch/control", get(twitch_auth_control))
        .route("/auth/twitch/message", get(twitch_auth_message))
        .route("/auth/twitch/callback", get(twitch_auth_callback))
        .merge(static_router())
        .layer(cors);
    let addr = SocketAddr::from(([0,0,0,0], config.restport));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    log::success(&format!("REST/static server listening on http://{addr}"));
    axum::serve(listener, app).await
}
