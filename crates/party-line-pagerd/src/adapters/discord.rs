//! Discord, over the gateway websocket for inbound and REST for outbound.
//!
//! Discord's inbound is gateway-websocket-only; there is no polling endpoint,
//! so this is the one adapter that cannot be hand-rolled on `reqwest` the way
//! Telegram, Mastodon and Signal are. `twilight-gateway` carries the socket,
//! `twilight-http` carries the `POST`.
//!
//! Message content in DMs is exempt from the privileged `MESSAGE_CONTENT`
//! intent (Discord's own gateway docs list "Content in DMs with the app" as
//! an exception), so the bot needs only the plain `DIRECT_MESSAGES` intent
//! and no verification gauntlet. A bot may only DM someone it shares a guild
//! with, which is why enrollment is "join this server, then DM the bot"
//! rather than a user-installed app: see the README for the lobby-server
//! design that keeps that mutual guild from leaking the roster.
//!
//! Only messages with no `guild_id` are treated as commands, and any message
//! from another bot (including the broadcast this adapter just sent, which
//! comes back over the same gateway connection) is dropped, so a broadcast
//! cannot echo back into the parser.
//!
//! Endpoint addresses are the sender's Discord snowflake, immutable and
//! unaffected by username changes.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use party_line_pager_core::{EndpointId, Style};
use tokio::sync::mpsc;
use twilight_gateway::{Event, EventTypeFlags, Intents, Shard, ShardId, StreamExt as _};
use twilight_http::Client;
use twilight_model::gateway::payload::incoming::MessageCreate;
use twilight_model::id::marker::UserMarker;
use twilight_model::id::Id;

use crate::config;
use crate::transport::{Incoming, OutMessage, Transport};

pub struct Discord {
    http: Client,
    token: String,
}

impl Discord {
    pub fn new(cfg: &config::Discord) -> Result<Self> {
        // twilight's gateway websocket brings its own rustls stack (it does
        // not go through crate::crypto::client_config() the way the plain
        // reqwest-based adapters do), so it needs the same one-time provider
        // install Matrix does for the same reason.
        crate::crypto::install_default_provider();

        Ok(Self {
            http: Client::new(cfg.token.clone()),
            token: cfg.token.clone(),
        })
    }
}

#[async_trait]
impl Transport for Discord {
    fn name(&self) -> &str {
        "discord"
    }

    async fn run(&self, tx: mpsc::Sender<Incoming>) -> Result<()> {
        let mut shard = Shard::new(ShardId::ONE, self.token.clone(), Intents::DIRECT_MESSAGES);

        while let Some(item) = shard.next_event(EventTypeFlags::MESSAGE_CREATE).await {
            let event = match item {
                Ok(event) => event,
                Err(e) => {
                    // Transient: the shard handles its own reconnect and
                    // resume internally. Only a closed stream (`None` above)
                    // is unrecoverable enough to hand back to the supervisor.
                    tracing::warn!(error = ?e, "discord gateway error, continuing");
                    continue;
                }
            };
            if let Event::MessageCreate(message) = event {
                if let Some(incoming) = extract(&message) {
                    if tx.send(incoming).await.is_err() {
                        return Ok(()); // engine is gone, so are we
                    }
                }
            }
        }
        Err(anyhow!("discord gateway closed"))
    }

    async fn send(&self, address: &str, msg: &OutMessage) -> Result<()> {
        let user_id: Id<UserMarker> = address
            .parse()
            .with_context(|| format!("{address:?} is not a Discord user id"))?;

        let channel = self
            .http
            .create_private_channel(user_id)
            .await
            .with_context(|| format!("could not open a DM with {address}"))?
            .model()
            .await
            .context("discord sent an unparseable private-channel response")?;

        self.http
            .create_message(channel.id)
            .content(&msg.render(Style::Markdown))
            .await
            .with_context(|| format!("discord send to {address} failed"))?;
        Ok(())
    }
}

/// Keeps only DMs from someone who is not a bot: guild traffic and the bot's
/// own broadcast never reach the engine.
fn extract(message: &MessageCreate) -> Option<Incoming> {
    if message.guild_id.is_some() {
        return None;
    }
    if message.author.bot {
        return None;
    }
    if message.content.is_empty() {
        return None;
    }
    let endpoint = EndpointId::new("discord", message.author.id.to_string()).ok()?;
    Some(Incoming {
        endpoint,
        text: message.content.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message_create(json: serde_json::Value) -> MessageCreate {
        serde_json::from_value(json).expect("test fixture did not match twilight's Message shape")
    }

    /// The keys a real Discord `MESSAGE_CREATE` payload actually carries.
    /// Twilight's `Message` has many `Option` fields with no `#[serde(default)]`,
    /// so unlike a hand-trimmed fixture, every optional key here is still
    /// present, just null, matching what Discord itself sends.
    fn base_fields() -> serde_json::Value {
        serde_json::json!({
            "id": "334385199974967042",
            "channel_id": "290926798999357250",
            "author": {
                "id": "53908099506183680",
                "username": "Mason",
                "discriminator": "9999",
                "avatar": null,
                "bot": false
            },
            "content": "signal",
            "timestamp": "2017-07-11T17:27:07.299000+00:00",
            "edited_timestamp": null,
            "tts": false,
            "mention_everyone": false,
            "mentions": [],
            "mention_roles": [],
            "attachments": [],
            "embeds": [],
            "pinned": false,
            "type": 0,
            "guild_id": null,
            "member": null,
            "flags": null,
            "call": null,
            "application_id": null,
            "application": null,
            "activity": null,
            "interaction": null,
            "interaction_metadata": null,
            "message_reference": null,
            "referenced_message": null,
            "role_subscription_data": null,
            "thread": null,
            "webhook_id": null,
            "poll": null
        })
    }

    fn with(overrides: serde_json::Value) -> MessageCreate {
        let mut fields = base_fields();
        let obj = fields.as_object_mut().unwrap();
        for (k, v) in overrides.as_object().unwrap() {
            obj.insert(k.clone(), v.clone());
        }
        message_create(fields)
    }

    #[test]
    fn a_dm_becomes_an_incoming_command() {
        let got = extract(&with(serde_json::json!({}))).unwrap();
        assert_eq!(got.endpoint.to_string(), "discord:53908099506183680");
        assert_eq!(got.text, "signal");
    }

    #[test]
    fn guild_traffic_is_never_treated_as_a_command() {
        let msg = with(serde_json::json!({"guild_id": "999999999999999999"}));
        assert!(extract(&msg).is_none());
    }

    #[test]
    fn a_bots_message_is_never_treated_as_a_command() {
        let msg = with(serde_json::json!({"author": {
            "id": "53908099506183680", "username": "Mason", "discriminator": "9999",
            "avatar": null, "bot": true
        }}));
        assert!(
            extract(&msg).is_none(),
            "must drop other bots and this bot's own broadcast alike"
        );
    }

    #[test]
    fn empty_content_is_ignored() {
        let msg = with(serde_json::json!({"content": ""}));
        assert!(extract(&msg).is_none());
    }
}
