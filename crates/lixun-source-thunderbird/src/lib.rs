//! Thunderbird source plugin: gloda-SQLite messages + mbox attachments.
//!
//! Daemon loads this plugin only when the user config contains a
//! `[thunderbird]` section. ThunderbirdFactory parses the section and
//! produces one or two `IndexerSource` instances (gloda plus, optionally,
//! tb_attachments). Profile auto-discovery is internal to this crate —
//! neither the daemon nor any other crate needs to call find_profile.

pub mod attachments;
pub mod gloda;
pub mod mbox;

mod factory;

pub use attachments::ThunderbirdAttachmentsSource;
pub use factory::ThunderbirdFactory;
pub use gloda::GlodaSource;
