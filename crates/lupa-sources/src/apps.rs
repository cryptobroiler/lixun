//! Applications source — scan .desktop files.

use anyhow::Result;
use lupa_core::{Action, Category, DocId, Document};
use std::path::{Path, PathBuf};

/// Applications source — scans XDG_DATA_DIRS for .desktop files.
pub struct AppsSource {
    pub search_dirs: Vec<PathBuf>,
}

impl AppsSource {
    pub fn new() -> Self {
        let mut dirs = Vec::new();

        if let Ok(xdd) = std::env::var("XDG_DATA_DIRS") {
            for dir in xdd.split(':') {
                dirs.push(PathBuf::from(dir).join("applications"));
            }
        } else {
            dirs.push(PathBuf::from("/usr/share/applications"));
            dirs.push(PathBuf::from("/usr/local/share/applications"));
        }

        if let Ok(local) = std::env::var("XDG_DATA_HOME") {
            dirs.push(PathBuf::from(local).join("applications"));
        } else {
            if let Ok(home) = std::env::var("HOME") {
                dirs.push(PathBuf::from(home).join(".local/share/applications"));
            }
        }

        Self { search_dirs: dirs }
    }
}

impl AppsSource {
    fn parse_desktop_file(path: &Path) -> Option<(String, String, String, bool, Option<PathBuf>)> {
        let content = std::fs::read_to_string(path).ok()?;

        let mut name = String::new();
        let mut exec = String::new();
        let mut terminal = false;
        let mut in_desktop_entry = false;

        for line in content.lines() {
            if line.trim() == "[Desktop Entry]" {
                in_desktop_entry = true;
                continue;
            }
            if line.starts_with('[') {
                in_desktop_entry = false;
                continue;
            }
            if !in_desktop_entry {
                continue;
            }

            if let Some((key, val)) = line.split_once('=') {
                match key.trim() {
                    "Name" if name.is_empty() => {
                        name = val.trim().to_string();
                    }
                    "Exec" => {
                        exec = val.trim().to_string();
                        // Remove field codes (%f, %F, %u, etc.)
                        exec = exec.split_whitespace()
                            .filter(|s| !s.starts_with('%'))
                            .collect::<Vec<_>>()
                            .join(" ");
                    }
                    "Terminal" => {
                        terminal = val.trim().eq_ignore_ascii_case("true");
                    }
                    _ => {}
                }
            }
        }

        if name.is_empty() || exec.is_empty() {
            return None;
        }

        let file_stem = path.file_stem()?.to_string_lossy().to_string();
        let working_dir = None; // Could be parsed from Path key

        Some((file_stem, name, exec, terminal, working_dir))
    }
}

impl crate::Source for AppsSource {
    fn name(&self) -> &'static str {
        "applications"
    }

    fn index_all(&self) -> Result<Vec<Document>> {
        let mut docs = Vec::new();

        for dir in &self.search_dirs {
            if !dir.exists() {
                continue;
            }

            for entry in std::fs::read_dir(dir)? {
                let entry = entry?;
                let path = entry.path();

                if !path.extension().map(|e| e == "desktop").unwrap_or(false) {
                    continue;
                }

                if let Some((desktop_id, name, exec, terminal, working_dir)) = Self::parse_desktop_file(&path) {
                    docs.push(Document {
                        id: DocId(format!("app:{}", desktop_id)),
                        category: Category::App,
                        title: name,
                        subtitle: desktop_id.clone(),
                        body: None,
                        path: path.to_string_lossy().to_string(),
                        mtime: 0,
                        size: 0,
                        action: Action::Launch {
                            exec,
                            terminal,
                            working_dir,
                        },
                        extract_fail: false,
                    });
                }
            }
        }

        tracing::info!("Applications: indexed {} apps", docs.len());
        Ok(docs)
    }
}
