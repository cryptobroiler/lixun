//! Search service — wraps LupaIndex for the GUI.

use anyhow::Result;
use lupa_core::{Hit, Query};
use lupa_index::LupaIndex;
use std::sync::Arc;
use tokio::sync::RwLock;

/// In-process search service (bypasses IPC for GUI).
#[derive(Clone)]
pub struct SearchService {
    index: Arc<RwLock<LupaIndex>>,
}

impl SearchService {
    pub fn new(index: LupaIndex) -> Self {
        Self {
            index: Arc::new(RwLock::new(index)),
        }
    }

    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<Hit>> {
        let index = self.index.read().await;
        index.search(&Query {
            text: query.to_string(),
            limit,
        })
    }

    /// Returns the shared index for external access (e.g. file watcher).
    pub fn index(&self) -> Arc<RwLock<LupaIndex>> {
        Arc::clone(&self.index)
    }
}
