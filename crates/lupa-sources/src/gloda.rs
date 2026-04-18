//! Gloda source — Thunderbird global-messages-db.sqlite.

use anyhow::Result;
use lupa_core::{Action, Category, DocId, Document};
use rusqlite::{Connection, OpenFlags};
use std::path::PathBuf;
use std::time::Duration;

/// Retry a fallible operation with bounded backoff for "busy" errors.
/// Generic over error type and classifier for testability.
fn with_busy_retry_generic<T, E, F, C>(backoffs: &[Duration], classify: C, mut f: F) -> Result<T, E>
where
    F: FnMut() -> Result<T, E>,
    C: Fn(&E) -> bool,
{
    let mut last_err = None;
    for i in 0..=backoffs.len() {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) => {
                if !classify(&e) {
                    return Err(e);
                }
                last_err = Some(e);
                if i < backoffs.len() {
                    std::thread::sleep(backoffs[i]);
                }
            }
        }
    }
    Err(last_err.unwrap())
}

/// Classify rusqlite errors as "busy" (DATABASE_BUSY or DATABASE_LOCKED).
fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(ffi, _)
        if ffi.code == rusqlite::ErrorCode::DatabaseBusy
        || ffi.code == rusqlite::ErrorCode::DatabaseLocked)
}

/// Concrete retry helper for rusqlite operations with default backoffs.
fn with_busy_retry<T, F>(mut f: F) -> rusqlite::Result<T>
where
    F: FnMut() -> rusqlite::Result<T>,
{
    with_busy_retry_generic(
        &[
            Duration::from_millis(100),
            Duration::from_millis(500),
            Duration::from_millis(2000),
        ],
        is_busy,
        &mut f,
    )
}

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
        for entry in std::fs::read_dir(&tb_path).ok()?.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.contains(".default") {
                return Some(entry.path());
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

    fn open_db(&self) -> rusqlite::Result<Connection> {
        let db_path = self.profile_path.join("global-messages-db.sqlite");
        with_busy_retry(|| {
            Connection::open_with_flags(
                &db_path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
        })
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
            icon_name: Some("mail-message".into()),
            kind_label: Some("Email".into()),
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

        let result: rusqlite::Result<Vec<Document>> = with_busy_retry(|| {
            let conn = self.open_db()?;
            let docs = query_messages(&conn, self.last_key)?;
            Ok(docs)
        });

        match result {
            Ok(docs) => {
                tracing::info!("Gloda: indexed {} messages", docs.len());
                Ok(docs)
            }
            Err(e) => {
                if is_busy(&e) {
                    tracing::warn!("gloda busy after retries; deferring to next poll cycle");
                    Ok(Vec::new())
                } else {
                    Err(e.into())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

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

    #[test]
    fn retries_on_busy_then_succeeds() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let result: Result<i32, i32> = with_busy_retry_generic(
            &[Duration::ZERO, Duration::ZERO, Duration::ZERO],
            |e: &i32| *e == 5,
            move || {
                let n = c.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err(5)
                } else {
                    Ok(42)
                }
            },
        );
        assert_eq!(result.unwrap(), 42);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn non_busy_error_not_retried() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let result: Result<i32, i32> = with_busy_retry_generic(
            &[Duration::ZERO, Duration::ZERO, Duration::ZERO],
            |e: &i32| *e == 5,
            move || {
                let _ = c.fetch_add(1, Ordering::SeqCst);
                Err(99)
            },
        );
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), 99);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn all_retries_exhausted() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let result: Result<i32, i32> = with_busy_retry_generic(
            &[Duration::ZERO, Duration::ZERO, Duration::ZERO],
            |e: &i32| *e == 5,
            move || {
                let _ = c.fetch_add(1, Ordering::SeqCst);
                Err(5)
            },
        );
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), 5);
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }
}
