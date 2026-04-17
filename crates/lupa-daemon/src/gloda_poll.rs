use anyhow::Result;
use lupa_index::LupaIndex;
use lupa_sources::gloda::GlodaSource;
use lupa_sources::Source;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use crate::cursors::Cursors;

pub async fn start(
    profile_path: PathBuf,
    state_dir: PathBuf,
    index: Arc<RwLock<LupaIndex>>,
) -> Result<()> {
    let poll_interval = Duration::from_secs(30);

    loop {
        tokio::time::sleep(poll_interval).await;

        let cursors = Cursors::load(&state_dir);
        if cursors.last_gloda_key == 0 {
            continue;
        }

        let source = GlodaSource::new(profile_path.clone(), cursors.last_gloda_key);

        let docs = match source.index_all() {
            Ok(docs) => docs,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("database is locked") || msg.contains("busy") {
                    tracing::warn!("Gloda DB locked, retrying next poll cycle");
                } else {
                    tracing::error!("Gloda poll error: {}", e);
                }
                continue;
            }
        };

        if docs.is_empty() {
            continue;
        }

        let max_key = docs.iter()
            .filter_map(|d| d.id.0.strip_prefix("mail:"))
            .filter_map(|s: &str| s.parse::<u64>().ok())
            .max()
            .unwrap_or(cursors.last_gloda_key);

        let mut idx = index.write().await;
        let mut writer = idx.writer(128_000_000)?;

        for doc in &docs {
            if let Err(e) = idx.upsert(doc, &mut writer) {
                tracing::error!("Gloda upsert error for {}: {}", doc.id.0, e);
            }
        }

        if let Err(e) = idx.commit(&mut writer) {
            tracing::error!("Gloda commit error: {}", e);
            continue;
        }

        let mut cursors = Cursors::load(&state_dir);
        cursors.last_gloda_key = max_key;
        if let Err(e) = cursors.save(&state_dir) {
            tracing::error!("Failed to save Gloda cursor: {}", e);
        }

        tracing::info!("Gloda poll: indexed {} new messages (cursor: {})", docs.len(), max_key);
    }
}
