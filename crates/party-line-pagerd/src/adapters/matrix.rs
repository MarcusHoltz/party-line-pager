//! Matrix, over `matrix-sdk`.
//!
//! End-to-end encrypted by default, which makes it the best place to receive a
//! room secret out of the seven transports.
//!
//! Only messages in **direct** rooms are treated as commands, and the bot's own
//! messages are skipped so a broadcast cannot echo back into the parser.
//!
//! Matrix is the only transport where being talked to starts with an invitation
//! rather than a message, so the adapter accepts them: see [`Matrix::accept`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use party_line_pager_core::{EndpointId, Style};
use matrix_sdk::config::SyncSettings;
use matrix_sdk::ruma::events::room::member::StrippedRoomMemberEvent;
use matrix_sdk::ruma::events::room::message::{
    MessageType, RoomMessageEventContent, SyncRoomMessageEvent,
};
use matrix_sdk::ruma::OwnedUserId;
use matrix_sdk::ruma::UserId;
use matrix_sdk::{Client, Room, RoomState};
use tokio::sync::mpsc;

use crate::config;
use crate::transport::{Incoming, OutMessage, Transport};

/// How many times an invite is retried before it is given up on.
///
/// A join straight after an invite can lose a race with the homeserver's own
/// view of the room and come back as an error that is gone a second later.
const JOIN_ATTEMPTS: u32 = 5;

pub struct Matrix {
    client: Client,
    me: OwnedUserId,
    /// The supervisor restarts `run` after every sync failure, and event
    /// handlers registered with the SDK are cumulative. Registering twice would
    /// deliver every message twice, so this happens exactly once.
    handler_registered: AtomicBool,
}

impl Matrix {
    pub async fn new(cfg: &config::Matrix) -> Result<Self> {
        crate::crypto::install_default_provider();

        let client = Client::builder()
            .homeserver_url(&cfg.homeserver)
            .sqlite_store(&cfg.store_path, None)
            .build()
            .await
            .with_context(|| format!("could not reach the homeserver {}", cfg.homeserver))?;

        client
            .matrix_auth()
            .login_username(&cfg.user, &cfg.password)
            .initial_device_display_name("PartyLinePager")
            .await
            .context("Matrix login failed")?;

        let me = client
            .user_id()
            .context("logged in but the homeserver returned no user id")?
            .to_owned();

        Ok(Self {
            client,
            me,
            handler_registered: AtomicBool::new(false),
        })
    }
}

#[async_trait]
impl Transport for Matrix {
    fn name(&self) -> &str {
        "matrix"
    }

    async fn run(&self, tx: mpsc::Sender<Incoming>) -> Result<()> {
        let me = self.me.clone();

        if !self.handler_registered.swap(true, Ordering::SeqCst) {
            self.register_invite_handler(me.clone());
            self.register_handler(tx, me);
        }

        // Never returns unless the connection is unrecoverable.
        self.client
            .sync(SyncSettings::default())
            .await
            .context("Matrix sync ended")?;
        Ok(())
    }

    async fn send(&self, address: &str, msg: &OutMessage) -> Result<()> {
        let user_id = UserId::parse(address)
            .with_context(|| format!("{address:?} is not a Matrix user id"))?;

        // Reuse the existing direct room when there is one, so a subscriber
        // does not collect a new room per broadcast.
        let room = match self.client.get_dm_room(&user_id) {
            Some(room) => room,
            None => self
                .client
                .create_dm(&user_id)
                .await
                .with_context(|| format!("could not open a direct room with {address}"))?,
        };

        // Both halves, not just the HTML: `formatted_body` is an enrichment
        // and a client that ignores it falls back to the plain body, so
        // sending only markup would leave those clients with nothing.
        room.send(RoomMessageEventContent::text_html(
            msg.render(Style::Plain),
            msg.render(Style::MatrixHtml),
        ))
        .await
        .with_context(|| format!("Matrix send to {address} failed"))?;
        Ok(())
    }
}

impl Matrix {
    /// Accepts invitations, and remembers which ones were direct.
    ///
    /// A Matrix conversation starts with an invitation, not a message. An
    /// invite that is never accepted has no timeline to sync, so the message
    /// somebody sends straight after inviting the bot is never delivered
    /// anywhere. Without this the bot is simply deaf to every new subscriber,
    /// which is what `tests/matrix_live.rs` demonstrates when this handler is
    /// taken out.
    ///
    /// Invitations to ordinary rooms are accepted too, and deliberately left
    /// unmarked. Idling in a room is how people find the bot, exactly as on
    /// IRC, and leaving it unmarked is what keeps room traffic out of the
    /// command parser.
    ///
    /// The `set_is_direct` call is what makes `Room::is_direct` true on our
    /// side later, and it is not redundant even though the live tests pass
    /// without it. `is_direct` on a joined room reads *our own* `m.direct`
    /// account data; Synapse happens to write that for the invitee when the
    /// invite carries the flag, but that is one homeserver's behaviour rather
    /// than something the specification promises, and the spec puts `m.direct`
    /// in the client's hands. Doing it here costs one request per new
    /// subscriber and removes the dependency.
    fn register_invite_handler(&self, me: OwnedUserId) {
        self.client
            .add_event_handler(move |event: StrippedRoomMemberEvent, room: Room| {
                let me = me.clone();
                async move {
                    // Membership events for everybody else in the room arrive
                    // here too. Only our own invitation is ours to answer.
                    if event.state_key != me || room.state() != RoomState::Invited {
                        return;
                    }

                    let mut backoff = Duration::from_millis(250);
                    for attempt in 1..=JOIN_ATTEMPTS {
                        match room.join().await {
                            Ok(()) => break,
                            Err(e) if attempt == JOIN_ATTEMPTS => {
                                tracing::warn!(
                                    room = %room.room_id(),
                                    error = ?e,
                                    "gave up joining a Matrix room we were invited to"
                                );
                                return;
                            }
                            Err(e) => {
                                tracing::debug!(
                                    room = %room.room_id(),
                                    error = ?e,
                                    "join failed, retrying"
                                );
                                tokio::time::sleep(backoff).await;
                                backoff *= 2;
                            }
                        }
                    }

                    if event.content.is_direct == Some(true) {
                        if let Err(e) = room.set_is_direct(true).await {
                            tracing::warn!(
                                room = %room.room_id(),
                                error = ?e,
                                "joined a direct room but could not record it as direct, \
                                 so messages in it will be ignored"
                            );
                        }
                    }
                }
            });
    }

    fn register_handler(&self, tx: mpsc::Sender<Incoming>, me: OwnedUserId) {
        self.client.add_event_handler(
            move |event: SyncRoomMessageEvent, room: Room| {
                let tx = tx.clone();
                let me = me.clone();
                async move {
                    // Joined rooms only: an invite we have not accepted has no
                    // history to act on.
                    if room.state() != RoomState::Joined {
                        return;
                    }
                    let Some(original) = event.as_original() else {
                        return; // redacted
                    };
                    if original.sender == me {
                        return; // our own broadcast, echoed back by sync
                    }
                    let MessageType::Text(ref text) = original.content.msgtype else {
                        return;
                    };
                    match room.is_direct().await {
                        Ok(true) => {}
                        Ok(false) => return,
                        Err(e) => {
                            tracing::warn!(error = ?e, "could not tell whether a room is direct");
                            return;
                        }
                    }
                    let Ok(endpoint) = EndpointId::new("matrix", original.sender.as_str()) else {
                        return;
                    };
                    let _ = tx
                        .send(Incoming {
                            endpoint,
                            text: text.body.clone(),
                        })
                        .await;
                }
            },
        );
    }
}
