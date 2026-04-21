//! Main window construction: layer-shell setup, entry + list, keyboard
//! bindings, animations, Toggle ping to the daemon.
//!
//! Service mode (G1.6): the window is built once per process and toggled
//! via `LauncherController::{show, hide, toggle, quit}` driven by
//! `gui_server`. `animate_hide` no longer calls `app.quit()`; only the
//! daemon's explicit `GuiCommand::Quit` triggers process exit, via
//! `LauncherController::quit`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use gtk::gio;
use gtk::prelude::*;
use gtk4_layer_shell::{Edge, LayerShell};
use lixun_core::Category;

use crate::factory::{
    add_css_class, clear_cached_hits, create_list_factory, update_results, with_cached_hits,
};
use crate::ipc::{IpcClient, start_ipc_thread};
use crate::status::StatusBar;

pub(crate) type CategoryFilter = std::rc::Rc<std::cell::Cell<Option<Category>>>;

/// Frozen snapshot of a user search session, captured on hide and
/// restored on show. Mirrors Spotlight's UX: if the user dismisses
/// the launcher without launching anything (Escape, focus-loss,
/// toggle-off, preview), their query, results, and cursor position
/// survive so the next show picks up exactly where they left off.
///
/// Only a launch action (Enter, double-click, calculator copy)
/// clears this cache — everything else keeps it. A silent
/// background re-search is issued on restore so the displayed rows
/// reflect any fs-watcher or gloda updates that happened while the
/// launcher was hidden.
#[derive(Clone)]
pub(crate) struct SessionSnapshot {
    pub(crate) query: String,
    pub(crate) hits: Vec<lixun_core::Hit>,
    /// DocId of the selected hit at hide time. Restored by DocId
    /// rather than by index because the silent refresh may reorder
    /// the list; matching on identity keeps the cursor on the same
    /// logical item (or falls back to index 0 if it's gone).
    pub(crate) selected_doc_id: Option<String>,
    /// Which category chip was active. `None` = "All".
    pub(crate) category: Option<Category>,
    /// Which chip button index was active (0..4). Saved separately
    /// from `category` because chip 0 is All (category=None) but
    /// distinct from a future explicit "uncategorized" filter.
    pub(crate) chip_index: usize,
}

const EMBEDDED_STYLESHEET: &str = include_str!("../style.css");

pub(crate) const DEFAULT_TOP_MARGIN: i32 = 140;

/// Transition latch duration. `connect_leave` fires spuriously during the
/// show transition on some compositors (Hyprland, sway); ignoring leave
/// events for this window after each show prevents a show-leave-hide
/// flicker cycle. 150 ms covers the 120 ms fade-slide-in animation plus
/// a small compositor focus-settle margin.
const JUST_SHOWED_GUARD_MS: u64 = 150;

/// Lives for the whole GUI process lifetime. Owns every widget the
/// service-mode command handlers (`show`, `hide`, `toggle`, `quit`,
/// `clear_session`) need to mutate, plus the `session_epoch` that the
/// IPC thread checks before committing search replies. All methods
/// assume they are called on the GTK main thread; the `gui_server`
/// module funnels commands here via `glib::spawn_future_local`.
pub(crate) struct LauncherController {
    window: gtk::ApplicationWindow,
    entry: gtk::Entry,
    chips: std::rc::Rc<CategoryChips>,
    selection: gtk::SingleSelection,
    scrolled: gtk::ScrolledWindow,
    status: std::rc::Rc<StatusBar>,
    model: gtk::StringList,
    current_category: CategoryFilter,
    pending_debounce: std::rc::Rc<std::cell::RefCell<Option<glib::SourceId>>>,
    last_query: std::rc::Rc<std::cell::RefCell<String>>,
    session_epoch: Arc<AtomicU64>,
    just_showed_until: std::rc::Rc<std::cell::Cell<Instant>>,
    filter: gtk::CustomFilter,
    /// Snapshot of the last dismissed-but-not-launched session.
    /// Populated by `persist_session` on soft hide, consumed by
    /// `restore_session` on next show, emptied by `clear_session`
    /// on any launch action.
    cached_session: std::rc::Rc<std::cell::RefCell<Option<SessionSnapshot>>>,
    ipc: IpcClient,
    /// Latch set by `restore_session` so the entry's
    /// connect_changed handler can short-circuit its debounced
    /// search. Critical for selection preservation — see
    /// `restore_session` docstring.
    is_restoring: std::rc::Rc<std::cell::Cell<bool>>,
    /// True when the user has explicitly moved the cursor off the
    /// top row (↑ / ↓ / click / restored via cached session). The
    /// response poller only preserves selection by DocId when this
    /// is true; a fresh keystroke, which clears the flag, makes the
    /// poller always snap to row 0 so ranking order wins (Spotlight
    /// semantic). Without this, the preserve-by-DocId path — useful
    /// during silent refresh — would also chase the previous row's
    /// DocId across every keystroke, warping the cursor to wherever
    /// the new ranking happens to place it (reported as "ends up in
    /// middle of list after second query").
    user_selected_override: std::rc::Rc<std::cell::Cell<bool>>,
}

impl LauncherController {
    pub(crate) fn is_visible(&self) -> bool {
        self.window.is_visible()
    }

    /// Make the window visible. Returns the resulting visibility
    /// (`true` on success). Recomputes the monitor so re-shows track
    /// the pointer across multi-monitor setups.
    ///
    /// If a session snapshot was cached by the previous soft-hide,
    /// restore it before presenting: the user sees their prior
    /// query, results, and selection immediately (no flash of empty
    /// launcher), with a silent background refresh catching the
    /// results up to any index changes that happened in between.
    pub(crate) fn show(&self) -> bool {
        self.recompute_monitor();

        let snapshot = self.cached_session.borrow_mut().take();
        if let Some(snapshot) = snapshot {
            self.restore_session(&snapshot);
        }

        self.window.remove_css_class("lixun-hiding");
        self.window.add_css_class("lixun-showing");
        self.window.set_visible(true);
        self.entry.grab_focus();
        self.entry.set_position(-1);
        self.just_showed_until
            .set(Instant::now() + Duration::from_millis(JUST_SHOWED_GUARD_MS));

        let window_weak = self.window.downgrade();
        glib::timeout_add_local_once(Duration::from_millis(120), move || {
            if let Some(w) = window_weak.upgrade() {
                w.remove_css_class("lixun-showing");
            }
        });
        true
    }

    /// Soft-hide: make the window invisible but keep the current
    /// session (query + results + selection) in `cached_session` so
    /// the next `show()` restores it. This is the Spotlight-style
    /// default for every dismiss that is NOT a launch action
    /// (Escape, focus-loss, toggle-off, preview-open, preview-close).
    /// Does NOT exit the process; only `quit()` does.
    pub(crate) fn hide(&self) -> bool {
        self.persist_session();
        self.animate_hide();
        false
    }

    /// Hard-hide: clear the session completely, then hide. Used by
    /// every launch-completing action (Enter, primary/secondary,
    /// double-click, calculator copy) where the user has finished
    /// the task and expects a fresh launcher next time.
    pub(crate) fn clear_and_hide(&self) -> bool {
        self.clear_session();
        self.animate_hide();
        false
    }

    fn animate_hide(&self) {
        self.window.remove_css_class("lixun-showing");
        self.window.add_css_class("lixun-hiding");

        let window_weak = self.window.downgrade();
        glib::timeout_add_local_once(Duration::from_millis(120), move || {
            if let Some(w) = window_weak.upgrade() {
                w.set_visible(false);
                w.remove_css_class("lixun-hiding");
            }
        });
    }

    /// Flip visibility. Single source of truth for service-mode toggle:
    /// daemon just sends `GuiCommand::Toggle`, the GUI inspects
    /// `window.is_visible()` and picks show or hide.
    pub(crate) fn toggle(&self) -> bool {
        if self.window.is_visible() {
            self.hide()
        } else {
            self.show()
        }
    }

    /// Exit the GTK application. Called only from the daemon's
    /// `GuiCommand::Quit` path (graceful shutdown).
    pub(crate) fn quit(&self) {
        self.window.close();
        if let Some(app) = self.window.application() {
            app.quit();
        }
    }

    /// Drop only the cached session snapshot without touching the
    /// live UI state. Called by `GuiCommand::ClearSession` from the
    /// daemon after a preview process exits with the "launched"
    /// sentinel — the launcher is already hidden (persist_session
    /// fired during the Space → preview handoff), and we only need
    /// to invalidate the cache so the next show opens blank.
    pub(crate) fn drop_cached_session(&self) {
        self.cached_session.borrow_mut().take();
    }

    /// Mark the selection as user-chosen so the response poller's
    /// preserve-by-DocId path activates on the next reply. Called
    /// by keymap navigation (↑/↓/Ctrl variants) and factory
    /// click/tap handlers. Cleared automatically on fresh keystroke
    /// in the entry handler.
    pub(crate) fn mark_user_selected(&self) {
        self.user_selected_override.set(true);
    }

    /// Reset every piece of session state so the next show is clean.
    /// Called by launch-completing actions via `clear_and_hide`,
    /// and on explicit cache invalidation. Bumps `session_epoch`
    /// first so any in-flight search replies land in a new epoch
    /// and get discarded by the IPC poller. Drops the cached
    /// session snapshot — after this, the next show is a blank
    /// launcher.
    pub(crate) fn clear_session(&self) {
        self.session_epoch.fetch_add(1, Ordering::SeqCst);
        self.cached_session.borrow_mut().take();
        self.user_selected_override.set(false);

        if let Some(id) = self.pending_debounce.borrow_mut().take() {
            id.remove();
        }

        self.entry.set_text("");
        self.chips.activate_index(0);
        self.current_category.set(None);
        self.filter.changed(gtk::FilterChange::Different);

        let n = self.model.n_items();
        for _ in 0..n {
            self.model.remove(0);
        }
        self.selection.set_selected(gtk::INVALID_LIST_POSITION);
        clear_cached_hits();

        self.scrolled.set_visible(false);
        self.scrolled.set_vexpand(false);
        self.chips.container.set_visible(false);
        self.status.hide();

        self.last_query.borrow_mut().clear();
    }

    /// Capture the current session into `cached_session` and then
    /// quiesce in-flight IPC + debounce without touching the UI
    /// state (entry text, model items, selection remain intact in
    /// case of abort — though nothing currently aborts a hide).
    /// The UI itself gets hidden by `animate_hide`; this method
    /// only deals with state management.
    ///
    /// Called by soft-hide paths: Escape, focus-loss, toggle-off,
    /// preview invocation. If the current query is empty there is
    /// no session worth saving — clear the cache instead, so a
    /// "blank launcher → Escape → Super+Space" cycle doesn't
    /// restore a ghost of some previous non-empty session.
    fn persist_session(&self) {
        self.session_epoch.fetch_add(1, Ordering::SeqCst);
        if let Some(id) = self.pending_debounce.borrow_mut().take() {
            id.remove();
        }

        let query = self.entry.text().to_string();
        if query.is_empty() {
            self.cached_session.borrow_mut().take();
            return;
        }

        let selected_doc_id = {
            let idx = self.selection.selected();
            self.selection.item(idx).and_then(|obj| {
                obj.downcast::<gtk::StringObject>()
                    .ok()
                    .map(|s| s.string().to_string())
            })
        };

        let hits = with_cached_hits(|h| h.to_vec());
        let snapshot = SessionSnapshot {
            query,
            hits,
            selected_doc_id,
            category: self.current_category.get(),
            chip_index: self.chips.active_index().unwrap_or(0),
        };
        *self.cached_session.borrow_mut() = Some(snapshot);
    }

    /// Restore a `SessionSnapshot` captured by `persist_session`
    /// into the UI and fire a silent background re-search so the
    /// displayed rows catch up with any index updates that
    /// happened while the launcher was hidden.
    ///
    /// Two non-obvious gotchas:
    ///
    /// - `is_restoring` latch neutralises the entry's
    ///   connect_changed handler while we call `entry.set_text`.
    ///   Without it the handler schedules a duplicate debounced
    ///   search that races our silent refresh; whichever reply
    ///   wins hits the response poller, which then recomputes
    ///   the cursor from its own `prior_selected` snapshot and
    ///   can land on the wrong row.
    ///
    /// - `filter.changed` must be called AFTER `update_results`,
    ///   not only before. FilterListModel does not recompute
    ///   `n_items` eagerly on child-model append; without the
    ///   second invalidation the DocId lookup below sees an empty
    ///   filtered view and falls back to index 0 — which is the
    ///   exact bug report ("selection always 1st row") this
    ///   method exists to fix.
    fn restore_session(&self, snapshot: &SessionSnapshot) {
        self.is_restoring.set(true);

        self.chips.activate_index(snapshot.chip_index);
        self.current_category.set(snapshot.category);
        self.filter.changed(gtk::FilterChange::Different);

        *self.last_query.borrow_mut() = snapshot.query.clone();

        update_results(&self.model, &self.selection, &snapshot.hits);
        self.filter.changed(gtk::FilterChange::Different);

        let selected_idx = snapshot
            .selected_doc_id
            .as_deref()
            .and_then(|want| {
                (0..self.selection.n_items()).find(|&i| {
                    self.selection
                        .item(i)
                        .and_then(|o| o.downcast::<gtk::StringObject>().ok())
                        .map(|s| s.string() == want)
                        .unwrap_or(false)
                })
            })
            .unwrap_or(0);
        if self.selection.n_items() > 0 {
            self.selection.set_selected(selected_idx);
        }

        self.chips.container.set_visible(true);
        if !snapshot.hits.is_empty() {
            self.scrolled.set_visible(true);
            self.scrolled.set_vexpand(true);
        }

        self.entry.set_text(&snapshot.query);
        self.entry.set_position(-1);

        // The restored cursor is user intent from the prior session;
        // arm the override so the silent refresh's poller run
        // preserves the DocId we just selected rather than snapping
        // the cursor to row 0.
        self.user_selected_override.set(true);

        let epoch = self.session_epoch.load(Ordering::SeqCst);
        let _ = self
            .ipc
            .request_tx
            .send((snapshot.query.clone(), 30, epoch));

        self.is_restoring.set(false);
    }

    fn recompute_monitor(&self) {
        if let Some(display) = gtk::gdk::Display::default()
            && let Some(seat) = display.default_seat()
            && let Some(pointer) = seat.pointer()
        {
            let (surface, _, _) = pointer.surface_at_position();
            if let Some(surface) = surface {
                let monitor = display.monitor_at_surface(&surface);
                self.window.set_monitor(monitor.as_ref());
            }
        }
    }
}

pub(crate) fn build_window(app: &gtk::Application) -> Result<()> {
    let session_epoch = Arc::new(AtomicU64::new(0));
    let ipc = start_ipc_thread(Arc::clone(&session_epoch));
    let daemon_config = lixun_daemon::config::Config::load()?;

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .default_width(720)
        .decorated(false)
        .build();
    window.set_widget_name("lixun-root");

    window.init_layer_shell();
    window.set_layer(gtk4_layer_shell::Layer::Overlay);
    // Anchor only Top. Leaving Left and Right unanchored lets the
    // layer-shell compositor center the window horizontally on the
    // monitor — anchoring both edges would stretch the surface to
    // the full screen width, which we explicitly do not want here.
    // Vertical position is pinned by the top margin.
    window.set_anchor(Edge::Top, true);
    window.set_keyboard_mode(gtk4_layer_shell::KeyboardMode::OnDemand);

    window.set_margin(Edge::Top, DEFAULT_TOP_MARGIN);
    add_css_class(&window, "lixun-window");

    let display = gtk::gdk::Display::default().unwrap();

    // Resolve window size as a percentage of the primary monitor.
    // The launcher opens on the monitor containing the pointer at
    // spawn time, but at this point we don't know which monitor —
    // `window.monitor()` returns None until present(). Use the
    // first monitor as a reasonable default; if the user has
    // wildly different monitor sizes this will still look correct
    // within each monitor's proportions because the CSS min-width
    // / min-height act as absolute floors.
    if let Some(monitor) = display.monitors().item(0).and_downcast::<gtk::gdk::Monitor>() {
        let geom = monitor.geometry();
        let w = (geom.width() * i32::from(daemon_config.gui.width_percent) / 100)
            .min(daemon_config.gui.max_width_px);
        let h = (geom.height() * i32::from(daemon_config.gui.height_percent) / 100)
            .min(daemon_config.gui.max_height_px);
        window.set_default_size(w, h);
        window.set_size_request(w, h);
    }

    let provider = gtk::CssProvider::new();
    provider.load_from_string(EMBEDDED_STYLESHEET);
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    lixun_preview::install_user_css(&display);

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 6);
    vbox.set_margin_start(16);
    vbox.set_margin_end(16);
    vbox.set_margin_top(12);
    // Minimal bottom margin — the scrolled results area is the last
    // child and we want it to extend as close to the window bottom
    // as possible to maximise visible row count.
    vbox.set_margin_bottom(2);

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

    // Scrolled size policy lives in CSS (.lixun-results min-height)
    // to keep all layout tuning in one place. Avoid hardcoding
    // content height here — CSS wins anyway via GTK's cascade.
    let scrolled = gtk::ScrolledWindow::builder().vexpand(true).build();
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

    let chips_rc = std::rc::Rc::new(chips);
    let pending_debounce: std::rc::Rc<std::cell::RefCell<Option<glib::SourceId>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let last_query: std::rc::Rc<std::cell::RefCell<String>> =
        std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    let just_showed_until: std::rc::Rc<std::cell::Cell<Instant>> =
        std::rc::Rc::new(std::cell::Cell::new(Instant::now()));

    let cached_session: std::rc::Rc<std::cell::RefCell<Option<SessionSnapshot>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let is_restoring: std::rc::Rc<std::cell::Cell<bool>> =
        std::rc::Rc::new(std::cell::Cell::new(false));
    let user_selected_override: std::rc::Rc<std::cell::Cell<bool>> =
        std::rc::Rc::new(std::cell::Cell::new(false));

    let controller = std::rc::Rc::new(LauncherController {
        window: window.clone(),
        entry: entry.clone(),
        chips: std::rc::Rc::clone(&chips_rc),
        selection: selection.clone(),
        scrolled: scrolled.clone(),
        status: std::rc::Rc::clone(&status_bar),
        model: model.clone(),
        current_category: std::rc::Rc::clone(&current_category),
        pending_debounce: std::rc::Rc::clone(&pending_debounce),
        last_query: std::rc::Rc::clone(&last_query),
        session_epoch: Arc::clone(&session_epoch),
        just_showed_until: std::rc::Rc::clone(&just_showed_until),
        filter: filter.clone(),
        cached_session: std::rc::Rc::clone(&cached_session),
        ipc: ipc.clone(),
        is_restoring: std::rc::Rc::clone(&is_restoring),
        user_selected_override: std::rc::Rc::clone(&user_selected_override),
    });

    let close_action = gio::SimpleAction::new("close-launcher", None);
    let controller_for_close = std::rc::Rc::clone(&controller);
    close_action.connect_activate(move |_, _| {
        controller_for_close.hide();
    });
    app.add_action(&close_action);

    let clear_action = gio::SimpleAction::new("clear-and-hide-launcher", None);
    let controller_for_clear = std::rc::Rc::clone(&controller);
    clear_action.connect_activate(move |_, _| {
        controller_for_clear.clear_and_hide();
    });
    app.add_action(&clear_action);

    install_response_poller(
        ipc.clone(),
        model.clone(),
        filter.clone(),
        selection.clone(),
        list_view.clone(),
        chips_rc.container.clone(),
        scrolled.clone(),
        std::rc::Rc::clone(&status_bar),
        std::rc::Rc::clone(&last_query),
        std::rc::Rc::clone(&user_selected_override),
    );

    install_entry_handler(
        &entry,
        ipc.clone(),
        model.clone(),
        selection.clone(),
        chips_rc.container.clone(),
        scrolled.clone(),
        std::rc::Rc::clone(&status_bar),
        std::rc::Rc::clone(&last_query),
        std::rc::Rc::clone(&pending_debounce),
        Arc::clone(&session_epoch),
        std::rc::Rc::clone(&is_restoring),
        std::rc::Rc::clone(&user_selected_override),
    );

    crate::keymap::install_keyboard_handler(
        &window,
        &list_view,
        &entry,
        &selection,
        &filter_model,
        &model,
        std::rc::Rc::clone(&chips_rc),
        std::rc::Rc::clone(&status_bar),
        &scrolled,
        &chips_rc.container,
        ipc.clone(),
        daemon_config.keybindings.clone(),
        std::rc::Rc::clone(&controller),
    );

    let focus_ctrl = gtk::EventControllerFocus::new();
    let entry_for_focus_enter = entry.clone();
    focus_ctrl.connect_enter(move |_| {
        entry_for_focus_enter.grab_focus();
    });
    let controller_for_leave = std::rc::Rc::clone(&controller);
    let just_showed_for_leave = std::rc::Rc::clone(&just_showed_until);
    focus_ctrl.connect_leave(move |_| {
        if Instant::now() < just_showed_for_leave.get() {
            tracing::debug!("gui: spurious leave during show transition, ignored");
            return;
        }
        controller_for_leave.hide();
    });
    window.add_controller(focus_ctrl);

    controller.show();

    crate::gui_server::start(std::rc::Rc::clone(&controller))?;

    tracing::info!("Lixun GUI window shown");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn install_response_poller(
    ipc: IpcClient,
    model: gtk::StringList,
    filter: gtk::CustomFilter,
    selection: gtk::SingleSelection,
    list_view: gtk::ListView,
    chips_container: gtk::Box,
    scrolled: gtk::ScrolledWindow,
    status: std::rc::Rc<StatusBar>,
    last_query: std::rc::Rc<std::cell::RefCell<String>>,
    user_selected_override: std::rc::Rc<std::cell::Cell<bool>>,
) {
    let responses = Arc::clone(&ipc.responses);
    let calculation = Arc::clone(&ipc.calculation);
    let epoch = Arc::clone(&ipc.response_epoch);
    let last_epoch = std::rc::Rc::new(std::cell::Cell::new(0u64));

    glib::timeout_add_local(Duration::from_millis(50), move || {
        let current = epoch.load(Ordering::SeqCst);
        if current != last_epoch.get() {
            last_epoch.set(current);
            // Snapshot the DocId of the currently-selected row before
            // we clear the model. A silent refresh fired by
            // restore_session lands here too, and we must not warp
            // the cursor off the user's restored choice just because
            // the index generated a fresh ranking. If the same DocId
            // is present in the new hits we restore the cursor
            // there; otherwise we fall back to position 0 as a new
            // search does.
            // DocId-based selection preservation only runs when
            // the user has explicitly moved the cursor off row 0
            // (via ↑/↓/click/restored snapshot). On plain typing
            // the override is cleared by the entry handler, and
            // the poller snaps to row 0 so ranking order wins —
            // matching the Spotlight UX where each new query
            // starts at the top. Without this distinction, the
            // cursor landed on row 0 after one keystroke, then
            // chased that row 0 DocId into wherever the next
            // ranking placed it (often mid-list or near the end).
            let preserve_doc_id = user_selected_override.get();
            let prior_selected = if preserve_doc_id {
                let idx = selection.selected();
                selection.item(idx).and_then(|obj| {
                    obj.downcast::<gtk::StringObject>()
                        .ok()
                        .map(|s| s.string().to_string())
                })
            } else {
                None
            };
            let hits_snapshot = {
                let mut hits = responses.lock().unwrap();
                std::mem::take(&mut *hits)
            };
            let calc_snapshot = {
                let mut c = calculation.lock().unwrap();
                c.take()
            };
            update_results(&model, &selection, &hits_snapshot);
            filter.changed(gtk::FilterChange::Different);
            if !hits_snapshot.is_empty() {
                // Compute the desired index in the source hits list,
                // then translate it to the filtered view position by
                // looking up the StringObject (DocId) in the live
                // SingleSelection (which iterates the filtered
                // FilterListModel). set_selected on a filter-backed
                // SingleSelection silently no-ops when the requested
                // index exceeds the post-filter n_items, so we must
                // resolve against selection.n_items() — not the raw
                // hits_snapshot length — and we must do it AFTER the
                // filter.changed invalidation above so n_items
                // reflects the new view.
                let wanted_doc = prior_selected.clone().or_else(|| {
                    hits_snapshot.first().map(|h| h.id.0.clone())
                });
                let new_idx = wanted_doc
                    .as_deref()
                    .and_then(|want| {
                        (0..selection.n_items()).find(|&i| {
                            selection
                                .item(i)
                                .and_then(|o| o.downcast::<gtk::StringObject>().ok())
                                .map(|s| s.string() == want)
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(0);
                if selection.n_items() > 0 {
                    selection.set_selected(new_idx);
                    list_view.scroll_to(new_idx, gtk::ListScrollFlags::NONE, None);
                }
            }

            if let Some(calc) = calc_snapshot.as_ref() {
                chips_container.set_visible(true);
                scrolled.set_visible(false);
                scrolled.set_vexpand(false);
                status.show_calculation(calc);
            } else if hits_snapshot.is_empty() {
                let q = last_query.borrow().clone();
                if !q.is_empty() {
                    chips_container.set_visible(true);
                    scrolled.set_visible(false);
                    scrolled.set_vexpand(false);
                    status.show_empty(&q);
                    selection.set_selected(gtk::INVALID_LIST_POSITION);
                } else {
                    chips_container.set_visible(false);
                    scrolled.set_visible(false);
                    scrolled.set_vexpand(false);
                    status.hide();
                }
            } else {
                chips_container.set_visible(true);
                scrolled.set_visible(true);
                scrolled.set_vexpand(true);
                status.hide();
            }
        }
        glib::ControlFlow::Continue
    });
}

#[allow(clippy::too_many_arguments)]
fn install_entry_handler(
    entry: &gtk::Entry,
    ipc: IpcClient,
    model: gtk::StringList,
    selection: gtk::SingleSelection,
    chips_container: gtk::Box,
    scrolled: gtk::ScrolledWindow,
    status: std::rc::Rc<StatusBar>,
    last_query: std::rc::Rc<std::cell::RefCell<String>>,
    pending_debounce: std::rc::Rc<std::cell::RefCell<Option<glib::SourceId>>>,
    session_epoch: Arc<AtomicU64>,
    is_restoring: std::rc::Rc<std::cell::Cell<bool>>,
    user_selected_override: std::rc::Rc<std::cell::Cell<bool>>,
) {
    entry.connect_changed(move |e| {
        if is_restoring.get() {
            return;
        }
        let text = e.text().to_string();

        if let Some(id) = pending_debounce.borrow_mut().take() {
            id.remove();
        }

        if text.is_empty() {
            // Disable autoselect around the bulk clear so
            // SingleSelection's interpolation formula
            // (gtksingleselection.c:253-296) does not drift the
            // selected index toward the end of the list on every
            // per-row items-changed emission. Re-enable after the
            // clear and pin selection to INVALID explicitly.
            selection.set_autoselect(false);
            let n = model.n_items();
            for _ in 0..n {
                model.remove(0);
            }
            selection.set_selected(gtk::INVALID_LIST_POSITION);
            selection.set_autoselect(true);
            user_selected_override.set(false);
            chips_container.set_visible(false);
            scrolled.set_visible(false);
            scrolled.set_vexpand(false);
            status.hide();
            return;
        }

        // Fresh keystroke => fresh ranking, row 0 wins. Clear the
        // override so the poller snaps to row 0 on this response.
        user_selected_override.set(false);

        chips_container.set_visible(true);

        let ipc = ipc.clone();
        let status = std::rc::Rc::clone(&status);
        let q = text.clone();
        let last_q = std::rc::Rc::clone(&last_query);
        let pending_self = std::rc::Rc::clone(&pending_debounce);
        let epoch = Arc::clone(&session_epoch);
        let id = glib::timeout_add_local_once(Duration::from_millis(80), move || {
            *last_q.borrow_mut() = q.clone();
            status.show_loading();
            let epoch_snapshot = epoch.load(Ordering::SeqCst);
            let _ = ipc.request_tx.send((q, 30, epoch_snapshot));
            *pending_self.borrow_mut() = None;
        });
        *pending_debounce.borrow_mut() = Some(id);
    });
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

    pub(crate) fn active_index(&self) -> Option<usize> {
        self.buttons.iter().position(|b| b.is_active())
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
