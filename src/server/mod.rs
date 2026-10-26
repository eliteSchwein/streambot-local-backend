pub mod rest;
pub mod response;
pub mod websocket;
pub mod rpc;

pub mod registry;


use std::{future::Future, io, thread};
use tokio::sync::{mpsc, oneshot};

pub struct ServerService {
    rest_stop: Option<oneshot::Sender<()>>,
    websocket_stop: Option<oneshot::Sender<()>>,
    rest_thread: Option<thread::JoinHandle<()>>,
    websocket_thread: Option<thread::JoinHandle<()>>,
    exits: mpsc::UnboundedReceiver<(&'static str, io::Result<()>)>,
}

fn run_server<F>(future: F, mut shutdown: oneshot::Receiver<()>) -> io::Result<()>
where F: Future<Output=io::Result<()>> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(async move {
        tokio::select! { result = future => result, _ = &mut shutdown => Ok(()) }
    })
}

pub fn init(config: &crate::config::Config) -> io::Result<ServerService> {
    let (exit_tx, exits) = mpsc::unbounded_channel();
    let (rest_stop, rest_rx) = oneshot::channel();
    let rest_config = config.server.clone();
    let rest_exits = exit_tx.clone();
    let rest_thread = thread::Builder::new().name("streambot-rest".into()).spawn(move || {
        let result = run_server(rest::run(rest_config), rest_rx);
        let _ = rest_exits.send(("REST", result));
    })?;

    let (websocket_stop, websocket_rx) = oneshot::channel();
    let websocket_config = config.server.clone();
    let websocket_thread = match thread::Builder::new().name("streambot-websocket".into()).spawn(move || {
        let result = run_server(websocket::run(websocket_config), websocket_rx);
        let _ = exit_tx.send(("WebSocket", result));
    }) {
        Ok(thread) => thread,
        Err(error) => {
            let _ = rest_stop.send(());
            let _ = rest_thread.join();
            return Err(error);
        }
    };
    Ok(ServerService { rest_stop: Some(rest_stop), websocket_stop: Some(websocket_stop),
        rest_thread: Some(rest_thread), websocket_thread: Some(websocket_thread), exits })
}

impl ServerService {
    pub async fn wait_for_exit(&mut self) -> Option<(&'static str, io::Result<()>)> {
        self.exits.recv().await
    }
    pub async fn shutdown(&mut self) -> io::Result<()> {
        if let Some(tx) = self.rest_stop.take() { let _ = tx.send(()); }
        if let Some(tx) = self.websocket_stop.take() { let _ = tx.send(()); }
        let rest = self.rest_thread.take();
        let websocket = self.websocket_thread.take();
        tokio::task::spawn_blocking(move || {
            for thread in [rest, websocket].into_iter().flatten() {
                thread.join().map_err(|_| io::Error::other("server thread panicked"))?;
            }
            Ok::<(), io::Error>(())
        }).await.map_err(io::Error::other)?
    }
}
