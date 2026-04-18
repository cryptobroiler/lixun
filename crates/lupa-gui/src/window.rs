//! Main window construction: layer-shell setup, entry + list, keyboard
//! bindings, animations, Toggle ping to the daemon.

use std::io::Write;
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
        provider.load_from_data(
            ".lupa-window { background-color: rgba(30,30,35,0.85); border-radius: 12px; } \
             .lupa-entry { font-size: 28px; padding: 14px 20px; background-color: rgba(20,20,25,0.9); color: #e0e0e0; border: none; border-radius: 8px; } \
             .lupa-results { background-color: transparent; } \
             .lupa-hit { padding: 8px 16px; border-radius: 6px; } \
             .lupa-title { font-weight: bold; color: #f0f0f0; } \
             .lupa-subtitle { color: #888888; font-size: 12px; } \
             .lupa-badge { color: #666666; font-size: 10px; }"
        );
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
    window.set_child(Some(&vbox));

    let model2 = model.clone();
    let responses = Arc::clone(&ipc.responses);
    glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
        let mut hits = responses.lock().unwrap();
        if !hits.is_empty() {
            let snapshot = std::mem::take(&mut *hits);
            update_results(&model2, &snapshot);
        }
        glib::ControlFlow::Continue
    });

    let debounce_state: Arc<Mutex<(String, Instant)>> =
        Arc::new(Mutex::new((String::new(), Instant::now())));
    let ipc_for_debounce = ipc.clone();
    let debounce_state_for_timeout = Arc::clone(&debounce_state);
    glib::timeout_add_local(std::time::Duration::from_millis(40), move || {
        let mut state = debounce_state_for_timeout.lock().unwrap();
        if !state.0.is_empty() && state.1.elapsed() >= std::time::Duration::from_millis(40) {
            let _ = ipc_for_debounce.request_tx.send((state.0.clone(), 30));
            state.0.clear();
        }
        glib::ControlFlow::Continue
    });
    let debounce_state_for_entry = Arc::clone(&debounce_state);
    entry.connect_changed(move |e| {
        let mut state = debounce_state_for_entry.lock().unwrap();
        state.0 = e.text().to_string();
        state.1 = Instant::now();
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
