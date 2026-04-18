use anyhow::Result;
use lupa_index::LupaIndex;
use lupa_sources::Source;
use lupa_sources::apps::AppsSource;
use lupa_sources::thunderbird_attachments::ThunderbirdAttachmentsSource;
use notify::{Config, Event, RecursiveMode, Watcher as NotifyWatcher};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

const REINDEX_COOLDOWN: Duration = Duration::from_secs(30);

pub async fn start(
    apps_source: Arc<AppsSource>,
    attachments_source: Option<Arc<ThunderbirdAttachmentsSource>>,
    index: Arc<RwLock<LupaIndex>>,
) -> Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(1000);

    let mut watcher = notify::RecommendedWatcher::new(
        move |res: notify::Result<Event>| {
            if let Ok(event) = res {
                let _ = tx.blocking_send(event);
            }
        },
        Config::default(),
    )?;

    for dir in &apps_source.search_dirs {
        if dir.exists()
            && let Err(e) = watcher.watch(dir, RecursiveMode::NonRecursive)
        {
            tracing::warn!("Failed to watch app dir {:?}: {}", dir, e);
        }
    }

    if let Some(ref att_source) = attachments_source {
        let mail_path = att_source.profile_path.join("Mail");
        let imap_path = att_source.profile_path.join("ImapMail");
        for base in [&mail_path, &imap_path] {
            if base.exists()
                && let Err(e) = watcher.watch(base, RecursiveMode::Recursive)
            {
                tracing::warn!("Failed to watch mail dir {:?}: {}", base, e);
            }
        }
    }

    let debounce_ms = 2000;
    let mut last_apps_reindex: Option<Instant> = None;
    let mut last_attachments_reindex: Option<Instant> = None;

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
            break;
        }

        let has_app_events = events.iter().any(|e| {
            e.paths
                .iter()
                .any(|p| p.extension().map(|e| e == "desktop").unwrap_or(false))
        });

        if has_app_events {
            let cooldown_expired = last_apps_reindex
                .map(|t| t.elapsed() >= REINDEX_COOLDOWN)
                .unwrap_or(true);

            if cooldown_expired {
                if let Err(e) = reindex_apps(&apps_source, &index).await {
                    tracing::error!("Apps reindex error: {}", e);
                }
                last_apps_reindex = Some(Instant::now());
            } else {
                tracing::debug!("Apps reindex skipped (cooldown active)");
            }
        }

        if let Some(attachments_source) = attachments_source.as_ref() {
            let has_mbox_events = events.iter().any(|e| {
                e.paths.iter().any(|p| {
                    p.is_file()
                        && !p
                            .file_name()
                            .map(|n| n.to_string_lossy().ends_with(".msf"))
                            .unwrap_or(true)
                })
            });

            if has_mbox_events {
                let cooldown_expired = last_attachments_reindex
                    .map(|t| t.elapsed() >= REINDEX_COOLDOWN)
                    .unwrap_or(true);

                if cooldown_expired {
                    if let Err(e) = reindex_attachments(attachments_source, &index).await {
                        tracing::error!("Attachments reindex error: {}", e);
                    }
                    last_attachments_reindex = Some(Instant::now());
                } else {
                    tracing::debug!("Attachments reindex skipped (cooldown active)");
                }
            }
        }
    }

    Ok(())
}

async fn reindex_apps(source: &Arc<AppsSource>, index: &Arc<RwLock<LupaIndex>>) -> Result<()> {
    let docs = source.index_all()?;
    let mut idx = index.write().await;
    let mut writer = idx.writer(64_000_000)?;
    for doc in &docs {
        idx.upsert(doc, &mut writer)?;
    }
    idx.commit(&mut writer)?;
    tracing::info!("Apps watcher: reindexed {} apps", docs.len());
    Ok(())
}

async fn reindex_attachments(
    source: &Arc<ThunderbirdAttachmentsSource>,
    index: &Arc<RwLock<LupaIndex>>,
) -> Result<()> {
    let docs = source.index_all()?;
    let mut idx = index.write().await;
    let mut writer = idx.writer(64_000_000)?;
    for doc in &docs {
        idx.upsert(doc, &mut writer)?;
    }
    idx.commit(&mut writer)?;
    tracing::info!("Attachments watcher: reindexed {} attachments", docs.len());
    Ok(())
}
