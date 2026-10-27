//! Independent Ollama worker: health, model inventory and non-blocking REST requests.
//! TODO: migrate local installer, download/pull progress, managed process lifecycle and model switching.
use super::{Worker, read_integrations};
use std::io;
use serde_json::{Value, json};
use tokio::sync::oneshot;
use std::time::Duration;

pub struct OllamaService { worker: Worker }
pub fn init() -> io::Result<OllamaService> {
    Ok(OllamaService { worker: Worker::start("ollama", run)? })
}
impl OllamaService { pub async fn shutdown(&mut self) -> io::Result<()> { self.worker.shutdown().await } }

async fn run(mut stop: oneshot::Receiver<()>) {
    let http = reqwest::Client::builder().timeout(Duration::from_secs(5)).build().expect("HTTP client");
    let mut ticker = tokio::time::interval(Duration::from_secs(15));
    loop {
        tokio::select! {
            _ = &mut stop => break,
            _ = ticker.tick() => {
                let settings = read_integrations();
                let ollama = &settings["ollama"];
                let enabled = ollama["enabled"].as_bool().unwrap_or(false);
                let external = ollama["external"].as_bool().unwrap_or(false);
                let provider = ollama["external_provider"].as_str().unwrap_or("ollama");
                let url = if external {
                    if provider == "openai" { ollama["external_openai"]["url"].as_str() }
                    else { ollama["external_ollama"]["url"].as_str() }
                } else { Some("http://127.0.0.1:11434") };
                let base = url.unwrap_or("").trim_end_matches('/');
                let running = if enabled && provider != "openai" && !base.is_empty() {
                    http.get(format!("{base}/api/tags")).send().await.ok().filter(|r|r.status().is_success())
                } else { None };
                let is_running = running.is_some();
                let models: Vec<String> = if let Some(response) = running {
                    let data: Value = response.json().await.unwrap_or(Value::Null);
                    data["models"].as_array().map(|v|v.iter().filter_map(|x|x["name"].as_str().map(str::to_string)).collect()).unwrap_or_default()
                } else { Vec::new() };
                crate::server::registry::notify("notify_ollama_update", json!({
                    "enabled":enabled, "external":external, "external_provider":provider,
                    "model":ollama["model"], "running":is_running, "models":models,
                    "external_url": if external { base } else { "" },
                    "has_api_key": if provider == "openai" { ollama["external_openai"]["api_key"].as_str().is_some_and(|s|!s.is_empty()) }
                      else { ollama["external_ollama"]["api_key"].as_str().is_some_and(|s|!s.is_empty()) },
                    "installing":false, "changing_model":false
                }));
            }
        }
    }
}
