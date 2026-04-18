//! Keyboard bindings for the launcher window.
//!
//! Centralizes all key handling so window.rs stays small. Extended in later
//! waves for category filters (Ctrl+1..4), category jumps (Ctrl+Down/Up),
//! Quick Look (Space), and history navigation (↑ in empty entry).

use glib::clone;
use gtk::prelude::*;
use gtk4_layer_shell::{Edge, LayerShell};

use crate::actions::{copy_to_clipboard, execute_action, execute_secondary_action};
use crate::factory::with_cached_hits;
use crate::ipc::send_record_click;
use crate::window::{save_window_position, DEFAULT_TOP_MARGIN};

fn selected_hit<F: FnOnce(&lupa_core::Hit)>(
    selection: &gtk::SingleSelection,
    model: &gtk::StringList,
    f: F,
) {
    let idx = selection.selected();
    if let Some(item) = model.item(idx)
        && let Some(str_obj) = item.downcast_ref::<gtk::StringObject>()
    {
        let doc_id = str_obj.string().to_string();
        with_cached_hits(|hits| {
            if let Some(hit) = hits.iter().find(|h| h.id.0 == doc_id) {
                f(hit);
            }
        });
    }
}

pub(crate) fn install_keyboard_handler(
    window: &gtk::ApplicationWindow,
    list_view: &gtk::ListView,
    entry: &gtk::Entry,
    selection: &gtk::SingleSelection,
    model: &gtk::StringList,
) {
    let key_controller = gtk::EventControllerKey::new();
    key_controller.connect_key_pressed(clone!(
        #[strong] selection,
        #[strong] model,
        #[strong] window,
        #[strong] entry,
        move |_, key, _keycode, state| {
            let ctrl = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
            let shift = state.contains(gtk::gdk::ModifierType::SHIFT_MASK);
            let alt = state.contains(gtk::gdk::ModifierType::ALT_MASK);

            match key {
                gtk::gdk::Key::Up => {
                    if entry.text().is_empty() && entry.is_focus() {
                        return glib::signal::Propagation::Proceed;
                    }
                    let current = selection.selected();
                    if current > 0 {
                        selection.set_selected(current - 1);
                    }
                    glib::signal::Propagation::Stop
                }
                gtk::gdk::Key::Down => {
                    let current = selection.selected();
                    let n = selection.n_items();
                    if current + 1 < n {
                        selection.set_selected(current + 1);
                    }
                    glib::signal::Propagation::Stop
                }
                gtk::gdk::Key::Escape => {
                    window.hide();
                    glib::signal::Propagation::Stop
                }
                gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter => {
                    selected_hit(&selection, &model, |hit| {
                        send_record_click(&hit.id.0);
                        let result = if shift || ctrl {
                            execute_secondary_action(hit)
                        } else {
                            execute_action(hit)
                        };
                        if let Err(e) = result {
                            tracing::error!("Action failed: {}", e);
                        }
                    });
                    window.hide();
                    glib::signal::Propagation::Stop
                }
                gtk::gdk::Key::c | gtk::gdk::Key::C if ctrl => {
                    selected_hit(&selection, &model, copy_to_clipboard);
                    glib::signal::Propagation::Stop
                }
                gtk::gdk::Key::_0 if alt => {
                    window.set_margin(Edge::Top, DEFAULT_TOP_MARGIN);
                    window.set_margin(Edge::Left, 0);
                    save_window_position(DEFAULT_TOP_MARGIN, 0);
                    glib::signal::Propagation::Stop
                }
                _ => glib::signal::Propagation::Proceed,
            }
        }
    ));
    list_view.add_controller(key_controller);

    let entry_key_controller = gtk::EventControllerKey::new();
    entry_key_controller.connect_key_pressed(clone!(
        #[strong] window,
        move |_, key, _keycode, state| {
            let alt = state.contains(gtk::gdk::ModifierType::ALT_MASK);
            match key {
                gtk::gdk::Key::Escape => {
                    window.hide();
                    glib::signal::Propagation::Stop
                }
                gtk::gdk::Key::_0 if alt => {
                    window.set_margin(Edge::Top, DEFAULT_TOP_MARGIN);
                    window.set_margin(Edge::Left, 0);
                    save_window_position(DEFAULT_TOP_MARGIN, 0);
                    glib::signal::Propagation::Stop
                }
                _ => glib::signal::Propagation::Proceed,
            }
        }
    ));
    entry.add_controller(entry_key_controller);

    entry.connect_activate(clone!(
        #[strong] selection,
        #[strong] model,
        #[strong] window,
        move |_| {
            selected_hit(&selection, &model, |hit| {
                send_record_click(&hit.id.0);
                if let Err(e) = execute_action(hit) {
                    tracing::error!("Failed to execute action: {}", e);
                }
            });
            window.hide();
        }
    ));
}
