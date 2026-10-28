//! Shared API registration used by BOTH transport servers.
use std::sync::{Arc, OnceLock, RwLock};
use crate::abstracts::api::Api;

static APIS: OnceLock<RwLock<Vec<Arc<dyn Api>>>> = OnceLock::new();
fn storage() -> &'static RwLock<Vec<Arc<dyn Api>>> {
    APIS.get_or_init(|| RwLock::new(Vec::new()))
}

/// Register before `server::init()` so REST routes are included at startup.
pub fn register<A: Api>(api: A) -> Result<(), String> {
    let mut list = storage().write().map_err(|e| e.to_string())?;
    if let Some(route) = api.rest_endpoint() {
        if route.is_empty() || route.starts_with('/') || route.contains('?') {
            return Err(format!("Invalid REST API endpoint: {route}"));
        }
        if list.iter().any(|other| other.rest_endpoint() == Some(route)) {
            return Err(format!("Duplicate REST API endpoint: {route}"));
        }
    }
    if let Some(method) = api.websocket_method() {
        if method.is_empty() || matches!(method, "register_endpoints" | "unregister_endpoints") {
            return Err(format!("Invalid WebSocket API method: {method}"));
        }
        if list.iter().any(|other| other.websocket_method() == Some(method)) {
            return Err(format!("Duplicate WebSocket API method: {method}"));
        }
    }
    list.push(Arc::new(api));
    Ok(())
}

pub fn all() -> Vec<Arc<dyn Api>> {
    storage().read().expect("API registry poisoned").clone()
}

pub fn websocket(method: &str) -> Option<Arc<dyn Api>> {
    storage().read().ok()?.iter().find(|api| api.websocket_method() == Some(method)).cloned()
}
