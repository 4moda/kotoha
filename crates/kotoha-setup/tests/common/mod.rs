//! Shared test fixtures and local HTTP server helper for `kotoha-setup` integration tests.

#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

pub fn unique_temp_dir(prefix: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("{prefix}_{}_{seq}", std::process::id()))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub target: String,
    pub headers: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct ScriptedResponse {
    pub status_line: &'static str,
    pub headers: Vec<(&'static str, String)>,
    pub chunks: Vec<Vec<u8>>,
    pub omit_content_length: bool,
}

impl ScriptedResponse {
    pub fn ok_bytes(body: Vec<u8>) -> Self {
        Self {
            status_line: "200 OK",
            headers: vec![("Content-Type", "application/octet-stream".to_owned())],
            chunks: vec![body],
            omit_content_length: false,
        }
    }

    pub fn ok_chunked(chunks: Vec<Vec<u8>>) -> Self {
        Self {
            status_line: "200 OK",
            headers: vec![("Content-Type", "application/octet-stream".to_owned())],
            chunks,
            omit_content_length: false,
        }
    }

    pub fn status(status_line: &'static str, body: &[u8]) -> Self {
        Self {
            status_line,
            headers: vec![("Content-Type", "text/plain".to_owned())],
            chunks: vec![body.to_vec()],
            omit_content_length: false,
        }
    }

    pub fn redirect_302(location: String) -> Self {
        Self {
            status_line: "302 Found",
            headers: vec![("Location", location)],
            chunks: Vec::new(),
            omit_content_length: false,
        }
    }
}

pub struct LocalDownloadServer {
    pub base_url: String,
    recorded: Arc<Mutex<Vec<RecordedRequest>>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl LocalDownloadServer {
    pub async fn start<F>(handler: F) -> Self
    where
        F: Fn(&RecordedRequest) -> ScriptedResponse + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let recorded_bg = Arc::clone(&recorded);
        let handler = Arc::new(handler);
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => {
                        let Ok((mut stream, _)) = accepted else {
                            break;
                        };
                        let recorded_conn = Arc::clone(&recorded_bg);
                        let handler_conn = Arc::clone(&handler);
                        tokio::spawn(async move {
                            if let Some(req) = read_http_request(&mut stream).await {
                                recorded_conn.lock().expect("lock").push(req.clone());
                                let resp = handler_conn(&req);
                                let total_len: usize = resp.chunks.iter().map(Vec::len).sum();
                                let mut header = format!(
                                    "HTTP/1.1 {}\r\nConnection: close\r\n",
                                    resp.status_line
                                );
                                if !resp.omit_content_length {
                                    let _ = write!(header, "Content-Length: {total_len}\r\n");
                                }
                                for (k, v) in &resp.headers {
                                    let _ = write!(header, "{k}: {v}\r\n");
                                }
                                header.push_str("\r\n");
                                let _ = stream.write_all(header.as_bytes()).await;
                                for chunk in &resp.chunks {
                                    let _ = stream.write_all(chunk).await;
                                    let _ = stream.flush().await;
                                }
                                let _ = stream.shutdown().await;
                            }
                        });
                    }
                }
            }
        });

        Self {
            base_url: format!("http://{addr}"),
            recorded,
            shutdown_tx: Some(shutdown_tx),
        }
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.recorded.lock().expect("lock").clone()
    }
}

impl Drop for LocalDownloadServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

async fn read_http_request(stream: &mut tokio::net::TcpStream) -> Option<RecordedRequest> {
    let mut buf = Vec::with_capacity(2048);
    let mut tmp = [0u8; 1024];
    let header_end = loop {
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos;
        }
    };

    let header_text = String::from_utf8_lossy(&buf[..header_end]);
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_owned();
    let target = parts.next()?.to_owned();

    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
        }
    }

    Some(RecordedRequest {
        method,
        target,
        headers,
    })
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}
