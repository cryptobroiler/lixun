//! Lupa Sources — IndexerSource trait + built-in implementations.

pub mod apps;
pub mod exclude;
pub mod fs;
pub mod gloda;
pub mod manifest;
pub mod mbox;
pub mod mime_icons;
pub mod source;
pub mod thunderbird_attachments;

pub use source::{
    IndexerSource, Mutation, MutationSink, SourceContext, SourceEvent, SourceEventKind, WatchSpec,
};
