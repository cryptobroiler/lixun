//! ListView factory, result model updater, and cached hit store.

use std::cell::RefCell;

use gtk::prelude::*;
use lupa_core::{Category, Hit};

use crate::icons::resolve_icon;

pub(crate) const ICON_SIZE_NORMAL: i32 = 32;
pub(crate) const ICON_SIZE_TOP_HIT: i32 = 48;

pub(crate) fn add_css_class<W: gtk::prelude::WidgetExt>(widget: &W, class: &str) {
    widget.style_context().add_class(class);
}

fn category_kind_fallback(cat: &Category) -> &'static str {
    match cat {
        Category::App => "Application",
        Category::File => "File",
        Category::Mail => "Email",
        Category::Attachment => "Attachment",
    }
}

thread_local! {
    static CACHED_HITS: RefCell<Vec<Hit>> = const { RefCell::new(Vec::new()) };
}

pub(crate) fn cache_hits(hits: Vec<Hit>) {
    CACHED_HITS.with(|c| *c.borrow_mut() = hits);
}

pub(crate) fn with_cached_hits<R>(f: impl FnOnce(&[Hit]) -> R) -> R {
    CACHED_HITS.with(|c| f(&c.borrow()))
}

pub(crate) fn update_results(model: &gtk::StringList, hits: &[Hit]) {
    let n = model.n_items();
    for _ in 0..n {
        model.remove(0);
    }
    cache_hits(hits.to_vec());
    for hit in hits {
        model.append(&hit.id.0);
    }
}

pub(crate) fn create_list_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();

    factory.connect_setup(move |_, list_item| {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        add_css_class(&row, "lupa-hit");

        let icon = gtk::Image::new();
        icon.set_pixel_size(ICON_SIZE_NORMAL);
        icon.set_margin_start(4);
        row.append(&icon);

        let text_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text_box.set_hexpand(true);

        let title = gtk::Label::new(None);
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        add_css_class(&title, "lupa-title");
        text_box.append(&title);

        let subtitle = gtk::Label::new(None);
        subtitle.set_xalign(0.0);
        subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
        add_css_class(&subtitle, "lupa-subtitle");
        text_box.append(&subtitle);

        row.append(&text_box);

        let kind = gtk::Label::new(None);
        kind.set_xalign(1.0);
        add_css_class(&kind, "lupa-kind");
        row.append(&kind);

        let list_item = list_item
            .downcast_ref::<gtk::ListItem>()
            .expect("ListItem expected");
        list_item.set_child(Some(&row));
    });

    factory.connect_bind(move |_, list_item| {
        let list_item = list_item
            .downcast_ref::<gtk::ListItem>()
            .expect("ListItem expected");

        let position = list_item.position();
        let is_top_hit = position == 0;

        if let Some(str_obj) = list_item
            .item()
            .and_then(|i| i.downcast::<gtk::StringObject>().ok())
        {
            let doc_id = str_obj.string().to_string();
            with_cached_hits(|hits| {
                if let Some(hit) = hits.iter().find(|h| h.id.0 == doc_id) {
                    let row = list_item.child().and_downcast::<gtk::Box>().unwrap();

                    if is_top_hit {
                        row.add_css_class("lupa-top-hit");
                    } else {
                        row.remove_css_class("lupa-top-hit");
                    }

                    let icon = row.first_child().and_downcast::<gtk::Image>().unwrap();
                    let icon_size = if is_top_hit {
                        ICON_SIZE_TOP_HIT
                    } else {
                        ICON_SIZE_NORMAL
                    };
                    icon.set_pixel_size(icon_size);
                    if let Some(paintable) = resolve_icon(hit, icon_size) {
                        icon.set_paintable(Some(&paintable));
                    } else {
                        icon.set_icon_name(Some(match hit.category {
                            Category::App => "application-x-executable",
                            Category::File => "text-x-generic",
                            Category::Mail => "mail-message",
                            Category::Attachment => "mail-attachment",
                        }));
                    }

                    let text_box = icon.next_sibling().and_downcast::<gtk::Box>().unwrap();
                    let title = text_box.first_child().and_downcast::<gtk::Label>().unwrap();
                    title.set_text(&hit.title);

                    let subtitle = title.next_sibling().and_downcast::<gtk::Label>().unwrap();
                    subtitle.set_text(&hit.subtitle);

                    let kind = text_box
                        .next_sibling()
                        .and_downcast::<gtk::Label>()
                        .unwrap();
                    let kind_text = hit
                        .kind_label
                        .clone()
                        .unwrap_or_else(|| category_kind_fallback(&hit.category).to_string());
                    kind.set_text(&kind_text);
                }
            });
        }
    });

    factory
}
