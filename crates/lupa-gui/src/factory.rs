//! ListView factory, result model updater, and cached hit store.

use std::cell::RefCell;

use gtk::prelude::*;
use lupa_core::{Category, Hit};

pub(crate) fn category_icon(cat: &Category) -> &'static str {
    match cat {
        Category::App => "application-x-executable",
        Category::File => "text-x-generic",
        Category::Mail => "mail-message",
        Category::Attachment => "mail-attachment",
    }
}

pub(crate) fn add_css_class<W: gtk::prelude::WidgetExt>(widget: &W, class: &str) {
    widget.style_context().add_class(class);
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
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        add_css_class(&row, "lupa-hit");

        let icon = gtk::Image::new();
        icon.set_icon_size(gtk::IconSize::Large);
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

        let badge = gtk::Label::new(None);
        badge.set_xalign(1.0);
        add_css_class(&badge, "lupa-badge");
        row.append(&badge);

        let list_item = list_item
            .downcast_ref::<gtk::ListItem>()
            .expect("ListItem expected");
        list_item.set_child(Some(&row));
    });

    factory.connect_bind(move |_, list_item| {
        let list_item = list_item
            .downcast_ref::<gtk::ListItem>()
            .expect("ListItem expected");

        if let Some(str_obj) = list_item
            .item()
            .and_then(|i| i.downcast::<gtk::StringObject>().ok())
        {
            let doc_id = str_obj.string().to_string();
            with_cached_hits(|hits| {
                if let Some(hit) = hits.iter().find(|h| h.id.0 == doc_id) {
                    let row = list_item.child().and_downcast::<gtk::Box>().unwrap();
                    let icon = row.first_child().and_downcast::<gtk::Image>().unwrap();
                    icon.set_icon_name(Some(category_icon(&hit.category)));

                    let text_box = icon.next_sibling().and_downcast::<gtk::Box>().unwrap();
                    let title = text_box.first_child().and_downcast::<gtk::Label>().unwrap();
                    title.set_text(&hit.title);

                    let subtitle = title.next_sibling().and_downcast::<gtk::Label>().unwrap();
                    subtitle.set_text(&hit.subtitle);

                    let badge = text_box
                        .next_sibling()
                        .and_downcast::<gtk::Label>()
                        .unwrap();
                    badge.set_text(hit.category.as_str());
                }
            });
        }
    });

    factory
}
