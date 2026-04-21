//! Linker anchor for preview format plugins.
//!
//! Each tier-2 plugin task (G2.9–G2.15) adds three atomic edits
//! here and lands them together:
//!
//! 1. an optional path-dep on the plugin crate in `Cargo.toml`,
//! 2. a `<id> = ["dep:lixun-preview-<id>"]` entry under
//!    `[features]`,
//! 3. a `#[cfg(feature = "<id>")] use lixun_preview_<id> as _;`
//!    here so the plugin's `inventory::submit!` reaches the
//!    preview binary's link-time registry.
//!
//! In G2.8 the feature table is intentionally empty — Cargo cannot
//! declare `optional = true` dependencies on crates that do not
//! exist yet. This forces every plugin to land atomically as a
//! single commit covering crate + feature + linker use-stmt.
//!
//! The `use lixun_preview as _;` below keeps the trait crate
//! linked even when no plugins are enabled, so `select_plugin`
//! resolves cleanly against an empty registry.

use lixun_preview as _;

#[cfg(test)]
mod tests {
    #[test]
    fn bundle_links_the_trait_crate() {
        use lixun_core::{Action, Category, DocId, Hit};
        use std::path::PathBuf;

        let hit = Hit {
            id: DocId("fs:/tmp/demo.txt".into()),
            category: Category::File,
            title: "demo.txt".into(),
            subtitle: String::new(),
            icon_name: None,
            kind_label: None,
            score: 0.0,
            action: Action::OpenFile {
                path: PathBuf::from("/tmp/demo.txt"),
            },
            extract_fail: false,
        };
        assert!(lixun_preview::select_plugin(&hit).is_none());
    }
}
