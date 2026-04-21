//! Stub plugin for G2.8 QA. Matches every hit with the lowest
//! possible winning score (1) so any real plugin beats it as soon
//! as tier-2 plugins land. Removed by flipping the bin crate's
//! `default` feature list when G2.9 (text) is in.

use gtk::prelude::*;
use lixun_core::Hit;
use lixun_preview::{PreviewPlugin, PreviewPluginCfg, PreviewPluginEntry};

pub struct StubPlugin;

impl PreviewPlugin for StubPlugin {
    fn id(&self) -> &'static str {
        "stub"
    }

    fn match_score(&self, _hit: &Hit) -> u32 {
        1
    }

    fn build(&self, hit: &Hit, _cfg: &PreviewPluginCfg<'_>) -> anyhow::Result<gtk::Widget> {
        let label = gtk::Label::new(Some(&format!(
            "Preview (stub)\n{}\n{}",
            hit.title, hit.subtitle
        )));
        label.set_wrap(true);
        label.set_xalign(0.0);
        label.set_yalign(0.0);
        label.set_margin_top(24);
        label.set_margin_bottom(24);
        label.set_margin_start(24);
        label.set_margin_end(24);
        Ok(label.upcast())
    }
}

inventory::submit! {
    PreviewPluginEntry {
        factory: || Box::new(StubPlugin),
    }
}
