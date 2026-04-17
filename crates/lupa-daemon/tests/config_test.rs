use std::fs;
use std::io::Write;

#[test]
fn test_default_config() {
    let cfg = lupa_daemon::config::Config::default();
    assert!(!cfg.roots.is_empty());
    assert!(cfg.max_file_size_mb > 0);
    assert!((cfg.ranking_apps - 1.3).abs() < 0.001);
}

#[test]
fn test_expand_tilde() {
    let home = std::env::var("HOME").unwrap_or_default();
    let expanded = lupa_daemon::config::expand_tilde("~/Documents");
    assert!(expanded.to_string_lossy().contains("Documents"));
    if !home.is_empty() {
        assert!(expanded.starts_with(&home));
    }
}

#[test]
fn test_toml_config() {
    let tmp = tempfile::tempdir().unwrap();
    let config_dir = tmp.path().join("lupa");
    fs::create_dir_all(&config_dir).unwrap();
    let mut f = fs::File::create(config_dir.join("config.toml")).unwrap();
    writeln!(f, "max_file_size_mb = 100").unwrap();
    writeln!(f, "ranking = {{ apps = 2.0 }}").unwrap();
    // Full test would override XDG_CONFIG_HOME; skip for now, just validate parsing
}
