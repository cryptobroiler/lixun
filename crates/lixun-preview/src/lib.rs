//! Preview plugin trait + shared helpers for `lixun-preview`.
//!
//! Each format plugin (text, image, pdf, code, email, office, av)
//! lives in its own crate and registers itself via
//! `inventory::submit!(PreviewPluginEntry { .. })`. The preview
//! binary links them via `lixun-preview-bundle` and discovers them
//! at runtime by iterating `inventory::iter::<PreviewPluginEntry>`.
//!
//! The daemon does not depend on this crate and never names a
//! concrete plugin.

use lixun_core::Hit;

/// Config shipped to a plugin's `build()`, sourced from the
/// `[preview]` section of `~/.config/lixun/config.toml`.
///
/// The `section` is the raw `[preview.<plugin-id>]` TOML table if
/// the user wrote one; plugins parse it with their own schema.
/// `max_file_size_mb` is hoisted from the top-level
/// `preview.max_file_size_mb` for convenience — plugins that render
/// large binaries (images, PDFs, video) should honour it.
pub struct PreviewPluginCfg<'a> {
    pub section: Option<&'a toml::Value>,
    pub max_file_size_mb: u64,
}

impl<'a> PreviewPluginCfg<'a> {
    pub fn none() -> Self {
        Self {
            section: None,
            max_file_size_mb: 200,
        }
    }
}

/// A format plugin that renders a preview widget for a hit.
///
/// Implementations must:
/// - Return a stable, globally-unique `id()` used as plugin key and
///   as the `[preview.<id>]` config subtable name.
/// - Compute `match_score()` cheaply (no filesystem access, no
///   MIME sniffing of file contents); return `0` to decline.
/// - Keep `build()` effectively synchronous — under ~50 ms wall
///   time. Heavier work (PDF rasterisation, office conversion)
///   must return a placeholder widget immediately and offload
///   to a worker thread via `gio::spawn_blocking` +
///   `glib::MainContext::spawn_local`.
pub trait PreviewPlugin: Send + Sync + 'static {
    fn id(&self) -> &'static str;

    fn match_score(&self, hit: &Hit) -> u32;

    fn build(&self, hit: &Hit, cfg: &PreviewPluginCfg<'_>) -> anyhow::Result<gtk::Widget>;
}

pub struct PreviewPluginEntry {
    pub factory: fn() -> Box<dyn PreviewPlugin>,
}

inventory::collect!(PreviewPluginEntry);

/// Pick the best plugin for `hit`, or `None` if no plugin scores
/// above zero. Ties on score are broken by `id()` alphabetically
/// so the choice is deterministic across builds (inventory's own
/// iteration order is not guaranteed).
pub fn select_plugin(hit: &Hit) -> Option<Box<dyn PreviewPlugin>> {
    inventory::iter::<PreviewPluginEntry>
        .into_iter()
        .map(|entry| (entry.factory)())
        .filter_map(|p| {
            let score = p.match_score(hit);
            if score == 0 {
                None
            } else {
                Some((score, p))
            }
        })
        .max_by(|(s1, p1), (s2, p2)| s1.cmp(s2).then_with(|| p1.id().cmp(p2.id())))
        .map(|(_, p)| p)
}

/// Install the user's `~/.config/lixun/style.css` at
/// `APPLICATION + 1` priority on top of the built-in theme, if the
/// file exists. Mirrors the launcher's CSS loading
/// (`lixun-gui::window::build_window`) so both windows pick up the
/// same user theme from one source of truth.
///
/// No-op if the file is missing or the config dir cannot be
/// determined. Never panics; logs on failure.
pub fn install_user_css(display: &gtk::gdk::Display) {
    let Some(path) = dirs::config_dir().map(|d| d.join("lixun/style.css")) else {
        return;
    };
    if !path.exists() {
        return;
    }
    let provider = gtk::CssProvider::new();
    provider.load_from_path(&path);
    gtk::style_context_add_provider_for_display(
        display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
    );
    tracing::info!("preview: loaded external style.css from {:?}", path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use lixun_core::{Action, Category, DocId};
    use std::path::PathBuf;

    fn fake_hit() -> Hit {
        Hit {
            id: DocId("fs:/tmp/demo.txt".into()),
            category: Category::File,
            title: "demo.txt".into(),
            subtitle: "/tmp".into(),
            icon_name: None,
            kind_label: None,
            score: 1.0,
            action: Action::OpenFile {
                path: PathBuf::from("/tmp/demo.txt"),
            },
            extract_fail: false,
        }
    }

    struct ScorePlugin {
        id: &'static str,
        score: u32,
    }

    impl PreviewPlugin for ScorePlugin {
        fn id(&self) -> &'static str {
            self.id
        }
        fn match_score(&self, _: &Hit) -> u32 {
            self.score
        }
        fn build(&self, _: &Hit, _: &PreviewPluginCfg<'_>) -> anyhow::Result<gtk::Widget> {
            unreachable!("test plugin")
        }
    }

    fn choose(plugins: Vec<Box<dyn PreviewPlugin>>, hit: &Hit) -> Option<&'static str> {
        plugins
            .into_iter()
            .filter_map(|p| {
                let score = p.match_score(hit);
                if score == 0 {
                    None
                } else {
                    Some((score, p))
                }
            })
            .max_by(|(s1, p1), (s2, p2)| s1.cmp(s2).then_with(|| p1.id().cmp(p2.id())))
            .map(|(_, p)| {
                let id: &'static str = p.id();
                id
            })
    }

    #[test]
    fn highest_score_wins() {
        let hit = fake_hit();
        let winner = choose(
            vec![
                Box::new(ScorePlugin { id: "a", score: 10 }),
                Box::new(ScorePlugin { id: "b", score: 50 }),
                Box::new(ScorePlugin { id: "c", score: 30 }),
            ],
            &hit,
        );
        assert_eq!(winner, Some("b"));
    }

    #[test]
    fn ties_broken_by_id_alphabetical() {
        let hit = fake_hit();
        let winner = choose(
            vec![
                Box::new(ScorePlugin {
                    id: "zulu",
                    score: 50,
                }),
                Box::new(ScorePlugin {
                    id: "alpha",
                    score: 50,
                }),
                Box::new(ScorePlugin {
                    id: "mike",
                    score: 50,
                }),
            ],
            &hit,
        );
        assert_eq!(
            winner,
            Some("zulu"),
            "expected the lexicographically largest id on a score tie"
        );
    }

    #[test]
    fn zero_score_filtered() {
        let hit = fake_hit();
        let winner = choose(
            vec![
                Box::new(ScorePlugin {
                    id: "zero",
                    score: 0,
                }),
                Box::new(ScorePlugin {
                    id: "one",
                    score: 1,
                }),
            ],
            &hit,
        );
        assert_eq!(winner, Some("one"));
    }

    #[test]
    fn all_zero_returns_none() {
        let hit = fake_hit();
        let winner = choose(
            vec![
                Box::new(ScorePlugin { id: "a", score: 0 }),
                Box::new(ScorePlugin { id: "b", score: 0 }),
            ],
            &hit,
        );
        assert_eq!(winner, None);
    }

    #[test]
    fn empty_registry_returns_none() {
        let hit = fake_hit();
        let winner = choose(vec![], &hit);
        assert_eq!(winner, None);
    }

    #[test]
    fn preview_plugin_cfg_none_has_sensible_defaults() {
        let cfg = PreviewPluginCfg::none();
        assert!(cfg.section.is_none());
        assert_eq!(cfg.max_file_size_mb, 200);
    }
}
