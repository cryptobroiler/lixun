//! Main window construction: layer-shell setup, entry + list, keyboard
//! bindings, animations, Toggle ping to the daemon.

use std::io::Write;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use glib::clone;
use gtk::prelude::*;
use gtk4_layer_shell::LayerShell;
use lupa_ipc::{socket_path, Request, PROTOCOL_VERSION};

use crate::actions::{copy_to_clipboard, execute_action, execute_secondary_action};
use crate::factory::{add_css_class, create_list_factory, update_results, with_cached_hits};
use crate::ipc::{send_record_click, start_ipc_thread};
use crate::status::StatusBar;

const EMBEDDED_STYLESHEET: &str = include_str!("../style.css");

pub(crate) fn build_window(app: &gtk::Application) -> Result<()> {
    let ipc = start_ipc_thread();

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .default_width(720)
        .decorated(false)
        .build();

    window.init_layer_shell();
    window.set_layer(gtk4_layer_shell::Layer::Overlay);
    window.set_anchor(gtk4_layer_shell::Edge::Top, true);
    window.set_anchor(gtk4_layer_shell::Edge::Left, true);
    window.set_anchor(gtk4_layer_shell::Edge::Right, true);
    window.set_keyboard_mode(gtk4_layer_shell::KeyboardMode::OnDemand);
    window.set_margin(gtk4_layer_shell::Edge::Top, 140);
    window.set_margin(gtk4_layer_shell::Edge::Left, 0);
    window.set_margin(gtk4_layer_shell::Edge::Right, 0);
    add_css_class(&window, "lupa-window");

    if let Some(display) = gtk::gdk::Display::default()
        && let Some(seat) = display.default_seat()
        && let Some(pointer) = seat.pointer()
    {
        let (surface, _, _) = pointer.surface_at_position();
        if let Some(surface) = surface {
            let monitor = display.monitor_at_surface(&surface);
            window.set_monitor(monitor.as_ref());
        }
    }

    let provider = gtk::CssProvider::new();
    let css_path = dirs::config_dir()
        .map(|d| d.join("lupa/style.css"))
        .filter(|p| p.exists());

    if let Some(path) = css_path {
        provider.load_from_path(&path);
        tracing::info!("Loaded external style.css from {:?}", path);
    } else {
        provider.load_from_data(EMBEDDED_STYLESHEET);
    }
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().unwrap(),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 6);
    vbox.set_margin_start(16);
    vbox.set_margin_end(16);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);

    let entry = gtk::Entry::builder()
        .placeholder_text("Search\u{2026}")
        .hexpand(true)
        .build();
    add_css_class(&entry, "lupa-entry");
    vbox.append(&entry);

    let scrolled = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .min_content_height(60)
        .max_content_height(400)
        .build();
    add_css_class(&scrolled, "lupa-results");
    vbox.append(&scrolled);

    let model = gtk::StringList::new(&[]);
    let selection = gtk::SingleSelection::builder()
        .model(&model)
        .autoselect(false)
        .build();

    let list_view = gtk::ListView::builder()
        .model(&selection)
        .factory(&create_list_factory())
        .build();
    scrolled.set_child(Some(&list_view));

    let status_bar = std::rc::Rc::new(StatusBar::new());
    vbox.append(status_bar.widget());

    window.set_child(Some(&vbox));

    let model2 = model.clone();
    let responses = Arc::clone(&ipc.responses);
    let calculation = Arc::clone(&ipc.calculation);
    let epoch = Arc::clone(&ipc.response_epoch);
    let last_epoch = std::rc::Rc::new(std::cell::Cell::new(0u64));
    let status_for_poll = std::rc::Rc::clone(&status_bar);
    let last_query_for_poll = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    let last_query_poll_clone = last_query_for_poll.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
        let current = epoch.load(Ordering::SeqCst);
        if current != last_epoch.get() {
            last_epoch.set(current);
            let hits_snapshot = {
                let mut hits = responses.lock().unwrap();
                std::mem::take(&mut *hits)
            };
            let calc_snapshot = {
                let mut c = calculation.lock().unwrap();
                c.take()
            };
            update_results(&model2, &hits_snapshot);

            if let Some(calc) = calc_snapshot.as_ref() {
                status_for_poll.show_calculation(calc);
            } else if hits_snapshot.is_empty() {
                let q = last_query_poll_clone.borrow().clone();
                if !q.is_empty() {
                    status_for_poll.show_empty(&q);
                } else {
                    status_for_poll.hide();
                }
            } else {
                status_for_poll.hide();
            }
        }
        glib::ControlFlow::Continue
    });

    let debounce_state: Arc<Mutex<(String, Instant)>> =
        Arc::new(Mutex::new((String::new(), Instant::now())));
    let ipc_for_debounce = ipc.clone();
    let debounce_state_for_timeout = Arc::clone(&debounce_state);
    let status_for_debounce = std::rc::Rc::clone(&status_bar);
    let last_query_for_debounce = last_query_for_poll.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(40), move || {
        let mut state = debounce_state_for_timeout.lock().unwrap();
        if !state.0.is_empty() && state.1.elapsed() >= std::time::Duration::from_millis(40) {
            let q = state.0.clone();
            *last_query_for_debounce.borrow_mut() = q.clone();
            status_for_debounce.show_loading();
            let _ = ipc_for_debounce.request_tx.send((q, 30));
            state.0.clear();
        }
        glib::ControlFlow::Continue
    });
    let debounce_state_for_entry = Arc::clone(&debounce_state);
    let status_for_entry = std::rc::Rc::clone(&status_bar);
    let model_for_entry = model.clone();
    entry.connect_changed(move |e| {
        let text = e.text().to_string();
        let mut state = debounce_state_for_entry.lock().unwrap();
        state.0 = text.clone();
        state.1 = Instant::now();
        if text.is_empty() {
            let n = model_for_entry.n_items();
            for _ in 0..n {
                model_for_entry.remove(0);
            }
            status_for_entry.hide();
        }
    });

    let key_controller = gtk::EventControllerKey::new();
    key_controller.connect_key_pressed(
        clone!(#[strong] selection, #[strong] model, #[strong] window, move |_, key, _keycode, state| {
            match key {
                gtk::gdk::Key::Up => {
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
                    let idx = selection.selected();
                    if let Some(item) = model.item(idx)
                        && let Some(str_obj) = item.downcast_ref::<gtk::StringObject>()
                    {
                        let doc_id = str_obj.string().to_string();
                        with_cached_hits(|hits| {
                            if let Some(hit) = hits.iter().find(|h| h.id.0 == doc_id) {
                                send_record_click(&hit.id.0);
                                if state.contains(gtk::gdk::ModifierType::SHIFT_MASK) {
                                    if let Err(e) = execute_secondary_action(hit) {
                                        tracing::error!("Secondary action failed: {}", e);
                                    }
                                } else if let Err(e) = execute_action(hit) {
                                    tracing::error!("Failed to execute action: {}", e);
                                }
                                window.hide();
                            }
                        });
                    }
                    glib::signal::Propagation::Stop
                }
                gtk::gdk::Key::c | gtk::gdk::Key::C => {
                    if state.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
                        let idx = selection.selected();
                        if let Some(item) = model.item(idx)
                            && let Some(str_obj) = item.downcast_ref::<gtk::StringObject>()
                        {
                            let doc_id = str_obj.string().to_string();
                            with_cached_hits(|hits| {
                                if let Some(hit) = hits.iter().find(|h| h.id.0 == doc_id) {
                                    copy_to_clipboard(hit);
                                }
                            });
                        }
                        glib::signal::Propagation::Stop
                    } else {
                        glib::signal::Propagation::Proceed
                    }
                }
                _ => glib::signal::Propagation::Proceed,
            }
        }),
    );
    list_view.add_controller(key_controller);

    entry.connect_activate(clone!(#[strong] selection, #[strong] model, move |_| {
        let idx = selection.selected();
        if let Some(item) = model.item(idx)
            && let Some(str_obj) = item.downcast_ref::<gtk::StringObject>()
        {
            let doc_id = str_obj.string().to_string();
            with_cached_hits(|hits| {
                if let Some(hit) = hits.iter().find(|h| h.id.0 == doc_id) {
                    send_record_click(&hit.id.0);
                    if let Err(e) = execute_action(hit) {
                        tracing::error!("Failed to execute action: {}", e);
                    }
                }
            });
        }
    }));

    let focus_ctrl = gtk::EventControllerFocus::new();
    let window_for_focus = window.clone();
    focus_ctrl.connect_leave(move |_| {
        animate_hide(&window_for_focus);
    });
    window.add_controller(focus_ctrl);

    animate_show(&window);

    let sock = socket_path();
    if sock.exists() {
        let req = Request::Toggle;
        if let Ok(json) = serde_json::to_vec(&req) {
            let total_len = (2 + json.len()) as u32;
            let _ = std::os::unix::net::UnixStream::connect(&sock).and_then(|mut s| {
                let mut buf = Vec::with_capacity(4 + 2 + json.len());
                buf.extend_from_slice(&total_len.to_be_bytes());
                buf.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
                buf.extend_from_slice(&json);
                s.write_all(&buf)
            });
        }
    }

    tracing::info!("Lupa GUI window shown");
    Ok(())
}

fn animate_show(window: &gtk::ApplicationWindow) {
    window.remove_css_class("lupa-hiding");
    window.add_css_class("lupa-showing");
    window.show();

    let window_weak = window.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
        window_weak.remove_css_class("lupa-showing");
    });
}

fn animate_hide(window: &gtk::ApplicationWindow) {
    window.remove_css_class("lupa-showing");
    window.add_css_class("lupa-hiding");

    let window_weak = window.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
        window_weak.hide();
        window_weak.remove_css_class("lupa-hiding");
    });
}
