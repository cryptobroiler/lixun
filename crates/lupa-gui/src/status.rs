//! Bottom status bar: loading spinner, empty-state with web-search fallback,
//! inline calculator result. Exposed as a self-contained widget that `window.rs`
//! appends and drives.

use gtk::prelude::*;
use lupa_core::Calculation;

use crate::factory::add_css_class;

pub(crate) struct StatusBar {
    revealer: gtk::Revealer,
    content: gtk::Box,
}

impl StatusBar {
    pub(crate) fn new() -> Self {
        let revealer = gtk::Revealer::new();
        revealer.set_transition_type(gtk::RevealerTransitionType::Crossfade);
        revealer.set_transition_duration(200);
        revealer.set_reveal_child(false);

        let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        content.set_margin_top(6);
        content.set_margin_bottom(6);
        content.set_margin_start(8);
        content.set_margin_end(8);
        add_css_class(&content, "lupa-status");

        revealer.set_child(Some(&content));

        Self { revealer, content }
    }

    pub(crate) fn widget(&self) -> &gtk::Revealer {
        &self.revealer
    }

    fn clear(&self) {
        while let Some(child) = self.content.first_child() {
            self.content.remove(&child);
        }
    }

    pub(crate) fn show_loading(&self) {
        self.clear();
        let spinner = gtk::Spinner::new();
        spinner.start();
        let label = gtk::Label::new(Some("Searching\u{2026}"));
        add_css_class(&label, "lupa-status-label");
        self.content.append(&spinner);
        self.content.append(&label);
        self.revealer.set_reveal_child(true);
    }

    pub(crate) fn show_empty(&self, query: &str) {
        self.clear();
        let text = if query.is_empty() {
            "No results".to_string()
        } else {
            format!("No results for \u{201C}{}\u{201D}", query)
        };
        let label = gtk::Label::new(Some(&text));
        add_css_class(&label, "lupa-status-label");
        label.set_hexpand(true);
        label.set_halign(gtk::Align::Start);
        self.content.append(&label);

        if !query.is_empty() {
            let button = gtk::Button::with_label("Search the web");
            add_css_class(&button, "lupa-status-action");
            let q = query.to_string();
            button.connect_clicked(move |_| {
                let encoded = urlencode(&q);
                let url = format!("https://duckduckgo.com/?q={}", encoded);
                if let Err(e) = opener::open(&url) {
                    tracing::error!("Failed to open web search URL: {}", e);
                }
            });
            self.content.append(&button);
        }
        self.revealer.set_reveal_child(true);
    }

    pub(crate) fn show_calculation(&self, calc: &Calculation) {
        self.clear();
        let text = format!("{} = {}", calc.expr, calc.result);
        let label = gtk::Label::new(Some(&text));
        add_css_class(&label, "lupa-status-calc");
        label.set_hexpand(true);
        label.set_halign(gtk::Align::Start);
        self.content.append(&label);

        let button = gtk::Button::with_label("Copy");
        add_css_class(&button, "lupa-status-action");
        let result = calc.result.clone();
        button.connect_clicked(move |_| {
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&result);
            }
        });
        self.content.append(&button);

        self.revealer.set_reveal_child(true);
    }

    pub(crate) fn hide(&self) {
        self.revealer.set_reveal_child(false);
    }
}

fn urlencode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::urlencode;

    #[test]
    fn test_urlencode_plain() {
        assert_eq!(urlencode("hello"), "hello");
    }

    #[test]
    fn test_urlencode_spaces() {
        assert_eq!(urlencode("hello world"), "hello+world");
    }

    #[test]
    fn test_urlencode_special() {
        assert_eq!(urlencode("a&b=c"), "a%26b%3Dc");
    }

    #[test]
    fn test_urlencode_unicode() {
        assert_eq!(urlencode("café"), "caf%C3%A9");
    }
}
