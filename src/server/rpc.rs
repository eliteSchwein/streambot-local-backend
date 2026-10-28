//! JSON-RPC 2.0 framing shared by all WebSocket handlers.
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};

// IDs are only for requests WE initiate; responses always echo the caller's ID.
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

pub fn request(method: &str, params: Value) -> Value {
    let id = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    json!({"jsonrpc": "2.0", "method": method, "params": params, "id": id})
}

pub fn notification(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

pub fn success(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "result": result, "id": id})
}

pub fn error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "error": {"code": code, "message": message}, "id": id})
}

/// Returns None for notifications (which MUST NOT receive a response).
fn dispatch(value: Value) -> Option<Value> {
    let Some(obj) = value.as_object() else {
        return Some(error(Value::Null, -32600, "Invalid Request"));
    };
    let id = obj.get("id");
    let valid_id = id.is_none_or(|v| v.is_null() || v.is_string() || v.is_number());
    let valid_params = obj.get("params").is_none_or(|v| v.is_array() || v.is_object());
    let valid = obj.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
        && obj.get("method").and_then(Value::as_str).is_some()
        && valid_id && valid_params;
    if !valid {
        let response_id = if valid_id { id.cloned().unwrap_or(Value::Null) } else { Value::Null };
        return Some(error(response_id, -32600, "Invalid Request"));
    }
    let method = obj["method"].as_str().unwrap();
    // JSON-RPC notifications have NO id (id:null is still a request).
    let id = id?.clone();
    let response = match method {
        "status" => success(id, crate::status()),
        _ => error(id, -32601, "Method not found"),
    };
    Some(response)
}

pub fn handle_text(text: &str) -> Option<Value> {
    let value = match serde_json::from_str::<Value>(text) {
        Ok(value) => value,
        Err(_) => return Some(error(Value::Null, -32700, "Parse error")),
    };
    match value {
        Value::Array(batch) => {
            if batch.is_empty() { return Some(error(Value::Null, -32600, "Invalid Request")); }
            let replies: Vec<_> = batch.into_iter().filter_map(dispatch).collect();
            if replies.is_empty() { None } else { Some(Value::Array(replies)) }
        }
        other => dispatch(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids_increase() {
        let a = request("status", json!({}))["id"].as_u64().unwrap();
        let b = request("status", json!({}))["id"].as_u64().unwrap();
        assert_eq!(b, a + 1);
    }

    #[test]
    fn caller_ids_are_preserved() {
        let result = handle_text(r#"{"jsonrpc":"2.0","method":"status","id":"abc"}"#).unwrap();
        assert_eq!(result["id"], "abc");
        assert!(result.get("result").is_some());
    }

    #[test]
    fn notifications_receive_no_response() {
        assert!(handle_text(r#"{"jsonrpc":"2.0","method":"status"}"#).is_none());
    }

    #[test]
    fn invalid_json_uses_parse_error() {
        let result = handle_text("{").unwrap();
        assert_eq!(result["error"]["code"], -32700);
        assert!(result["id"].is_null());
    }

    #[test]
    fn batch_excludes_notifications() {
        let result = handle_text(r#"[{"jsonrpc":"2.0","method":"status","id":2},{"jsonrpc":"2.0","method":"status"}]"#).unwrap();
        assert_eq!(result.as_array().unwrap().len(), 1);
        assert_eq!(result[0]["id"], 2);
    }
}

/// Dispatch registered API methods asynchronously; leave protocol framing here.
async fn dispatch_api(value: Value) -> Option<Value> {
    let Some(obj) = value.as_object() else { return dispatch(value); };
    let Some(method) = obj.get("method").and_then(Value::as_str) else { return dispatch(value); };
    let Some(api) = super::apis::websocket(method) else { return dispatch(value); };
    let id = obj.get("id");
    let valid_id = id.is_none_or(|v| v.is_null() || v.is_string() || v.is_number());
    let valid_params = obj.get("params").is_none_or(|v| v.is_object() || v.is_array());
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || !valid_id || !valid_params {
        return Some(error(id.filter(|_| valid_id).cloned().unwrap_or(Value::Null), -32600, "Invalid Request"));
    }
    let result = api.handle(obj.get("params").cloned().unwrap_or(json!({}))).await;
    // Notifications without IDs are fire-and-forget.
    id.map(|id| success(id.clone(), if result.is_null() { json!({"status":"okay"}) } else { result }))
}

pub async fn handle_text_async(text: &str) -> Option<Value> {
    let value = match serde_json::from_str::<Value>(text) {
        Ok(value) => value,
        Err(_) => return Some(error(Value::Null, -32700, "Parse error")),
    };
    match value {
        Value::Array(batch) => {
            if batch.is_empty() { return Some(error(Value::Null, -32600, "Invalid Request")); }
            let mut responses = Vec::new();
            for item in batch { if let Some(response) = dispatch_api(item).await { responses.push(response); } }
            if responses.is_empty() { None } else { Some(Value::Array(responses)) }
        }
        other => dispatch_api(other).await,
    }
}
