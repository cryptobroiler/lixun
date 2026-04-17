//! Shared types for Lupa — no runtime dependencies.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ── Categories ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Category {
    App,
    File,
    Mail,
    Attachment,
}

impl Category {
    pub fn label(&self) -> &'static str {
        match self {
            Category::App => "APP",
            Category::File => "FILE",
            Category::Mail => "MAIL",
            Category::Attachment => "ATTACHMENT",
        }
    }

    pub fn default_ranking_boost(&self) -> f32 {
        match self {
            Category::App => 1.3,
            Category::File => 1.2,
            Category::Mail => 1.0,
            Category::Attachment => 0.9,
        }
    }
}

// ── Actions ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Action {
    /// Launch a desktop application
    Launch {
        exec: String,
        terminal: bool,
        working_dir: Option<String>,
    },
    /// Open a file with xdg-open
    OpenFile { path: PathBuf },
    /// Show a file in the file manager (via dbus FileManager1)
    ShowInFileManager { path: PathBuf },
    /// Open a mail in Thunderbird via mid: URI
    OpenMail { message_id: String },
    /// Extract an attachment to a temp file and open it
    OpenAttachment {
        mbox_path: PathBuf,
        byte_offset: u64,
        length: u64,
        mime: String,
        filename: String,
    },
    /// Open the parent mail for an attachment
    OpenParentMail { message_id: String },
    /// Copy to clipboard
    Copy { text: String },
}

// ── Hit ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hit {
    pub id: String,
    pub category: Category,
    pub title: String,
    pub subtitle: String,
    pub path: PathBuf,
    pub score: f32,
    #[serde(skip)]
    pub action: Action,
    /// Number of times the user has selected this hit
    pub click_count: u32,
    /// Whether content extraction failed for this document
    pub extract_fail: bool,
}

impl Default for Action {
    fn default() -> Self {
        Action::OpenFile { path: PathBuf::new() }
    }
}

// ── Query ───────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Query {
    pub text: String,
    pub limit: u32,
    pub categories: Vec<Category>,
}

impl Query {
    pub fn new(text: impl Into<String>, limit: u32) -> Self {
        Self {
            text: text.into(),
            limit,
            categories: vec![
                Category::App,
                Category::File,
                Category::Mail,
                Category::Attachment,
            ],
        }
    }
}

// ── Document (for indexing) ────────────────────────────────

#[derive(Debug, Clone)]
pub struct Document {
    pub id: String,
    pub category: Category,
    pub title: String,
    pub subtitle: String,
    pub body: String,       // extracted content
    pub path: PathBuf,
    pub mtime: u64,
    pub size: u64,
    pub action: Action,
    pub extract_fail: bool,
}

// ── Status ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub indexed_docs: u64,
    pub last_reindex: Option<DateTime<Utc>>,
    pub errors: u32,
}
