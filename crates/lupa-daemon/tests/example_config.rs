use lupa_daemon::config::Config;

#[test]
fn bundled_example_config_parses() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/config.example.toml"
    );
    let content =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {}", path, e));
    let cfg = Config::from_toml_str(&content).expect("example must parse");
    assert!(!cfg.exclude.is_empty());
    assert!(!cfg.exclude_regex.is_empty());
    assert_eq!(cfg.keybindings.global_toggle, "Super+space");
}
