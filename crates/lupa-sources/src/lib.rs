//! Lupa Sources — data source trait + implementations.

pub mod apps;
pub mod fs;
pub mod gloda;
pub mod mbox;
pub mod manifest;
pub mod thunderbird_attachments;

use anyhow::Result;
use lupa_core::Document;

/// A data source that yields documents.
pub trait Source {
    /// Name of this source (for logging).
    fn name(&self) -> &'static str;

    /// Index all documents from this source.
    fn index_all(&self) -> Result<Vec<Document>>;
}
