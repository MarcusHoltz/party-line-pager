//! Signal, through a [signal-cli-rest-api] container.
//!
//! The native Rust option (`presage`) has not been published to crates.io since
//! 2023, and a Signal client is not a thing to run on an unmaintained library.
//! The REST container is actively maintained and keeps `signal-cli` and its JVM
//! out of this binary entirely.
//!
//! Endpoint addresses are E.164 numbers, which is what `/v2/send` wants back.
//!
//! Receiving assumes the container runs in `MODE=json-rpc` (upstream's
//! recommended mode, and what `docker-compose.yml` sets): in that mode
//! `/v1/receive/{number}` is a websocket, not a pollable GET, so this holds
//! one open per [`Transport::run`] rather than polling. The container filters
//! by number server-side, and each frame is a single `{"account", "envelope"}`
//! object rather than the array `normal`/`native` mode's GET returns, so
//! frames are unwrapped into a one-element slice before reusing [`extract`].
//! A message that arrives while nothing is connected is not queued for the
//! next connect, unlike Telegram's backlog: json-rpc mode drops it.
//!
//! [signal-cli-rest-api]: https://github.com/bbernhard/signal-cli-rest-api

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use party_line_pager_core::{EndpointId, Style};
use futures_util::StreamExt;
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::config;
use crate::transport::{Incoming, OutMessage, Transport};

pub struct Signal {
    rest_url: String,
    number: String,
    client: reqwest::Client,
}

impl Signal {
    pub fn new(cfg: &config::Signal) -> Result<Self> {
        Ok(Self {
            rest_url: cfg.rest_url.trim_end_matches('/').to_string(),
            number: cfg.number.clone(),
            client: reqwest::Client::builder()
                .user_agent(crate::USER_AGENT)
                .timeout(Duration::from_secs(60))
                .build()?,
        })
    }
}

#[async_trait]
impl Transport for Signal {
    fn name(&self) -> &str {
        "signal"
    }

    async fn run(&self, tx: mpsc::Sender<Incoming>) -> Result<()> {
        let url = receive_url(&self.rest_url, &self.number);
        let (mut ws, _response) = tokio_tungstenite::connect_async(&url)
            .await
            .with_context(|| format!("signal websocket connect to {url} failed"))?;

        while let Some(msg) = ws.next().await {
            let msg = msg.context("signal websocket read failed")?;
            let WsMessage::Text(text) = msg else {
                continue; // ping/pong/binary/close: tungstenite handles the handshake side
            };
            let Ok(envelope) = serde_json::from_str::<Envelope>(&text) else {
                // signal-cli-rest-api also pushes its own {"msg":"..."} error
                // frames down this socket; do not tear down the connection
                // over one frame that is not an envelope.
                tracing::warn!(frame = %text, "signal websocket sent an unparseable frame");
                continue;
            };
            for incoming in extract(std::slice::from_ref(&envelope)) {
                if tx.send(incoming).await.is_err() {
                    return Ok(());
                }
            }
        }
        Err(anyhow!("signal websocket closed"))
    }

    async fn send(&self, address: &str, msg: &OutMessage) -> Result<()> {
        self.client
            .post(format!("{}/v2/send", self.rest_url))
            .json(&serde_json::json!({
                // signal-cli-rest-api has a `text_mode: styled`, deliberately
                // not used: it is version-dependent and there is no way to
                // detect support, so a mismatch would ship the markers as text.
                "message": msg.render(Style::Plain),
                "number": self.number,
                "recipients": [address],
            }))
            .send()
            .await
            .context("signal send failed")?
            .error_for_status()?;
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct Envelope {
    envelope: Inner,
}

#[derive(Debug, Deserialize)]
struct Inner {
    #[serde(default)]
    source: Option<String>,
    #[serde(default, rename = "sourceNumber")]
    source_number: Option<String>,
    #[serde(default, rename = "dataMessage")]
    data_message: Option<DataMessage>,
}

#[derive(Debug, Deserialize)]
struct DataMessage {
    #[serde(default)]
    message: Option<String>,
    /// Present when the message went to a group rather than to the bot.
    #[serde(default, rename = "groupInfo")]
    group_info: Option<serde_json::Value>,
}

/// The websocket URL for `/v1/receive/{number}`, `rest_url`'s scheme swapped
/// for its websocket equivalent.
fn receive_url(rest_url: &str, number: &str) -> String {
    let scheme_swapped = rest_url
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    format!("{scheme_swapped}/v1/receive/{number}")
}

/// Keeps only one-to-one text messages.
fn extract(envelopes: &[Envelope]) -> Vec<Incoming> {
    envelopes
        .iter()
        .filter_map(|e| {
            let data = e.envelope.data_message.as_ref()?;
            if data.group_info.is_some() {
                return None;
            }
            let text = data.message.as_ref()?;
            // Prefer the E.164 number: it is what /v2/send accepts back.
            let address = e
                .envelope
                .source_number
                .as_ref()
                .or(e.envelope.source.as_ref())?;
            let endpoint = EndpointId::new("signal", address).ok()?;
            Some(Incoming {
                endpoint,
                text: text.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelopes(json: &str) -> Vec<Envelope> {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn direct_messages_become_commands() {
        let got = extract(&envelopes(
            r#"[{"envelope":{
                "source":"+15551234567","sourceNumber":"+15551234567",
                "dataMessage":{"message":"signal","timestamp":1}
            }}]"#,
        ));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].endpoint.to_string(), "signal:+15551234567");
        assert_eq!(got[0].text, "signal");
    }

    #[test]
    fn group_messages_are_ignored() {
        let got = extract(&envelopes(
            r#"[{"envelope":{
                "source":"+15551234567",
                "dataMessage":{"message":"signal","groupInfo":{"groupId":"abc"}}
            }}]"#,
        ));
        assert!(got.is_empty());
    }

    #[test]
    fn receipts_and_typing_indicators_are_ignored() {
        let got = extract(&envelopes(
            r#"[
                {"envelope":{"source":"+1","receiptMessage":{"when":1}}},
                {"envelope":{"source":"+1","typingMessage":{"action":"STARTED"}}}
            ]"#,
        ));
        assert!(got.is_empty());
    }

    #[test]
    fn falls_back_to_source_when_the_number_field_is_absent() {
        let got = extract(&envelopes(
            r#"[{"envelope":{"source":"+15559876543","dataMessage":{"message":"help"}}}]"#,
        ));
        assert_eq!(got[0].endpoint.to_string(), "signal:+15559876543");
    }

    #[tokio::test]
    async fn send_posts_v2_send_with_our_number_and_the_recipient() {
        let (addr, requests) = crate::mock::http_stub(200).await;
        let signal = Signal::new(&config::Signal {
            enabled: true,
            rest_url: format!("http://{addr}"),
            number: "+15550000000".into(),
        })
        .unwrap();

        signal
            .send("+15551234567", &OutMessage::plain("line is up"))
            .await
            .unwrap();

        let request = requests.lock().unwrap().first().cloned().unwrap();
        assert!(request.contains("\"number\":\"+15550000000\""), "{request}");
        assert!(request.contains("\"recipients\":[\"+15551234567\"]"), "{request}");
        assert!(request.contains("line is up"), "{request}");
        assert!(
            request.contains(crate::USER_AGENT),
            "every request has to name the software; some servers reject anonymous \
             clients before reading anything else: {request}"
        );
    }

    #[test]
    fn receive_url_swaps_the_scheme_for_websocket() {
        assert_eq!(
            receive_url("http://signal-cli-rest-api:8080", "+15550000000"),
            "ws://signal-cli-rest-api:8080/v1/receive/+15550000000"
        );
        assert_eq!(
            receive_url("https://signal.example.org", "+15550000000"),
            "wss://signal.example.org/v1/receive/+15550000000"
        );
    }

    /// Guards the exact bug that shipped: signal-cli-rest-api's `MODE=json-rpc`
    /// (what `docker-compose.yml` sets) serves `/v1/receive` as a websocket
    /// carrying single `{"account","envelope"}` objects, not the JSON array a
    /// plain GET would return. A regression back to HTTP polling, or a wire
    /// format mismatch, fails this rather than only failing in production.
    #[tokio::test]
    async fn run_forwards_a_direct_message_delivered_over_the_websocket() {
        let (addr, frames) = crate::mock::ws_stub().await;
        let signal = Signal::new(&config::Signal {
            enabled: true,
            rest_url: format!("http://{addr}"),
            number: "+15551234567".into(),
        })
        .unwrap();

        let (tx, mut rx) = mpsc::channel(8);
        let running = tokio::spawn(async move { signal.run(tx).await });

        // A frame signal-cli-rest-api's own error channel might send, ahead of
        // a real envelope: proves one bad frame does not kill the connection.
        frames.send(r#"{"msg":"rate limited"}"#.to_string()).unwrap();
        frames
            .send(
                r#"{"account":"+15551234567","envelope":{
                    "source":"+15559876543","sourceNumber":"+15559876543",
                    "dataMessage":{"message":"help","timestamp":1}
                }}"#
                .to_string(),
            )
            .unwrap();

        let incoming = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("run() must forward the websocket message before this times out")
            .unwrap();
        assert_eq!(incoming.endpoint.to_string(), "signal:+15559876543");
        assert_eq!(incoming.text, "help");

        running.abort();
    }
}
