//! lupad — Lupa daemon: IPC server, indexer, filesystem watcher.

use anyhow::Result;
use bytes::{BufMut, BytesMut};
use chrono::Utc;
use lupa_ipc::{Request, Response, socket_path, PROTOCOL_VERSION};
use std::os::unix::io::AsRawFd;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::RwLock;
use futures::StreamExt;

mod history;
use history::ClickHistory;

use lupa_daemon::config;
use lupa_daemon::search_service::SearchService;

mod watcher;

#[derive(Debug, Clone, Default)]
struct IndexStats {
    indexed_docs: u64,
    last_reindex: Option<chrono::DateTime<Utc>>,
}

fn pid_path() -> std::path::PathBuf {
    let runtime = dirs::runtime_dir()
        .unwrap_or_else(|| std::path::PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })));
    runtime.join("lupa.pid")
}

fn try_single_instance() -> Result<std::fs::File> {
    let path = pid_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&path)?;

    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if ret != 0 {
        anyhow::bail!("another instance of lupad is already running (pid file: {:?})", path);
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
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("lupa=info".parse()?),
        )
        .init();

    tracing::info!("lupad starting...");

    let _lock = try_single_instance()?;

    let config = config::Config::load()?;
    tracing::info!("Config loaded: roots={:?}", config.roots);

    let index_path = config.state_dir.join("index");
    let mut index = lupa_index::LupaIndex::create_or_open(index_path.to_str().unwrap())?;
    let mut writer = index.writer(128_000_000)?;

    let sources = config.build_sources()?;
    let stats = Arc::new(RwLock::new(IndexStats::default()));

    for source in &sources {
        tracing::info!("Indexing source: {}", source.name());
        let docs = source.index_all()?;
        let mut stats_lock = stats.write().await;
        for doc in docs {
            index.upsert(&doc, &mut writer)?;
            stats_lock.indexed_docs += 1;
        }
        index.commit(&mut writer)?;
        stats_lock.last_reindex = Some(Utc::now());
        tracing::info!("Source {} done", source.name());
    }

    tracing::info!("Total indexed: {}", stats.read().await.indexed_docs);

    let history = ClickHistory::load(&config.state_dir)?;
    let history = Arc::new(RwLock::new(history));

    let search = SearchService::new(index);

    let watcher_roots = config.roots.clone();
    let watcher_exclude = config.exclude.clone();
    let watcher_max_size = config.max_file_size_mb;
    let watcher_index = search.index();

    tokio::spawn(async move {
        if let Err(e) = watcher::start(watcher_roots, watcher_exclude, watcher_max_size, watcher_index).await {
            tracing::error!("Watcher error: {}", e);
        }
    });

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

    let state_dir = config.state_dir.clone();

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);

    let shutdown_tx_signal = shutdown_tx.clone();
    let mut signals = signal_hook_tokio::Signals::new([
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
    ])?;
    tokio::spawn(async move {
        while let Some(sig) = signals.next().await {
            tracing::info!("Received signal {}, shutting down...", sig);
            let _ = shutdown_tx_signal.send(()).await;
            break;
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
                let stats = Arc::clone(&stats);

                tokio::spawn(async move {
                    if let Err(e) = handle_client(stream, search, history, stats).await {
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
                tracing::info!("Shutdown complete");
                break;
            }
        }
    }

    Ok(())
}

async fn handle_client(
    mut stream: tokio::net::UnixStream,
    search: SearchService,
    history: Arc<RwLock<ClickHistory>>,
    stats: Arc<RwLock<IndexStats>>,
) -> anyhow::Result<()> {
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await?;
    let len = u32::from_be_bytes(header) as usize;
    if len < 2 {
        return anyhow::bail!("frame too short for version");
    }
    let mut version_buf = [0u8; 2];
    stream.read_exact(&mut version_buf).await?;
    let version = u16::from_be_bytes(version_buf);
    if version != PROTOCOL_VERSION {
        let resp = Response::Error(format!("version mismatch: expected {}, got {}", PROTOCOL_VERSION, version));
        let json = serde_json::to_vec(&resp)?;
        let out_len = (json.len() as u32).to_be_bytes();
        stream.write_all(&out_len).await?;
        stream.write_all(&json).await?;
        return Ok(());
    }
    let mut buf = vec![0u8; len - 2];
    stream.read_exact(&mut buf).await?;

    let req: Request = serde_json::from_slice(&buf)?;

    let resp = match req {
        Request::Toggle => Response::Ok,
        Request::Show => Response::Ok,
        Request::Hide => Response::Ok,
        Request::Search { q, limit } => {
            match search.search(&q, limit).await {
                Ok(mut hits) => {
                    let history = history.read().await;
                    for hit in &mut hits {
                        hit.score += history.bonus(&hit.id.0);
                    }
                    hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
                    Response::Hits(hits)
                }
                Err(e) => Response::Error(e.to_string()),
            }
        }
        Request::Reindex { paths: _ } => Response::Ok,
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
    };

    let json = serde_json::to_vec(&resp)?;
    let total_len = (2 + json.len()) as u32;
    let mut out = BytesMut::with_capacity(4 + total_len as usize);
    out.put_u32(total_len);
    out.put_u16(PROTOCOL_VERSION);
    out.put_slice(&json);
    stream.write_all(&out).await?;

    Ok(())
}
