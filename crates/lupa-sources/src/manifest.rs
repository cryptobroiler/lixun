use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Manifest {
    files: HashMap<String, i64>,
}

impl Manifest {
    pub fn load(state_dir: &Path) -> Self {
        let path = state_dir.join("manifest.json");
        match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, state_dir: &Path) {
        let path = state_dir.join("manifest.json");
        if let Ok(content) = serde_json::to_string(self) {
            let _ = std::fs::write(&path, content);
        }
    }

    pub fn is_unchanged(&self, path: &str, mtime: i64) -> bool {
        self.files.get(path).copied() == Some(mtime)
    }

    pub fn update(&mut self, path: String, mtime: i64) {
        self.files.insert(path, mtime);
    }

    pub fn remove(&mut self, path: &str) {
        self.files.remove(path);
    }

    pub fn known_paths(&self) -> impl Iterator<Item = &String> {
        self.files.keys()
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }
}
