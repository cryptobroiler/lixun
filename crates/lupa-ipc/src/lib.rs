//! Lupa IPC — unix-socket protocol and codec.
//!
//! Transport: unix-socket at `$XDG_RUNTIME_DIR/lupa.sock`, fallback `/tmp/lupa-$UID.sock`.
//! Framing: `u32` big-endian length prefix + JSON payload.
//! Socket permissions: `0600`.

use bytes::{Buf, BufMut, BytesMut};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio_util::codec::{Decoder, Encoder};

use lupa_core::Hit;

pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    Toggle,
    Show,
    Hide,
    Search { q: String, limit: u32 },
    Reindex { paths: Vec<PathBuf> },
    Status,
    RecordClick { doc_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    Ok,
    Hits(Vec<Hit>),
    Status {
        indexed_docs: u64,
        last_reindex: Option<DateTime<Utc>>,
        errors: u32,
    },
    Visibility {
        visible: bool,
    },
    Error(String),
}

/// Framing codec: u32 BE length + u16 version + JSON payload.
pub struct FrameCodec {
    state: DecodeState,
}

enum DecodeState {
    Header,
    Version(usize),
    Payload(usize),
}

impl Default for FrameCodec {
    fn default() -> Self {
        Self {
            state: DecodeState::Header,
        }
    }
}

impl Encoder<Request> for FrameCodec {
    type Error = std::io::Error;

    fn encode(&mut self, item: Request, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let json = serde_json::to_vec(&item)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let total_len = 2 + json.len();
        dst.put_u32(total_len as u32);
        dst.put_u16(PROTOCOL_VERSION);
        dst.put_slice(&json);
        Ok(())
    }
}

impl Decoder for FrameCodec {
    type Item = Request;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        loop {
            match self.state {
                DecodeState::Header => {
                    if src.len() < 4 {
                        return Ok(None);
                    }
                    let len = src.get_u32() as usize;
                    if len < 2 {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "frame too short for version",
                        ));
                    }
                    self.state = DecodeState::Version(len);
                }
                DecodeState::Version(len) => {
                    if src.len() < 2 {
                        return Ok(None);
                    }
                    let version = src.get_u16();
                    if version != PROTOCOL_VERSION {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!(
                                "version mismatch: expected {}, got {}",
                                PROTOCOL_VERSION, version
                            ),
                        ));
                    }
                    self.state = DecodeState::Payload(len - 2);
                }
                DecodeState::Payload(len) => {
                    if src.len() < len {
                        return Ok(None);
                    }
                    let data = src.split_to(len);
                    let req: Request = serde_json::from_slice(&data)
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                    self.state = DecodeState::Header;
                    return Ok(Some(req));
                }
            }
        }
    }
}

/// Determine the socket path.
pub fn socket_path() -> PathBuf {
    let uid = get_uid();
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        let path = PathBuf::from(runtime_dir).join("lupa.sock");
        return path;
    }
    PathBuf::from(format!("/tmp/lupa-{}.sock", uid))
}

fn get_uid() -> u32 {
    // Try UID environment variable first (set by login/systemd)
    if let Ok(uid) = std::env::var("UID") {
        if let Ok(uid) = uid.parse::<u32>() {
            return uid;
        }
    }
    // Fallback: read from /proc (Linux-specific)
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("Uid:") {
                if let Ok(uid) = rest
                    .trim()
                    .split_whitespace()
                    .next()
                    .unwrap_or("0")
                    .parse::<u32>()
                {
                    return uid;
                }
            }
        }
    }
    // Last resort fallback
    1000
}
