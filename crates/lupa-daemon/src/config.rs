//! Configuration — ~/.config/lupa/config.toml

use anyhow::Result;
use std::path::PathBuf;



#[derive(Debug)]
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
    pub fn default() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home".to_string());
        let state_dir = PathBuf::from(&home).join(".local/state/lupa");

        Self {
            roots: vec![PathBuf::from(home)],
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
            state_dir,
        }
    }

    pub fn build_sources(&self) -> Result<Vec<Box<dyn lupa_sources::Source>>> {
        let mut sources: Vec<Box<dyn lupa_sources::Source>> = Vec::new();

        // Filesystem source
        sources.push(Box::new(lupa_sources::fs::FsSource::new(
            self.roots.clone(),
            self.exclude.clone(),
            self.max_file_size_mb,
        )));

        // Apps source
        sources.push(Box::new(lupa_sources::apps::AppsSource::new()));

        // Gloda source (if Thunderbird profile exists)
        if let Some(profile) = lupa_sources::gloda::GlodaSource::find_profile() {
            sources.push(Box::new(lupa_sources::gloda::GlodaSource::new(profile.clone(), 0)));

            sources.push(Box::new(lupa_sources::thunderbird_attachments::ThunderbirdAttachmentsSource::new(profile)));
        }

        Ok(sources)
    }
}

/// Load config from ~/.config/lupa/config.toml or use defaults.
pub fn load() -> Result<Config> {
    let config_path = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("lupa/config.toml");

    if config_path.exists() {
        // TODO: Parse TOMl config
        tracing::info!("Loading config from {:?}", config_path);
    }

    Ok(Config::default())
}
