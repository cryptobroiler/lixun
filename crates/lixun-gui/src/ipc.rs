//! IPC client: background thread sending search requests and a fire-and-forget
//! record-click helper.
//!
//! Service-mode session epoch (G1.6): each request carries the session
//! epoch at send-time. The response thread compares that snapshot to the
//! current epoch before committing results; hides bump the epoch via
//! `LauncherController::reset_session` so stale replies land in a new
//! session and are discarded.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use lixun_core::{Calculation, Hit};
use lixun_ipc::{socket_path, Request, Response, PROTOCOL_VERSION};

pub(crate) struct IpcClient {
    pub(crate) request_tx: mpsc::Sender<(String, u32, u64)>,
    pub(crate) responses: Arc<Mutex<Vec<Hit>>>,
    pub(crate) calculation: Arc<Mutex<Option<Calculation>>>,
    pub(crate) response_epoch: Arc<AtomicU64>,
}

impl Clone for IpcClient {
    fn clone(&self) -> Self {
        Self {
            request_tx: self.request_tx.clone(),
            responses: Arc::clone(&self.responses),
            calculation: Arc::clone(&self.calculation),
            response_epoch: Arc::clone(&self.response_epoch),
        }
    }
}

pub(crate) fn start_ipc_thread(session_epoch: Arc<AtomicU64>) -> IpcClient {
    let (tx, rx) = mpsc::channel::<(String, u32, u64)>();
    let responses: Arc<Mutex<Vec<Hit>>> = Arc::new(Mutex::new(Vec::new()));
    let calculation: Arc<Mutex<Option<Calculation>>> = Arc::new(Mutex::new(None));
    let response_epoch: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    let resp_clone = Arc::clone(&responses);
    let calc_clone = Arc::clone(&calculation);
    let epoch_clone = Arc::clone(&response_epoch);

    std::thread::spawn(move || {
        while let Ok((query, limit, epoch_at_send)) = rx.recv() {
            let sock = socket_path();
            let req = Request::Search { q: query, limit };
            let json = match serde_json::to_vec(&req) {
                Ok(j) => j,
                Err(e) => {
                    tracing::error!("Failed to serialize search request: {}", e);
                    continue;
                }
            };
            let total_len = (2 + json.len()) as u32;
            let mut buf = Vec::with_capacity(4 + 2 + json.len());
            buf.extend_from_slice(&total_len.to_be_bytes());
            buf.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
            buf.extend_from_slice(&json);

            let mut stream = match std::os::unix::net::UnixStream::connect(&sock) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("Failed to connect to daemon at {:?}: {}", sock, e);
                    continue;
                }
            };

            if let Err(e) = stream.write_all(&buf) {
                tracing::error!("Failed to send search request: {}", e);
                continue;
            }

            let mut header = [0u8; 4];
            if let Err(e) = stream.read_exact(&mut header) {
                tracing::error!("Failed to read response header: {}", e);
                continue;
            }
            let resp_len = u32::from_be_bytes(header) as usize;
            if resp_len < 2 {
                tracing::error!("Response frame too short");
                continue;
            }
            let mut _version = [0u8; 2];
            if let Err(e) = stream.read_exact(&mut _version) {
                tracing::error!("Failed to read response version: {}", e);
                continue;
            }
            let mut resp_buf = vec![0u8; resp_len - 2];
            if let Err(e) = stream.read_exact(&mut resp_buf) {
                tracing::error!("Failed to read response body: {}", e);
                continue;
            }

            if epoch_at_send != session_epoch.load(Ordering::SeqCst) {
                tracing::debug!(
                    "ipc: dropping reply from stale session (sent in epoch {})",
                    epoch_at_send
                );
                continue;
            }

            match serde_json::from_slice::<Response>(&resp_buf) {
                Ok(Response::Hits(hits)) => {
                    if let Ok(mut r) = resp_clone.lock() {
                        *r = hits;
                    }
                    if let Ok(mut c) = calc_clone.lock() {
                        *c = None;
                    }
                    epoch_clone.fetch_add(1, Ordering::SeqCst);
                }
                Ok(Response::HitsWithExtras { hits, calculation }) => {
                    if let Ok(mut r) = resp_clone.lock() {
                        *r = hits;
                    }
                    if let Ok(mut c) = calc_clone.lock() {
                        *c = calculation;
                    }
                    epoch_clone.fetch_add(1, Ordering::SeqCst);
                }
                Ok(Response::Error(msg)) => {
                    tracing::error!("Daemon error: {}", msg);
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::error!("Failed to parse response: {}", e);
                }
            }
        }
    });

    IpcClient {
        request_tx: tx,
        responses,
        calculation,
        response_epoch,
    }
}

#[allow(dead_code)]
pub(crate) fn send_record_query(q: &str) {
    let sock = socket_path();
    let req = Request::RecordQuery { q: q.to_string() };
    let Ok(json) = serde_json::to_vec(&req) else {
        return;
    };
    let total_len = (2 + json.len()) as u32;
    let mut buf = Vec::with_capacity(4 + 2 + json.len());
    buf.extend_from_slice(&total_len.to_be_bytes());
    buf.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    buf.extend_from_slice(&json);

    if let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&sock) {
        let _ = stream.write_all(&buf);
    }
}

pub(crate) fn request_search_history(limit: u32) -> Vec<String> {
    let sock = socket_path();
    let req = Request::SearchHistory { limit };
    let Ok(json) = serde_json::to_vec(&req) else {
        return Vec::new();
    };
    let total_len = (2 + json.len()) as u32;
    let mut buf = Vec::with_capacity(4 + 2 + json.len());
    buf.extend_from_slice(&total_len.to_be_bytes());
    buf.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    buf.extend_from_slice(&json);

    let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&sock) else {
        return Vec::new();
    };
    if stream.write_all(&buf).is_err() {
        return Vec::new();
    }

    let mut header = [0u8; 4];
    if stream.read_exact(&mut header).is_err() {
        return Vec::new();
    }
    let resp_len = u32::from_be_bytes(header) as usize;
    if resp_len < 2 {
        return Vec::new();
    }
    let mut version = [0u8; 2];
    if stream.read_exact(&mut version).is_err() {
        return Vec::new();
    }
    let mut resp_buf = vec![0u8; resp_len - 2];
    if stream.read_exact(&mut resp_buf).is_err() {
        return Vec::new();
    }
    match serde_json::from_slice::<Response>(&resp_buf) {
        Ok(Response::Queries(qs)) => qs,
        _ => Vec::new(),
    }
}

pub(crate) fn send_record_click(doc_id: &str) {
    let sock = socket_path();
    let req = Request::RecordClick {
        doc_id: doc_id.to_string(),
    };
    let Ok(json) = serde_json::to_vec(&req) else {
        return;
    };
    let total_len = (2 + json.len()) as u32;
    let mut buf = Vec::with_capacity(4 + 2 + json.len());
    buf.extend_from_slice(&total_len.to_be_bytes());
    buf.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    buf.extend_from_slice(&json);

    if let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&sock) {
        let _ = stream.write_all(&buf);
    }
}

pub(crate) fn send_preview_request(hit: &Hit) {
    let sock = socket_path();
    let req = Request::Preview {
        hit: Box::new(hit.clone()),
    };
    let Ok(json) = serde_json::to_vec(&req) else {
        return;
    };
    let total_len = (2 + json.len()) as u32;
    let mut buf = Vec::with_capacity(4 + 2 + json.len());
    buf.extend_from_slice(&total_len.to_be_bytes());
    buf.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    buf.extend_from_slice(&json);

    tracing::info!("gui: send_preview_request hit_id={}", hit.id.0);
    if let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&sock) {
        let _ = stream.write_all(&buf);
    }
}
