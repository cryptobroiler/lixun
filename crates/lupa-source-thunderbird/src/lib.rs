//! Thunderbird source plugin: gloda-SQLite messages + mbox attachments.
//!
//! Both sources share Thunderbird profile auto-discovery via `find_profile`
//! and the mbox parser. They register at daemon startup as
//! `builtin:gloda` and `builtin:tb_attachments` when a profile is found.

pub mod attachments;
pub mod gloda;
pub mod mbox;

pub use attachments::ThunderbirdAttachmentsSource;
pub use gloda::GlodaSource;

pub fn find_profile() -> Option<std::path::PathBuf> {
    GlodaSource::find_profile()
}
