//! Main window construction: layer-shell setup, entry + list, keyboard
//! bindings, animations, Toggle ping to the daemon.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::Result;
use gtk::gio;
use gtk::prelude::*;
use gtk4_layer_shell::{Edge, LayerShell};
use lixun_core::Category;

use crate::factory::{add_css_class, create_list_factory, update_results, with_cached_hits};
use crate::ipc::start_ipc_thread;
use crate::status::StatusBar;

pub(crate) type CategoryFilter = std::rc::Rc<std::cell::Cell<Option<Category>>>;

const EMBEDDED_STYLESHEET: &str = include_str!("../style.css");

pub(crate) const DEFAULT_TOP_MARGIN: i32 = 140;

pub(crate) fn build_window(app: &gtk::Application) -> Result<()> {
    let ipc = start_ipc_thread();
    let daemon_config = lixun_daemon::config::Config::load()?;

    // No default_height: layer-shell anchors top/left/right only, so the
    // window surface must size to content. A default_height would pin the
    // surface at that height regardless of hidden children, preventing
    // the empty-results collapse (G0.2).
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .default_width(720)
        .decorated(false)
        .build();
    // Widget names are the stable CSS id anchors documented in
    // `docs/style.example.css`. Keep them in sync with that file.
    window.set_widget_name("lixun-root");

    window.init_layer_shell();
    window.set_layer(gtk4_layer_shell::Layer::Overlay);
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Left, true);
    window.set_anchor(Edge::Right, true);
    window.set_keyboard_mode(gtk4_layer_shell::KeyboardMode::OnDemand);

    window.set_margin(Edge::Top, DEFAULT_TOP_MARGIN);
    window.set_margin(Edge::Left, 0);
    window.set_margin(Edge::Right, 0);
    add_css_class(&window, "lixun-window");

    // App-level `close-launcher` action so feature modules (factory's
    // double-click gesture) can dismiss the launcher without holding
    // a window reference.
    let close_action = gio::SimpleAction::new("close-launcher", None);
    let window_weak = window.downgrade();
    close_action.connect_activate(move |_, _| {
        if let Some(w) = window_weak.upgrade() {
            animate_hide(&w);
        }
    });
    app.add_action(&close_action);

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
    provider.load_from_string(EMBEDDED_STYLESHEET);
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().unwrap(),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let css_path = dirs::config_dir()
        .map(|d| d.join("lixun/style.css"))
        .filter(|p| p.exists());

    if let Some(path) = css_path {
        let override_provider = gtk::CssProvider::new();
        override_provider.load_from_path(&path);
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().unwrap(),
            &override_provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
        tracing::info!("Loaded external style.css from {:?}", path);
    }

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 6);
    vbox.set_margin_start(16);
    vbox.set_margin_end(16);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);

    let entry = gtk::Entry::builder()
        .placeholder_text("Search\u{2026}")
        .hexpand(true)
        .build();
    entry.set_widget_name("lixun-entry");
    add_css_class(&entry, "lixun-entry");
    vbox.append(&entry);

    let current_category: CategoryFilter = std::rc::Rc::new(std::cell::Cell::new(None));
    let chips = build_category_chips(&current_category);
    chips.container.set_visible(false);
    vbox.append(&chips.container);

    let scrolled = gtk::ScrolledWindow::builder()
        .vexpand(false)
        .min_content_height(320)
        .max_content_height(520)
        .build();
    scrolled.set_widget_name("lixun-results-scroll");
    add_css_class(&scrolled, "lixun-results");
    scrolled.set_visible(false);
    vbox.append(&scrolled);

    let model = gtk::StringList::new(&[]);

    let filter = gtk::CustomFilter::new({
        let current = std::rc::Rc::clone(&current_category);
        move |obj| {
            let Some(filter_cat) = current.get() else {
                return true;
            };
            let Some(str_obj) = obj.downcast_ref::<gtk::StringObject>() else {
                return true;
            };
            let doc_id = str_obj.string().to_string();
            with_cached_hits(|hits| {
                hits.iter()
                    .find(|h| h.id.0 == doc_id)
                    .map(|h| h.category == filter_cat)
                    .unwrap_or(true)
            })
        }
    });

    let filter_model = gtk::FilterListModel::new(Some(model.clone()), Some(filter.clone()));

    let selection = gtk::SingleSelection::builder()
        .model(&filter_model)
        .autoselect(true)
        .build();

    let list_view = gtk::ListView::builder()
        .model(&selection)
        .factory(&create_list_factory())
        .build();
    list_view.set_widget_name("lixun-results");
    scrolled.set_child(Some(&list_view));

    chips.wire_toggle({
        let filter = filter.clone();
        move || filter.changed(gtk::FilterChange::Different)
    });

    let status_bar = std::rc::Rc::new(StatusBar::new());
    vbox.append(status_bar.widget());

    window.set_child(Some(&vbox));

    let model2 = model.clone();
    let filter_for_poll = filter.clone();
    let selection_for_poll = selection.clone();
    let list_view_for_poll = list_view.clone();
    let chips_for_poll = chips.container.clone();
    let scrolled_for_poll = scrolled.clone();
    let responses = Arc::clone(&ipc.responses);
    let calculation = Arc::clone(&ipc.calculation);
    let epoch = Arc::clone(&ipc.response_epoch);
    let last_epoch = std::rc::Rc::new(std::cell::Cell::new(0u64));
    let status_for_poll = std::rc::Rc::clone(&status_bar);
    let last_query_for_poll = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    let last_query_poll_clone = last_query_for_poll.clone();
    let selection_for_empty = selection.clone();
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
            filter_for_poll.changed(gtk::FilterChange::Different);
            if !hits_snapshot.is_empty() {
                selection_for_poll.set_selected(0);
                list_view_for_poll.scroll_to(0, gtk::ListScrollFlags::NONE, None);
            }

            if let Some(calc) = calc_snapshot.as_ref() {
                chips_for_poll.set_visible(true);
                scrolled_for_poll.set_visible(false);
                scrolled_for_poll.set_vexpand(false);
                status_for_poll.show_calculation(calc);
            } else if hits_snapshot.is_empty() {
                let q = last_query_poll_clone.borrow().clone();
                if !q.is_empty() {
                    chips_for_poll.set_visible(true);
                    scrolled_for_poll.set_visible(false);
                    scrolled_for_poll.set_vexpand(false);
                    status_for_poll.show_empty(&q);
                    selection_for_empty.set_selected(gtk::INVALID_LIST_POSITION);
                } else {
                    chips_for_poll.set_visible(false);
                    scrolled_for_poll.set_visible(false);
                    scrolled_for_poll.set_vexpand(false);
                    status_for_poll.hide();
                }
            } else {
                chips_for_poll.set_visible(true);
                scrolled_for_poll.set_visible(true);
                scrolled_for_poll.set_vexpand(true);
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
    let chips_for_entry = chips.container.clone();
    let scrolled_for_entry = scrolled.clone();
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
            chips_for_entry.set_visible(false);
            scrolled_for_entry.set_visible(false);
            scrolled_for_entry.set_vexpand(false);
            status_for_entry.hide();
            return;
        }

        chips_for_entry.set_visible(true);

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

    let chips_rc = std::rc::Rc::new(chips);
    crate::keymap::install_keyboard_handler(
        &window,
        &list_view,
        &entry,
        &selection,
        &filter_model,
        &model,
        std::rc::Rc::clone(&chips_rc),
        std::rc::Rc::clone(&status_bar),
        ipc.clone(),
        daemon_config.keybindings.clone(),
    );

    let focus_ctrl = gtk::EventControllerFocus::new();
    let window_for_focus = window.clone();
    // Layer-shell OnDemand hands us keyboard focus asynchronously; re-grab
    // Entry on focus-enter so Space/printables reach it, not window accels.
    let entry_for_focus_enter = entry.clone();
    focus_ctrl.connect_enter(move |_| {
        entry_for_focus_enter.grab_focus();
    });
    focus_ctrl.connect_leave(move |_| {
        animate_hide(&window_for_focus);
    });
    window.add_controller(focus_ctrl);

    animate_show(&window);
    entry.grab_focus();

    tracing::info!("Lixun GUI window shown");
    Ok(())
}

pub(crate) struct CategoryChips {
    pub(crate) container: gtk::Box,
    pub(crate) buttons: [gtk::ToggleButton; 5],
}

impl CategoryChips {
    pub(crate) fn wire_toggle<F>(&self, on_change: F)
    where
        F: Fn() + 'static + Clone,
    {
        for button in &self.buttons {
            let cb = on_change.clone();
            button.connect_toggled(move |_| {
                cb();
            });
        }
    }

    pub(crate) fn activate_index(&self, index: usize) {
        if let Some(btn) = self.buttons.get(index) {
            btn.set_active(true);
        }
    }
}

fn build_category_chips(current: &CategoryFilter) -> CategoryChips {
    let container = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    container.set_widget_name("lixun-chips");
    container.set_margin_top(4);
    container.set_margin_bottom(2);
    add_css_class(&container, "lixun-chips");

    let labels = [
        ("All", None),
        ("Apps", Some(Category::App)),
        ("Files", Some(Category::File)),
        ("Mail", Some(Category::Mail)),
        ("Attachments", Some(Category::Attachment)),
    ];

    let mut buttons: Vec<gtk::ToggleButton> = Vec::with_capacity(5);
    let group_anchor: Option<gtk::ToggleButton> = None;
    let mut group_anchor = group_anchor;

    for (label, _cat) in &labels {
        let b = gtk::ToggleButton::with_label(label);
        add_css_class(&b, "lixun-chip");
        if let Some(anchor) = group_anchor.as_ref() {
            b.set_group(Some(anchor));
        } else {
            group_anchor = Some(b.clone());
        }
        container.append(&b);
        buttons.push(b);
    }

    buttons[0].set_active(true);

    for (button, (_, cat)) in buttons.iter().zip(labels.iter()) {
        let current_clone = std::rc::Rc::clone(current);
        let cat = *cat;
        button.connect_toggled(move |b| {
            if b.is_active() {
                current_clone.set(cat);
            }
        });
    }

    let buttons_arr: [gtk::ToggleButton; 5] = buttons
        .try_into()
        .expect("exactly 5 chip buttons constructed");

    CategoryChips {
        container,
        buttons: buttons_arr,
    }
}

fn animate_show(window: &gtk::ApplicationWindow) {
    window.remove_css_class("lixun-hiding");
    window.add_css_class("lixun-showing");
    window.set_visible(true);

    let window_weak = window.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
        window_weak.remove_css_class("lixun-showing");
    });
}

fn animate_hide(window: &gtk::ApplicationWindow) {
    window.remove_css_class("lixun-showing");
    window.add_css_class("lixun-hiding");

    let window_weak = window.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
        window_weak.close();
        if let Some(app) = window_weak.application() {
            app.quit();
        }
    });
}
