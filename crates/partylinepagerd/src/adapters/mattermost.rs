//! Mattermost, over its REST API and WebSocket event stream.
//!
//! Self-hosted, so there is no roster-leak or platform-policy tradeoff to
//! design around the way there is for Discord: the admin already runs the
//! server, and a bot account is first-class. A personal or bot access token
//! authenticates both the REST calls and the WebSocket.
//!
//! Only **direct** posts count as commands (`channel_type == "D"`). The bot's
//! own posts are dropped too, so replying to a command cannot loop back into
//! the parser as a second command.
//!
//! Endpoint addresses are Mattermost's 26-character user ids, immutable across
//! username changes. `send` opens (or reopens; the call is idempotent) the
//! direct channel between the bot and the address, then posts into it. There
//! is no channel-id bookkeeping to maintain between calls.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use partylinepager_core::{EndpointId, Style};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::config;
use crate::transport::{Incoming, OutMessage, Transport};

pub struct Mattermost {
    base_url: String,
    token: String,
    /// This bot's own user id, used to build the direct channel and to drop
    /// its own posts back out of the event stream.
    self_id: String,
    client: reqwest::Client,
}

impl Mattermost {
    pub async fn new(cfg: &config::Mattermost) -> Result<Self> {
        let base_url = cfg.base_url.trim_end_matches('/').to_string();
        let client = reqwest::Client::builder()
            .user_agent(crate::USER_AGENT)
            .timeout(Duration::from_secs(30))
            .build()?;

        let me: User = client
            .get(format!("{base_url}/api/v4/users/me"))
            .bearer_auth(&cfg.access_token)
            .send()
            .await
            .context("mattermost users/me request failed")?
            .error_for_status()?
            .json()
            .await
            .context("mattermost sent unparseable JSON for users/me")?;

        Ok(Self {
            base_url,
            token: cfg.access_token.clone(),
            self_id: me.id,
            client,
        })
    }
}

#[async_trait]
impl Transport for Mattermost {
    fn name(&self) -> &str {
        "mattermost"
    }

    async fn run(&self, tx: mpsc::Sender<Incoming>) -> Result<()> {
        let url = websocket_url(&self.base_url);
        let (mut ws, _response) = tokio_tungstenite::connect_async(&url)
            .await
            .with_context(|| format!("mattermost websocket connect to {url} failed"))?;

        // Authenticate over the socket itself: there is no header-based
        // handshake for this endpoint. The server answers with a `hello`
        // event on success; a frame that fails auth would come back as an
        // error event instead, which the loop below simply will not match and
        // so leaves the connection to eventually time out. Good enough: the
        // supervisor retries a connection that never produces anything.
        ws.send(WsMessage::Text(
            serde_json::json!({
                "seq": 1,
                "action": "authentication_challenge",
                "data": {"token": self.token},
            })
            .to_string()
            .into(),
        ))
        .await
        .context("mattermost websocket authentication failed")?;

        while let Some(msg) = ws.next().await {
            let msg = msg.context("mattermost websocket read failed")?;
            let WsMessage::Text(text) = msg else {
                continue; // ping/pong/binary/close
            };
            let Ok(event) = serde_json::from_str::<Event>(&text) else {
                continue;
            };
            if let Some(incoming) = extract(&event, &self.self_id) {
                if tx.send(incoming).await.is_err() {
                    return Ok(());
                }
            }
        }
        Err(anyhow!("mattermost websocket closed"))
    }

    async fn send(&self, address: &str, msg: &OutMessage) -> Result<()> {
        let channel: Channel = self
            .client
            .post(format!("{}/api/v4/channels/direct", self.base_url))
            .bearer_auth(&self.token)
            .json(&[self.self_id.as_str(), address])
            .send()
            .await
            .context("mattermost direct-channel open failed")?
            .error_for_status()?
            .json()
            .await
            .context("mattermost sent unparseable JSON for channels/direct")?;

        self.client
            .post(format!("{}/api/v4/posts", self.base_url))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "channel_id": channel.id,
                "message": msg.render(Style::Markdown),
            }))
            .send()
            .await
            .context("mattermost post failed")?
            .error_for_status()?;
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct User {
    id: String,
}

#[derive(Debug, Deserialize)]
struct Channel {
    id: String,
}

#[derive(Debug, Deserialize)]
struct Event {
    event: String,
    #[serde(default)]
    data: EventData,
}

#[derive(Debug, Default, Deserialize)]
struct EventData {
    #[serde(default)]
    channel_type: Option<String>,
    /// The post, JSON-encoded a second time inside this string. Mattermost's
    /// own webapp does the same double parse; it is not a quirk of this
    /// client.
    #[serde(default)]
    post: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Post {
    user_id: String,
    message: String,
}

/// The websocket URL for `/api/v4/websocket`, `base_url`'s scheme swapped for
/// its websocket equivalent.
fn websocket_url(base_url: &str) -> String {
    let scheme_swapped = base_url
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    format!("{scheme_swapped}/api/v4/websocket")
}

/// Keeps only direct posts from someone other than the bot itself.
fn extract(event: &Event, self_id: &str) -> Option<Incoming> {
    if event.event != "posted" {
        return None;
    }
    if event.data.channel_type.as_deref() != Some("D") {
        return None;
    }
    let post: Post = serde_json::from_str(event.data.post.as_deref()?).ok()?;
    if post.user_id == self_id {
        return None;
    }
    let endpoint = EndpointId::new("mattermost", &post.user_id).ok()?;
    Some(Incoming {
        endpoint,
        text: post.message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn posted_event(channel_type: &str, user_id: &str, message: &str) -> Event {
        let post = serde_json::json!({"user_id": user_id, "message": message}).to_string();
        serde_json::from_value(serde_json::json!({
            "event": "posted",
            "data": {"channel_type": channel_type, "post": post},
        }))
        .unwrap()
    }

    #[test]
    fn a_direct_post_becomes_an_incoming_command() {
        let event = posted_event("D", "u1", "signal");
        let got = extract(&event, "bot1").unwrap();
        assert_eq!(got.endpoint.to_string(), "mattermost:u1");
        assert_eq!(got.text, "signal");
    }

    #[test]
    fn channel_traffic_is_never_treated_as_a_command() {
        for channel_type in ["O", "P", "G"] {
            let event = posted_event(channel_type, "u1", "signal");
            assert!(
                extract(&event, "bot1").is_none(),
                "{channel_type} must not be actionable"
            );
        }
    }

    #[test]
    fn the_bots_own_post_is_never_treated_as_a_command() {
        let event = posted_event("D", "bot1", "onion + secret");
        assert!(extract(&event, "bot1").is_none());
    }

    #[test]
    fn non_posted_events_are_ignored() {
        for event in [
            r#"{"event":"hello","data":{}}"#,
            r#"{"event":"typing","data":{"channel_type":"D"}}"#,
        ] {
            let event: Event = serde_json::from_str(event).unwrap();
            assert!(extract(&event, "bot1").is_none());
        }
    }

    #[test]
    fn websocket_url_swaps_the_scheme() {
        assert_eq!(
            websocket_url("http://mattermost-test:8065"),
            "ws://mattermost-test:8065/api/v4/websocket"
        );
        assert_eq!(
            websocket_url("https://mattermost.example.org"),
            "wss://mattermost.example.org/api/v4/websocket"
        );
    }

    #[tokio::test]
    async fn new_fetches_the_bots_own_user_id() {
        let (addr, _requests) = crate::mock::http_stub_body(200, r#"{"id":"bot123"}"#.into()).await;
        let mattermost = Mattermost::new(&config::Mattermost {
            enabled: true,
            base_url: format!("http://{addr}"),
            access_token: "t".into(),
        })
        .await
        .unwrap();
        assert_eq!(mattermost.self_id, "bot123");
    }

    #[tokio::test]
    async fn send_opens_a_direct_channel_then_posts_into_it() {
        let (addr, requests) = crate::mock::http_stub_body(200, r#"{"id":"chan123"}"#.into()).await;
        let mattermost = Mattermost {
            base_url: format!("http://{addr}"),
            token: "t".into(),
            self_id: "bot1".into(),
            client: reqwest::Client::new(),
        };

        mattermost
            .send("u1", &OutMessage::plain("onion + secret"))
            .await
            .unwrap();

        let reqs = requests.lock().unwrap().clone();
        assert_eq!(reqs.len(), 2, "{reqs:?}");
        assert!(reqs[0].contains("/api/v4/channels/direct"), "{}", reqs[0]);
        assert!(reqs[0].contains("bot1") && reqs[0].contains("u1"), "{}", reqs[0]);
        assert!(reqs[1].contains("/api/v4/posts"), "{}", reqs[1]);
        assert!(reqs[1].contains("\"channel_id\":\"chan123\""), "{}", reqs[1]);
        assert!(reqs[1].contains("onion + secret"), "{}", reqs[1]);
    }

    #[tokio::test]
    async fn run_forwards_a_direct_message_delivered_over_the_websocket() {
        let (addr, frames) = crate::mock::ws_stub().await;
        let mattermost = Mattermost {
            base_url: format!("http://{addr}"),
            token: "t".into(),
            self_id: "bot1".into(),
            client: reqwest::Client::new(),
        };

        let (tx, mut rx) = mpsc::channel(8);
        let running = tokio::spawn(async move { mattermost.run(tx).await });

        // A "hello" event, and one for a channel the bot is not being DMed
        // in: neither should ever reach the engine.
        frames.send(r#"{"event":"hello","data":{}}"#.to_string()).unwrap();
        frames
            .send(
                serde_json::json!({
                    "event": "posted",
                    "data": {
                        "channel_type": "O",
                        "post": serde_json::json!({"user_id": "u1", "message": "signal"}).to_string(),
                    },
                })
                .to_string(),
            )
            .unwrap();
        frames
            .send(
                serde_json::json!({
                    "event": "posted",
                    "data": {
                        "channel_type": "D",
                        "post": serde_json::json!({"user_id": "u1", "message": "signal"}).to_string(),
                    },
                })
                .to_string(),
            )
            .unwrap();

        let incoming = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("run() must forward the websocket message before this times out")
            .unwrap();
        assert_eq!(incoming.endpoint.to_string(), "mattermost:u1");
        assert_eq!(incoming.text, "signal");

        running.abort();
    }
}
