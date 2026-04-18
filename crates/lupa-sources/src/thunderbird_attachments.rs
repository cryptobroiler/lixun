//! Thunderbird attachments source — parse mbox files for attachments.

use crate::mbox;
use anyhow::Result;
use lupa_core::{Action, Category, DocId, Document};
use std::path::PathBuf;

pub struct ThunderbirdAttachmentsSource {
    pub profile_path: PathBuf,
    pub max_attachment_bytes: u64,
}

impl ThunderbirdAttachmentsSource {
    pub fn new(profile_path: PathBuf, max_attachment_bytes: u64) -> Self {
        Self {
            profile_path,
            max_attachment_bytes,
        }
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

                let Ok(mbox_bytes) = std::fs::read(path) else {
                    continue;
                };
                let Ok(parts) = mbox::parse_mbox_parts(path) else {
                    continue;
                };

                for part in parts {
                    let id = DocId(format!(
                        "att:{}#{}",
                        part.message_id.clone().unwrap_or_else(|| mbox::fallback_id(
                            &part.mbox_path,
                            part.msg_byte_offset
                        )),
                        part.part_index
                    ));

                    let (body, extract_fail) = if part.part_body_length > self.max_attachment_bytes
                    {
                        (None, true)
                    } else {
                        let start = part.part_body_byte_offset as usize;
                        let end = start + part.part_body_length as usize;
                        if end > mbox_bytes.len() {
                            (None, true)
                        } else {
                            let raw = &mbox_bytes[start..end];
                            match mbox::decode_bytes(raw, part.encoding) {
                                Ok(decoded) => {
                                    let ext_hint = std::path::Path::new(&part.filename)
                                        .extension()
                                        .and_then(|ext| ext.to_str());
                                    match lupa_extract::extract_bytes(&decoded, ext_hint) {
                                        Ok(text) if !text.is_empty() => (Some(text), false),
                                        Ok(_) => (None, false),
                                        Err(_) => (None, true),
                                    }
                                }
                                Err(_) => (None, true),
                            }
                        }
                    };

                    docs.push(Document {
                        id,
                        category: Category::Attachment,
                        title: part.filename.clone(),
                        subtitle: part
                            .subject
                            .clone()
                            .unwrap_or_else(|| "(no subject)".into()),
                        body,
                        path: part.mbox_path.to_string_lossy().to_string(),
                        mtime: 0,
                        size: part.part_body_length,
                        action: Action::OpenAttachment {
                            mbox_path: part.mbox_path.clone(),
                            byte_offset: part.part_body_byte_offset,
                            length: part.part_body_length,
                            mime: part.mime.clone(),
                            encoding: part.encoding.as_mime_str().to_string(),
                            suggested_filename: part.filename.clone(),
                        },
                        extract_fail,
                    });
                }
            }
        }

        tracing::info!("Thunderbird attachments: {} documents", docs.len());
        Ok(docs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Source;
    use base64::Engine;
    use tempfile::tempdir;

    #[test]
    fn test_thunderbird_attachments_source_integration() {
        let dir = tempdir().unwrap();
        let profile = dir.path();
        let inbox_dir = profile.join("Mail").join("Local Folders");
        std::fs::create_dir_all(&inbox_dir).unwrap();

        let body = base64::engine::general_purpose::STANDARD.encode("UNIQUE_TEST_MARKER");
        let mbox = format!(
            "From alice@example.com Wed Jan 01 00:00:00 2025\nFrom: alice@example.com\nSubject: Searchable attachment\nMessage-ID: <integration@example.com>\nContent-Type: multipart/mixed; boundary=\"BB\"\nMIME-Version: 1.0\n\n--BB\nContent-Type: text/plain\n\nhello\n--BB\nContent-Type: text/plain; name=\"note.txt\"\nContent-Disposition: attachment; filename=\"note.txt\"\nContent-Transfer-Encoding: base64\n\n{body}\n--BB--\n"
        );
        std::fs::write(inbox_dir.join("Inbox"), mbox).unwrap();

        let source = ThunderbirdAttachmentsSource::new(profile.to_path_buf(), 100 * 1024 * 1024);
        let docs = source.index_all().unwrap();

        assert!(docs.iter().any(|doc| {
            doc.body
                .as_deref()
                .is_some_and(|body| body.contains("UNIQUE_TEST_MARKER"))
        }));
    }
}
