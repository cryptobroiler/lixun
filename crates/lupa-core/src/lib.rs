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
        encoding: String,
        suggested_filename: String,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_category_as_str() {
        assert_eq!(Category::App.as_str(), "app");
        assert_eq!(Category::File.as_str(), "file");
        assert_eq!(Category::Mail.as_str(), "mail");
        assert_eq!(Category::Attachment.as_str(), "attachment");
    }

    #[test]
    fn test_category_ranking_boost() {
        assert_eq!(Category::App.ranking_boost(), 1.3);
        assert_eq!(Category::File.ranking_boost(), 1.2);
        assert_eq!(Category::Mail.ranking_boost(), 1.0);
        assert_eq!(Category::Attachment.ranking_boost(), 0.9);
    }

    #[test]
    fn test_category_serde_roundtrip() {
        let cat = Category::App;
        let json = serde_json::to_string(&cat).unwrap();
        let decoded: Category = serde_json::from_str(&json).unwrap();
        assert_eq!(cat, decoded);
    }

    #[test]
    fn test_doc_id_equality() {
        let a = DocId("fs:/tmp/test.txt".to_string());
        let b = DocId("fs:/tmp/test.txt".to_string());
        let c = DocId("fs:/tmp/other.txt".to_string());
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn test_action_serde_roundtrip() {
        let actions = vec![
            Action::Launch {
                exec: "firefox".to_string(),
                terminal: false,
                working_dir: None,
            },
            Action::OpenFile {
                path: PathBuf::from("/tmp/test.txt"),
            },
            Action::ShowInFileManager {
                path: PathBuf::from("/tmp"),
            },
            Action::OpenMail {
                message_id: "12345".to_string(),
            },
            Action::OpenAttachment {
                mbox_path: PathBuf::from("/tmp/mail.mbox"),
                byte_offset: 100,
                length: 500,
                mime: "application/pdf".to_string(),
                encoding: "base64".to_string(),
                suggested_filename: "test.pdf".to_string(),
            },
        ];

        for action in actions {
            let json = serde_json::to_string(&action).unwrap();
            let decoded: Action = serde_json::from_str(&json).unwrap();
            match (&action, &decoded) {
                (Action::Launch { exec: e1, .. }, Action::Launch { exec: e2, .. }) => {
                    assert_eq!(e1, e2);
                }
                (Action::OpenFile { path: p1 }, Action::OpenFile { path: p2 }) => {
                    assert_eq!(p1, p2);
                }
                (
                    Action::ShowInFileManager { path: p1 },
                    Action::ShowInFileManager { path: p2 },
                ) => {
                    assert_eq!(p1, p2);
                }
                (Action::OpenMail { message_id: m1 }, Action::OpenMail { message_id: m2 }) => {
                    assert_eq!(m1, m2);
                }
                (
                    Action::OpenAttachment {
                        mbox_path: mb1,
                        byte_offset: bo1,
                        length: l1,
                        mime: mi1,
                        encoding: en1,
                        suggested_filename: sf1,
                    },
                    Action::OpenAttachment {
                        mbox_path: mb2,
                        byte_offset: bo2,
                        length: l2,
                        mime: mi2,
                        encoding: en2,
                        suggested_filename: sf2,
                    },
                ) => {
                    assert_eq!(mb1, mb2);
                    assert_eq!(bo1, bo2);
                    assert_eq!(l1, l2);
                    assert_eq!(mi1, mi2);
                    assert_eq!(en1, en2);
                    assert_eq!(sf1, sf2);
                }
                _ => panic!("Action variant mismatch"),
            }
        }
    }

    #[test]
    fn test_query_clone() {
        let q = Query {
            text: "hello world".to_string(),
            limit: 10,
        };
        let q2 = q.clone();
        assert_eq!(q.text, q2.text);
        assert_eq!(q.limit, q2.limit);
    }
}
