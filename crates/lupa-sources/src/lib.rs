//! Lupa Sources — IndexerSource trait + core built-in implementations (fs, apps).
//!
//! Opt-in source plugins live in sibling crates: `lupa-source-thunderbird`
//! (gloda + mbox attachments), `lupa-source-maildir`.

pub mod apps;
pub mod exclude;
pub mod fs;
pub mod manifest;
pub mod mime_icons;
pub mod source;

pub use source::{
    IndexerSource, Mutation, MutationSink, PluginBuildContext, PluginFactory, PluginInstance,
    SourceContext, SourceEvent, SourceEventKind, WatchSpec,
};
