pub mod config;
pub mod hotkeys;

pub use lupa_indexer::index_service;
pub use lupa_indexer::indexer;

#[allow(unused_imports)]
use lupa_plugin_bundle as _;
