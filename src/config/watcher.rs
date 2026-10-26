use crate::{config::file::ConfigFiles, internal::log};
use notify::{EventKind, RecursiveMode, Watcher};
use std::{path::Path, sync::Arc};

/// Call once after cache startup. Keep the returned watcher alive while the backend runs.
pub fn watch_settings(manager: Arc<ConfigFiles>) -> notify::Result<notify::RecommendedWatcher> {
    let root = manager.root().to_path_buf();
    let settings = root.join("streambot-settings.json");
    let handle = tokio::runtime::Handle::current();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        match event {
            Ok(event) => {
                let relevant = matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_))
                    && event.paths.iter().any(|path| path == &settings);
                if relevant {
                    let manager = manager.clone();
                    handle.spawn(async move {
                        if let Err(error) = manager.invalidate(Path::new("streambot-settings.json")).await {
                            log::warn(&format!("Settings cache invalidation failed: {error}"));
                        } else {
                            log::notice("streambot-settings.json changed; cache invalidated");
                        }
                    });
                }
            }
            Err(error) => log::warn(&format!("Settings watcher error: {error}")),
        }
    })?;
    std::fs::create_dir_all(&root)?;
    // Watch the directory: also detects atomic replacement/rename of the settings file.
    watcher.watch(&root, RecursiveMode::NonRecursive)?;
    Ok(watcher)
}
