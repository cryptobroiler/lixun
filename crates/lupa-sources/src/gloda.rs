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

/// Query messages from a Gloda connection. Extracted for testability against
/// an in-memory rusqlite `Connection` seeded with the real Gloda schema.
pub fn query_messages(conn: &Connection, last_key: u64) -> rusqlite::Result<Vec<Document>> {
    let mut stmt = conn.prepare(
        "SELECT m.id, m.messageKey, m.headerMessageID, \
                mt.c1subject, mt.c3author, mt.c0body \
         FROM messages m \
         LEFT JOIN messagesText_content mt ON m.id = mt.docid \
         WHERE m.id > ? AND m.deleted = 0 \
         ORDER BY m.id ASC \
         LIMIT 10000",
    )?;

    let rows = stmt.query_map([last_key as i64], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Option<i64>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
        ))
    })?;

    let mut docs = Vec::new();
    for row in rows {
        let (id, message_key, header_message_id, subject, author, body_opt) = row?;

        let header_id = header_message_id
            .filter(|s| !s.is_empty())
            .map(strip_angle_brackets);
        let message_id = header_id
            .or_else(|| message_key.map(|k| k.to_string()))
            .unwrap_or_default();

        docs.push(Document {
            id: DocId(format!("mail:{}", id)),
            category: Category::Mail,
            title: subject.unwrap_or_else(|| "(no subject)".into()),
            subtitle: author.unwrap_or_default(),
            body: body_opt.filter(|s| !s.is_empty()),
            path: format!("thunderbird:{}", id),
            mtime: 0,
            size: 0,
            action: Action::OpenMail { message_id },
            extract_fail: false,
        });
    }

    Ok(docs)
}

/// Strip a single matching pair of angle brackets from an RFC-5322 Message-ID.
/// Returns the input unchanged if it is not a balanced `<...>` pair.
fn strip_angle_brackets(s: String) -> String {
    if s.len() >= 2 && s.starts_with('<') && s.ends_with('>') {
        s[1..s.len() - 1].to_string()
    } else {
        s
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
        let docs = query_messages(&conn, self.last_key)?;
        tracing::info!("Gloda: indexed {} messages", docs.len());
        Ok(docs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup_schema(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE messages (
                id INTEGER PRIMARY KEY,
                folderID INTEGER,
                messageKey INTEGER,
                conversationID INTEGER NOT NULL DEFAULT 0,
                date INTEGER,
                headerMessageID TEXT,
                deleted INTEGER NOT NULL DEFAULT 0,
                jsonAttributes TEXT,
                notability INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE 'messagesText_content' (
                docid INTEGER PRIMARY KEY,
                'c0body', 'c1subject', 'c2attachmentNames', 'c3author', 'c4recipients'
            );",
        )
        .unwrap();
    }

    #[test]
    fn test_gloda_query_against_real_schema() {
        let conn = Connection::open_in_memory().unwrap();
        setup_schema(&conn);
        conn.execute(
            "INSERT INTO messages (id, messageKey, headerMessageID, deleted, conversationID) \
             VALUES (1, 42, '<abc@example.com>', 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messagesText_content (docid, c0body, c1subject, c3author) \
             VALUES (1, 'Hello body', 'Hello', 'alice@test')",
            [],
        )
        .unwrap();

        let docs = query_messages(&conn, 0).unwrap();
        assert_eq!(docs.len(), 1);
        let d = &docs[0];
        assert_eq!(d.title, "Hello");
        assert_eq!(d.subtitle, "alice@test");
        assert!(d.body.is_some());
        assert!(d.body.as_ref().unwrap().contains("Hello body"));
        match &d.action {
            Action::OpenMail { message_id } => assert_eq!(message_id, "abc@example.com"),
            _ => panic!("expected OpenMail action"),
        }
    }

    #[test]
    fn test_gloda_deleted_filtered() {
        let conn = Connection::open_in_memory().unwrap();
        setup_schema(&conn);
        conn.execute(
            "INSERT INTO messages (id, messageKey, headerMessageID, deleted, conversationID) \
             VALUES (1, 1, '<one@x>', 0, 0), (2, 2, '<two@x>', 1, 0)",
            [],
        )
        .unwrap();

        let docs = query_messages(&conn, 0).unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].id.0, "mail:1");
    }

    #[test]
    fn test_gloda_missing_fts_row() {
        let conn = Connection::open_in_memory().unwrap();
        setup_schema(&conn);
        conn.execute(
            "INSERT INTO messages (id, messageKey, headerMessageID, deleted, conversationID) \
             VALUES (1, 1, '<no-fts@x>', 0, 0)",
            [],
        )
        .unwrap();

        let docs = query_messages(&conn, 0).unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].title, "(no subject)");
        assert!(docs[0].body.is_none());
    }

    #[test]
    fn test_gloda_fallback_to_message_key() {
        let conn = Connection::open_in_memory().unwrap();
        setup_schema(&conn);
        conn.execute(
            "INSERT INTO messages (id, messageKey, headerMessageID, deleted, conversationID) \
             VALUES (1, 42, NULL, 0, 0)",
            [],
        )
        .unwrap();

        let docs = query_messages(&conn, 0).unwrap();
        assert_eq!(docs.len(), 1);
        match &docs[0].action {
            Action::OpenMail { message_id } => assert_eq!(message_id, "42"),
            _ => panic!("expected OpenMail action"),
        }
    }

    #[test]
    fn test_strip_angle_brackets() {
        assert_eq!(strip_angle_brackets("<a>".into()), "a");
        assert_eq!(strip_angle_brackets("<a@b.com>".into()), "a@b.com");
        assert_eq!(strip_angle_brackets("noangles".into()), "noangles");
        assert_eq!(strip_angle_brackets("<unbalanced".into()), "<unbalanced");
        assert_eq!(strip_angle_brackets("".into()), "");
    }
}
