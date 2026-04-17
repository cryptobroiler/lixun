//! Gloda source — Thunderbird global-messages-db.sqlite.

use anyhow::Result;
use lupa_core::{Action, Category, DocId, Document};
use rusqlite::{Connection, OpenFlags};
use std::path::PathBuf;

pub struct GlodaSource {
    pub profile_path: PathBuf,
    pub last_key: u64,
}

impl GlodaSource {
    pub fn find_profile() -> Option<PathBuf> {
        let home = std::env::var("HOME").ok()?;
        let tb_path = PathBuf::from(&home).join(".thunderbird");
        if !tb_path.exists() {
            return None;
        }
        for entry in std::fs::read_dir(&tb_path).ok()? {
            if let Ok(entry) = entry {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.contains(".default") {
                    return Some(entry.path());
                }
            }
        }
        None
    }

    pub fn new(profile_path: PathBuf, last_key: u64) -> Self {
        Self {
            profile_path,
            last_key,
        }
    }

    fn open_db(&self) -> Result<Connection> {
        let db_path = self.profile_path.join("global-messages-db.sqlite");
        let conn = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        Ok(conn)
    }
}

impl crate::Source for GlodaSource {
    fn name(&self) -> &'static str {
        "gloda"
    }

    fn index_all(&self) -> Result<Vec<Document>> {
        let db_path = self.profile_path.join("global-messages-db.sqlite");
        if !db_path.exists() {
            tracing::warn!("Gloda DB not found at {:?}", db_path);
            return Ok(Vec::new());
        }

        let conn = self.open_db()?;
        let mut stmt = conn.prepare(
            "SELECT m.id, m.subject, m.author, m.message_key, mt.content
             FROM messages m
             LEFT JOIN messagesText_content mt ON m.id = mt.id
             WHERE m.id > ?
             ORDER BY m.id ASC
             LIMIT 10000",
        )?;

        let rows = stmt.query_map([self.last_key], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1).unwrap_or_default(),
                row.get::<_, String>(2).unwrap_or_default(),
                row.get::<_, String>(3).unwrap_or_default(),
                row.get::<_, String>(4).unwrap_or_default(),
            ))
        })?;

        let mut docs = Vec::new();
        for row in rows {
            let (id, subject, author, message_key, body) = row?;
            docs.push(Document {
                id: DocId(format!("mail:{}", id)),
                category: Category::Mail,
                title: subject.clone(),
                subtitle: author.clone(),
                body: if body.is_empty() { None } else { Some(body) },
                path: format!("thunderbird:{}", id),
                mtime: 0,
                size: 0,
                action: Action::OpenMail {
                    message_id: message_key,
                },
                extract_fail: false,
            });
        }

        tracing::info!("Gloda: indexed {} messages", docs.len());
        Ok(docs)
    }
}
