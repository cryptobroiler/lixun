//! Main window construction: layer-shell setup, entry + list, keyboard
//! bindings, animations, Toggle ping to the daemon.

use std::io::Write;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::Result;
use gtk::prelude::*;
use gtk4_layer_shell::{Edge, LayerShell};
use lupa_ipc::{socket_path, Request, PROTOCOL_VERSION};

use crate::factory::{add_css_class, create_list_factory, update_results};
use crate::ipc::start_ipc_thread;
use crate::status::StatusBar;

const EMBEDDED_STYLESHEET: &str = include_str!("../style.css");

pub(crate) const DEFAULT_TOP_MARGIN: i32 = 140;

fn window_state_path() -> Option<std::path::PathBuf> {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .map(|d| d.join("lupa/window.json"))
}

pub(crate) fn save_window_position(top: i32, left: i32) {
    let Some(path) = window_state_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body = format!(r#"{{"top":{top},"left":{left}}}"#);
    if let Err(e) = std::fs::write(&path, body) {
        tracing::debug!("Failed to persist window position: {}", e);
    }
}

fn load_window_position() -> Option<(i32, i32)> {
    let path = window_state_path()?;
    let body = std::fs::read_to_string(&path).ok()?;
    let top = extract_int(&body, "top")?;
    let left = extract_int(&body, "left")?;
    Some((top, left))
}

fn extract_int(s: &str, key: &str) -> Option<i32> {
    let needle = format!("\"{key}\":");
    let idx = s.find(&needle)? + needle.len();
    let rest = &s[idx..];
    let end = rest
        .find(|c: char| c != '-' && !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

pub(crate) fn build_window(app: &gtk::Application) -> Result<()> {
    let ipc = start_ipc_thread();

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .default_width(720)
        .decorated(false)
        .build();

    window.init_layer_shell();
    window.set_layer(gtk4_layer_shell::Layer::Overlay);
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Left, true);
    window.set_anchor(Edge::Right, true);
    window.set_keyboard_mode(gtk4_layer_shell::KeyboardMode::OnDemand);

    let (top_margin, left_margin) = load_window_position().unwrap_or((DEFAULT_TOP_MARGIN, 0));
    window.set_margin(Edge::Top, top_margin);
    window.set_margin(Edge::Left, left_margin);
    window.set_margin(Edge::Right, 0);
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

    let pending_source: std::rc::Rc<std::cell::RefCell<Option<glib::SourceId>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let ipc_for_entry = ipc.clone();
    let status_for_entry = std::rc::Rc::clone(&status_bar);
    let model_for_entry = model.clone();
    let last_query_for_entry = last_query_for_poll.clone();
    let pending_source_for_entry = std::rc::Rc::clone(&pending_source);
    entry.connect_changed(move |e| {
        let text = e.text().to_string();

        if let Some(id) = pending_source_for_entry.borrow_mut().take() {
            id.remove();
        }

        if text.is_empty() {
            let n = model_for_entry.n_items();
            for _ in 0..n {
                model_for_entry.remove(0);
            }
            status_for_entry.hide();
            return;
        }

        let ipc = ipc_for_entry.clone();
        let status = std::rc::Rc::clone(&status_for_entry);
        let q = text.clone();
        let last_q = last_query_for_entry.clone();
        let pending_self = std::rc::Rc::clone(&pending_source_for_entry);
        let id = glib::timeout_add_local_once(std::time::Duration::from_millis(80), move || {
            *last_q.borrow_mut() = q.clone();
            status.show_loading();
            let _ = ipc.request_tx.send((q, 30));
            *pending_self.borrow_mut() = None;
        });
        *pending_source_for_entry.borrow_mut() = Some(id);
    });

    crate::keymap::install_keyboard_handler(&window, &list_view, &entry, &selection, &model);

    install_drag_handler(&window);

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

fn install_drag_handler(window: &gtk::ApplicationWindow) {
    let drag = gtk::GestureDrag::new();
    drag.set_propagation_phase(gtk::PropagationPhase::Capture);

    let start_top = std::rc::Rc::new(std::cell::Cell::new(0i32));
    let start_left = std::rc::Rc::new(std::cell::Cell::new(0i32));

    let w = window.clone();
    let st = std::rc::Rc::clone(&start_top);
    let sl = std::rc::Rc::clone(&start_left);
    drag.connect_drag_begin(move |_, _, _| {
        st.set(w.margin(Edge::Top));
        sl.set(w.margin(Edge::Left));
    });

    let w = window.clone();
    let st = std::rc::Rc::clone(&start_top);
    let sl = std::rc::Rc::clone(&start_left);
    drag.connect_drag_update(move |_, dx, dy| {
        let new_top = (st.get() as f64 + dy).max(0.0) as i32;
        let new_left = (sl.get() as f64 + dx) as i32;
        w.set_margin(Edge::Top, new_top);
        w.set_margin(Edge::Left, new_left);
    });

    let w = window.clone();
    drag.connect_drag_end(move |_, _, _| {
        save_window_position(w.margin(Edge::Top), w.margin(Edge::Left));
    });

    window.add_controller(drag);
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
