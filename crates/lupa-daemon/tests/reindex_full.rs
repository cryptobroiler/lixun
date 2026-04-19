use lupa_daemon::config::{Config, Keybindings};
use lupa_daemon::index_service::spawn_writer_service;
use lupa_daemon::indexer;
use lupa_sources::manifest::Manifest;

fn test_config(root: std::path::PathBuf, state_dir: std::path::PathBuf) -> Config {
    Config {
        roots: vec![root],
        exclude: Vec::new(),
        exclude_regex: Vec::new(),
        max_file_size_mb: 1,
        extractor_timeout_secs: 1,
        ranking_apps: 1.0,
        ranking_files: 1.0,
        ranking_mail: 1.0,
        ranking_attachments: 1.0,
        keybindings: Keybindings::default(),
        state_dir,
        maildir: Vec::new(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reindex_full_rebuilds_manifest_and_drops_phantom_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let real_file = root.join("real.txt");
    std::fs::write(&real_file, b"content").unwrap();

    let state_dir = tmp.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    let mut stale = Manifest::default();
    stale.update("/tmp/phantom/does-not-exist.txt".to_string(), 12345);
    stale.update(real_file.to_string_lossy().to_string(), 0);
    stale.save(&state_dir);
    assert_eq!(Manifest::load(&state_dir).len(), 2);

    let index_dir = state_dir.join("index");
    std::fs::create_dir_all(&index_dir).unwrap();
    let index = lupa_index::LupaIndex::create_or_open(index_dir.to_str().unwrap()).unwrap();
    let (mutation_tx, _search, _writer_handle) = spawn_writer_service(index).unwrap();

    let config = test_config(root.clone(), state_dir.clone());
    let registry = lupa_indexer::SourceRegistry::new();
    let outcome = indexer::reindex_full(&mutation_tx, &config, &registry, &state_dir)
        .await
        .unwrap();

    assert!(outcome.fs_docs >= 1, "real file must be indexed");

    let after = Manifest::load(&state_dir);
    assert!(
        !after
            .known_paths()
            .any(|p| p == "/tmp/phantom/does-not-exist.txt"),
        "phantom entry must be dropped"
    );
    assert!(
        after
            .known_paths()
            .any(|p| p == real_file.to_string_lossy().as_ref()),
        "real file must be present"
    );
}
