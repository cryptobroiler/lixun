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

pub const PROTOCOL_VERSION: u16 = 2;

/// The oldest protocol version this build can negotiate with.
pub const MIN_PROTOCOL_VERSION: u16 = 1;

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
    HitsWithExtras {
        hits: Vec<Hit>,
        calculation: Option<lupa_core::Calculation>,
    },
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
    if let Ok(uid) = std::env::var("UID")
        && let Ok(uid) = uid.parse::<u32>()
    {
        return uid;
    }
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("Uid:")
                && let Ok(uid) = rest
                    .split_whitespace()
                    .next()
                    .unwrap_or("0")
                    .parse::<u32>()
            {
                return uid;
            }
        }
    }
    1000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode_toggle() {
        let mut codec = FrameCodec::default();
        let mut buf = BytesMut::new();
        codec.encode(Request::Toggle, &mut buf).unwrap();

        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert!(matches!(decoded, Request::Toggle));
    }

    #[test]
    fn test_encode_decode_search() {
        let mut codec = FrameCodec::default();
        let mut buf = BytesMut::new();
        let req = Request::Search {
            q: "hello world".to_string(),
            limit: 10,
        };
        codec.encode(req.clone(), &mut buf).unwrap();

        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        match decoded {
            Request::Search { q, limit } => {
                assert_eq!(q, "hello world");
                assert_eq!(limit, 10);
            }
            _ => panic!("Expected Search variant"),
        }
    }

    #[test]
    fn test_encode_decode_reindex() {
        let mut codec = FrameCodec::default();
        let mut buf = BytesMut::new();
        let req = Request::Reindex {
            paths: vec![PathBuf::from("/tmp/test")],
        };
        codec.encode(req, &mut buf).unwrap();

        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        match decoded {
            Request::Reindex { paths } => {
                assert_eq!(paths.len(), 1);
                assert_eq!(paths[0], PathBuf::from("/tmp/test"));
            }
            _ => panic!("Expected Reindex variant"),
        }
    }

    #[test]
    fn test_encode_decode_status() {
        let mut codec = FrameCodec::default();
        let mut buf = BytesMut::new();
        codec.encode(Request::Status, &mut buf).unwrap();

        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        assert!(matches!(decoded, Request::Status));
    }

    #[test]
    fn test_version_mismatch() {
        let mut codec = FrameCodec::default();
        let mut buf = BytesMut::new();
        // Manually craft a frame with wrong version
        let json = serde_json::to_vec(&Request::Toggle).unwrap();
        buf.put_u32((2 + json.len()) as u32);
        buf.put_u16(99);
        buf.put_slice(&json);

        let result = codec.decode(&mut buf);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("version mismatch"));
    }

    #[test]
    fn test_incomplete_frame_returns_none() {
        let mut codec = FrameCodec::default();
        let mut buf = BytesMut::new();
        buf.put_u32(10);
        let result = codec.decode(&mut buf).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_empty_buffer_returns_none() {
        let mut codec = FrameCodec::default();
        let mut buf = BytesMut::new();
        let result = codec.decode(&mut buf).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_multiple_requests_in_buffer() {
        let mut codec = FrameCodec::default();
        let mut buf = BytesMut::new();
        codec.encode(Request::Toggle, &mut buf).unwrap();
        codec.encode(Request::Status, &mut buf).unwrap();

        let first = codec.decode(&mut buf).unwrap().unwrap();
        assert!(matches!(first, Request::Toggle));

        let second = codec.decode(&mut buf).unwrap().unwrap();
        assert!(matches!(second, Request::Status));
    }

    #[test]
    fn test_socket_path_format() {
        let path = socket_path();
        assert!(path.to_string_lossy().contains("lupa.sock"));
    }

    #[test]
    fn test_hits_with_extras_roundtrip_empty() {
        let resp = Response::HitsWithExtras {
            hits: vec![],
            calculation: None,
        };
        let json = serde_json::to_vec(&resp).unwrap();
        let decoded: Response = serde_json::from_slice(&json).unwrap();
        match decoded {
            Response::HitsWithExtras { hits, calculation } => {
                assert!(hits.is_empty());
                assert!(calculation.is_none());
            }
            _ => panic!("expected HitsWithExtras"),
        }
    }

    #[test]
    fn test_hits_with_extras_roundtrip_with_calculation() {
        let resp = Response::HitsWithExtras {
            hits: vec![],
            calculation: Some(lupa_core::Calculation {
                expr: "2+2".into(),
                result: "4".into(),
            }),
        };
        let json = serde_json::to_vec(&resp).unwrap();
        let decoded: Response = serde_json::from_slice(&json).unwrap();
        match decoded {
            Response::HitsWithExtras { calculation, .. } => {
                let c = calculation.expect("calculation present");
                assert_eq!(c.expr, "2+2");
                assert_eq!(c.result, "4");
            }
            _ => panic!("expected HitsWithExtras"),
        }
    }

    #[test]
    fn test_hits_variant_still_parses_v1() {
        let resp = Response::Hits(vec![]);
        let json = serde_json::to_vec(&resp).unwrap();
        let decoded: Response = serde_json::from_slice(&json).unwrap();
        assert!(matches!(decoded, Response::Hits(h) if h.is_empty()));
    }
}
