//! Configuration — ~/.config/lupa/config.toml

use anyhow::Result;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct ConfigToml {
    roots: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
    max_file_size_mb: Option<u64>,
    extractor_timeout_secs: Option<u64>,
    ranking: Option<RankingToml>,
}

#[derive(Debug, Deserialize)]
struct RankingToml {
    apps: Option<f32>,
    files: Option<f32>,
    mail: Option<f32>,
    attachments: Option<f32>,
}

pub struct Config {
    pub roots: Vec<PathBuf>,
    pub exclude: Vec<String>,
    pub max_file_size_mb: u64,
    pub extractor_timeout_secs: u64,
    pub ranking_apps: f32,
    pub ranking_files: f32,
    pub ranking_mail: f32,
    pub ranking_attachments: f32,
    pub state_dir: PathBuf,
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_path = config_dir().join("lupa/config.toml");
        let mut cfg = Self::default();

        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)?;
            let parsed: ConfigToml = toml::from_str(&content)?;

            if let Some(roots) = parsed.roots {
                cfg.roots = roots.iter().map(|s| expand_tilde(s)).collect();
            }
            if let Some(exclude) = parsed.exclude {
                cfg.exclude = exclude;
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
        }

        Ok(cfg)
    }

    pub fn default() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home".into());
        Self {
            roots: vec![PathBuf::from(&home)],
            exclude: vec![
                ".cache".into(),
                ".local/share/Trash".into(),
                "node_modules".into(),
                "target".into(),
                ".git".into(),
                ".venv".into(),
                "__pycache__".into(),
                ".thunderbird".into(),
            ],
            max_file_size_mb: 50,
            extractor_timeout_secs: 15,
            ranking_apps: 1.3,
            ranking_files: 1.2,
            ranking_mail: 1.0,
            ranking_attachments: 0.9,
            state_dir: state_dir(),
        }
    }

    pub fn build_fs_source(&self) -> Result<lupa_sources::fs::FsSource> {
        Ok(lupa_sources::fs::FsSource::new(
            self.roots.clone(),
            self.exclude.clone(),
            self.max_file_size_mb,
        ))
    }

    pub fn build_sources(&self) -> Result<Vec<Box<dyn lupa_sources::Source>>> {
        let mut sources: Vec<Box<dyn lupa_sources::Source>> = Vec::new();

        sources.push(Box::new(lupa_sources::fs::FsSource::new(
            self.roots.clone(),
            self.exclude.clone(),
            self.max_file_size_mb,
        )));

        sources.push(Box::new(lupa_sources::apps::AppsSource::new()));

        if let Some(profile) = lupa_sources::gloda::GlodaSource::find_profile() {
            sources.push(Box::new(lupa_sources::gloda::GlodaSource::new(
                profile.clone(),
                0,
            )));
            sources.push(Box::new(
                lupa_sources::thunderbird_attachments::ThunderbirdAttachmentsSource::new(
                    profile,
                    self.max_file_size_mb * 1024 * 1024,
                ),
            ));
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
