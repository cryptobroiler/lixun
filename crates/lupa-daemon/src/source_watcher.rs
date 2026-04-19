use lupa_daemon::index_service::{IndexMutationTx, Mutation};
use anyhow::Result;
use lupa_sources::Source;
use lupa_sources::apps::AppsSource;
use lupa_sources::thunderbird_attachments::ThunderbirdAttachmentsSource;
use notify::{Config, Event, RecursiveMode, Watcher as NotifyWatcher};
use std::sync::Arc;
use std::time::{Duration, Instant};

const REINDEX_COOLDOWN: Duration = Duration::from_secs(30);

pub async fn start(
    apps_source: Arc<AppsSource>,
    attachments_source: Option<Arc<ThunderbirdAttachmentsSource>>,
    mutation_tx: IndexMutationTx,
) -> Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();

    let mut watcher = notify::RecommendedWatcher::new(
        move |res: notify::Result<Event>| {
            if let Ok(event) = res {
                let _ = tx.send(event);
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
                if let Err(e) = reindex_apps(&apps_source, &mutation_tx).await {
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
                    if let Err(e) = reindex_attachments(attachments_source).await {
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

async fn reindex_apps(source: &Arc<AppsSource>, mutation_tx: &IndexMutationTx) -> Result<()> {
    let docs = source.index_all()?;
    let count = docs.len();
    mutation_tx.send(Mutation::UpsertMany(docs)).await?;
    mutation_tx.barrier().await?;
    tracing::info!("Apps watcher: reindexed {} apps", count);
    Ok(())
}

async fn reindex_attachments(_source: &Arc<ThunderbirdAttachmentsSource>) -> Result<()> {
    tracing::info!(
        "Attachments watcher: change detected, full reindex skipped to avoid reloading entire Thunderbird attachment corpus"
    );
    Ok(())
}
