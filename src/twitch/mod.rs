pub mod auth;
pub mod client;
pub mod events;
pub use client::TwitchClient;


use std::{io, thread};
use tokio::sync::{mpsc, oneshot};

pub struct TwitchService {
    stop: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
    exits: mpsc::UnboundedReceiver<io::Result<()>>,
}

pub fn init(config: &crate::config::Config) -> io::Result<TwitchService> {
    let twitch_config = config.twitch.clone();
    let cloud_config = config.cloud.clone();
    let (stop, stop_rx) = oneshot::channel();
    let (exit_tx, exits) = mpsc::unbounded_channel();
    let thread = thread::Builder::new().name("streambot-twitch".into()).spawn(move || {
        let result = run_worker(twitch_config, cloud_config, stop_rx);
        let _ = exit_tx.send(result);
    })?;
    Ok(TwitchService { stop: Some(stop), thread: Some(thread), exits })
}

fn run_worker(twitch: crate::config::TwitchConfig, cloud: crate::config::CloudConfig,
              mut stop: oneshot::Receiver<()>) -> io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(async move {
        crate::server::registry::notify("notify_twitch_connection", serde_json::json!({"state":"connecting","connected":false}));
        let client = tokio::select! {
            result = TwitchClient::connect(&twitch, &cloud) => match result {
                Ok(Some(client)) => {
                    crate::server::registry::notify("notify_twitch_connection", serde_json::json!({"state":"connected","connected":true}));
                    Some(client)
                }
                Ok(None) => {
                    crate::server::registry::notify("notify_twitch_connection", serde_json::json!({"state":"auth_required","connected":false}));
                    None
                }
                Err(error) => {
                    crate::internal::log::warn(&format!("Twitch initialization failed: {error}"));
                    crate::server::registry::notify("notify_twitch_connection", serde_json::json!({"state":"error","connected":false,"message":error.to_string()}));
                    None
                }
            },
            _ = &mut stop => return Ok(()),
        };
        let _client = client;
        // TODO: EventSub / incoming chat / reconnect lifecycle.
        let _ = stop.await;
        crate::server::registry::notify("notify_twitch_connection", serde_json::json!({"state":"disconnected","connected":false}));
        Ok(())
    })
}

impl TwitchService {
    pub async fn wait_for_exit(&mut self) -> Option<io::Result<()>> { self.exits.recv().await }
    pub async fn shutdown(&mut self) -> io::Result<()> {
        if let Some(tx) = self.stop.take() { let _ = tx.send(()); }
        if let Some(thread) = self.thread.take() {
            tokio::task::spawn_blocking(move || thread.join().map_err(|_| io::Error::other("Twitch thread panicked")))
                .await.map_err(io::Error::other)??;
        }
        Ok(())
    }
}
