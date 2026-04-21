//! ListView factory, result model updater, and cached hit store.

use std::cell::RefCell;

use gtk::gdk;
use gtk::gio;
use gtk::prelude::*;
use lixun_core::{Action, Category, Hit};

use crate::actions::{copy_to_clipboard, execute_action, execute_secondary_action};
use crate::icons::resolve_icon;
use crate::ipc::send_preview_request;
use crate::ipc::send_record_click;

pub(crate) const ICON_SIZE_NORMAL: i32 = 32;
pub(crate) const ICON_SIZE_TOP_HIT: i32 = 48;

pub(crate) fn add_css_class<W: gtk::prelude::WidgetExt>(widget: &W, class: &str) {
    widget.add_css_class(class);
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

pub(crate) fn clear_cached_hits() {
    CACHED_HITS.with(|c| c.borrow_mut().clear());
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

pub(crate) fn synthetic_history_hits(queries: &[String]) -> Vec<Hit> {
    use lixun_core::{Action, DocId};
    queries
        .iter()
        .enumerate()
        .map(|(i, q)| Hit {
            id: DocId(format!("history:{i}:{q}")),
            category: Category::File,
            title: q.clone(),
            subtitle: "Recent search".to_string(),
            icon_name: Some("document-open-recent".to_string()),
            kind_label: Some("Recent".to_string()),
            score: 0.0,
            action: Action::ReplaceQuery { q: q.clone() },
            extract_fail: false,
        })
        .collect()
}

fn build_menu_for(category: &Category) -> gio::Menu {
    let menu = gio::Menu::new();
    match category {
        Category::App => {
            menu.append(Some("Launch"), Some("row.open"));
            menu.append(Some("Copy name"), Some("row.copy"));
        }
        Category::File => {
            menu.append(Some("Open"), Some("row.open"));
            menu.append(Some("Reveal in File Manager"), Some("row.reveal"));
            menu.append(Some("Copy path"), Some("row.copy"));
            menu.append(Some("Quick Look"), Some("row.quicklook"));
            menu.append(Some("Get Info"), Some("row.info"));
        }
        Category::Mail => {
            menu.append(Some("Open in Thunderbird"), Some("row.open"));
            menu.append(Some("Copy subject"), Some("row.copy"));
        }
        Category::Attachment => {
            menu.append(Some("Open"), Some("row.open"));
            menu.append(Some("Quick Look"), Some("row.quicklook"));
            menu.append(Some("Copy filename"), Some("row.copy"));
            menu.append(Some("Open parent mail"), Some("row.reveal"));
        }
    }
    menu
}

fn install_action_group(row: &gtk::Box, hit: &Hit) {
    let group = gio::SimpleActionGroup::new();

    let open_hit = hit.clone();
    let open = gio::SimpleAction::new("open", None);
    open.connect_activate(move |_, _| {
        send_record_click(&open_hit.id.0);
        if let Err(e) = execute_action(&open_hit) {
            tracing::error!("Action failed: {}", e);
        }
    });
    group.add_action(&open);

    let reveal_hit = hit.clone();
    let reveal = gio::SimpleAction::new("reveal", None);
    reveal.connect_activate(move |_, _| {
        if let Err(e) = execute_secondary_action(&reveal_hit) {
            tracing::error!("Reveal failed: {}", e);
        }
    });
    group.add_action(&reveal);

    let copy_hit = hit.clone();
    let copy = gio::SimpleAction::new("copy", None);
    copy.connect_activate(move |_, _| {
        copy_to_clipboard(&copy_hit);
    });
    group.add_action(&copy);

    let quick_hit = hit.clone();
    let quick = gio::SimpleAction::new("quicklook", None);
    quick.connect_activate(move |_, _| {
        send_preview_request(&quick_hit);
    });
    group.add_action(&quick);

    let info_hit = hit.clone();
    let info_row = row.clone();
    let info = gio::SimpleAction::new("info", None);
    info.connect_activate(move |_, _| {
        show_get_info_popover(&info_row, &info_hit);
    });
    group.add_action(&info);

    row.insert_action_group("row", Some(&group));
}

fn show_get_info_popover(anchor: &gtk::Box, hit: &Hit) {
    let popover = gtk::Popover::new();
    popover.set_parent(anchor);
    popover.set_has_arrow(true);

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 4);
    vbox.set_margin_top(8);
    vbox.set_margin_bottom(8);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    let title = gtk::Label::new(Some(&hit.title));
    title.set_xalign(0.0);
    add_css_class(&title, "lixun-title");
    vbox.append(&title);

    let path_label = gtk::Label::new(Some(&hit.subtitle));
    path_label.set_xalign(0.0);
    path_label.set_selectable(true);
    path_label.set_wrap(true);
    add_css_class(&path_label, "lixun-subtitle");
    vbox.append(&path_label);

    if let Some(kind) = hit.kind_label.as_deref() {
        let kind_label = gtk::Label::new(Some(&format!("Kind: {}", kind)));
        kind_label.set_xalign(0.0);
        add_css_class(&kind_label, "lixun-subtitle");
        vbox.append(&kind_label);
    }

    popover.set_child(Some(&vbox));
    popover.popup();
}

fn hit_file_path(hit: &Hit) -> Option<std::path::PathBuf> {
    match &hit.action {
        Action::OpenFile { path } | Action::ShowInFileManager { path } => Some(path.clone()),
        _ => None,
    }
}

fn clear_row_controllers(row: &gtk::Box) {
    let controllers = row.observe_controllers();
    let n = controllers.n_items();
    let mut to_remove: Vec<gtk::EventController> = Vec::new();
    for i in 0..n {
        if let Some(ctrl) = controllers.item(i).and_downcast::<gtk::EventController>() {
            to_remove.push(ctrl);
        }
    }
    for c in to_remove {
        row.remove_controller(&c);
    }
}

fn install_right_click_popover(row: &gtk::Box, category: &Category) {
    let menu = build_menu_for(category);
    let popover = gtk::PopoverMenu::from_model(Some(&menu));
    popover.set_parent(row);
    popover.set_has_arrow(false);

    let gesture = gtk::GestureClick::new();
    gesture.set_button(gdk::BUTTON_SECONDARY);
    let popover_for_gesture = popover.clone();
    gesture.connect_pressed(move |_g, _n_press, x, y| {
        let rect = gdk::Rectangle::new(x as i32, y as i32, 1, 1);
        popover_for_gesture.set_pointing_to(Some(&rect));
        popover_for_gesture.popup();
    });
    row.add_controller(gesture);
}

fn install_double_click_open(row: &gtk::Box, hit: &Hit) {
    let gesture = gtk::GestureClick::new();
    gesture.set_button(gdk::BUTTON_PRIMARY);
    let hit = hit.clone();
    gesture.connect_pressed(move |_g, n_press, _x, _y| {
        if n_press != 2 {
            return;
        }
        send_record_click(&hit.id.0);
        if let Err(e) = execute_action(&hit) {
            tracing::error!("double-click open failed: {}", e);
            return;
        }
        if let Some(app) = gio::Application::default() {
            app.activate_action("close-launcher", None);
        }
    });
    row.add_controller(gesture);
}

fn install_drag_source(row: &gtk::Box, hit: &Hit) {
    let Some(path) = hit_file_path(hit) else {
        return;
    };

    let drag = gtk::DragSource::new();
    drag.set_actions(gdk::DragAction::COPY);

    let hit_for_icon = hit.clone();
    drag.connect_prepare(move |source, _x, _y| {
        let file = gio::File::for_path(&path);
        let uri = file.uri();
        let content = gdk::ContentProvider::for_value(&uri.to_value());
        if let Some(paintable) = resolve_icon(&hit_for_icon, ICON_SIZE_NORMAL) {
            source.set_icon(Some(&paintable), 0, 0);
        }
        Some(content)
    });

    row.add_controller(drag);
}

pub(crate) fn create_list_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();

    factory.connect_setup(move |_, list_item| {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.set_widget_name("lixun-hit");
        add_css_class(&row, "lixun-hit");

        let icon = gtk::Image::new();
        icon.set_pixel_size(ICON_SIZE_NORMAL);
        icon.set_margin_start(4);
        row.append(&icon);

        let text_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text_box.set_hexpand(true);

        let title = gtk::Label::new(None);
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        add_css_class(&title, "lixun-title");
        text_box.append(&title);

        let subtitle = gtk::Label::new(None);
        subtitle.set_xalign(0.0);
        subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
        add_css_class(&subtitle, "lixun-subtitle");
        text_box.append(&subtitle);

        row.append(&text_box);

        let kind = gtk::Label::new(None);
        kind.set_xalign(1.0);
        add_css_class(&kind, "lixun-kind");
        row.append(&kind);

        let list_item = list_item
            .downcast_ref::<gtk::ListItem>()
            .expect("ListItem expected");
        list_item.set_child(Some(&row));

        // Re-apply hero styling (large icon + card frame) whenever
        // this item's selected state flips. The "top-hit" visual
        // follows keyboard/mouse selection, not position.
        list_item.connect_selected_notify(|list_item| {
            apply_selected_styling(list_item);
        });
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

                    let text_box = row
                        .first_child()
                        .and_then(|c| c.next_sibling())
                        .and_downcast::<gtk::Box>()
                        .unwrap();
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

                    clear_row_controllers(&row);
                    install_action_group(&row, hit);
                    install_right_click_popover(&row, &hit.category);
                    install_double_click_open(&row, hit);
                    if matches!(hit.category, Category::File | Category::Attachment) {
                        install_drag_source(&row, hit);
                    }
                }
            });
        }

        apply_selected_styling(list_item);
    });

    factory
}

/// Apply the hero "top-hit" visuals to the row iff the list item is
/// currently selected. The visuals include a larger icon, a card-like
/// frame, and a bigger title, all driven from the `lixun-top-hit` CSS
/// class plus an icon-size swap. Called on initial bind and on every
/// selection change so the hero styling follows the user's cursor
/// rather than sitting statically on position 0.
fn apply_selected_styling(list_item: &gtk::ListItem) {
    let Some(row) = list_item.child().and_downcast::<gtk::Box>() else {
        return;
    };
    let Some(icon) = row.first_child().and_downcast::<gtk::Image>() else {
        return;
    };
    let is_hero = list_item.is_selected();
    if is_hero {
        row.add_css_class("lixun-top-hit");
    } else {
        row.remove_css_class("lixun-top-hit");
    }
    let icon_size = if is_hero {
        ICON_SIZE_TOP_HIT
    } else {
        ICON_SIZE_NORMAL
    };
    icon.set_pixel_size(icon_size);

    let Some(str_obj) = list_item
        .item()
        .and_then(|i| i.downcast::<gtk::StringObject>().ok())
    else {
        return;
    };
    let doc_id = str_obj.string().to_string();
    with_cached_hits(|hits| {
        if let Some(hit) = hits.iter().find(|h| h.id.0 == doc_id) {
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
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_for_file_has_expected_items() {
        let menu = build_menu_for(&Category::File);
        assert_eq!(menu.n_items(), 5);
    }

    #[test]
    fn menu_for_app_has_expected_items() {
        let menu = build_menu_for(&Category::App);
        assert_eq!(menu.n_items(), 2);
    }

    #[test]
    fn menu_for_mail_has_expected_items() {
        let menu = build_menu_for(&Category::Mail);
        assert_eq!(menu.n_items(), 2);
    }

    #[test]
    fn menu_for_attachment_has_expected_items() {
        let menu = build_menu_for(&Category::Attachment);
        assert_eq!(menu.n_items(), 4);
    }
}
