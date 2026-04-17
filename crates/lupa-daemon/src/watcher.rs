use anyhow::Result;
use lupa_core::{Action, Category, DocId, Document};
use lupa_index::LupaIndex;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};

/// Debounced file watcher that keeps the index in sync.
pub async fn start(
    roots: Vec<PathBuf>,
    exclude: Vec<String>,
    max_file_size_mb: u64,
    index: Arc<RwLock<LupaIndex>>,
) -> Result<()> {
    // Set up debounced channel
    let (tx, mut rx) = mpsc::channel::<Event>(1000);

    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<Event>| {
            if let Ok(event) = res {
                let _ = tx.blocking_send(event);
            }
        },
        Config::default(),
    )?;

    for root in &roots {
        watcher.watch(root, RecursiveMode::Recursive)?;
    }

    tracing::info!("File watcher started on {} roots", roots.len());

    // Debounce loop
    let debounce_ms = 500;
    loop {
        // Collect events within debounce window
        let mut events = Vec::new();
        if let Some(event) = rx.recv().await {
            events.push(event);
            // Drain remaining events within window
            loop {
                match tokio::time::timeout(Duration::from_millis(debounce_ms), rx.recv()).await {
                    Ok(Some(ev)) => events.push(ev),
                    Ok(None) => break,
                    Err(_) => break, // timeout
                }
            }
        } else {
            break Ok(()); // channel closed
        }

        // Process batch
        let mut idx = index.write().await;
        let mut writer = idx.writer(128_000_000)?;
        let mut changes = 0;

        for event in events {
            match event.kind {
                EventKind::Remove(_) => {
                    for path in &event.paths {
                        let id = format!("fs:{}", path.to_string_lossy());
                        idx.delete_by_id(&id, &mut writer)?;
                        changes += 1;
                    }
                }
                EventKind::Modify(notify::event::ModifyKind::Name(notify::event::RenameMode::From)) => {
                    for path in &event.paths {
                        let id = format!("fs:{}", path.to_string_lossy());
                        idx.delete_by_id(&id, &mut writer)?;
                        changes += 1;
                    }
                }
                EventKind::Modify(notify::event::ModifyKind::Name(notify::event::RenameMode::To)) => {
                    for path in &event.paths {
                        if path.is_file() {
                            let path_str = path.to_string_lossy();
                            if !should_exclude(&path_str, &exclude) {
                                if let Ok(doc) = index_file(path, max_file_size_mb) {
                                    idx.upsert(&doc, &mut writer)?;
                                    changes += 1;
                                }
                            }
                        }
                    }
                }
                EventKind::Create(_) | EventKind::Modify(_) => {
                    for path in &event.paths {
                        if path.is_file() {
                            let path_str = path.to_string_lossy();
                            if should_exclude(&path_str, &exclude) {
                                continue;
                            }
                            if let Ok(doc) = index_file(path, max_file_size_mb) {
                                idx.upsert(&doc, &mut writer)?;
                                changes += 1;
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        if changes > 0 {
            idx.commit(&mut writer)?;
            tracing::debug!("Watcher: committed {} changes", changes);
        }
    }
}

fn should_exclude(path: &str, exclude: &[String]) -> bool {
    for pat in exclude {
        if path.contains(pat.as_str()) {
            return true;
        }
    }
    false
}

pub fn index_file(path: &std::path::Path, max_file_size_mb: u64) -> Result<Document> {
    let path_str = path.to_string_lossy().to_string();
    let filename = path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let metadata = std::fs::metadata(path)?;
    let mtime = metadata.modified()
        .map(|t| t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0))
        .unwrap_or(0);
    let size = metadata.len();

    let max_size = max_file_size_mb * 1024 * 1024;
    let (body, extract_fail) = if size <= max_size {
        match lupa_sources::fs::FsSource::extract_content(path) {
            Ok(Some(text)) => (Some(text), false),
            Ok(None) => (None, false),
            Err(_) => (None, true),
        }
    } else {
        (None, false)
    };

    Ok(Document {
        id: DocId(format!("fs:{}", path_str)),
        category: Category::File,
        title: filename,
        subtitle: path_str.clone(),
        body,
        path: path_str,
        mtime,
        size,
        action: Action::OpenFile { path: path.to_path_buf() },
        extract_fail,
    })
}
