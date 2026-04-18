//! lupad — Lupa daemon: IPC server, indexer, filesystem watcher.

use anyhow::Result;
use bytes::{BufMut, BytesMut};
use chrono::Utc;
use futures::StreamExt;
use lupa_index::LupaIndex;
use lupa_ipc::{MIN_PROTOCOL_VERSION, PROTOCOL_VERSION, Request, Response, socket_path};
use std::os::unix::io::AsRawFd;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::RwLock;

mod cursors;
mod gloda_poll;
mod history;
mod query_log;
use history::ClickHistory;
use query_log::QueryLog;

use lupa_daemon::config;
use lupa_daemon::search_service::SearchService;
use lupa_sources::Source;

mod source_watcher;
mod watcher;

#[derive(Debug, Clone, Default)]
struct IndexStats {
    indexed_docs: u64,
    last_reindex: Option<chrono::DateTime<Utc>>,
}

#[derive(Debug, Clone, Default)]
struct GuiState {
    visible: bool,
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
    let index = lupa_index::LupaIndex::create_or_open(index_path.to_str().unwrap())?;

    let stats = Arc::new(RwLock::new(IndexStats::default()));
    let poll_state_dir = config.state_dir.clone();
    let state_dir = config.state_dir.clone();
    let watcher_roots = config.roots.clone();
    let watcher_exclude = config.exclude.clone();
    let watcher_max_size = config.max_file_size_mb;
    let shared_config = Arc::new(config);

    let history = ClickHistory::load(&state_dir)?;
    let history = Arc::new(RwLock::new(history));

    let query_log = QueryLog::load(&state_dir)?;
    let query_log = Arc::new(RwLock::new(query_log));

    let gui_state = Arc::new(RwLock::new(GuiState::default()));

    let search = SearchService::new(index);

    let indexer_index = search.index();
    let indexer_stats = Arc::clone(&stats);
    let indexer_state_dir = state_dir.clone();
    let indexer_config = Arc::clone(&shared_config);
    tokio::spawn(async move {
        if let Err(e) = run_incremental_indexer(
            indexer_index,
            indexer_stats,
            indexer_state_dir,
            indexer_config,
        )
        .await
        {
            tracing::error!("Incremental indexer error: {}", e);
        }
    });

    let watcher_index = search.index();

    tokio::spawn(async move {
        if let Err(e) = watcher::start(
            watcher_roots,
            watcher_exclude,
            watcher_max_size,
            watcher_index,
        )
        .await
        {
            tracing::error!("Watcher error: {}", e);
        }
    });

    if let Some(profile) = lupa_sources::gloda::GlodaSource::find_profile() {
        let poll_index = search.index();
        let poll_profile = profile.clone();
        tokio::spawn(async move {
            if let Err(e) = gloda_poll::start(poll_profile, poll_state_dir, poll_index).await {
                tracing::error!("Gloda poll error: {}", e);
            }
        });
        tracing::info!("Gloda poller started (30s interval)");

        let apps = Arc::new(lupa_sources::apps::AppsSource::new());
        let attachments = Arc::new(
            lupa_sources::thunderbird_attachments::ThunderbirdAttachmentsSource::new(
                profile,
                shared_config.max_file_size_mb * 1024 * 1024,
            ),
        );
        let source_index = search.index();
        tokio::spawn(async move {
            if let Err(e) = source_watcher::start(apps, Some(attachments), source_index).await {
                tracing::error!("Source watcher error: {}", e);
            }
        });
        tracing::info!("Apps + attachments watchers started");
    } else {
        let apps = Arc::new(lupa_sources::apps::AppsSource::new());
        let source_index = search.index();
        tokio::spawn(async move {
            if let Err(e) = source_watcher::start(apps, None, source_index).await {
                tracing::error!("Source watcher error: {}", e);
            }
        });
        tracing::info!("Apps watcher started (no Thunderbird profile)");
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

    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, _) = match result {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::debug!("Accept error: {}", e);
                        continue;
                    }
                };
                let search = search.clone();
                let history = Arc::clone(&history);
                let query_log = Arc::clone(&query_log);
                let stats = Arc::clone(&stats);
                let gui_state = Arc::clone(&gui_state);
                let shared_config = Arc::clone(&shared_config);

                tokio::spawn(async move {
                    if let Err(e) = handle_client(stream, search, history, query_log, stats, gui_state, shared_config).await {
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
                break;
            }
        }
    }

    Ok(())
}

async fn run_incremental_indexer(
    index: Arc<RwLock<LupaIndex>>,
    stats: Arc<RwLock<IndexStats>>,
    state_dir: std::path::PathBuf,
    config: Arc<config::Config>,
) -> Result<()> {
    let mut manifest = lupa_sources::manifest::Manifest::load(&state_dir);

    // Filesystem source
    {
        let fs_source = config.build_fs_source()?;
        let (docs, deleted_ids) = fs_source.index_incremental(&mut manifest)?;

        if !docs.is_empty() || !deleted_ids.is_empty() {
            let mut idx = index.write().await;
            let mut writer = idx.writer(128_000_000)?;

            for doc in &docs {
                idx.upsert(doc, &mut writer)?;
            }
            for id in &deleted_ids {
                idx.delete_by_id(id, &mut writer)?;
            }
            idx.commit(&mut writer)?;

            {
                let mut stats_lock = stats.write().await;
                stats_lock.indexed_docs += docs.len() as u64;
                stats_lock.last_reindex = Some(Utc::now());
            }

            tracing::info!(
                "Filesystem incremental: +{} docs, -{} deleted",
                docs.len(),
                deleted_ids.len()
            );
        } else {
            tracing::info!("Filesystem: no changes");
        }
    }

    // Other sources (apps, gloda, attachments)
    {
        let all_docs = {
            let sources = config.build_sources()?;
            let mut docs: Vec<(String, Vec<lupa_core::Document>)> = Vec::new();
            for source in &sources {
                if source.name() == "filesystem" {
                    continue;
                }
                let d = source.index_all()?;
                docs.push((source.name().to_string(), d));
            }
            docs
        };

        for (name, docs) in &all_docs {
            let mut idx = index.write().await;
            let mut writer = idx.writer(64_000_000)?;
            for doc in docs {
                idx.upsert(doc, &mut writer)?;
            }
            idx.commit(&mut writer)?;
            {
                let mut stats_lock = stats.write().await;
                stats_lock.indexed_docs += docs.len() as u64;
                stats_lock.last_reindex = Some(Utc::now());
            }
            tracing::info!("Source {} indexed: {} docs", name, docs.len());
        }
    }

    manifest.save(&state_dir);
    Ok(())
}

async fn do_reindex(
    search: &SearchService,
    stats: &Arc<RwLock<IndexStats>>,
    config: &config::Config,
    paths: Vec<std::path::PathBuf>,
) -> Result<usize, anyhow::Error> {
    let index = search.index();
    let mut idx = index.write().await;
    let mut writer = idx.writer(128_000_000)?;
    let mut count = 0;

    if paths.is_empty() {
        let sources = config.build_sources()?;
        for source in &sources {
            tracing::info!("Reindexing source: {}", source.name());
            let docs = source.index_all()?;
            for doc in &docs {
                idx.upsert(doc, &mut writer)?;
                count += 1;
            }
        }
    } else {
        for path in &paths {
            if path.is_file() {
                if let Ok(doc) = watcher::index_file(path, config.max_file_size_mb) {
                    idx.upsert(&doc, &mut writer)?;
                    count += 1;
                }
            } else if path.is_dir() {
                let source = lupa_sources::fs::FsSource::new(
                    vec![path.clone()],
                    config.exclude.clone(),
                    config.max_file_size_mb,
                );
                let docs = source.index_all()?;
                for doc in &docs {
                    idx.upsert(doc, &mut writer)?;
                    count += 1;
                }
            }
        }
    }

    idx.commit(&mut writer)?;
    {
        let mut stats_lock = stats.write().await;
        stats_lock.indexed_docs += count as u64;
        stats_lock.last_reindex = Some(Utc::now());
    }

    tracing::info!("Reindex complete: {} documents processed", count);
    Ok(count)
}

async fn handle_client(
    mut stream: tokio::net::UnixStream,
    search: SearchService,
    history: Arc<RwLock<ClickHistory>>,
    query_log: Arc<RwLock<QueryLog>>,
    stats: Arc<RwLock<IndexStats>>,
    gui_state: Arc<RwLock<GuiState>>,
    config: Arc<config::Config>,
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
            state.visible = !state.visible;
            Response::Visibility {
                visible: state.visible,
            }
        }
        Request::Show => {
            let mut state = gui_state.write().await;
            state.visible = true;
            Response::Visibility { visible: true }
        }
        Request::Hide => {
            let mut state = gui_state.write().await;
            state.visible = false;
            Response::Visibility { visible: false }
        }
        Request::Search { q, limit } => match search.search(&q, limit).await {
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
            let result = do_reindex(&search, &stats, &config, paths).await;
            match result {
                Ok(_count) => Response::Status {
                    indexed_docs: stats.read().await.indexed_docs,
                    last_reindex: stats.read().await.last_reindex,
                    errors: 0,
                },
                Err(e) => Response::Error(format!("Reindex failed: {}", e)),
            }
        }
        Request::Status => {
            let stats = stats.read().await;
            Response::Status {
                indexed_docs: stats.indexed_docs,
                last_reindex: stats.last_reindex,
                errors: 0,
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
