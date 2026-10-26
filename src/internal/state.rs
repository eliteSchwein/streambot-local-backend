use std::sync::RwLock;

pub static STARTUP_STAGE: RwLock<&str> = RwLock::new("unknown");
pub static BACKEND_READY: RwLock<bool> = RwLock::new(false);
pub static IS_SAVING: RwLock<bool> = RwLock::new(false);

pub fn reset() {
    set_stage("unknown");
    set_ready(false);
    *IS_SAVING.write().unwrap() = false;
}
pub fn set_stage(stage: &'static str) { *STARTUP_STAGE.write().unwrap() = stage; }
pub fn set_ready(ready: bool) { *BACKEND_READY.write().unwrap() = ready; }
pub fn status() -> serde_json::Value {
    serde_json::json!({
        "bootup_stage": *STARTUP_STAGE.read().unwrap(),
        "ready": *BACKEND_READY.read().unwrap(),
    })
}
