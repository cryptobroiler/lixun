use crate::index_service::{IndexMutationTx, Mutation, fs_doc_id, index_file};
use anyhow::Result;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc;

pub async fn start(
    roots: Vec<PathBuf>,
    exclude: Vec<String>,
    max_file_size_mb: u64,
    mutation_tx: IndexMutationTx,
) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Event>();

    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<Event>| {
            if let Ok(event) = res {
                let _ = tx.send(event);
            }
        },
        Config::default(),
    )?;

    for root in &roots {
        if let Err(e) = watcher.watch(root, RecursiveMode::Recursive) {
            tracing::warn!("Watcher: failed to watch root {:?}: {}", root, e);
        }
    }

    tracing::info!("File watcher started on {} roots", roots.len());

    let debounce_ms = 500;
    loop {
        let mut events = Vec::new();
        if let Some(event) = rx.recv().await {
            events.push(event);
            loop {
                match tokio::time::timeout(Duration::from_millis(debounce_ms), rx.recv()).await {
                    Ok(Some(ev)) => events.push(ev),
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
        } else {
            break Ok(());
        }

        let mut upserts: Vec<lupa_core::Document> = Vec::new();
        let mut deletes: Vec<String> = Vec::new();

        for event in events {
            match event.kind {
                EventKind::Remove(_) => {
                    for path in &event.paths {
                        deletes.push(fs_doc_id(path));
                    }
                }
                EventKind::Modify(notify::event::ModifyKind::Name(
                    notify::event::RenameMode::From,
                )) => {
                    for path in &event.paths {
                        deletes.push(fs_doc_id(path));
                    }
                }
                EventKind::Modify(notify::event::ModifyKind::Name(
                    notify::event::RenameMode::To,
                )) => {
                    for path in &event.paths {
                        if path.is_file() {
                            let path_str = path.to_string_lossy();
                            if !should_exclude(&path_str, &exclude)
                                && let Ok(doc) = index_file(path, max_file_size_mb)
                            {
                                upserts.push(doc);
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
                                upserts.push(doc);
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        if !deletes.is_empty() {
            mutation_tx.send(Mutation::DeleteMany(deletes)).await?;
        }
        if !upserts.is_empty() {
            mutation_tx.send(Mutation::UpsertMany(upserts)).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_exclude_matches_substring() {
        let exclude = vec![".cache".into(), "node_modules".into(), ".swp".into()];
        assert!(should_exclude("/home/u/.cache/foo", &exclude));
        assert!(should_exclude("/home/u/p/node_modules/x", &exclude));
        assert!(should_exclude("/home/u/tmp/.file.swp", &exclude));
        assert!(!should_exclude("/home/u/tmp/file.txt", &exclude));
    }

    #[test]
    fn should_exclude_empty_list() {
        let exclude: Vec<String> = vec![];
        assert!(!should_exclude("/any/path", &exclude));
    }
}
