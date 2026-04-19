use std::fs;
use std::io::Write;

use lupa_daemon::config::Config;

#[test]
fn test_default_config() {
    let cfg = Config::default();
    assert!(!cfg.roots.is_empty());
    assert!(cfg.max_file_size_mb > 0);
    assert!((cfg.ranking_apps - 1.3).abs() < 0.001);
    assert_eq!(cfg.keybindings.close, "Escape");
    assert_eq!(cfg.keybindings.global_toggle, "Super+space");
    assert!(cfg.exclude_regex.is_empty());
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
}

#[test]
fn user_exclude_merges_with_defaults() {
    let toml = r#"
        exclude = ["MyProject/tmp", ".vscode"]
    "#;
    let cfg = Config::from_toml_str(toml).unwrap();
    assert!(
        cfg.exclude.iter().any(|s| s == ".cache"),
        "default must survive"
    );
    assert!(
        cfg.exclude.iter().any(|s| s == "node_modules"),
        "default must survive"
    );
    assert!(
        cfg.exclude.iter().any(|s| s == "MyProject/tmp"),
        "user entry must be added"
    );
    assert!(
        cfg.exclude.iter().any(|s| s == ".vscode"),
        "user entry must be added"
    );
}

#[test]
fn exclude_regex_compiled_from_toml() {
    let toml = r#"
        exclude_regex = ['\.~lock\..*#$', '\.pyc$']
    "#;
    let cfg = Config::from_toml_str(toml).unwrap();
    assert_eq!(cfg.exclude_regex.len(), 2);
    assert!(cfg
        .exclude_regex
        .iter()
        .any(|r| r.is_match("/home/u/Docs/.~lock.Report.xlsx#")));
    assert!(cfg.exclude_regex.iter().any(|r| r.is_match("/a/b/foo.pyc")));
}

#[test]
fn invalid_regex_patterns_dropped_not_fatal() {
    let toml = r#"
        exclude_regex = ['\.~lock\..*#$', '[unterminated', '\.tmp$']
    "#;
    let cfg = Config::from_toml_str(toml).unwrap();
    assert_eq!(
        cfg.exclude_regex.len(),
        2,
        "bad pattern dropped; good ones kept"
    );
}

#[test]
fn missing_exclude_sections_leave_defaults_intact() {
    let cfg = Config::from_toml_str("max_file_size_mb = 25").unwrap();
    let defaults = Config::default().exclude;
    assert_eq!(cfg.exclude, defaults);
    assert!(cfg.exclude_regex.is_empty());
    assert_eq!(cfg.max_file_size_mb, 25);
}
