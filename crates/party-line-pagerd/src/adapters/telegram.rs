//! Telegram, over the Bot API's long-poll endpoint.
//!
//! No framework here on purpose. The partyline pager needs exactly two calls,
//! `getUpdates` and `sendMessage`, and a bot exposed to strangers is a poor
//! place to carry a dispatcher framework's dependency tree.
//!
//! Endpoint addresses are numeric chat ids, which never change, so the roster
//! survives a user renaming themselves.

use std::sync::atomic::{AtomicI64, Ordering};

use anyhow::{Context, Result};
use async_trait::async_trait;
use party_line_pager_core::{EndpointId, Style};
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::config;
use crate::transport::{Incoming, OutMessage, Transport};

/// Seconds Telegram holds a `getUpdates` request open with nothing to say.
const LONG_POLL_SECS: u64 = 30;

pub struct Telegram {
    api_base: String,
    client: reqwest::Client,
    offset: AtomicI64,
}

impl Telegram {
    pub fn new(cfg: &config::Telegram) -> Result<Self> {
        Self::with_base(format!("https://api.telegram.org/bot{}", cfg.token))
    }

    fn with_base(api_base: String) -> Result<Self> {
        Ok(Self {
            api_base,
            client: reqwest::Client::builder()
                .user_agent(crate::USER_AGENT)
                // Long poll plus headroom, so a quiet chat is not a timeout.
                .timeout(std::time::Duration::from_secs(LONG_POLL_SECS + 15))
                .build()?,
            offset: AtomicI64::new(0),
        })
    }
}

impl Telegram {
    /// Confirms the backlog without acting on it.
    ///
    /// Telegram holds unconfirmed updates for about 24 hours and replays them
    /// to the next `getUpdates` with offset 0. Without this, restarting the
    /// daemon would re-run every command it had already handled, and a `signal`
    /// from last night would summon people to a party nobody is at.
    async fn prime(&self) -> Result<()> {
        let response: Envelope<Vec<Update>> = self
            .client
            .get(format!("{}/getUpdates", self.api_base))
            .query(&[("timeout", "0"), ("offset", "-1"), ("limit", "1")])
            .send()
            .await
            .context("telegram priming getUpdates failed")?
            .json()
            .await
            .context("telegram sent unparseable JSON while priming")?;

        if let Some(last) = response.result.unwrap_or_default().last() {
            self.offset.store(last.update_id + 1, Ordering::SeqCst);
            tracing::info!(
                skipped_through = last.update_id,
                "discarded the telegram backlog from before startup"
            );
        }
        Ok(())
    }
}

#[async_trait]
impl Transport for Telegram {
    fn name(&self) -> &str {
        "telegram"
    }

    async fn run(&self, tx: mpsc::Sender<Incoming>) -> Result<()> {
        // Only on a cold start. After a reconnect the offset is already past
        // the backlog, and re-priming would drop messages that arrived during
        // the outage.
        if self.offset.load(Ordering::SeqCst) == 0 {
            if let Err(e) = self.prime().await {
                tracing::warn!(error = ?e, "telegram priming failed, continuing");
            }
        }

        loop {
            let offset = self.offset.load(Ordering::SeqCst);
            let response: Envelope<Vec<Update>> = self
                .client
                .get(format!("{}/getUpdates", self.api_base))
                .query(&[
                    ("timeout", LONG_POLL_SECS.to_string()),
                    ("offset", offset.to_string()),
                    ("allowed_updates", "[\"message\"]".to_string()),
                ])
                .send()
                .await
                .context("telegram getUpdates failed")?
                .json()
                .await
                .context("telegram sent unparseable JSON")?;

            if !response.ok {
                anyhow::bail!("telegram refused getUpdates: {:?}", response.description);
            }

            let updates = response.result.unwrap_or_default();
            if let Some(highest) = updates.iter().map(|u| u.update_id).max() {
                // Acknowledge by asking for everything after this one.
                self.offset.store(highest + 1, Ordering::SeqCst);
            }

            for incoming in extract(&updates) {
                if tx.send(incoming).await.is_err() {
                    return Ok(()); // engine is gone, so are we
                }
            }
        }
    }

    async fn send(&self, address: &str, msg: &OutMessage) -> Result<()> {
        let response: Envelope<serde_json::Value> = self
            .client
            .post(format!("{}/sendMessage", self.api_base))
            // HTML rather than MarkdownV2 on purpose. MarkdownV2 requires
            // escaping eighteen characters anywhere they appear, and an
            // unescaped one is a 400 rather than a cosmetic slip: a subscriber
            // note containing a full stop would lose the whole broadcast. HTML
            // mode needs three characters escaped and `render` handles them.
            .json(&serde_json::json!({
                "chat_id": address,
                "text": msg.render(Style::TelegramHtml),
                "parse_mode": "HTML",
                "disable_web_page_preview": true,
            }))
            .send()
            .await
            .context("telegram sendMessage failed")?
            .json()
            .await?;

        if !response.ok {
            anyhow::bail!(
                "telegram refused sendMessage to {address}: {:?}",
                response.description
            );
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    ok: bool,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    result: Option<T>,
}

#[derive(Debug, Deserialize)]
struct Update {
    update_id: i64,
    #[serde(default)]
    message: Option<Message>,
}

#[derive(Debug, Deserialize)]
struct Message {
    chat: Chat,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Chat {
    id: i64,
    #[serde(rename = "type")]
    kind: String,
}

/// Keeps only private text messages.
///
/// Group and channel traffic is dropped before it ever reaches the parser, so
/// adding the bot to a busy group cannot turn shouting into commands.
fn extract(updates: &[Update]) -> Vec<Incoming> {
    updates
        .iter()
        .filter_map(|update| {
            let message = update.message.as_ref()?;
            if message.chat.kind != "private" {
                return None;
            }
            let text = message.text.as_ref()?;
            let endpoint = EndpointId::new("telegram", message.chat.id.to_string()).ok()?;
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

    fn updates(json: &str) -> Vec<Update> {
        serde_json::from_str::<Envelope<Vec<Update>>>(json)
            .unwrap()
            .result
            .unwrap()
    }

    #[test]
    fn private_text_messages_become_commands() {
        let got = extract(&updates(
            r#"{"ok":true,"result":[
                {"update_id":10,"message":{"chat":{"id":4242,"type":"private"},"text":"signal"}}
            ]}"#,
        ));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].endpoint.to_string(), "telegram:4242");
        assert_eq!(got[0].text, "signal");
    }

    #[test]
    fn group_traffic_is_ignored_entirely() {
        let got = extract(&updates(
            r#"{"ok":true,"result":[
                {"update_id":11,"message":{"chat":{"id":-100,"type":"group"},"text":"signal"}},
                {"update_id":12,"message":{"chat":{"id":-101,"type":"supergroup"},"text":"signal"}},
                {"update_id":13,"message":{"chat":{"id":-102,"type":"channel"},"text":"signal"}}
            ]}"#,
        ));
        assert!(got.is_empty(), "only direct messages may reach the engine");
    }

    #[test]
    fn non_text_updates_are_skipped() {
        let got = extract(&updates(
            r#"{"ok":true,"result":[
                {"update_id":14,"message":{"chat":{"id":1,"type":"private"}}},
                {"update_id":15}
            ]}"#,
        ));
        assert!(got.is_empty());
    }

    #[tokio::test]
    async fn priming_skips_the_backlog_instead_of_replaying_it() {
        let (addr, _requests) = crate::mock::http_stub_body(
            200,
            r#"{"ok":true,"result":[{"update_id":42,"message":{"chat":{"id":1,"type":"private"},"text":"signal"}}]}"#
                .to_string(),
        )
        .await;
        let telegram = Telegram::with_base(format!("http://{addr}")).unwrap();

        telegram.prime().await.unwrap();

        assert_eq!(
            telegram.offset.load(Ordering::SeqCst),
            43,
            "the next poll must ask for updates after the backlog"
        );
    }

    #[tokio::test]
    async fn priming_on_an_empty_backlog_leaves_the_offset_alone() {
        let (addr, _requests) =
            crate::mock::http_stub_body(200, r#"{"ok":true,"result":[]}"#.to_string()).await;
        let telegram = Telegram::with_base(format!("http://{addr}")).unwrap();

        telegram.prime().await.unwrap();

        assert_eq!(telegram.offset.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn send_posts_the_flattened_message() {
        let (addr, requests) = crate::mock::http_stub(200).await;
        let telegram = Telegram::with_base(format!("http://{addr}")).unwrap();

        // The stub answers with an empty body, so the JSON decode fails after
        // the request is made. The request itself is what matters here.
        let _ = telegram
            .send("4242", &OutMessage::titled("PartyLinePager", "line is up"))
            .await;

        let request = requests.lock().unwrap().first().cloned().unwrap();
        assert!(request.contains("\"chat_id\":\"4242\""), "{request}");
        assert!(request.contains("line is up"), "{request}");
        assert!(
            request.contains(crate::USER_AGENT),
            "every request has to name the software; some servers reject anonymous \
             clients before reading anything else: {request}"
        );
    }
}
