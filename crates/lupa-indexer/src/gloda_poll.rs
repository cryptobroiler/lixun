use crate::cursors::Cursors;
use crate::index_service::{IndexMutationTx, Mutation};
use anyhow::Result;
use lupa_sources::Source;
use lupa_sources::gloda::GlodaSource;
use std::path::PathBuf;
use std::time::Duration;

pub async fn start(
    profile_path: PathBuf,
    state_dir: PathBuf,
    mutation_tx: IndexMutationTx,
) -> Result<()> {
    let poll_interval = Duration::from_secs(30);

    loop {
        tokio::time::sleep(poll_interval).await;

        let cursors = Cursors::load(&state_dir);

        let batch_size = 250;
        let source = GlodaSource::new(profile_path.clone(), cursors.last_gloda_key, batch_size);

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

        let max_key = docs
            .iter()
            .filter_map(|d| d.id.0.strip_prefix("mail:"))
            .filter_map(|s: &str| s.parse::<u64>().ok())
            .max()
            .unwrap_or(cursors.last_gloda_key);

        let count = docs.len();
        if let Err(e) = mutation_tx.send(Mutation::UpsertMany(docs)).await {
            tracing::error!("Gloda: failed to send mutation: {}", e);
            continue;
        }
        if let Err(e) = mutation_tx.barrier().await {
            tracing::error!("Gloda: barrier failed: {}", e);
            continue;
        }

        let mut cursors = Cursors::load(&state_dir);
        cursors.last_gloda_key = max_key;
        if let Err(e) = cursors.save(&state_dir) {
            tracing::error!("Failed to save Gloda cursor: {}", e);
        }

        tracing::info!(
            "Gloda poll: indexed {} new messages (cursor: {})",
            count,
            max_key
        );
    }
}
