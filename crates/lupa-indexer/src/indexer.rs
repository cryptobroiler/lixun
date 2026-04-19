//! Library-side indexing entry points so the logic can be tested without
//! the daemon binary scaffolding in `main.rs`.

use anyhow::Result;
use lupa_core::Document;
use lupa_sources::manifest::Manifest;
use lupa_sources::Source;
use std::collections::HashSet;
use std::path::Path;

use crate::index_service::{IndexMutationTx, Mutation, SearchHandle};
use crate::sources_api::IndexerSources;

pub struct ReindexOutcome {
    pub total_docs: usize,
    pub fs_docs: usize,
    pub other_docs: usize,
}

pub async fn reindex_full(
    mutation_tx: &IndexMutationTx,
    config: &dyn IndexerSources,
    state_dir: &Path,
) -> Result<ReindexOutcome> {
    let state_dir_owned = state_dir.to_path_buf();
    let _ = std::fs::remove_file(state_dir_owned.join("manifest.json"));

    let fs_count = {
        let (batch_tx, mut batch_rx) = tokio::sync::mpsc::channel::<Vec<Document>>(2);
        let fs_source = config.build_fs_source()?;
        let empty_ids: HashSet<String> = HashSet::new();
        let runtime = tokio::runtime::Handle::current();
        let state_dir_for_walk = state_dir_owned.clone();

        let walk_task = tokio::task::spawn_blocking(move || -> Result<()> {
            let mut manifest = Manifest::default();
            let _deleted = fs_source.index_incremental_batched(
                &mut manifest,
                &empty_ids,
                |docs| {
                    runtime
                        .block_on(batch_tx.send(docs))
                        .map_err(|_| anyhow::anyhow!("reindex consumer closed"))?;
                    Ok(())
                },
            )?;
            manifest.save(&state_dir_for_walk);
            Ok(())
        });

        let mut fs_count = 0usize;
        while let Some(batch) = batch_rx.recv().await {
            fs_count += batch.len();
            mutation_tx.send(Mutation::UpsertMany(batch)).await?;
            let _ = mutation_tx.barrier().await?;
        }
        walk_task.await??;
        fs_count
    };

    let indexed_sources: Vec<(&'static str, Vec<Document>)> = {
        let sources = config.build_sources()?;
        let mut out = Vec::new();
        for source in &sources {
            tracing::info!("Reindexing source: {}", source.name());
            out.push((source.name(), source.index_all()?));
        }
        out
    };
    let mut other_count = 0usize;
    for (name, docs) in indexed_sources {
        other_count += docs.len();
        mutation_tx.send(Mutation::UpsertMany(docs)).await?;
        let _ = mutation_tx.barrier().await?;
        tracing::info!("Source {} indexed", name);
    }
    mutation_tx.commit_now().await?;

    tracing::info!(
        "Full reindex: {} fs docs, {} other docs, manifest rebuilt",
        fs_count,
        other_count
    );

    Ok(ReindexOutcome {
        total_docs: fs_count + other_count,
        fs_docs: fs_count,
        other_docs: other_count,
    })
}

pub async fn reindex_paths(
    mutation_tx: &IndexMutationTx,
    config: &dyn IndexerSources,
    paths: &[std::path::PathBuf],
) -> Result<usize> {
    let mut all_docs: Vec<Document> = Vec::new();

    for path in paths {
        if path.is_file() {
            if let Ok(doc) = crate::index_service::index_file(path, config.max_file_size_mb()) {
                all_docs.push(doc);
            }
        } else if path.is_dir() {
            let source = lupa_sources::fs::FsSource::new(
                vec![path.clone()],
                config.exclude().to_vec(),
                config.max_file_size_mb(),
            );
            all_docs.extend(source.index_all()?);
        }
    }

    let count = all_docs.len();
    mutation_tx.send(Mutation::UpsertMany(all_docs)).await?;
    mutation_tx.commit_now().await?;
    Ok(count)
}

pub async fn run_incremental(
    mutation_tx: &IndexMutationTx,
    search: &SearchHandle,
    config: &dyn IndexerSources,
    state_dir: &Path,
) -> Result<(usize, usize)> {
    let mut manifest = Manifest::load(state_dir);
    let indexed_ids: HashSet<String> = search.all_doc_ids().await.unwrap_or_else(|e| {
        tracing::warn!(
            "Could not enumerate existing index doc ids ({}); treating index as empty, will re-surface all manifest entries",
            e
        );
        HashSet::new()
    });
    tracing::info!(
        "Incremental indexer: manifest has {} entries, index has {} docs",
        manifest.len(),
        indexed_ids.len()
    );

    let fs_count = {
        let (batch_tx, mut batch_rx) = tokio::sync::mpsc::channel::<Vec<Document>>(2);
        let fs_source = config.build_fs_source()?;
        let indexed_ids_cloned = indexed_ids.clone();
        let manifest_for_walk = std::mem::take(&mut manifest);
        let runtime = tokio::runtime::Handle::current();

        let walk_task = tokio::task::spawn_blocking(move || -> Result<(Manifest, Vec<String>)> {
            let mut manifest = manifest_for_walk;
            let deleted = fs_source.index_incremental_batched(
                &mut manifest,
                &indexed_ids_cloned,
                |docs| {
                    runtime
                        .block_on(batch_tx.send(docs))
                        .map_err(|_| anyhow::anyhow!("indexer consumer closed"))?;
                    Ok(())
                },
            )?;
            Ok((manifest, deleted))
        });

        let mut fs_count = 0usize;
        while let Some(batch) = batch_rx.recv().await {
            fs_count += batch.len();
            mutation_tx.send(Mutation::UpsertMany(batch)).await?;
            let _ = mutation_tx.barrier().await?;
        }

        let (returned_manifest, deleted_ids) = walk_task.await??;
        manifest = returned_manifest;

        if !deleted_ids.is_empty() {
            let del_count = deleted_ids.len();
            mutation_tx.send(Mutation::DeleteMany(deleted_ids)).await?;
            let _ = mutation_tx.barrier().await?;
            tracing::info!(
                "Filesystem incremental: +{} docs, -{} deleted",
                fs_count,
                del_count
            );
        } else if fs_count > 0 {
            tracing::info!("Filesystem incremental: +{} docs", fs_count);
        }

        fs_count
    };

    let indexed_sources: Vec<(&'static str, Vec<Document>)> = {
        let sources = config.build_sources()?;
        let mut out = Vec::new();
        for source in &sources {
            out.push((source.name(), source.index_all()?));
        }
        out
    };

    let mut other_count = 0usize;
    for (name, docs) in indexed_sources {
        other_count += docs.len();
        mutation_tx.send(Mutation::UpsertMany(docs)).await?;
        let _ = mutation_tx.barrier().await?;
        tracing::info!("Source {} indexed", name);
    }

    manifest.save(state_dir);
    Ok((fs_count, other_count))
}
