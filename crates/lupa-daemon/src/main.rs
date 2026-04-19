//! lupad — Lupa daemon: IPC server, indexer, filesystem watcher.

#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use anyhow::Result;
use bytes::{BufMut, BytesMut};
use chrono::Utc;
use futures::StreamExt;
use lupa_ipc::{MIN_PROTOCOL_VERSION, PROTOCOL_VERSION, Request, Response, socket_path};
use std::os::unix::io::AsRawFd;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::RwLock;


mod history;
mod query_log;
use history::ClickHistory;
use query_log::QueryLog;

use lupa_daemon::config;
use lupa_daemon::hotkeys;
use lupa_daemon::index_service::{self, IndexMutationTx, SearchHandle};
use lupa_daemon::indexer;

use lupa_indexer::plugin_fs_watcher;
use lupa_indexer::watcher;

#[derive(Debug, Clone, Default)]
struct IndexStats {
    indexed_docs: u64,
    last_reindex: Option<chrono::DateTime<Utc>>,
}

#[derive(Debug, Clone, Default)]
struct GuiState {
    visible: bool,
    pid: Option<u32>,
}

fn process_alive(pid: u32) -> bool {
    // Treat zombie (exited but not reaped) processes as NOT alive.
    // kill(pid, 0) returns 0 even for zombies, so we also inspect /proc status.
    let rc = unsafe { libc::kill(pid as i32, 0) };
    let kill_said_alive = rc == 0
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    if !kill_said_alive {
        return false;
    }
    let status_path = format!("/proc/{}/status", pid);
    let Ok(status) = std::fs::read_to_string(&status_path) else {
        return false;
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("State:") {
            let trimmed = rest.trim_start();
            if trimmed.starts_with('Z') || trimmed.starts_with('X') {
                return false;
            }
            return true;
        }
    }
    true
}

fn spawn_gui(
    gui_state: Arc<RwLock<GuiState>>,
) -> anyhow::Result<u32> {
    let mut child = tokio::process::Command::new("lupa-gui").spawn()?;
    let pid = child
        .id()
        .ok_or_else(|| anyhow::anyhow!("spawned lupa-gui has no pid"))?;
    tokio::spawn(async move {
        let _ = child.wait().await;
        let mut state = gui_state.write().await;
        if state.pid == Some(pid) {
            state.pid = None;
            state.visible = false;
        }
    });
    Ok(pid)
}

fn terminate_gui(pid: u32) {
    let _ = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
}

fn pid_path() -> std::path::PathBuf {
    let runtime = dirs::runtime_dir().unwrap_or_else(|| {
        std::path::PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }))
    });
    runtime.join("lupa.pid")
}

fn try_single_instance() -> Result<std::fs::File> {
    let path = pid_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)?;

    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if ret != 0 {
        anyhow::bail!(
            "another instance of lupad is already running (pid file: {:?})",
            path
        );
    }

    use std::io::Write;
    let _ = file.set_len(0);
    let _ = writeln!(&file, "{}", std::process::id());
    Ok(file)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive("lupa=info".parse()?),
        )
        .init();

    tracing::info!("lupad starting...");

    let _lock = try_single_instance()?;

    let config = config::Config::load()?;
    tracing::info!("Config loaded: roots={:?}", config.roots);

    lupa_extract::init_capabilities(lupa_extract::ExtractorCapabilities::probe(
        std::time::Duration::from_secs(config.extractor_timeout_secs),
    ));

    let index_path = config.state_dir.join("index");
    let registry = {
        let mut r = lupa_indexer::SourceRegistry::new();
        let sources_state_dir = config.state_dir.join("sources");
        register_builtin_sources(&mut r, &config, &sources_state_dir);
        Arc::new(r)
    };
    let (index, rebuilt_from_scratch) = lupa_index::LupaIndex::create_or_open_with_plugins(
        index_path.to_str().unwrap(),
        &registry.plugin_fields_by_kind,
    )?;

    let stats = Arc::new(RwLock::new(IndexStats::default()));
    let state_dir = config.state_dir.clone();
    let watcher_roots = config.roots.clone();
    let watcher_exclude = config.exclude.clone();
    let watcher_exclude_regex = config.exclude_regex.clone();
    let watcher_max_size = config.max_file_size_mb;
    let shared_config = Arc::new(config);

    let history = ClickHistory::load(&state_dir)?;
    let history = Arc::new(RwLock::new(history));

    let query_log = QueryLog::load(&state_dir)?;
    let query_log = Arc::new(RwLock::new(query_log));

    let gui_state = Arc::new(RwLock::new(GuiState::default()));

    let global_toggle_rx = hotkeys::spawn_global_toggle_listener(
        shared_config.keybindings.global_toggle.clone(),
        state_dir.clone(),
    ).await;
    let mut global_toggle_rx = match global_toggle_rx {
        Ok(rx) => Some(rx),
        Err(e) => {
            tracing::warn!("Global shortcut portal unavailable: {}", e);
            None
        }
    };

    let (mutation_tx, search, _writer_handle) = index_service::spawn_writer_service(index)?;

    let indexer_mutation = mutation_tx.clone();
    let indexer_search = search.clone();
    let indexer_stats = Arc::clone(&stats);
    let indexer_state_dir = state_dir.clone();
    let indexer_config = Arc::clone(&shared_config);
    let indexer_registry = Arc::clone(&registry);
    tokio::spawn(async move {
        if let Err(e) = run_incremental_indexer(
            indexer_mutation,
            indexer_search,
            indexer_stats,
            indexer_state_dir,
            indexer_config,
            indexer_registry,
            rebuilt_from_scratch,
        )
        .await
        {
            tracing::error!("Incremental indexer error: {}", e);
        }
    });

    let watcher_mutation = mutation_tx.clone();
    tokio::spawn(async move {
        if let Err(e) = watcher::start(
            watcher_roots,
            watcher_exclude,
            watcher_exclude_regex,
            watcher_max_size,
            watcher_mutation,
        )
        .await
        {
            tracing::error!("Watcher error: {}", e);
        }
    });

    let indexer_sink = Arc::new(lupa_indexer::WriterSink::new(mutation_tx.clone()));
    {
        let tick_handles = lupa_indexer::tick_scheduler::spawn_all(&registry, indexer_sink.clone());
        tracing::info!(
            "tick scheduler: {} source instance(s) with tick_interval",
            tick_handles.len()
        );
        std::mem::forget(tick_handles);
    }

    {
        let pfw_registry = Arc::clone(&registry);
        let pfw_sink = indexer_sink.clone();
        tokio::spawn(async move {
            if let Err(e) = plugin_fs_watcher::start(pfw_registry, pfw_sink).await {
                tracing::error!("plugin fs watcher error: {}", e);
            }
        });
    }

    let socket_path = socket_path();
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }

    tracing::info!("Listening on {:?}", socket_path);

    let listener = tokio::net::UnixListener::bind(&socket_path)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(&socket_path)?;
        let mut perms = metadata.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&socket_path, perms)?;
    }

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);

    let shutdown_tx_signal = shutdown_tx.clone();
    let mut signals = signal_hook_tokio::Signals::new([
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
    ])?;
    tokio::spawn(async move {
        if let Some(sig) = signals.next().await {
            tracing::info!("Received signal {}, shutting down...", sig);
            let _ = shutdown_tx_signal.send(()).await;
        }
    });

    #[allow(unreachable_code)]
    loop {
        tokio::select! {
            _ = async {
                if let Some(rx) = &mut global_toggle_rx {
                    rx.recv().await
                } else {
                    futures::future::pending().await
                }
            } => {
                let mut state = gui_state.write().await;
                if let Some(pid) = state.pid
                    && !process_alive(pid)
                {
                    state.pid = None;
                    state.visible = false;
                }
                if state.visible {
                    if let Some(pid) = state.pid {
                        terminate_gui(pid);
                    }
                    state.pid = None;
                    state.visible = false;
                } else {
                    match spawn_gui(Arc::clone(&gui_state)) {
                        Ok(pid) => {
                            state.pid = Some(pid);
                            state.visible = true;
                        }
                        Err(e) => tracing::error!("Failed to spawn GUI from global shortcut: {}", e),
                    }
                }
            }
            result = listener.accept() => {
                let (stream, _) = match result {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::debug!("Accept error: {}", e);
                        continue;
                    }
                };
                let search = search.clone();
                let mutation_tx = mutation_tx.clone();
                let history = Arc::clone(&history);
                let query_log = Arc::clone(&query_log);
                let stats = Arc::clone(&stats);
                let gui_state = Arc::clone(&gui_state);
                let shared_config = Arc::clone(&shared_config);
                let client_registry = Arc::clone(&registry);

                tokio::spawn(async move {
                    if let Err(e) = handle_client(stream, search, mutation_tx, history, query_log, stats, gui_state, shared_config, client_registry).await {
                        tracing::debug!("Client error: {}", e);
                    }
                });
            }
            _ = shutdown_rx.recv() => {
                tracing::info!("Shutting down gracefully...");
                let _ = std::fs::remove_file(&socket_path);
                let history = history.read().await;
                if let Err(e) = history.save(&state_dir) {
                    tracing::error!("Failed to save click history: {}", e);
                }
                let log = query_log.read().await;
                if let Err(e) = log.save(&state_dir) {
                    tracing::error!("Failed to save query log: {}", e);
                }
                tracing::info!("Shutdown complete");
                std::process::exit(0);
            }
        }
    }

    #[allow(unreachable_code)]
    Ok(())
}

async fn run_incremental_indexer(
    mutation_tx: IndexMutationTx,
    search: SearchHandle,
    stats: Arc<RwLock<IndexStats>>,
    state_dir: std::path::PathBuf,
    config: Arc<config::Config>,
    registry: Arc<lupa_indexer::SourceRegistry>,
    rebuilt_from_scratch: bool,
) -> Result<()> {
    let (fs_count, other_count) = indexer::run_incremental(
        &mutation_tx,
        &search,
        config.as_ref(),
        registry.as_ref(),
        &state_dir,
        rebuilt_from_scratch,
    )
    .await?;
    let total = fs_count + other_count;
    if total > 0 {
        let mut stats_lock = stats.write().await;
        stats_lock.indexed_docs += total as u64;
        stats_lock.last_reindex = Some(Utc::now());
    }
    Ok(())
}

async fn do_reindex(
    mutation_tx: &IndexMutationTx,
    stats: &Arc<RwLock<IndexStats>>,
    config: &config::Config,
    registry: &lupa_indexer::SourceRegistry,
    state_dir: &std::path::Path,
    paths: Vec<std::path::PathBuf>,
) -> Result<usize, anyhow::Error> {
    let count = if paths.is_empty() {
        indexer::reindex_full(mutation_tx, config, registry, state_dir)
            .await?
            .total_docs
    } else {
        indexer::reindex_paths(mutation_tx, config, &paths).await?
    };

    {
        let mut stats_lock = stats.write().await;
        stats_lock.indexed_docs += count as u64;
        stats_lock.last_reindex = Some(Utc::now());
    }

    tracing::info!("Reindex complete: {} documents processed", count);
    Ok(count)
}

fn collect_watcher_stats() -> lupa_ipc::WatcherStats {
    let (directories, excluded, errors, overflow_events) = watcher::stats();
    lupa_ipc::WatcherStats {
        directories,
        excluded,
        errors,
        overflow_events,
    }
}

fn collect_writer_stats() -> lupa_ipc::WriterStats {
    let (commits, last_commit_latency_ms, generation) = index_service::stats();
    lupa_ipc::WriterStats {
        commits,
        last_commit_latency_ms: last_commit_latency_ms.min(u32::MAX as u64) as u32,
        generation,
    }
}

fn collect_memory_stats() -> lupa_ipc::MemoryStats {
    let Ok(text) = std::fs::read_to_string("/proc/self/status") else {
        return lupa_ipc::MemoryStats::default();
    };
    let mut m = lupa_ipc::MemoryStats::default();
    for line in text.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let Some(kb_str) = rest.split_whitespace().next() else {
            continue;
        };
        let Ok(kb) = kb_str.parse::<u64>() else {
            continue;
        };
        let bytes = kb.saturating_mul(1024);
        match key {
            "VmRSS" => m.rss_bytes = bytes,
            "VmPeak" => m.vm_peak_bytes = bytes,
            "VmSize" => m.vm_size_bytes = bytes,
            "VmSwap" => m.vm_swap_bytes = bytes,
            _ => {}
        }
    }
    m
}

#[allow(clippy::too_many_arguments)]
async fn handle_client(
    mut stream: tokio::net::UnixStream,
    search: SearchHandle,
    mutation_tx: IndexMutationTx,
    history: Arc<RwLock<ClickHistory>>,
    query_log: Arc<RwLock<QueryLog>>,
    stats: Arc<RwLock<IndexStats>>,
    gui_state: Arc<RwLock<GuiState>>,
    config: Arc<config::Config>,
    registry: Arc<lupa_indexer::SourceRegistry>,
) -> anyhow::Result<()> {
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await?;
    let len = u32::from_be_bytes(header) as usize;
    if len < 2 {
        anyhow::bail!("frame too short for version");
    }
    let mut version_buf = [0u8; 2];
    stream.read_exact(&mut version_buf).await?;
    let version = u16::from_be_bytes(version_buf);
    if !(MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION).contains(&version) {
        let resp = Response::Error(format!(
            "unsupported protocol version: got {}, supported {}..={}",
            version, MIN_PROTOCOL_VERSION, PROTOCOL_VERSION
        ));
        let json = serde_json::to_vec(&resp)?;
        let out_len = (json.len() as u32).to_be_bytes();
        stream.write_all(&out_len).await?;
        stream.write_all(&json).await?;
        return Ok(());
    }
    let negotiated_version = version;
    let mut buf = vec![0u8; len - 2];
    stream.read_exact(&mut buf).await?;

    let req: Request = serde_json::from_slice(&buf)?;

    let resp = match req {
        Request::Toggle => {
            let mut state = gui_state.write().await;
            if let Some(pid) = state.pid
                && !process_alive(pid)
            {
                state.pid = None;
                state.visible = false;
            }

            if state.visible {
                if let Some(pid) = state.pid {
                    terminate_gui(pid);
                }
                state.pid = None;
                state.visible = false;
            } else {
                let pid = spawn_gui(Arc::clone(&gui_state))?;
                state.pid = Some(pid);
                state.visible = true;
            }
            Response::Visibility {
                visible: state.visible,
            }
        }
        Request::Show => {
            let mut state = gui_state.write().await;
            if let Some(pid) = state.pid
                && !process_alive(pid)
            {
                state.pid = None;
                state.visible = false;
            }
            if !state.visible {
                let pid = spawn_gui(Arc::clone(&gui_state))?;
                state.pid = Some(pid);
                state.visible = true;
            }
            Response::Visibility { visible: state.visible }
        }
        Request::Hide => {
            let mut state = gui_state.write().await;
            if let Some(pid) = state.pid {
                terminate_gui(pid);
            }
            state.pid = None;
            state.visible = false;
            Response::Visibility { visible: false }
        }
        Request::Search { q, limit } => match search.search(&lupa_core::Query { text: q.clone(), limit }).await {
            Ok(mut hits) => {
                let history = history.read().await;
                for hit in &mut hits {
                    hit.score += history.bonus(&hit.id.0);
                }
                hits.sort_by(|a, b| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                if negotiated_version >= 2 {
                    let calculation = lupa_index::calculator::detect(&q);
                    Response::HitsWithExtras { hits, calculation }
                } else {
                    Response::Hits(hits)
                }
            }
            Err(e) => Response::Error(e.to_string()),
        },
        Request::Reindex { paths } => {
            let result = do_reindex(&mutation_tx, &stats, &config, registry.as_ref(), &config.state_dir, paths).await;
            match result {
                Ok(_count) => {
                    let s = stats.read().await;
                    Response::Status {
                        indexed_docs: s.indexed_docs,
                        last_reindex: s.last_reindex,
                        errors: 0,
                        watcher: Some(collect_watcher_stats()),
                        writer: Some(collect_writer_stats()),
                        memory: Some(collect_memory_stats()),
                    }
                }
                Err(e) => Response::Error(format!("Reindex failed: {}", e)),
            }
        }
        Request::Status => {
            let s = stats.read().await;
            Response::Status {
                indexed_docs: s.indexed_docs,
                last_reindex: s.last_reindex,
                errors: 0,
                watcher: Some(collect_watcher_stats()),
                writer: Some(collect_writer_stats()),
                memory: Some(collect_memory_stats()),
            }
        }
        Request::RecordClick { doc_id } => {
            let mut history = history.write().await;
            history.record_click(&doc_id);
            Response::Ok
        }
        Request::RecordQuery { q } => {
            if lupa_index::calculator::detect(&q).is_some() {
                Response::Ok
            } else {
                let mut log = query_log.write().await;
                log.record_query(&q);
                Response::Ok
            }
        }
        Request::SearchHistory { limit } => {
            let log = query_log.read().await;
            Response::Queries(log.recent(limit as usize))
        }
    };

    let json = serde_json::to_vec(&resp)?;
    let total_len = (2 + json.len()) as u32;
    let mut out = BytesMut::with_capacity(4 + total_len as usize);
    out.put_u32(total_len);
    out.put_u16(negotiated_version);
    out.put_slice(&json);
    stream.write_all(&out).await?;

    Ok(())
}

fn register_builtin_sources(
    registry: &mut lupa_indexer::SourceRegistry,
    config: &config::Config,
    state_dir_root: &std::path::Path,
) {
    registry.register(
        "builtin:apps".into(),
        state_dir_root,
        Arc::new(lupa_sources::apps::AppsSource::new()),
    );

    if config.thunderbird.enabled {
        #[allow(deprecated)]
        let profile = config
            .thunderbird
            .profile_override
            .clone()
            .or_else(lupa_source_thunderbird::find_profile);
        if let Some(profile) = profile {
            if !profile.exists() {
                tracing::warn!(
                    "thunderbird: configured profile {:?} does not exist; skipping gloda + attachments",
                    profile
                );
            } else {
                registry.register(
                    "builtin:gloda".into(),
                    state_dir_root,
                    Arc::new(lupa_source_thunderbird::GlodaSource::new(
                        profile.clone(),
                        0,
                        config.thunderbird.gloda_batch_size,
                    )),
                );
                if config.thunderbird.attachments {
                    registry.register(
                        "builtin:tb_attachments".into(),
                        state_dir_root,
                        Arc::new(lupa_source_thunderbird::ThunderbirdAttachmentsSource::new(
                            profile,
                            config.max_file_size_mb * 1024 * 1024,
                        )),
                    );
                } else {
                    tracing::info!(
                        "thunderbird: attachments disabled by config; registering gloda only"
                    );
                }
            }
        }
    } else {
        tracing::info!("thunderbird: disabled by config; skipping gloda + attachments");
    }

    let fs = lupa_sources::fs::FsSource::with_regex(
        config.roots.clone(),
        config.exclude.clone(),
        config.exclude_regex.clone(),
        config.max_file_size_mb,
    );
    registry.register("builtin:fs".into(), state_dir_root, Arc::new(fs));

    for md in &config.maildir {
        let source = lupa_source_maildir::MaildirSource::new(md.paths.clone(), md.open_cmd.clone());
        registry.register(md.id.clone(), state_dir_root, Arc::new(source));
        tracing::info!(
            "maildir source '{}' registered with {} root(s)",
            md.id,
            md.paths.len()
        );
    }
}
