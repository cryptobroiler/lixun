//! Thunderbird attachments source — parse mbox files for attachments.

use anyhow::Result;
use lupa_core::{Action, Category, DocId, Document};
use std::path::PathBuf;

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

        for base in [&mail_path, &imap_path] {
            if !base.exists() {
                continue;
            }
            for entry in walkdir::WalkDir::new(base)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if name.ends_with(".msf") || name.starts_with('.') {
                    continue;
                }

                if let Ok(parsed) = parse_mbox_attachments(path) {
                    docs.extend(parsed);
                }
            }
        }

        tracing::info!("Thunderbird attachments: {} documents", docs.len());
        Ok(docs)
    }
}

fn parse_mbox_attachments(path: &std::path::Path) -> Result<Vec<Document>> {
    let bytes = std::fs::read(path)?;
    // mailparse can parse a single message, not full mbox
    // For MVP, scan for "Content-Disposition: attachment" markers
    // Full implementation: split mbox by "From " lines, parse each message

    let content = String::from_utf8_lossy(&bytes);
    let mut docs = Vec::new();
    let mut msg_idx = 0;

    for msg in content.split("\nFrom ") {
        if msg.contains("Content-Disposition: attachment") || msg.contains("filename=\"") {
            // Extract filename from header
            if let Some(filename) = extract_filename(msg) {
                let byte_offset = msg.as_ptr() as usize - bytes.as_ptr() as usize;
                docs.push(Document {
                    id: DocId(format!("att:{}#{}", path.to_string_lossy(), msg_idx)),
                    category: Category::Attachment,
                    title: filename.clone(),
                    subtitle: extract_subject(msg).unwrap_or_else(|| "unknown".into()),
                    body: None, // Would need to extract and decode attachment bytes
                    path: path.to_string_lossy().to_string(),
                    mtime: 0,
                    size: 0,
                    action: Action::OpenAttachment {
                        mbox_path: path.to_path_buf(),
                        byte_offset: byte_offset as u64,
                        length: msg.len() as u64,
                        mime: guess_mime(&filename),
                    },
                    extract_fail: false,
                });
                msg_idx += 1;
            }
        }
    }

    Ok(docs)
}

fn extract_filename(msg: &str) -> Option<String> {
    for line in msg.lines() {
        if let Some(pos) = line.find("filename=\"") {
            let start = pos + 10;
            if let Some(end) = line[start..].find('"') {
                return Some(line[start..start + end].to_string());
            }
        }
    }
    None
}

fn extract_subject(msg: &str) -> Option<String> {
    for line in msg.lines() {
        if line.starts_with("Subject: ") {
            return Some(line["Subject: ".len()..].to_string());
        }
    }
    None
}

fn guess_mime(filename: &str) -> String {
    match filename.rsplit('.').next() {
        Some("pdf") => "application/pdf".into(),
        Some("docx") => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document".into()
        }
        Some("xlsx") => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".into(),
        Some("pptx") => {
            "application/vnd.openxmlformats-officedocument.presentationml.presentation".into()
        }
        Some("txt") => "text/plain".into(),
        _ => "application/octet-stream".into(),
    }
}
