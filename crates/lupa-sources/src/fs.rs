//! Filesystem source — walk directories, extract text, produce Documents.
//!
//! Two-pass indexing:
//! 1. **Metadata pass** (sequential): collects path, filename, mtime, size.
//! 2. **Content pass** (rayon pool): extracts body text in parallel.

use anyhow::Result;
use lupa_core::{Action, Category, DocId, Document};
use rayon::prelude::*;
use std::path::{Path, PathBuf};

use lupa_extract;

/// Filesystem source configuration.
pub struct FsSource {
    pub roots: Vec<PathBuf>,
    pub exclude: Vec<String>,
    pub max_file_size_mb: u64,
}

/// Intermediate metadata collected during the first pass.
struct FileMeta {
    path: PathBuf,
    path_str: String,
    filename: String,
    mtime: i64,
    size: u64,
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

    /// Extract content from a file. Public for the watcher to reuse.
    pub fn extract_content(path: &Path) -> Result<Option<String>> {
        let text = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            lupa_extract::extract_path(path)
        }))
        .map_err(|_| anyhow::anyhow!("extractor panicked"))??;
        if text.is_empty() {
            return Ok(None);
        }
        Ok(Some(text))
    }

    /// Build a rayon thread pool sized to min(num_cpus, 4).
    fn build_pool() -> rayon::ThreadPool {
        let num_threads = std::cmp::min(num_cpus::get(), 4);
        rayon::ThreadPoolBuilder::new()
            .num_threads(num_threads)
            .build()
            .expect("failed to build rayon pool")
    }
}

impl crate::Source for FsSource {
    fn name(&self) -> &'static str {
        "filesystem"
    }

    fn index_all(&self) -> Result<Vec<Document>> {
        let max_size = self.max_file_size_mb * 1024 * 1024;

        // --- Pass 1: Metadata (sequential, I/O bound for directory traversal) ---
        let mut metas: Vec<FileMeta> = Vec::new();

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
                let Ok(metadata) = std::fs::metadata(path) else {
                    continue;
                };

                let mtime = metadata
                    .modified()
                    .map(|t| {
                        t.duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs() as i64)
                            .unwrap_or(0)
                    })
                    .unwrap_or(0);

                metas.push(FileMeta {
                    path: path.to_path_buf(),
                    path_str: path.to_string_lossy().to_string(),
                    filename: path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    mtime,
                    size: metadata.len(),
                });
            }
        }

        tracing::info!(
            "Filesystem: {} files found, extracting content in parallel...",
            metas.len()
        );

        // --- Pass 2: Content extraction (parallel via rayon) ---
        let pool = Self::build_pool();
        let docs: Vec<Document> = pool.install(|| {
            metas
                .into_par_iter()
                .map(|meta| {
                    let (body, extract_fail) = if meta.size <= max_size {
                        match Self::extract_content(&meta.path) {
                            Ok(Some(text)) => (Some(text), false),
                            Ok(None) => (None, false),
                            Err(_) => (None, true),
                        }
                    } else {
                        (None, false)
                    };

                    Document {
                        id: DocId(format!("fs:{}", meta.path_str)),
                        category: Category::File,
                        title: meta.filename,
                        subtitle: meta.path_str.clone(),
                        body,
                        path: meta.path_str,
                        mtime: meta.mtime,
                        size: meta.size,
                        action: Action::OpenFile { path: meta.path },
                        extract_fail,
                    }
                })
                .collect()
        });

        let extract_fails = docs.iter().filter(|d| d.extract_fail).count();
        tracing::info!(
            "Filesystem: indexed {} documents ({} extract failures)",
            docs.len(),
            extract_fails
        );
        Ok(docs)
    }
}
