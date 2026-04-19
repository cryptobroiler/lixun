//! Lupa Sources — data source trait + implementations.

pub mod apps;
pub mod exclude;
pub mod fs;
pub mod gloda;
pub mod manifest;
pub mod mbox;
pub mod mime_icons;
pub mod source;
pub mod thunderbird_attachments;

use anyhow::Result;
use lupa_core::Document;

pub use source::{
    IndexerSource, Mutation, MutationSink, SourceContext, SourceEvent, SourceEventKind, WatchSpec,
};

/// Legacy trait — will be removed after all built-ins migrate to `IndexerSource`.
pub trait Source {
    fn name(&self) -> &'static str;
    fn index_all(&self) -> Result<Vec<Document>>;
}
