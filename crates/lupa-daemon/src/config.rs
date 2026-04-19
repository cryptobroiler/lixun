//! Configuration — ~/.config/lupa/config.toml

use anyhow::Result;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct ConfigToml {
    roots: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
    exclude_regex: Option<Vec<String>>,
    max_file_size_mb: Option<u64>,
    extractor_timeout_secs: Option<u64>,
    ranking: Option<RankingToml>,
    keybindings: Option<KeybindingsToml>,
}

#[derive(Debug, Deserialize)]
struct RankingToml {
    apps: Option<f32>,
    files: Option<f32>,
    mail: Option<f32>,
    attachments: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct KeybindingsToml {
    close: Option<String>,
    primary_action: Option<String>,
    secondary_action: Option<String>,
    copy: Option<String>,
    quick_look: Option<String>,
    history_up: Option<String>,
    next_result: Option<String>,
    previous_result: Option<String>,
    next_category: Option<String>,
    previous_category: Option<String>,
    filter_all: Option<String>,
    filter_apps: Option<String>,
    filter_files: Option<String>,
    filter_mail: Option<String>,
    filter_attachments: Option<String>,
    global_toggle: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Keybindings {
    pub close: String,
    pub primary_action: String,
    pub secondary_action: String,
    pub copy: String,
    pub quick_look: String,
    pub history_up: String,
    pub next_result: String,
    pub previous_result: String,
    pub next_category: String,
    pub previous_category: String,
    pub filter_all: String,
    pub filter_apps: String,
    pub filter_files: String,
    pub filter_mail: String,
    pub filter_attachments: String,
    pub global_toggle: String,
}

pub struct Config {
    pub roots: Vec<PathBuf>,
    pub exclude: Vec<String>,
    pub exclude_regex: Vec<regex::Regex>,
    pub max_file_size_mb: u64,
    pub extractor_timeout_secs: u64,
    pub ranking_apps: f32,
    pub ranking_files: f32,
    pub ranking_mail: f32,
    pub ranking_attachments: f32,
    pub keybindings: Keybindings,
    pub state_dir: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home".into());
        Self {
            roots: vec![PathBuf::from(&home)],
            exclude: default_excludes(),
            exclude_regex: Vec::new(),
            max_file_size_mb: 50,
            extractor_timeout_secs: 15,
            ranking_apps: 1.3,
            ranking_files: 1.2,
            ranking_mail: 1.0,
            ranking_attachments: 0.9,
            keybindings: Keybindings::default(),
            state_dir: state_dir(),
        }
    }
}

fn default_excludes() -> Vec<String> {
    vec![
        ".cache".into(),
        ".local/share/Trash".into(),
        ".steam".into(),
        ".var/app".into(),
        "node_modules".into(),
        "target".into(),
        ".git".into(),
        ".venv".into(),
        "__pycache__".into(),
        ".thunderbird".into(),
        ".swp".into(),
        ".swo".into(),
        ".swx".into(),
    ]
}

impl Default for Keybindings {
    fn default() -> Self {
        Self {
            close: "Escape".into(),
            primary_action: "Return".into(),
            secondary_action: "<Shift>Return".into(),
            copy: "<Ctrl>c".into(),
            quick_look: "space".into(),
            history_up: "Up".into(),
            next_result: "Down".into(),
            previous_result: "Up".into(),
            next_category: "<Ctrl>Down".into(),
            previous_category: "<Ctrl>Up".into(),
            filter_all: "<Ctrl>0".into(),
            filter_apps: "<Ctrl>1".into(),
            filter_files: "<Ctrl>2".into(),
            filter_mail: "<Ctrl>3".into(),
            filter_attachments: "<Ctrl>4".into(),
            global_toggle: "Super+space".into(),
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_path = config_dir().join("lupa/config.toml");
        if !config_path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&config_path)?;
        Self::from_toml_str(&content)
    }

    pub fn from_toml_str(content: &str) -> Result<Self> {
        let mut cfg = Self::default();
        let parsed: ConfigToml = toml::from_str(content)?;

        if let Some(roots) = parsed.roots {
            cfg.roots = roots.iter().map(|s| expand_tilde(s)).collect();
        }
        if let Some(extra) = parsed.exclude {
            cfg.exclude.extend(extra);
        }
        if let Some(patterns) = parsed.exclude_regex {
            for pat in patterns {
                match regex::Regex::new(&pat) {
                    Ok(r) => cfg.exclude_regex.push(r),
                    Err(e) => {
                        tracing::error!("config: skipping invalid exclude_regex '{}': {}", pat, e)
                    }
                }
            }
        }
        if let Some(max) = parsed.max_file_size_mb {
            cfg.max_file_size_mb = max;
        }
        if let Some(timeout) = parsed.extractor_timeout_secs {
            cfg.extractor_timeout_secs = timeout;
        }
        if let Some(ranking) = parsed.ranking {
            if let Some(v) = ranking.apps {
                cfg.ranking_apps = v;
            }
            if let Some(v) = ranking.files {
                cfg.ranking_files = v;
            }
            if let Some(v) = ranking.mail {
                cfg.ranking_mail = v;
            }
            if let Some(v) = ranking.attachments {
                cfg.ranking_attachments = v;
            }
        }
        if let Some(bindings) = parsed.keybindings {
            if let Some(v) = bindings.close {
                cfg.keybindings.close = v;
            }
            if let Some(v) = bindings.primary_action {
                cfg.keybindings.primary_action = v;
            }
            if let Some(v) = bindings.secondary_action {
                cfg.keybindings.secondary_action = v;
            }
            if let Some(v) = bindings.copy {
                cfg.keybindings.copy = v;
            }
            if let Some(v) = bindings.quick_look {
                cfg.keybindings.quick_look = v;
            }
            if let Some(v) = bindings.history_up {
                cfg.keybindings.history_up = v;
            }
            if let Some(v) = bindings.next_result {
                cfg.keybindings.next_result = v;
            }
            if let Some(v) = bindings.previous_result {
                cfg.keybindings.previous_result = v;
            }
            if let Some(v) = bindings.next_category {
                cfg.keybindings.next_category = v;
            }
            if let Some(v) = bindings.previous_category {
                cfg.keybindings.previous_category = v;
            }
            if let Some(v) = bindings.filter_all {
                cfg.keybindings.filter_all = v;
            }
            if let Some(v) = bindings.filter_apps {
                cfg.keybindings.filter_apps = v;
            }
            if let Some(v) = bindings.filter_files {
                cfg.keybindings.filter_files = v;
            }
            if let Some(v) = bindings.filter_mail {
                cfg.keybindings.filter_mail = v;
            }
            if let Some(v) = bindings.filter_attachments {
                cfg.keybindings.filter_attachments = v;
            }
            if let Some(v) = bindings.global_toggle {
                cfg.keybindings.global_toggle = v;
            }
        }

        Ok(cfg)
    }

    pub fn build_fs_source(&self) -> Result<lupa_sources::fs::FsSource> {
        Ok(lupa_sources::fs::FsSource::with_regex(
            self.roots.clone(),
            self.exclude.clone(),
            self.exclude_regex.clone(),
            self.max_file_size_mb,
        ))
    }

    pub fn build_sources(&self) -> Result<Vec<Box<dyn lupa_sources::Source>>> {
        let mut sources: Vec<Box<dyn lupa_sources::Source>> = Vec::new();

        sources.push(Box::new(lupa_sources::apps::AppsSource::new()));

        if let Some(profile) = lupa_sources::gloda::GlodaSource::find_profile() {
            sources.push(Box::new(lupa_sources::gloda::GlodaSource::new(
                profile.clone(),
                0,
                250,
            )));
            // Full Thunderbird attachment indexing is disabled by default for now.
            // It is too expensive for large mailboxes and was the main source of
            // multi-GB memory peaks during startup and watcher-triggered reindexing.
        }

        Ok(sources)
    }
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        let home = std::env::var("HOME").unwrap_or_default();
        PathBuf::from(home).join(rest)
    } else if path == "~" {
        PathBuf::from(std::env::var("HOME").unwrap_or_default())
    } else {
        PathBuf::from(path)
    }
}

fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn state_dir() -> PathBuf {
    dirs::state_dir()
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/state")
        })
        .join("lupa")
}
