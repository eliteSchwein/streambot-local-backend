use axum::{body::{to_bytes, Body}, extract::{DefaultBodyLimit, FromRequest, Multipart, Query}, http::{header, Method, Request, StatusCode}, response::{IntoResponse, Redirect}, routing::{get, post}, Router};
use serde::Deserialize;
use crate::twitch::auth::{AuthKind, TwitchAuth};
use std::{net::SocketAddr, path::PathBuf};
use tower_http::{cors::{Any, CorsLayer}, services::{ServeDir, ServeFile}};
use crate::{config::{Config, ServerConfig}, internal::log, status};
use axum::Json;
use serde_json::{json, Value};
use super::response::respond;
use crate::abstracts::api::{Api, UploadedFile};
use std::sync::Arc;

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


// This endpoint intentionally omits authentication secrets.
// Use Config::load() to read the same optional root .env as the backend.
async fn public_config_endpoint() -> Json<Value> {
    Json(json!({}))
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

// Size limits apply to each request (including non-file form fields).
const MAX_BODY: usize = 32 * 1024 * 1024;
const MAX_FILE: usize = 24 * 1024 * 1024;

async fn handle_api_post(api: Arc<dyn Api>, request: Request<Body>) -> axum::response::Response {
    let content_type = request.headers().get(header::CONTENT_TYPE)
        .and_then(|header| header.to_str().ok()).unwrap_or("").to_ascii_lowercase();

    let result = if content_type.starts_with("multipart/form-data") {
        let mut multipart = match Multipart::from_request(request, &()).await {
            Ok(multipart) => multipart,
            Err(err) => return (StatusCode::BAD_REQUEST, Json(json!({"data": {"error": err.to_string()}, "status": 400}))).into_response(),
        };
        let mut data = serde_json::Map::new();
        let mut files = Vec::new();
        let mut total = 0usize;
        loop {
            let field = match multipart.next_field().await {
                Ok(field) => field,
                Err(err) => return (StatusCode::BAD_REQUEST, Json(json!({"data": {"error": err.to_string()}, "status": 400}))).into_response(),
            };
            let Some(field) = field else { break };
            let name = field.name().unwrap_or("").to_owned();
            let filename = field.file_name().map(str::to_owned);
            let media_type = field.content_type().map(str::to_owned);
            let bytes = match field.bytes().await {
                Ok(bytes) => bytes,
                Err(err) => return (StatusCode::BAD_REQUEST, Json(json!({"data": {"error": err.to_string()}, "status": 400}))).into_response(),
            };
            total = total.saturating_add(bytes.len());
            if total > MAX_BODY || (filename.is_some() && bytes.len() > MAX_FILE) {
                return (StatusCode::PAYLOAD_TOO_LARGE, Json(json!({"data": {"error": "Upload exceeds configured size limit"}, "status": 413}))).into_response();
            }
            if filename.is_some() {
                files.push(UploadedFile { field_name: name, file_name: filename, content_type: media_type, data: bytes.to_vec() });
            } else {
                let text = match String::from_utf8(bytes.to_vec()) {
                    Ok(text) => text,
                    Err(_) => return (StatusCode::BAD_REQUEST, Json(json!({"data": {"error": "Non-file form fields must be UTF-8"}, "status": 400}))).into_response(),
                };
                // Repeated field names become arrays rather than losing data.
                match data.entry(name) {
                    serde_json::map::Entry::Vacant(entry) => { entry.insert(Value::String(text)); }
                    serde_json::map::Entry::Occupied(mut entry) => {
                        if let Value::Array(values) = entry.get_mut() { values.push(Value::String(text)); }
                        else { let previous = entry.insert(Value::Null); *entry.get_mut() = Value::Array(vec![previous, Value::String(text)]); }
                    }
                }
            }
        }
        api.handle_upload(Value::Object(data), files).await
    } else if content_type.starts_with("application/json") {
        let body = match to_bytes(request.into_body(), MAX_BODY).await {
            Ok(body) => body,
            Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, Json(json!({"data": {"error": "Request body too large"}, "status": 413}))).into_response(),
        };
        let data: Value = match serde_json::from_slice(&body) {
            Ok(data) => data,
            Err(err) => return (StatusCode::BAD_REQUEST, Json(json!({"data": {"error": err.to_string()}, "status": 400}))).into_response(),
        };
        api.handle(data).await
    } else {
        return (StatusCode::UNSUPPORTED_MEDIA_TYPE, Json(json!({"data": {"error": "Expected application/json or multipart/form-data"}, "status": 415}))).into_response();
    };
    let code = if result.get("error").is_some() { StatusCode::BAD_REQUEST } else { StatusCode::OK };
    respond(code, result).into_response()
}

pub async fn run(config: ServerConfig) -> std::io::Result<()> {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::HEAD, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE]);
    let mut api_router = Router::new();
    for api in super::apis::all() {
        let Some(endpoint) = api.rest_endpoint() else { continue };
        let path = format!("/api/{endpoint}");
        if api.rest_post() {
            let handler = api.clone();
            api_router = api_router.route(&path, post(move |request: Request<Body>| {
                let api = handler.clone();
                async move { handle_api_post(api, request).await }
            }).layer(DefaultBodyLimit::max(MAX_BODY)));
        } else {
            let handler = api.clone();
            api_router = api_router.route(&path, get(move || {
                let api = handler.clone();
                async move {
                    let result = api.handle(json!({})).await;
                    let code = if result.get("error").is_some() { StatusCode::BAD_REQUEST } else { StatusCode::OK };
                    respond(code, result)
                }
            }));
        }
    }
    let app = Router::new()
        .merge(api_router)
        .route("/status", get(|| async { respond(StatusCode::OK, status()) }))
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
