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

#[test]
fn empty_config_has_no_plugin_sections() {
    let cfg = Config::from_toml_str("").unwrap();
    assert!(cfg.plugin_sections.is_empty());
}

#[test]
fn unknown_top_level_keys_captured_as_plugin_sections() {
    let cfg = Config::from_toml_str(
        r#"
        max_file_size_mb = 25

        [thunderbird]
        enabled = true
        gloda_batch_size = 500

        [[maildir]]
        id = "personal"
        paths = ["~/Mail"]
    "#,
    )
    .unwrap();
    assert_eq!(cfg.plugin_sections.len(), 2);
    assert!(cfg.plugin_sections.contains_key("thunderbird"));
    assert!(cfg.plugin_sections.contains_key("maildir"));
}

#[test]
fn plugin_section_preserves_raw_toml_for_factory() {
    let cfg = Config::from_toml_str(
        r#"
        [thunderbird]
        gloda_batch_size = 1000
        profile = "~/.thunderbird/work"
    "#,
    )
    .unwrap();
    let raw = cfg.plugin_sections.get("thunderbird").unwrap();
    let table = raw.as_table().expect("thunderbird is a table");
    assert_eq!(
        table.get("gloda_batch_size").and_then(|v| v.as_integer()),
        Some(1000)
    );
    assert_eq!(
        table.get("profile").and_then(|v| v.as_str()),
        Some("~/.thunderbird/work")
    );
}

#[test]
fn plugin_section_maildir_preserved_as_array() {
    let cfg = Config::from_toml_str(
        r#"
        [[maildir]]
        id = "a"
        paths = ["/tmp/a"]

        [[maildir]]
        id = "b"
        paths = ["/tmp/b"]
    "#,
    )
    .unwrap();
    let raw = cfg.plugin_sections.get("maildir").unwrap();
    let arr = raw.as_array().expect("maildir is array-of-tables");
    assert_eq!(arr.len(), 2);
}

#[test]
fn known_keys_never_leak_into_plugin_sections() {
    let cfg = Config::from_toml_str(
        r#"
        roots = ["/tmp/custom"]
        exclude = [".foo"]
        max_file_size_mb = 100

        [ranking]
        apps = 2.0

        [keybindings]
        close = "Escape"
    "#,
    )
    .unwrap();
    assert!(cfg.plugin_sections.is_empty());
    assert_eq!(cfg.max_file_size_mb, 100);
}
