//! lupad — Lupa daemon: IPC server, indexer, filesystem watcher.

use anyhow::Result;

use lupa_daemon::config;


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

    // Start IPC server
    let socket_path = lupa_ipc::socket_path();
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
        let (reader, writer) = tokio::io::split(stream);

        // TODO: Handle IPC frames
        tracing::info!("Client connected");
    }
}
