//! Lupa Core — shared types with no runtime dependencies.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Categories of searchable items.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Category {
    App,
    File,
    Mail,
    Attachment,
}

impl Category {
    pub fn as_str(&self) -> &'static str {
        match self {
            Category::App => "app",
            Category::File => "file",
            Category::Mail => "mail",
            Category::Attachment => "attachment",
        }
    }

    pub fn ranking_boost(&self) -> f32 {
        match self {
            Category::App => 1.3,
            Category::File => 1.2,
            Category::Mail => 1.0,
            Category::Attachment => 0.9,
        }
    }
}

/// Stable document ID.
/// Format: `fs:<abspath>`, `app:<desktop-id>`, `mail:<gloda-id>`, `att:<mail-id>#<n>`
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DocId(pub String);

/// Action to perform on a hit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Action {
    /// Launch an application.
    Launch {
        exec: String,
        terminal: bool,
        working_dir: Option<PathBuf>,
    },
    /// Open a file with the default handler.
    OpenFile { path: PathBuf },
    /// Show file in file manager.
    ShowInFileManager { path: PathBuf },
    /// Open a mail message in Thunderbird.
    OpenMail { message_id: String },
    /// Extract an attachment to a temp file and open it.
    OpenAttachment {
        mbox_path: PathBuf,
        byte_offset: u64,
        length: u64,
        mime: String,
    },
    /// Open the parent mail for an attachment.
    OpenParentMail { message_id: String },
}

/// A search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hit {
    pub id: DocId,
    pub category: Category,
    pub title: String,
    pub subtitle: String,
    pub score: f32,
    pub action: Action,
    pub extract_fail: bool,
}

/// A document to be indexed.
#[derive(Debug, Clone)]
pub struct Document {
    pub id: DocId,
    pub category: Category,
    pub title: String,
    pub subtitle: String,
    pub body: Option<String>,
    pub path: String,
    pub mtime: i64,
    pub size: u64,
    pub action: Action,
    pub extract_fail: bool,
}

/// Search query.
#[derive(Debug, Clone)]
pub struct Query {
    pub text: String,
    pub limit: u32,
}
