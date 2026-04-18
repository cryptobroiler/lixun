//! Lupa GUI — GTK4 + gtk4-layer-shell launcher window.
//!
//! A standalone binary that connects to the lupad daemon via IPC socket
//! and provides a Spotlight-like search interface.

use std::cell::RefCell;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

use glib::clone;
use gtk::prelude::*;
use gtk4_layer_shell::LayerShell;
use lupa_core::{Action, Category, Hit};
use lupa_ipc::{PROTOCOL_VERSION, Request, Response, socket_path};

use anyhow::Result;

// ---------------------------------------------------------------------------
// IPC client — background thread with sync UnixStream
// ---------------------------------------------------------------------------

struct IpcClient {
    request_tx: mpsc::Sender<(String, u32)>,
    responses: Arc<Mutex<Vec<Hit>>>,
}

impl Clone for IpcClient {
    fn clone(&self) -> Self {
        Self {
            request_tx: self.request_tx.clone(),
            responses: Arc::clone(&self.responses),
        }
    }
}

fn start_ipc_thread() -> IpcClient {
    let (tx, rx) = mpsc::channel::<(String, u32)>();
    let responses: Arc<Mutex<Vec<Hit>>> = Arc::new(Mutex::new(Vec::new()));
    let resp_clone = Arc::clone(&responses);

    std::thread::spawn(move || {
        while let Ok((query, limit)) = rx.recv() {
            let sock = socket_path();
            let req = Request::Search { q: query, limit };
            let json = match serde_json::to_vec(&req) {
                Ok(j) => j,
                Err(e) => {
                    tracing::error!("Failed to serialize search request: {}", e);
                    continue;
                }
            };
            let total_len = (2 + json.len()) as u32;
            let mut buf = Vec::with_capacity(4 + 2 + json.len());
            buf.extend_from_slice(&total_len.to_be_bytes());
            buf.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
            buf.extend_from_slice(&json);

            let mut stream = match std::os::unix::net::UnixStream::connect(&sock) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("Failed to connect to daemon at {:?}: {}", sock, e);
                    continue;
                }
            };

            if let Err(e) = stream.write_all(&buf) {
                tracing::error!("Failed to send search request: {}", e);
                continue;
            }

            let mut header = [0u8; 4];
            if let Err(e) = stream.read_exact(&mut header) {
                tracing::error!("Failed to read response header: {}", e);
                continue;
            }
            let resp_len = u32::from_be_bytes(header) as usize;
            if resp_len < 2 {
                tracing::error!("Response frame too short");
                continue;
            }
            let mut _version = [0u8; 2];
            if let Err(e) = stream.read_exact(&mut _version) {
                tracing::error!("Failed to read response version: {}", e);
                continue;
            }
            let mut resp_buf = vec![0u8; resp_len - 2];
            if let Err(e) = stream.read_exact(&mut resp_buf) {
                tracing::error!("Failed to read response body: {}", e);
                continue;
            }

            match serde_json::from_slice::<Response>(&resp_buf) {
                Ok(Response::Hits(hits)) => {
                    if let Ok(mut r) = resp_clone.lock() {
                        *r = hits;
                    }
                }
                Ok(Response::Error(msg)) => {
                    tracing::error!("Daemon error: {}", msg);
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::error!("Failed to parse response: {}", e);
                }
            }
        }
    });

    IpcClient {
        request_tx: tx,
        responses,
    }
}

fn send_record_click(doc_id: &str) {
    let sock = socket_path();
    let req = Request::RecordClick {
        doc_id: doc_id.to_string(),
    };
    let Ok(json) = serde_json::to_vec(&req) else {
        return;
    };
    let total_len = (2 + json.len()) as u32;
    let mut buf = Vec::with_capacity(4 + 2 + json.len());
    buf.extend_from_slice(&total_len.to_be_bytes());
    buf.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    buf.extend_from_slice(&json);

    if let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&sock) {
        let _ = stream.write_all(&buf);
    }
}

// ---------------------------------------------------------------------------
// File manager integration
// ---------------------------------------------------------------------------

pub(crate) fn file_uri(abs: &std::path::Path) -> String {
    let mut out = String::from("file://");
    let s = abs.to_string_lossy();
    let mut first = true;
    for seg in s.split('/') {
        if !first {
            out.push('/');
        }
        first = false;
        for b in seg.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(b as char)
                }
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
    }
    out
}

fn show_in_file_manager(path: &std::path::Path) -> Result<()> {
    let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let uri = file_uri(&abs);
    let conn = zbus::blocking::Connection::session()?;
    let _ = conn.call_method(
        Some("org.freedesktop.FileManager1"),
        "/org/freedesktop/FileManager1",
        Some("org.freedesktop.FileManager1"),
        "ShowItems",
        &(vec![uri.as_str()], ""),
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Attachment decoding + temp-file management
// ---------------------------------------------------------------------------

pub(crate) fn decode_attachment(raw: &[u8], encoding: &str) -> Result<Vec<u8>> {
    use base64::Engine;
    match encoding.to_ascii_lowercase().as_str() {
        "base64" => {
            let filtered: Vec<u8> = raw
                .iter()
                .copied()
                .filter(|b| !b.is_ascii_whitespace())
                .collect();
            Ok(base64::engine::general_purpose::STANDARD.decode(filtered)?)
        }
        "quoted-printable" => Ok(quoted_printable::decode(
            raw,
            quoted_printable::ParseMode::Robust,
        )?),
        "7bit" | "8bit" | "binary" | "" => Ok(raw.to_vec()),
        other => anyhow::bail!("unsupported transfer encoding: {other}"),
    }
}

pub(crate) fn sanitize_filename(s: &str) -> String {
    let mut out: String = s
        .chars()
        .filter(|c| !matches!(*c, '/' | '\\' | '\0'))
        .collect();
    out = out.trim_start_matches('.').to_string();
    if out.is_empty() {
        out = "attachment".to_string();
    }
    if out.len() > 200 {
        out.truncate(200);
    }
    out
}

pub(crate) fn sweep_stale_attachments(dir: &std::path::Path, max_age: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for e in entries.flatten() {
        let Ok(meta) = e.metadata() else { continue };
        let Ok(mtime) = meta.modified() else { continue };
        if now.duration_since(mtime).unwrap_or_default() > max_age {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

// ---------------------------------------------------------------------------
// Execute action
// ---------------------------------------------------------------------------

fn execute_action(hit: &Hit) -> Result<()> {
    match &hit.action {
        Action::Launch {
            exec,
            terminal,
            working_dir,
        } => {
            if *terminal {
                let mut args: Vec<&str> = vec!["-e"];
                args.extend(exec.split_whitespace());
                std::process::Command::new("alacritty")
                    .args(&args)
                    .spawn()?;
            } else {
                let mut parts = exec.split_whitespace();
                if let Some(cmd) = parts.next() {
                    let mut builder = std::process::Command::new(cmd);
                    builder.args(parts);
                    if let Some(dir) = working_dir {
                        builder.current_dir(dir);
                    }
                    builder.spawn()?;
                }
            }
            Ok(())
        }
        Action::OpenFile { path } => {
            std::process::Command::new("xdg-open").arg(path).spawn()?;
            Ok(())
        }
        Action::ShowInFileManager { path } => {
            if path.is_dir() {
                std::process::Command::new("xdg-open").arg(path).spawn()?;
            } else {
                match show_in_file_manager(path) {
                    Ok(()) => {}
                    Err(e) => {
                        tracing::debug!(
                            "FileManager1 DBus call failed: {e}; falling back to xdg-open"
                        );
                        if let Some(parent) = path.parent() {
                            std::process::Command::new("xdg-open").arg(parent).spawn()?;
                        }
                    }
                }
            }
            Ok(())
        }
        Action::OpenMail { message_id } => {
            std::process::Command::new("thunderbird")
                .arg("-message")
                .arg(message_id)
                .spawn()?;
            Ok(())
        }
        Action::OpenAttachment {
            mbox_path,
            byte_offset,
            length,
            mime: _,
            encoding,
            suggested_filename,
        } => {
            use std::io::{Read, Seek, SeekFrom};
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

            let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                .ok()
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| {
                    let uid = unsafe { libc::getuid() };
                    std::path::PathBuf::from(format!("/tmp/lupa-{uid}"))
                });
            let dir = runtime_dir.join("lupa/attachments");
            std::fs::create_dir_all(&dir)?;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;

            sweep_stale_attachments(&dir, std::time::Duration::from_secs(600));

            let mut f = std::fs::File::open(mbox_path)?;
            f.seek(SeekFrom::Start(*byte_offset))?;
            let mut raw = vec![0u8; *length as usize];
            f.read_exact(&mut raw)?;

            let decoded = decode_attachment(&raw, encoding)?;

            let safe = sanitize_filename(suggested_filename);
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let target = dir.join(format!("{ts}-{safe}"));
            {
                let mut file = std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .mode(0o600)
                    .open(&target)?;
                std::io::Write::write_all(&mut file, &decoded)?;
            }
            std::process::Command::new("xdg-open")
                .arg(&target)
                .spawn()?;
            Ok(())
        }
        Action::OpenParentMail { message_id } => {
            std::process::Command::new("thunderbird")
                .arg("-message")
                .arg(message_id)
                .spawn()?;
            Ok(())
        }
    }
}

fn execute_secondary_action(hit: &Hit) -> Result<()> {
    match &hit.action {
        Action::ShowInFileManager { path } => {
            if path.is_dir() {
                std::process::Command::new("xdg-open").arg(path).spawn()?;
            } else {
                match show_in_file_manager(path) {
                    Ok(()) => {}
                    Err(e) => {
                        tracing::debug!(
                            "FileManager1 DBus call failed: {e}; falling back to xdg-open"
                        );
                        if let Some(parent) = path.parent() {
                            std::process::Command::new("xdg-open").arg(parent).spawn()?;
                        }
                    }
                }
            }
            Ok(())
        }
        Action::OpenParentMail { message_id } => {
            std::process::Command::new("thunderbird")
                .arg("-message")
                .arg(message_id)
                .spawn()?;
            Ok(())
        }
        _ => Ok(()),
    }
}

fn copy_to_clipboard(hit: &Hit) {
    let text = match &hit.action {
        Action::OpenFile { path } | Action::ShowInFileManager { path } => {
            path.to_string_lossy().to_string()
        }
        Action::OpenMail { message_id } | Action::OpenParentMail { message_id } => {
            message_id.clone()
        }
        Action::OpenAttachment { .. } => hit.title.clone(),
        _ => hit.title.clone(),
    };

    if let Some(display) = gtk::gdk::Display::default() {
        display.clipboard().set_text(&text);
    }
    tracing::info!("Copied to clipboard: {}", text);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn category_icon(cat: &Category) -> &'static str {
    match cat {
        Category::App => "application-x-executable",
        Category::File => "text-x-generic",
        Category::Mail => "mail-message",
        Category::Attachment => "mail-attachment",
    }
}

fn add_css_class<W: gtk::prelude::WidgetExt>(widget: &W, class: &str) {
    widget.style_context().add_class(class);
}

// ---------------------------------------------------------------------------
// Cached hits
// ---------------------------------------------------------------------------

thread_local! {
    static CACHED_HITS: RefCell<Vec<Hit>> = const { RefCell::new(Vec::new()) };
}

fn cache_hits(hits: Vec<Hit>) {
    CACHED_HITS.with(|c| *c.borrow_mut() = hits);
}

fn with_cached_hits<R>(f: impl FnOnce(&[Hit]) -> R) -> R {
    CACHED_HITS.with(|c| f(&c.borrow()))
}

// ---------------------------------------------------------------------------
// Update results
// ---------------------------------------------------------------------------

fn update_results(model: &gtk::StringList, hits: &[Hit]) {
    let n = model.n_items();
    for _ in 0..n {
        model.remove(0);
    }
    cache_hits(hits.to_vec());
    for hit in hits {
        model.append(&hit.id.0);
    }
}

// ---------------------------------------------------------------------------
// List factory
// ---------------------------------------------------------------------------

fn create_list_factory() -> gtk::SignalListItemFactory {
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

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

pub fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("lupa_gui=info".parse().unwrap()),
        )
        .init();

    let app = gtk::Application::builder()
        .application_id("hk.dkp.lupa.gui")
        .build();

    app.connect_activate(|app| {
        if let Err(e) = build_window(app) {
            tracing::error!("Failed to build window: {}", e);
        }
    });

    app.run_with_args(&Vec::<String>::new());
    Ok(())
}

fn build_window(app: &gtk::Application) -> Result<()> {
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

    // Multi-monitor: place on monitor under pointer
    if let Some(display) = gtk::gdk::Display::default() {
        if let Some(seat) = display.default_seat() {
            if let Some(pointer) = seat.pointer() {
                let (surface, _, _) = pointer.surface_at_position();
                if let Some(surface) = surface {
                    let monitor = display.monitor_at_surface(&surface);
                    window.set_monitor(monitor.as_ref());
                }
            }
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

    // Poll responses
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

    // Debounce
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

    // Keyboard nav
    let key_controller = gtk::EventControllerKey::new();
    key_controller.connect_key_pressed(
        clone!(@strong selection, @strong model, @strong window => move |_, key, _keycode, state| {
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
                    if let Some(item) = model.item(idx) {
                        if let Some(str_obj) = item.downcast_ref::<gtk::StringObject>() {
                            let doc_id = str_obj.string().to_string();
                            with_cached_hits(|hits| {
                                if let Some(hit) = hits.iter().find(|h| h.id.0 == doc_id) {
                                    send_record_click(&hit.id.0);
                                    if state.contains(gtk::gdk::ModifierType::SHIFT_MASK) {
                                        if let Err(e) = execute_secondary_action(hit) {
                                            tracing::error!("Secondary action failed: {}", e);
                                        }
                                    } else {
                                        if let Err(e) = execute_action(hit) {
                                            tracing::error!("Failed to execute action: {}", e);
                                        }
                                    }
                                    window.hide();
                                }
                            });
                        }
                    }
                    glib::signal::Propagation::Stop
                }
                gtk::gdk::Key::c | gtk::gdk::Key::C => {
                    if state.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
                        let idx = selection.selected();
                        if let Some(item) = model.item(idx) {
                            if let Some(str_obj) = item.downcast_ref::<gtk::StringObject>() {
                                let doc_id = str_obj.string().to_string();
                                with_cached_hits(|hits| {
                                    if let Some(hit) = hits.iter().find(|h| h.id.0 == doc_id) {
                                        copy_to_clipboard(hit);
                                    }
                                });
                            }
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

    // Enter on entry (no modifier — primary action)
    entry.connect_activate(clone!(@strong selection, @strong model => move |_| {
        let idx = selection.selected();
        if let Some(item) = model.item(idx) {
            if let Some(str_obj) = item.downcast_ref::<gtk::StringObject>() {
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
        }
    }));

    let focus_ctrl = gtk::EventControllerFocus::new();
    let window_for_focus = window.clone();
    focus_ctrl.connect_leave(move |_| {
        animate_hide(&window_for_focus);
    });
    window.add_controller(focus_ctrl);

    animate_show(&window);

    // Send Toggle to daemon
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

#[cfg(test)]
mod tests {
    use super::{decode_attachment, file_uri, sanitize_filename, sweep_stale_attachments};
    use std::path::Path;

    #[test]
    fn test_file_uri_ascii() {
        assert_eq!(file_uri(Path::new("/tmp/foo.txt")), "file:///tmp/foo.txt");
    }

    #[test]
    fn test_file_uri_spaces() {
        assert_eq!(
            file_uri(Path::new("/tmp/hello world.txt")),
            "file:///tmp/hello%20world.txt"
        );
    }

    #[test]
    fn test_file_uri_utf8() {
        assert_eq!(
            file_uri(Path::new("/home/u/Café.pdf")),
            "file:///home/u/Caf%C3%A9.pdf"
        );
    }

    #[test]
    fn test_file_uri_preserves_separators() {
        let result = file_uri(Path::new("/a/b/c/d/e.txt"));
        assert!(!result.contains("%2F"));
        assert!(!result.contains("%2f"));
        assert_eq!(result, "file:///a/b/c/d/e.txt");
    }

    #[test]
    fn test_decode_attachment_base64() {
        use base64::Engine;
        let plain = b"Hello World";
        let encoded = base64::engine::general_purpose::STANDARD.encode(plain);
        let decoded = decode_attachment(encoded.as_bytes(), "base64").unwrap();
        assert_eq!(decoded, plain);
    }

    #[test]
    fn test_decode_attachment_base64_strips_whitespace() {
        let raw = b"SGVs\r\nbG8=";
        let decoded = decode_attachment(raw, "base64").unwrap();
        assert_eq!(decoded, b"Hello");
    }

    #[test]
    fn test_decode_attachment_base64_case_insensitive_encoding_label() {
        let raw = b"SGVsbG8=";
        let decoded = decode_attachment(raw, "BASE64").unwrap();
        assert_eq!(decoded, b"Hello");
    }

    #[test]
    fn test_decode_attachment_qp() {
        let decoded = decode_attachment(b"Hello=20World", "quoted-printable").unwrap();
        assert_eq!(decoded, b"Hello World");
    }

    #[test]
    fn test_decode_attachment_passthrough_variants() {
        for enc in ["7bit", "8bit", "binary", ""] {
            let decoded = decode_attachment(b"Hello World", enc).unwrap();
            assert_eq!(decoded, b"Hello World", "encoding={enc}");
        }
    }

    #[test]
    fn test_decode_attachment_unknown_encoding_errors() {
        let result = decode_attachment(b"data", "rot13");
        assert!(result.is_err());
    }

    #[test]
    fn test_sanitize_filename_strips_path_traversal() {
        let s = sanitize_filename("../../../etc/passwd");
        assert!(!s.contains('/'));
        assert!(!s.contains('\\'));
    }

    #[test]
    fn test_sanitize_filename_strips_backslash_and_nul() {
        let s = sanitize_filename("foo\\bar\0baz");
        assert_eq!(s, "foobarbaz");
    }

    #[test]
    fn test_sanitize_filename_dot_prefix_stripped() {
        assert_eq!(sanitize_filename(".hidden"), "hidden");
        assert_eq!(sanitize_filename("....multi"), "multi");
    }

    #[test]
    fn test_sanitize_filename_empty_falls_back() {
        assert_eq!(sanitize_filename(""), "attachment");
        assert_eq!(sanitize_filename("///"), "attachment");
        assert_eq!(sanitize_filename("..."), "attachment");
    }

    #[test]
    fn test_sanitize_filename_length_cap() {
        let long = "a".repeat(500);
        let out = sanitize_filename(&long);
        assert!(out.len() <= 200);
    }

    #[test]
    fn test_sweep_stale_attachments() {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("lupa-sweep-test-{ts}"));
        std::fs::create_dir_all(&dir).unwrap();

        let old_path = dir.join("old.bin");
        let fresh_path = dir.join("fresh.bin");
        std::fs::write(&old_path, b"old").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(150));
        std::fs::write(&fresh_path, b"fresh").unwrap();

        sweep_stale_attachments(&dir, std::time::Duration::from_millis(75));

        let old_exists = old_path.exists();
        let fresh_exists = fresh_path.exists();
        let _ = std::fs::remove_dir_all(&dir);

        assert!(!old_exists, "stale file should be swept");
        assert!(fresh_exists, "fresh file should survive");
    }

    #[test]
    fn test_sweep_stale_attachments_nonexistent_dir_is_noop() {
        let p = std::path::Path::new("/nonexistent/lupa-sweep-test-path");
        sweep_stale_attachments(p, std::time::Duration::from_secs(1));
    }
}
