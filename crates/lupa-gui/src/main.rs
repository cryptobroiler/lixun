//! Lupa GUI — GTK4 + gtk4-layer-shell launcher window.
//!
//! A standalone binary that connects to the lupad daemon via IPC socket
//! and provides a Spotlight-like search interface.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use glib::clone;
use gtk::prelude::*;
use gtk4_layer_shell::LayerShell;
use lupa_core::{Action, Category, Hit};
use lupa_ipc::{socket_path, Request, Response};

use anyhow::Result;

// ---------------------------------------------------------------------------
// IPC helpers
// ---------------------------------------------------------------------------

async fn read_message<R>(reader: &mut R) -> std::io::Result<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut header = [0u8; 4];
    reader.read_exact(&mut header).await?;
    let len = u32::from_be_bytes(header) as usize;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    Ok(buf)
}

async fn write_message<W>(writer: &mut W, payload: &[u8]) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;
    let len = payload.len() as u32;
    let mut header = [0u8; 4];
    header.copy_from_slice(&len.to_be_bytes());
    writer.write_all(&header).await?;
    writer.write_all(payload).await?;
    writer.flush().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// IPC
// ---------------------------------------------------------------------------

async fn run_ipc_loop(
    mut search_rx: tokio::sync::mpsc::Receiver<String>,
    shared_responses: Arc<Mutex<Vec<Response>>>,
) {
    let sock = socket_path();
    let stream = match tokio::net::UnixStream::connect(&sock).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Failed to connect to daemon at {:?}: {}", sock, e);
            return;
        }
    };

    let (mut reader, mut writer) = stream.into_split();

    let shared_for_reader = Arc::clone(&shared_responses);
    tokio::spawn(async move {
        loop {
            match read_message(&mut reader).await {
                Ok(buf) => {
                    match serde_json::from_slice::<Response>(&buf) {
                        Ok(resp) => {
                            if let Ok(mut responses) = shared_for_reader.lock() {
                                responses.push(resp);
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to parse response: {}", e);
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to read from daemon: {}", e);
                    break;
                }
            }
        }
    });

    while let Some(query) = search_rx.recv().await {
        let req = Request::Search {
            q: query,
            limit: 20,
        };
        let json = match serde_json::to_vec(&req) {
            Ok(j) => j,
            Err(e) => {
                tracing::error!("Failed to serialize request: {}", e);
                continue;
            }
        };
        if let Err(e) = write_message(&mut writer, &json).await {
            tracing::error!("Failed to send request to daemon: {}", e);
        }
    }
}

// ---------------------------------------------------------------------------
// Execute action
// ---------------------------------------------------------------------------

fn execute_action(hit: &Hit) -> Result<()> {
    match &hit.action {
        Action::Launch { exec, terminal, working_dir } => {
            if *terminal {
                let mut args: Vec<&str> = vec!["-e"];
                args.extend(exec.split_whitespace());
                std::process::Command::new("alacritty").args(&args).spawn()?;
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
            } else if let Some(parent) = path.parent() {
                std::process::Command::new("xdg-open").arg(parent).spawn()?;
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
        } => {
            let tmp = std::env::temp_dir().join(format!(
                "lupa-att-{}-{}",
                std::process::id(),
                byte_offset
            ));
            use std::io::{Read, Seek, SeekFrom};
            let mut f = std::fs::File::open(mbox_path)?;
            f.seek(SeekFrom::Start(*byte_offset))?;
            let mut data = vec![0u8; *length as usize];
            f.read_exact(&mut data)?;
            std::fs::write(&tmp, &data)?;
            std::process::Command::new("xdg-open").arg(&tmp).spawn()?;
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

        let title = gtk::Label::new(None);
        title.set_hexpand(true);
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        add_css_class(&title, "lupa-title");
        row.append(&title);

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
                    let title = icon.next_sibling().and_downcast::<gtk::Label>().unwrap();
                    title.set_text(&hit.title);
                    let badge = title.next_sibling().and_downcast::<gtk::Label>().unwrap();
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
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let _rt = runtime.enter();

    let (search_tx, search_rx) = tokio::sync::mpsc::channel::<String>(32);
    let shared_responses: Arc<Mutex<Vec<Response>>> = Arc::new(Mutex::new(Vec::new()));
    let shared_for_ipc = Arc::clone(&shared_responses);
    let _ipc = runtime.spawn(run_ipc_loop(search_rx, shared_for_ipc));

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

    let provider = gtk::CssProvider::new();
    provider.load_from_data(
        ".lupa-window { background-color: rgba(30,30,35,0.85); border-radius: 12px; } \
         .lupa-entry { font-size: 28px; padding: 14px 20px; background-color: rgba(20,20,25,0.9); color: #e0e0e0; border: none; border-radius: 8px; } \
         .lupa-results { background-color: transparent; } \
         .lupa-hit { padding: 8px 16px; border-radius: 6px; } \
         .lupa-title { font-weight: bold; color: #f0f0f0; } \
         .lupa-badge { color: #666666; font-size: 10px; }"
    );
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
    let shared_for_poll = Arc::clone(&shared_responses);
    glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
        let mut responses = shared_for_poll.lock().unwrap();
        for resp in responses.drain(..) {
            if let Response::Hits(hits) = resp {
                update_results(&model2, &hits);
            }
        }
        glib::ControlFlow::Continue
    });

    // Debounced search
    let (debounce_tx, debounce_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let search_tx2 = search_tx.clone();
    runtime.spawn(async move { let mut debounce_rx = debounce_rx;
        use tokio::time::{Duration, Instant};
        let mut last_change = Instant::now();
        let mut pending = String::new();
        loop {
            let deadline = last_change + Duration::from_millis(40);
            tokio::select! {
                Some(text) = debounce_rx.recv() => {
                    pending = text;
                    last_change = Instant::now();
                }
                _ = tokio::time::sleep_until(deadline) => {
                    if !pending.is_empty() {
                        let _ = search_tx2.send(pending.clone()).await;
                        pending.clear();
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        }
    });
    entry.connect_changed(move |e| {
        let _ = debounce_tx.send(e.text().to_string());
    });

    // Keyboard nav
    let key_controller = gtk::EventControllerKey::new();
    key_controller.connect_key_pressed(clone!(@strong selection => move |_, key, _, _| {
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
            gtk::gdk::Key::Escape => glib::signal::Propagation::Stop,
            _ => glib::signal::Propagation::Proceed,
        }
    }));
    list_view.add_controller(key_controller);

    // Enter -> execute
    entry.connect_activate(clone!(@strong selection, @strong model => move |_| {
        let idx = selection.selected();
        if let Some(item) = model.item(idx) {
            if let Some(str_obj) = item.downcast_ref::<gtk::StringObject>() {
                let doc_id = str_obj.string().to_string();
                with_cached_hits(|hits| {
                    if let Some(hit) = hits.iter().find(|h| h.id.0 == doc_id) {
                        if let Err(e) = execute_action(hit) {
                            tracing::error!("Failed to execute action: {}", e);
                        }
                    }
                });
            }
        }
    }));

    // Focus-out -> hide
    let focus_ctrl = gtk::EventControllerFocus::new();
    let window_weak = window.clone();
    focus_ctrl.connect_leave(move |_| {
        window_weak.hide();
    });
    window.add_controller(focus_ctrl);

    window.show();

    // Send Toggle to daemon
    let sock = socket_path();
    if sock.exists() {
        let req = Request::Toggle;
        if let Ok(json) = serde_json::to_vec(&req) {
            let len = json.len() as u32;
            let _ = std::os::unix::net::UnixStream::connect(&sock).and_then(|mut s| {
                use std::io::Write;
                let mut buf = Vec::with_capacity(4 + json.len());
                buf.extend_from_slice(&len.to_be_bytes());
                buf.extend_from_slice(&json);
                s.write_all(&buf)
            });
        }
    }

    tracing::info!("Lupa GUI window shown");
    Ok(())
}

fn main() -> Result<()> {
    run()
}
