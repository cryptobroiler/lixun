//! Keyboard bindings for the launcher window.
//!
//! Centralizes all key handling so window.rs stays small. Extended in later
//! waves for category filters (Ctrl+1..4), category jumps (Ctrl+Down/Up),
//! Quick Look (Space), and history navigation (↑ in empty entry).

use glib::clone;
use gtk::prelude::*;
use lixun_core::Action;
use lixun_daemon::config::Keybindings;

use crate::actions::{copy_to_clipboard, execute_action, execute_secondary_action};
use crate::factory::{synthetic_history_hits, update_results, with_cached_hits};
use crate::ipc::{
    IpcClient, current_monitor_connector, request_search_history, send_preview_request,
    send_record_click,
};
use crate::status::StatusBar;
use crate::window::{CategoryChips, LauncherController};

fn selected_hit_in<F: FnOnce(&lixun_core::Hit)>(
    selection: &gtk::SingleSelection,
    filter_model: &gtk::FilterListModel,
    f: F,
) {
    let idx = selection.selected();
    if let Some(item) = filter_model.item(idx)
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

fn jump_to_next_category(
    selection: &gtk::SingleSelection,
    filter_model: &gtk::FilterListModel,
    direction: i32,
) {
    let n = filter_model.n_items();
    if n == 0 {
        return;
    }
    let current_idx = selection.selected();

    let current_cat = filter_model
        .item(current_idx)
        .and_then(|o| o.downcast::<gtk::StringObject>().ok())
        .and_then(|s| {
            let id = s.string().to_string();
            with_cached_hits(|hits| hits.iter().find(|h| h.id.0 == id).map(|h| h.category))
        });

    let range: Box<dyn Iterator<Item = u32>> = if direction > 0 {
        Box::new((current_idx + 1)..n)
    } else {
        Box::new((0..current_idx).rev())
    };

    for i in range {
        let cat = filter_model
            .item(i)
            .and_then(|o| o.downcast::<gtk::StringObject>().ok())
            .and_then(|s| {
                let id = s.string().to_string();
                with_cached_hits(|hits| {
                    hits.iter().find(|h| h.id.0 == id).map(|h| h.category)
                })
            });
        if cat != current_cat {
            selection.set_selected(i);
            return;
        }
    }
}

fn accel_matches(accel: &str, key: gtk::gdk::Key, state: gtk::gdk::ModifierType) -> bool {
    let Some((expected_key, expected_mods)) = gtk::accelerator_parse(accel) else {
        return false;
    };
    key == expected_key && state.contains(expected_mods)
}

/// True when the Entry or any of its descendants (the internal GtkText
/// delegate that actually owns keyboard focus) is the focused widget.
/// `gtk::Entry::is_focus()` returns false in that case because Entry is a
/// composite: GtkText is a child widget, not the Entry itself. Without
/// this helper our key dispatch would treat a typing Entry as unfocused
/// and swallow Space into `quick_look`.
fn entry_has_focus(entry: &gtk::Entry, window: &gtk::ApplicationWindow) -> bool {
    let Some(focus) = gtk::prelude::RootExt::focus(window) else {
        return false;
    };
    let entry_widget = entry.upcast_ref::<gtk::Widget>();
    &focus == entry_widget || focus.is_ancestor(entry)
}

/// Key should be forwarded to a focused text input instead of intercepted
/// by window-level shortcut dispatch. Shift alone is allowed (capitals);
/// Ctrl/Alt/Super/Meta/Hyper disqualify. Keys without a non-control
/// Unicode mapping (Escape, Return, arrows, Tab, F-keys) also disqualify.
fn is_printable_key(key: gtk::gdk::Key, state: gtk::gdk::ModifierType) -> bool {
    let non_shift_mods = gtk::gdk::ModifierType::CONTROL_MASK
        | gtk::gdk::ModifierType::ALT_MASK
        | gtk::gdk::ModifierType::SUPER_MASK
        | gtk::gdk::ModifierType::META_MASK
        | gtk::gdk::ModifierType::HYPER_MASK;
    if state.intersects(non_shift_mods) {
        return false;
    }
    matches!(key.to_unicode(), Some(c) if !c.is_control())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn install_keyboard_handler(
    window: &gtk::ApplicationWindow,
    list_view: &gtk::ListView,
    entry: &gtk::Entry,
    selection: &gtk::SingleSelection,
    filter_model: &gtk::FilterListModel,
    model: &gtk::StringList,
    chips: std::rc::Rc<CategoryChips>,
    status_bar: std::rc::Rc<StatusBar>,
    _ipc: IpcClient,
    keybindings: Keybindings,
    controller: std::rc::Rc<LauncherController>,
) {
    // Main key controller attached to window with Capture phase
    // This ensures all key handling works regardless of focus state
    let key_controller = gtk::EventControllerKey::new();
    key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    key_controller.connect_key_pressed(clone!(
        #[strong] selection,
        #[strong] filter_model,
        #[strong] list_view,
        #[strong] window,
        #[strong] entry,
        #[strong] chips,
        #[strong] keybindings,
        #[strong] controller,
        move |_, key, _keycode, state| {
            // Hard rule: printable unmodified keys belong to the focused
            // Entry. Forward them before any accel dispatch can swallow
            // them (e.g. bare-Space `quick_look` binding).
            if entry_has_focus(&entry, &window) && is_printable_key(key, state) {
                return glib::signal::Propagation::Proceed;
            }
            // BUG-5: focus has left the entry (list_view grabbed it on
            // Down, or user clicked a result row). A printable key that
            // is NOT a registered accel (quick_look=Space stays a
            // quick_look trigger) should warp back to the entry and
            // continue typing — that is the Raycast/Alfred contract.
            // Without this, once focus moves to the list the launcher
            // becomes a dead keyboard surface until Escape/Enter/click.
            if !entry_has_focus(&entry, &window)
                && is_printable_key(key, state)
                && !accel_matches(&keybindings.quick_look, key, state)
                && let Some(ch) = key.to_unicode()
            {
                let cur = entry.text().to_string();
                let mut appended = cur;
                appended.push(ch);
                entry.set_text(&appended);
                entry.set_position(-1);
                entry.grab_focus();
                // Move the caret to the end after grab_focus (which by
                // default selects all text).
                entry.set_position(-1);
                return glib::signal::Propagation::Stop;
            }
            let ctrl = state.contains(gtk::gdk::ModifierType::CONTROL_MASK);
            let shift = state.contains(gtk::gdk::ModifierType::SHIFT_MASK);
            if accel_matches(&keybindings.previous_result, key, state) {
                    // Up on empty entry = let entry_key_controller handle history
                    if entry.text().is_empty() && entry_has_focus(&entry, &window) {
                        return glib::signal::Propagation::Proceed;
                    }
                    // Defensive: no results, no list to navigate. Pin focus
                    // in the entry so GTK's default Up/Down focus-chain
                    // cannot warp focus to a sibling widget (the list is
                    // hidden, chip row, etc.) leaving the user stuck in
                    // a widget that doesn't accept printable keys.
                    // BUG-5 regression guard.
                    if filter_model.n_items() == 0 {
                        entry.grab_focus();
                        return glib::signal::Propagation::Stop;
                    }
                    let entry_had_focus = entry_has_focus(&entry, &window);
                    if ctrl {
                        jump_to_next_category(&selection, &filter_model, -1);
                        let target = selection.selected();
                        if target != gtk::INVALID_LIST_POSITION {
                            list_view.scroll_to(target, gtk::ListScrollFlags::FOCUS, None);
                        }
                        if entry_had_focus {
                            list_view.grab_focus();
                        }
                        return glib::signal::Propagation::Stop;
                    }
                    let current = selection.selected();
                    if current > 0 {
                        let target = current - 1;
                        selection.set_selected(target);
                        list_view.scroll_to(target, gtk::ListScrollFlags::FOCUS, None);
                        if entry_had_focus {
                            list_view.grab_focus();
                        }
                    } else {
                        // Already at the top row; Up returns focus to the Entry.
                        entry.grab_focus();
                    }
                    glib::signal::Propagation::Stop
            } else if accel_matches(&keybindings.next_result, key, state) {
                    // BUG-5 regression guard: same defensive pin as Up.
                    if filter_model.n_items() == 0 {
                        entry.grab_focus();
                        return glib::signal::Propagation::Stop;
                    }
                    let entry_had_focus = entry_has_focus(&entry, &window);
                    if ctrl {
                        jump_to_next_category(&selection, &filter_model, 1);
                        let target = selection.selected();
                        if target != gtk::INVALID_LIST_POSITION {
                            list_view.scroll_to(target, gtk::ListScrollFlags::FOCUS, None);
                        }
                        if entry_had_focus {
                            list_view.grab_focus();
                        }
                        return glib::signal::Propagation::Stop;
                    }
                    let current = selection.selected();
                    let n = selection.n_items();
                    if current + 1 < n {
                        let target = current + 1;
                        selection.set_selected(target);
                        list_view.scroll_to(target, gtk::ListScrollFlags::FOCUS, None);
                    }
                    if entry_had_focus && n > 0 {
                        list_view.grab_focus();
                    }
                    glib::signal::Propagation::Stop
            } else if accel_matches(&keybindings.close, key, state) {
                    controller.hide();
                    glib::signal::Propagation::Stop
            } else if accel_matches(&keybindings.primary_action, key, state)
                || accel_matches(&keybindings.secondary_action, key, state)
            {
                    let mut should_hide = true;
                    selected_hit_in(&selection, &filter_model, |hit| {
                        if let Action::ReplaceQuery { q } = &hit.action {
                            entry.set_text(q);
                            entry.set_position(-1);
                            entry.grab_focus();
                            should_hide = false;
                            return;
                        }
                        send_record_click(&hit.id.0);
                        let result = if accel_matches(&keybindings.secondary_action, key, state) || shift || ctrl {
                            execute_secondary_action(hit)
                        } else {
                            execute_action(hit)
                        };
                        if let Err(e) = result {
                            tracing::error!("Action failed: {}", e);
                        }
                    });
                    if should_hide {
                        controller.hide();
                    }
                    glib::signal::Propagation::Stop
            } else if accel_matches(&keybindings.copy, key, state) {
                    selected_hit_in(&selection, &filter_model, copy_to_clipboard);
                    glib::signal::Propagation::Stop
            } else if accel_matches(&keybindings.quick_look, key, state) && !entry_has_focus(&entry, &window) {
                    // Only trigger when focus is NOT in entry (i.e. list is focused)
                    // so space can still be typed into the search field.
                    let monitor = current_monitor_connector(&window);
                    selected_hit_in(&selection, &filter_model, |hit| {
                        send_preview_request(hit, monitor.clone());
                    });
                    glib::signal::Propagation::Stop
            } else if accel_matches(&keybindings.filter_all, key, state) {
                    chips.activate_index(0);
                    glib::signal::Propagation::Stop
            } else if accel_matches(&keybindings.filter_apps, key, state) {
                    chips.activate_index(1);
                    glib::signal::Propagation::Stop
            } else if accel_matches(&keybindings.filter_files, key, state) {
                    chips.activate_index(2);
                    glib::signal::Propagation::Stop
            } else if accel_matches(&keybindings.filter_mail, key, state) {
                    chips.activate_index(3);
                    glib::signal::Propagation::Stop
            } else if accel_matches(&keybindings.filter_attachments, key, state) {
                    chips.activate_index(4);
                    glib::signal::Propagation::Stop
            } else {
                glib::signal::Propagation::Proceed
            }
        }
    ));
    window.add_controller(key_controller);

    // Entry-level key controller for history navigation when entry is focused and empty.
    // Capture phase so we run BEFORE GtkText's built-in Up/Down handler (which
    // in a single-line GtkEntry is a no-op but still stops propagation,
    // preventing a default Bubble-phase controller from ever seeing the key).
    let entry_key_controller = gtk::EventControllerKey::new();
    entry_key_controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    entry_key_controller.connect_key_pressed(clone!(
        #[strong] entry,
        #[strong] selection,
        #[strong] model,
        #[strong] list_view,
        #[strong] status_bar,
        #[strong] keybindings,
        move |_, key, _keycode, state| {
            if accel_matches(&keybindings.history_up, key, state) && entry.text().is_empty() {
                    let queries = request_search_history(10);
                    if queries.is_empty() {
                        return glib::signal::Propagation::Stop;
                    }
                    let hits = synthetic_history_hits(&queries);
                    update_results(&model, &hits);
                    selection.set_selected(0);
                    list_view.scroll_to(0, gtk::ListScrollFlags::NONE, None);
                    status_bar.hide();
                    glib::signal::Propagation::Stop
            } else {
                glib::signal::Propagation::Proceed
            }
        }
    ));
    entry.add_controller(entry_key_controller);
}
