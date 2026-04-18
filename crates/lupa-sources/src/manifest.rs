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

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_default_manifest_is_empty() {
        let m = Manifest::default();
        assert_eq!(m.len(), 0);
    }

    #[test]
    fn test_update_and_is_unchanged() {
        let mut m = Manifest::default();
        m.update("/tmp/test.txt".to_string(), 12345);
        assert!(m.is_unchanged("/tmp/test.txt", 12345));
        assert!(!m.is_unchanged("/tmp/test.txt", 99999));
        assert!(!m.is_unchanged("/tmp/other.txt", 12345));
    }

    #[test]
    fn test_remove() {
        let mut m = Manifest::default();
        m.update("/tmp/test.txt".to_string(), 12345);
        assert_eq!(m.len(), 1);
        m.remove("/tmp/test.txt");
        assert_eq!(m.len(), 0);
        assert!(!m.is_unchanged("/tmp/test.txt", 12345));
    }

    #[test]
    fn test_known_paths() {
        let mut m = Manifest::default();
        m.update("/a.txt".to_string(), 1);
        m.update("/b.txt".to_string(), 2);
        m.update("/c.txt".to_string(), 3);
        let paths: Vec<_> = m.known_paths().collect();
        assert_eq!(paths.len(), 3);
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let mut m = Manifest::default();
        m.update("/tmp/a.txt".to_string(), 100);
        m.update("/tmp/b.txt".to_string(), 200);
        m.save(tmp.path());

        let loaded = Manifest::load(tmp.path());
        assert!(loaded.is_unchanged("/tmp/a.txt", 100));
        assert!(loaded.is_unchanged("/tmp/b.txt", 200));
        assert_eq!(loaded.len(), 2);
    }

    #[test]
    fn test_load_nonexistent_returns_default() {
        let tmp = tempfile::tempdir().unwrap();
        let m = Manifest::load(tmp.path());
        assert_eq!(m.len(), 0);
    }

    #[test]
    fn test_load_corrupt_json_returns_default() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("manifest.json"), "not json").unwrap();
        let m = Manifest::load(tmp.path());
        assert_eq!(m.len(), 0);
    }

    #[test]
    fn test_update_overwrites_mtime() {
        let mut m = Manifest::default();
        m.update("/tmp/test.txt".to_string(), 100);
        m.update("/tmp/test.txt".to_string(), 200);
        assert!(m.is_unchanged("/tmp/test.txt", 200));
        assert!(!m.is_unchanged("/tmp/test.txt", 100));
        assert_eq!(m.len(), 1);
    }
}
