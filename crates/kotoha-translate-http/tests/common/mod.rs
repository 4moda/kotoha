//! Shared local `tokio::net::TcpListener` HTTP mock server and request fixtures for `kotoha-translate-http`.

#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kotoha_core::domain::{
    HistoryEntry, LanguageCode, SpeakerKey, TranslateContext, TranslationRequest, UtteranceId,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

#[derive(Debug, Clone)]
pub struct RecordedHttpRequest {
    pub method: String,
    pub target: String,
    pub headers: HashMap<String, String>,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct ScriptedHttpResponse {
    pub status_line: &'static str,
    pub headers: Vec<(&'static str, String)>,
    pub body: String,
    pub delay: Duration,
}

impl ScriptedHttpResponse {
    pub fn json_ok(body: impl Into<String>) -> Self {
        Self {
            status_line: "200 OK",
            headers: vec![("Content-Type", "application/json".to_owned())],
            body: body.into(),
            delay: Duration::ZERO,
        }
    }

    pub fn json_status(status_line: &'static str, body: impl Into<String>) -> Self {
        Self {
            status_line,
            headers: vec![("Content-Type", "application/json".to_owned())],
            body: body.into(),
            delay: Duration::ZERO,
        }
    }

    pub fn redirect_302(location: String) -> Self {
        Self {
            status_line: "302 Found",
            headers: vec![("Location", location)],
            body: String::new(),
            delay: Duration::ZERO,
        }
    }
}

pub struct MockHttpServer {
    pub base_url: String,
    recorded: Arc<Mutex<Vec<RecordedHttpRequest>>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl MockHttpServer {
    pub async fn start<F>(handler: F) -> Self
    where
        F: Fn(&RecordedHttpRequest) -> ScriptedHttpResponse + Send + Sync + 'static,
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
                                if !resp.delay.is_zero() {
                                    tokio::time::sleep(resp.delay).await;
                                }
                                let mut raw = format!(
                                    "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                                    resp.status_line,
                                    resp.body.len()
                                );
                                for (k, v) in &resp.headers {
                                    let _ = write!(raw, "{k}: {v}\r\n");
                                }
                                raw.push_str("\r\n");
                                raw.push_str(&resp.body);
                                let _ = stream.write_all(raw.as_bytes()).await;
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

    pub fn requests(&self) -> Vec<RecordedHttpRequest> {
        self.recorded.lock().expect("lock").clone()
    }
}

impl Drop for MockHttpServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

async fn read_http_request(stream: &mut tokio::net::TcpStream) -> Option<RecordedHttpRequest> {
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

    let header_bytes = &buf[..header_end];
    let header_text = String::from_utf8_lossy(header_bytes);
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_owned();
    let target = parts.next()?.to_owned();

    let mut headers = HashMap::new();
    let mut content_length = 0usize;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim().to_owned();
            if key == "content-length" {
                content_length = val.parse().unwrap_or(0);
            }
            headers.insert(key, val);
        }
    }

    let body_start = header_end + 4;
    while buf.len() < body_start + content_length {
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body_end = (body_start + content_length).min(buf.len());
    let body = String::from_utf8_lossy(&buf[body_start..body_end]).into_owned();

    Some(RecordedHttpRequest {
        method,
        target,
        headers,
        body,
    })
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

pub fn sample_request(text: &str) -> TranslationRequest {
    sample_request_langs(text, "en", "ja")
}

pub fn sample_request_langs(text: &str, src: &str, tgt: &str) -> TranslationRequest {
    TranslationRequest {
        utterance_id: UtteranceId(42),
        speaker_key: SpeakerKey::new("mumble", 7),
        speaker_name: "Alice".to_owned(),
        text: text.to_owned(),
        src_lang: LanguageCode::new(src).unwrap(),
        tgt_lang: LanguageCode::new(tgt).unwrap(),
        context: TranslateContext {
            recent_history: vec![HistoryEntry {
                speaker_name: "Bob".to_owned(),
                original_text: "Ready to warp?".to_owned(),
                translated_text: "ワープの準備はいい？".to_owned(),
            }],
            glossary: BTreeMap::from([
                ("Ferox".to_owned(), "Ferox".to_owned()),
                ("M W D".to_owned(), "MWD".to_owned()),
                ("Logi".to_owned(), "ロジ".to_owned()),
            ]),
        },
    }
}
