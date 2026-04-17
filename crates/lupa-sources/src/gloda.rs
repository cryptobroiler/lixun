//! Gloda source — Thunderbird global-messages-db.sqlite.

use anyhow::Result;
use lupa_core::{Action, Category, DocId, Document};
use std::path::PathBuf;

/// Gloda mail source.
pub struct GlodaSource {
    pub profile_path: PathBuf,
    pub last_key: u64,
}

impl GlodaSource {
    /// Try to find the Thunderbird profile.
    pub fn find_profile() -> Option<PathBuf> {
        let home = std::env::var("HOME").ok()?;
        let tb_path = PathBuf::from(&home).join(".thunderbird");

        if !tb_path.exists() {
            return None;
        }

        for entry in std::fs::read_dir(&tb_path).ok()? {
            if let Ok(entry) = entry {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.ends_with(".default") || name.ends_with(".default-release") || name.contains(".default") {
                    return Some(entry.path());
                }
            }
        }
        None
    }

    pub fn new(profile_path: PathBuf, last_key: u64) -> Self {
        Self { profile_path, last_key }
    }
}

impl crate::Source for GlodaSource {
    fn name(&self) -> &'static str {
        "gloda"
    }

    fn index_all(&self) -> Result<Vec<Document>> {
        let db_path = self.profile_path.join("global-messages-db.sqlite");

        if !db_path.exists() {
            tracing::warn!("Gloda database not found at {:?}", db_path);
            return Ok(Vec::new());
        }

        let mut docs = Vec::new();

        // We'd use rusqlite here; for now, stub with a simple query
        // In the full implementation:
        // let conn = rusqlite::Connection::open_with_flags(
        //     &db_path,
        //     rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        // )?;
        //
        // Query: SELECT m.id, m.subject, m.author, mt.content FROM messages m
        //   JOIN messagesText_content mt ON m.id = mt.id
        //   WHERE m.id > ? ORDER BY m.id LIMIT 1000

        // Placeholder — will be implemented with rusqlite in the daemon
        tracing::info!("Gloda: source available, would index from {:?}", db_path);

        Ok(docs)
    }
}
