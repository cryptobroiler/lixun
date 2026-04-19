use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Cursors {
    pub last_gloda_key: u64,
}

impl Cursors {
    pub fn load(state_dir: &Path) -> Self {
        let path = state_dir.join("cursors.json");
        match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, state_dir: &Path) -> Result<()> {
        let path = state_dir.join("cursors.json");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        Ok(())
    }
}
