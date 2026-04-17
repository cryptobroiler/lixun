//! Filesystem source — walk directories, extract text, produce Documents.

use anyhow::Result;
use lupa_core::{Action, Category, DocId, Document};
use std::path::{Path, PathBuf};

use lupa_extract;

/// Filesystem source configuration.
pub struct FsSource {
    pub roots: Vec<PathBuf>,
    pub exclude: Vec<String>,
    pub max_file_size_mb: u64,
}

impl FsSource {
    pub fn new(roots: Vec<PathBuf>, exclude: Vec<String>, max_file_size_mb: u64) -> Self {
        Self {
            roots,
            exclude,
            max_file_size_mb,
        }
    }

    fn should_exclude(&self, name: &str) -> bool {
        self.exclude.iter().any(|pat| name == pat.as_str())
    }

    fn extract_content(path: &Path) -> Result<Option<String>> {
        let text = lupa_extract::extract_path(path)?;
        if text.is_empty() {
            return Ok(None);
        }
        Ok(Some(text))
    }
}

impl crate::Source for FsSource {
    fn name(&self) -> &'static str {
        "filesystem"
    }

    fn index_all(&self) -> Result<Vec<Document>> {
        let mut docs = Vec::new();
        let max_size = self.max_file_size_mb * 1024 * 1024;

        for root in &self.roots {
            for entry in walkdir::WalkDir::new(root)
                .into_iter()
                .filter_entry(|e| !self.should_exclude(e.file_name().to_string_lossy().as_ref()))
                .filter_map(|e| e.ok())
            {
                if !entry.file_type().is_file() {
                    continue;
                }

                let path = entry.path();
                let path_str = path.to_string_lossy().to_string();
                let filename = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();

                let metadata = std::fs::metadata(path)?;
                let mtime = metadata
                    .modified()
                    .map(|t| t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0))
                    .unwrap_or(0);
                let size = metadata.len();

                // Content pass
                let (body, extract_fail) = if size <= max_size {
                    match Self::extract_content(path) {
                        Ok(Some(text)) => (Some(text), false),
                        Ok(None) => (None, false),
                        Err(_) => (None, true),
                    }
                } else {
                    (None, false)
                };

                docs.push(Document {
                    id: DocId(format!("fs:{}", path_str)),
                    category: Category::File,
                    title: filename,
                    subtitle: path_str.clone(),
                    body,
                    path: path_str,
                    mtime,
                    size,
                    action: Action::OpenFile { path: path.to_path_buf() },
                    extract_fail,
                });
            }
        }

        tracing::info!("Filesystem: indexed {} documents", docs.len());
        Ok(docs)
    }
}
