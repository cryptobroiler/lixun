//! lupad — Lupa daemon: IPC server, indexer, filesystem watcher.

use anyhow::Result;
use bytes::{BufMut, BytesMut};
use lupa_ipc::{Request, Response, socket_path};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::RwLock;

mod history;
use history::ClickHistory;

use lupa_daemon::config;
use lupa_daemon::search_service::SearchService;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("lupa=info".parse()?),
        )
        .init();

    tracing::info!("lupad starting...");

    let config = config::Config::load()?;
    tracing::info!("Config loaded: roots={:?}", config.roots);

    // Initialize index
    let index_path = config.state_dir.join("index");
    let mut index = lupa_index::LupaIndex::create_or_open(index_path.to_str().unwrap())?;
    let mut writer = index.writer(128_000_000)?; // 128MB heap

    // Index all sources
    let sources = config.build_sources()?;
    let mut total_docs = 0u64;

    for source in &sources {
        tracing::info!("Indexing source: {}", source.name());
        let docs = source.index_all()?;
        for doc in docs {
            index.upsert(&doc, &mut writer)?;
            total_docs += 1;
        }
        index.commit(&mut writer)?;
        tracing::info!("Source {} done", source.name());
    }

    tracing::info!("Total indexed: {}", total_docs);

    // Load click history
    let history = ClickHistory::load(&config.state_dir)?;
    let history = Arc::new(RwLock::new(history));

    // Wrap index for SearchService
    let search = Arc::new(SearchService::new(index));

    // Start IPC server
    let socket_path = socket_path();
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }

    tracing::info!("Listening on {:?}", socket_path);

    let listener = tokio::net::UnixListener::bind(&socket_path)?;

    // Set socket permissions to 0600
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(&socket_path)?;
        let mut perms = metadata.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&socket_path, perms)?;
    }

    loop {
        let (stream, _) = listener.accept().await?;
        let search = Arc::clone(&search);
        let history = Arc::clone(&history);

        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, search, history).await {
                tracing::debug!("Client error: {}", e);
            }
        });
    }
}

async fn handle_client(
    mut stream: tokio::net::UnixStream,
    search: Arc<SearchService>,
    history: Arc<RwLock<ClickHistory>>,
) -> anyhow::Result<()> {
    // Read request
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).await?;
    let len = u32::from_be_bytes(header) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;

    let req: Request = serde_json::from_slice(&buf)?;

    let resp = match req {
        Request::Toggle => {
            // TODO: send show/hide to GUI channel
            Response::Ok
        }
        Request::Show => Response::Ok,
        Request::Hide => Response::Ok,
        Request::Search { q, limit } => {
            match search.search(&q, limit).await {
                Ok(mut hits) => {
                    // Apply click-history ranking bonus
                    let history = history.read().await;
                    for hit in &mut hits {
                        hit.score += history.bonus(&hit.id.0);
                    }
                    // Sort by score descending
                    hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
                    Response::Hits(hits)
                }
                Err(e) => Response::Error(e.to_string()),
            }
        }
        Request::Reindex { paths } => Response::Ok, // TODO
        Request::Status => {
            Response::Status {
                indexed_docs: 0,
                last_reindex: None,
                errors: 0,
            }
        }
    };

    // Write response
    let json = serde_json::to_vec(&resp)?;
    let len = json.len() as u32;
    let mut out = BytesMut::with_capacity(4 + json.len());
    out.put_u32(len);
    out.put_slice(&json);
    stream.write_all(&out).await?;

    Ok(())
}
