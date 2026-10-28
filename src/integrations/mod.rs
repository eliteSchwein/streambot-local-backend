pub mod yolobox;

use std::{io, thread};
use tokio::sync::oneshot;

pub struct Worker {
    name: &'static str,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Worker {
    pub fn start<F, Fut>(name: &'static str, runner: F) -> io::Result<Self>
    where F: FnOnce(oneshot::Receiver<()>) -> Fut + Send + 'static,
          Fut: std::future::Future<Output = ()> + 'static {
        let (tx, rx) = oneshot::channel();
        let thread = thread::Builder::new().name(format!("streamding-{name}"))
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(rt) => rt.block_on(runner(rx)),
                    Err(err) => crate::internal::log::error(&format!("{name} runtime: {err}")),
                }
            })?;
        Ok(Self { name, stop: Some(tx), thread: Some(thread) })
    }
    pub async fn shutdown(&mut self) -> io::Result<()> {
        if let Some(tx) = self.stop.take() { let _ = tx.send(()); }
        if let Some(thread) = self.thread.take() {
            let name = self.name;
            tokio::task::spawn_blocking(move || thread.join().map_err(|_| io::Error::other(format!("{name} worker panicked"))))
                .await.map_err(io::Error::other)??;
        }
        Ok(())
    }
}

pub struct Integrations { pub yolobox: yolobox::YoloboxService }
pub fn init(_config: &crate::config::Config) -> io::Result<Integrations> {
    let yolobox = yolobox::init()?;
    Ok(Integrations { yolobox })
}
impl Integrations {
    pub async fn shutdown(&mut self) -> io::Result<()> {
        self.yolobox.shutdown().await
    }
}

pub(crate) fn integrations_path() -> std::path::PathBuf {
    crate::config::file::config_root().join("integrations.json")
}
pub(crate) fn read_integrations() -> serde_json::Value {
    std::fs::read(integrations_path()).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}
pub(crate) fn publish_connection(name: &str, enabled: bool, state: &str, connected: bool) {
    crate::server::registry::notify("notify_connection_update", serde_json::json!({(name): {"enabled": enabled, "connected":connected, "state":state}}));
}
