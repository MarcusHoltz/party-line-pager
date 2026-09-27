//! Test doubles: an in-memory transport and a minimal HTTP stub.
//!
//! These live in the library rather than a `tests/` helper so that unit tests in
//! every module can share them. They pull in no extra dependencies.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Result};
use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use crate::transport::{Incoming, OutMessage, Transport};

/// A transport that records what it was asked to send.
pub struct MockTransport {
    name: String,
    sent: Mutex<Vec<(String, OutMessage)>>,
    failing: Mutex<HashSet<String>>,
    delay: Mutex<Duration>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
}

impl MockTransport {
    pub fn new(name: &str) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_string(),
            sent: Mutex::new(Vec::new()),
            failing: Mutex::new(HashSet::new()),
            delay: Mutex::new(Duration::ZERO),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
        })
    }

    /// Everything this transport was asked to deliver, in completion order.
    pub fn sent(&self) -> Vec<(String, OutMessage)> {
        self.sent.lock().unwrap().clone()
    }

    /// Bodies only, as plain text, for terser assertions.
    ///
    /// Plain rather than the styled variants on purpose: engine tests assert
    /// on what a reply *says*, and markup is `render`'s business, covered by
    /// its own tests.
    pub fn bodies(&self) -> Vec<String> {
        self.sent()
            .into_iter()
            .map(|(_, m)| m.body.plain())
            .collect()
    }

    /// Bodies sent to one address.
    pub fn bodies_to(&self, address: &str) -> Vec<String> {
        self.sent()
            .into_iter()
            .filter(|(to, _)| to == address)
            .map(|(_, m)| m.body.plain())
            .collect()
    }

    pub fn clear(&self) {
        self.sent.lock().unwrap().clear();
    }

    /// Make delivery to this address fail.
    pub fn fail_for(&self, address: &str) {
        self.failing.lock().unwrap().insert(address.to_string());
    }

    pub fn set_delay(&self, delay: Duration) {
        *self.delay.lock().unwrap() = delay;
    }

    pub fn max_concurrent(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Transport for MockTransport {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(&self, _tx: mpsc::Sender<Incoming>) -> Result<()> {
        // Engine tests drive commands directly, so there is nothing to poll.
        std::future::pending::<()>().await;
        Ok(())
    }

    async fn send(&self, address: &str, msg: &OutMessage) -> Result<()> {
        let current = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(current, Ordering::SeqCst);

        let delay = *self.delay.lock().unwrap();
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }

        let failing = self.failing.lock().unwrap().contains(address);
        if !failing {
            self.sent
                .lock()
                .unwrap()
                .push((address.to_string(), msg.clone()));
        }
        self.in_flight.fetch_sub(1, Ordering::SeqCst);

        if failing {
            bail!("mock transport refused {address}");
        }
        Ok(())
    }
}

/// Writes an executable shell script and returns its path.
pub fn write_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A one-shot HTTP server that answers every request with `status` and records
/// each request in full, headers and body. Used to stand in for the Apprise
/// sidecar and for the three adapters that speak plain REST.
///
/// The headers are recorded and not just the body because at least one thing
/// worth asserting lives up there: a request with no `User-Agent` is rejected
/// outright by some servers, which is a bug you cannot see by looking at the
/// JSON you sent.
pub async fn http_stub(status: u16) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    http_stub_body(status, String::new()).await
}

/// As [`http_stub`], but every response carries `body`.
pub async fn http_stub_body(
    status: u16,
    body: String,
) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&requests);
    let body = Arc::new(body);

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let sink = Arc::clone(&sink);
            let body = Arc::clone(&body);
            tokio::spawn(async move {
                let mut raw = Vec::new();
                let mut buf = [0u8; 4096];

                // Read headers, then exactly as many body bytes as advertised.
                loop {
                    let Ok(n) = socket.read(&mut buf).await else {
                        return;
                    };
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&buf[..n]);

                    let text = String::from_utf8_lossy(&raw).to_string();
                    let Some(header_end) = text.find("\r\n\r\n") else {
                        continue;
                    };
                    let want: usize = text[..header_end]
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().ok())?
                        })
                        .unwrap_or(0);
                    if raw.len() >= header_end + 4 + want {
                        sink.lock().unwrap().push(text);
                        break;
                    }
                }

                let reason = if (200..300).contains(&status) { "OK" } else { "ERR" };
                let _ = socket
                    .write_all(
                        format!(
                            "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        )
                        .as_bytes(),
                    )
                    .await;
                let _ = socket.shutdown().await;
            });
        }
    });

    (addr, requests)
}

/// A one-shot websocket server that accepts a single connection and forwards
/// every string handed to the returned sender down the socket as a text
/// frame. Stands in for signal-cli-rest-api's `/v1/receive` endpoint in
/// `MODE=json-rpc`, which is a websocket rather than a pollable GET.
pub async fn ws_stub() -> (SocketAddr, mpsc::UnboundedSender<String>) {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    tokio::spawn(async move {
        let Ok((socket, _)) = listener.accept().await else {
            return;
        };
        let Ok(mut ws) = tokio_tungstenite::accept_async(socket).await else {
            return;
        };
        while let Some(text) = rx.recv().await {
            if ws.send(Message::Text(text.into())).await.is_err() {
                return;
            }
        }
    });

    (addr, tx)
}
