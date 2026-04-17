//! Thunderbird attachments source — parse mbox files for attachments.

use anyhow::Result;
use lupa_core::{Action, Category, DocId, Document};
use std::path::PathBuf;

/// Thunderbird attachments source.
pub struct ThunderbirdAttachmentsSource {
    pub profile_path: PathBuf,
}

impl ThunderbirdAttachmentsSource {
    pub fn new(profile_path: PathBuf) -> Self {
        Self { profile_path }
    }
}

impl crate::Source for ThunderbirdAttachmentsSource {
    fn name(&self) -> &'static str {
        "thunderbird_attachments"
    }

    fn index_all(&self) -> Result<Vec<Document>> {
        let mail_path = self.profile_path.join("Mail");
        let imap_path = self.profile_path.join("ImapMail");

        let mut docs = Vec::new();

        for base_path in [&mail_path, &imap_path] {
            if !base_path.exists() {
                continue;
            }

            // Walk mbox files
            for entry in walkdir::WalkDir::new(base_path)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();

                // Skip .msf metadata files
                if name.ends_with(".msf") || name.starts_with('.') {
                    continue;
                }

                // Parse mbox for attachments
                // Full implementation would use mailparse crate here
                // For now, log and continue
                tracing::debug!("Would parse mbox: {:?}", path);
            }
        }

        tracing::info!("Thunderbird attachments: scanned, found {} documents", docs.len());
        Ok(docs)
    }
}
