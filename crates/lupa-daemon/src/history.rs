//! Click-history ranking — local JSON store.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ClickHistory {
    counts: HashMap<String, u64>, // doc_id -> click count
}

impl ClickHistory {
    pub fn load(state_dir: &PathBuf) -> Result<Self> {
        let path = state_dir.join("history.json");
        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            let history: ClickHistory = serde_json::from_str(&content)?;
            Ok(history)
        } else {
            Ok(ClickHistory::default())
        }
    }

    pub fn save(&self, state_dir: &PathBuf) -> Result<()> {
        std::fs::create_dir_all(state_dir)?;
        let path = state_dir.join("history.json");
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        Ok(())
    }

    pub fn record_click(&mut self, doc_id: &str) {
        let count = self.counts.entry(doc_id.to_string()).or_insert(0);
        *count += 1;
    }

    /// Returns the bonus score for a document.
    pub fn bonus(&self, doc_id: &str) -> f32 {
        if let Some(&count) = self.counts.get(doc_id) {
            (1.0 + count as f32).ln() * 0.1
        } else {
            0.0
        }
    }
}
