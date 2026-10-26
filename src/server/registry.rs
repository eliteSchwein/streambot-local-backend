//! Thread-safe notification registry. Updates can originate on any Tokio runtime / OS thread.
use serde_json::{Value, json};
use std::{collections::HashMap, sync::{Mutex, OnceLock}};
use tokio::sync::broadcast;

#[derive(Clone, Debug)]
pub struct Update { pub method: String, pub data: Value }

pub struct Registry {
    tx: broadcast::Sender<Update>,
    snapshots: Mutex<HashMap<String, Value>>,
}

static GLOBAL: OnceLock<Registry> = OnceLock::new();
pub fn get() -> &'static Registry {
    GLOBAL.get_or_init(|| {
        let (tx, _) = broadcast::channel(256);
        Registry {tx, snapshots: Mutex::new(HashMap::new())}
    })
}

impl Registry {
    pub fn subscribe(&self) -> broadcast::Receiver<Update> { self.tx.subscribe() }
    pub fn notify(&self, method: &str, data: Value) {
        // Save authoritative full state; do not clear it when a websocket disconnects.
        self.snapshots.lock().unwrap().insert(method.to_owned(), data.clone());
        let _ = self.tx.send(Update { method: method.to_owned(), data });
    }
    pub fn snapshot(&self, method: &str) -> Option<Value> {
        self.snapshots.lock().unwrap().get(method).cloned()
    }
    pub fn list(&self) -> Vec<String> {
        let mut keys: Vec<_> = self.snapshots.lock().unwrap().keys().cloned().collect();
        keys.sort(); keys
    }
}

pub fn notify(method: &str, data: Value) { get().notify(method, data); }

/// JSON Merge Patch-style delta; arrays are atomic, and deleted fields become null.
/// `None` indicates no change. Every receiver keeps its own last-sent baseline.
pub fn delta(previous: &Value, next: &Value) -> Option<Value> {
    if previous == next { return None; }
    match (previous, next) {
        (Value::Object(a), Value::Object(b)) => {
            let mut changes = serde_json::Map::new();
            for (key, value) in b {
                match a.get(key) {
                    Some(prev) => { if let Some(change) = delta(prev, value) { changes.insert(key.clone(), change); } }
                    None => { changes.insert(key.clone(), value.clone()); }
                }
            }
            for key in a.keys() { if !b.contains_key(key) { changes.insert(key.clone(), Value::Null); } }
            if changes.is_empty() { None } else { Some(Value::Object(changes)) }
        }
        _ => Some(next.clone()),
    }
}

pub fn notification(method: &str, data: Value, partial: bool) -> Value {
    let mut value = json!({"jsonrpc":"2.0", "method":method, "params":data});
    if partial { value["partial"] = Value::Bool(true); }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn delta_removes_keys() {
        assert_eq!(delta(&json!({"a":1,"b":2}), &json!({"a":1})), Some(json!({"b":null})));
    }
    #[test] fn notification_has_no_id() {
        assert!(notification("notify_test", json!({}), false).get("id").is_none());
    }
}
