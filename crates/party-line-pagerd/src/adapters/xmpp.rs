//! XMPP, over `tokio-xmpp`.
//!
//! Only `chat` stanzas count. `groupchat` and `error` types are ignored, so a
//! MUC the bot happens to join cannot drive it.
//!
//! The client cannot be shared across tasks, so outbound messages travel to the
//! connection task through a channel and wait for an acknowledgement. When the
//! connection is down, [`Transport::send`] fails immediately rather than
//! queueing a stanza nobody will ever read.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use party_line_pager_core::{EndpointId, Style};
use futures_util::StreamExt;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_xmpp::connect::DnsConfig;
use tokio_xmpp::parsers::jid::{BareJid, Jid};
use tokio_xmpp::parsers::message::{Lang, Message, MessageType};
use tokio_xmpp::parsers::presence::Presence;
use tokio_xmpp::xmlstream::Timeouts;
use tokio_xmpp::{Client, Event};

use crate::config;
use crate::transport::{Incoming, OutMessage, Transport};

struct Outgoing {
    to: String,
    body: String,
    ack: oneshot::Sender<Result<()>>,
}

/// Ceiling on how long a queued stanza waits for the connection task to
/// acknowledge it. Belt and braces behind the fanout's own timeout.
const ACK_TIMEOUT: Duration = Duration::from_secs(30);

/// The client port every XMPP server listens on, and the fallback the SRV
/// lookup uses when a domain publishes no `_xmpp-client._tcp` record.
const CLIENT_PORT: u16 = 5222;

/// How long a connection has to finish logging in before the adapter gives up.
///
/// `tokio-xmpp` reconnects on its own and never reports a login it has stopped
/// believing in: a rejected password produces a debug line inside the library
/// and another attempt, forever. From the outside that is indistinguishable
/// from a server that is merely slow, so the only signal left is time. Long
/// enough that a loaded or distant server is not mistaken for a wrong password,
/// short enough that somebody watching the log finds out today.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(60);

pub struct Xmpp {
    jid: BareJid,
    password: String,
    tls: bool,
    /// Replaced on every reconnect, because the old channel is closed when the
    /// connection task drains it.
    outbox: Mutex<mpsc::Sender<Outgoing>>,
    /// Taken by [`Transport::run`]. Present exactly once.
    inbox: Mutex<Option<mpsc::Receiver<Outgoing>>>,
    /// True while the connection task is draining the outbox. Without this, a
    /// send while disconnected would sit in the channel waiting for an
    /// acknowledgement that nobody is left to give.
    draining: AtomicBool,
}

impl Xmpp {
    pub fn new(cfg: &config::Xmpp) -> Result<Self> {
        crate::crypto::install_default_provider();

        let jid: BareJid = cfg
            .jid
            .parse()
            .with_context(|| format!("{:?} is not a bare JID", cfg.jid))?;
        let (outbox, inbox) = mpsc::channel(64);

        Ok(Self {
            jid,
            password: cfg.password.clone(),
            tls: cfg.tls,
            outbox: Mutex::new(outbox),
            inbox: Mutex::new(Some(inbox)),
            draining: AtomicBool::new(false),
        })
    }

    /// Opens a connection to the server the JID's domain names.
    ///
    /// With TLS on this is the ordinary path: an SRV lookup for
    /// `_xmpp-client._tcp`, falling back to the domain's own address on the
    /// client port, then STARTTLS. With TLS off there is nothing to look up,
    /// because the only server anybody points a plaintext client at is one they
    /// named on purpose.
    fn connect(&self) -> Client {
        if self.tls {
            return Client::new(self.jid.clone(), self.password.clone());
        }

        tracing::warn!(
            jid = %self.jid,
            "connecting to XMPP without TLS: the password is going over the wire in the clear"
        );
        Client::new_plaintext(
            self.jid.clone(),
            self.password.clone(),
            DnsConfig::no_srv(self.jid.domain().as_str(), CLIENT_PORT),
            Timeouts::default(),
        )
    }
}

#[async_trait]
impl Transport for Xmpp {
    fn name(&self) -> &str {
        "xmpp"
    }

    async fn run(&self, tx: mpsc::Sender<Incoming>) -> Result<()> {
        let mut outgoing = self
            .inbox
            .lock()
            .await
            .take()
            .context("the XMPP adapter was already running")?;

        let mut client = self.connect();

        let login_deadline = tokio::time::sleep(LOGIN_TIMEOUT);
        tokio::pin!(login_deadline);
        let mut online = false;

        let result = loop {
            tokio::select! {
                () = &mut login_deadline, if !online => {
                    break Err(anyhow!(
                        "XMPP did not finish logging in within {}s; check the JID and the password",
                        LOGIN_TIMEOUT.as_secs()
                    ));
                }
                event = client.next() => {
                    let Some(event) = event else {
                        break Err(anyhow!("XMPP stream ended"));
                    };
                    match event {
                        Event::Online { .. } => {
                            if let Err(e) = announce_presence(&mut client).await {
                                break Err(e);
                            }
                            // Only now. Setting this before the session is
                            // bound makes `send` claim the transport is up
                            // while the login is still in flight, or has
                            // already been rejected, which is exactly the
                            // false success the flag exists to prevent.
                            self.draining.store(true, Ordering::SeqCst);
                            online = true;
                        }
                        // tokio-xmpp reconnects by itself, and left alone it
                        // will do that forever, including against a password
                        // the server is never going to accept. That produces
                        // an adapter which is simply silent, with nothing in
                        // the log to say why. Handing the failure up instead
                        // puts the reason in the log once per attempt and
                        // hands the retrying to `transport::supervise`, whose
                        // backoff is the one the rest of the daemon uses.
                        Event::Disconnected(e) => {
                            break Err(anyhow::Error::new(e).context("XMPP disconnected"));
                        }
                        Event::Stanza(stanza) => {
                            if let Some(incoming) = interpret(Some(stanza)) {
                                if tx.send(incoming).await.is_err() {
                                    break Ok(());
                                }
                            }
                        }
                    }
                }
                Some(out) = outgoing.recv() => {
                    let sent = send_stanza(&mut client, &out.to, &out.body).await;
                    let _ = out.ack.send(sent);
                }
            }
        };

        self.draining.store(false, Ordering::SeqCst);

        // Drain whatever is still queued and fail it, so no caller is left
        // waiting on an acknowledgement that will never come.
        outgoing.close();
        while let Some(stranded) = outgoing.recv().await {
            let _ = stranded
                .ack
                .send(Err(anyhow!("XMPP disconnected before the message went out")));
        }

        // Hand a fresh channel back so a reconnect can pick it up.
        let (outbox, inbox) = mpsc::channel(64);
        *self.outbox.lock().await = outbox;
        *self.inbox.lock().await = Some(inbox);
        result
    }

    async fn send(&self, address: &str, msg: &OutMessage) -> Result<()> {
        if !self.draining.load(Ordering::SeqCst) {
            return Err(anyhow!("XMPP is not connected right now"));
        }

        let (ack, wait) = oneshot::channel();
        self.outbox
            .lock()
            .await
            .try_send(Outgoing {
                to: address.to_string(),
                // XEP-0393 message styling would give this bold and monospace,
                // but it is advisory and client support is uneven enough that
                // half the roster would read the markers as literal text.
                body: msg.render(Style::Plain),
                ack,
            })
            .map_err(|_| anyhow!("XMPP is not connected right now"))?;

        match tokio::time::timeout(ACK_TIMEOUT, wait).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(anyhow!("XMPP connection dropped before the message went out")),
            Err(_) => Err(anyhow!("XMPP did not acknowledge the message in time")),
        }
    }
}

/// Publishes `<presence/>`, which is what makes the bot reachable at all.
///
/// A message addressed to a bare JID is delivered to that account's *available*
/// resources, and a client that has bound a session but published no presence
/// has none. The bot is then logged in, visibly connected on the server, and
/// silently unreachable: every `signal` anybody sends it is dropped or spooled
/// into offline storage it will never read, because the roster only ever knows
/// people by their bare JID. Outbound still works, which is what makes this
/// worth a comment: the adapter looks half-alive rather than broken.
async fn announce_presence(client: &mut Client) -> Result<()> {
    client
        .send_stanza(Presence::available().into())
        .await
        .map(|_token| ())
        .context("could not publish XMPP presence")
}

async fn send_stanza(client: &mut Client, to: &str, body: &str) -> Result<()> {
    let to: Jid = to
        .parse()
        .with_context(|| format!("{to:?} is not a JID"))?;
    let mut message = Message::new(Some(to));
    message.type_ = MessageType::Chat;
    message.bodies.insert(Lang::default(), body.to_string());
    client
        .send_stanza(message.into())
        .await
        // The token tracks delivery through the stream. Queued is good enough
        // here: the fanout already treats every transport as best effort.
        .map(|_token| ())
        .context("XMPP send failed")
}

/// Turns a stanza into an [`Incoming`], keeping only one-to-one chat messages
/// that carry a body.
fn interpret(stanza: Option<tokio_xmpp::Stanza>) -> Option<Incoming> {
    let message = Message::try_from(stanza?).ok()?;
    if matches!(message.type_, MessageType::Error | MessageType::Groupchat) {
        return None;
    }
    // Not `bodies[""]`. Bodies are keyed by their effective `xml:lang`, which
    // XML says is inherited, and a server that stamps `xml:lang='en'` on the
    // <message/> therefore files the body under "en" rather than under the
    // empty string. Looking only under the empty string means every message
    // from such a server -- ejabberd does this by default -- parses fine and
    // then vanishes. An empty preference list says the same thing the bot
    // means: no language is better than any other, take whichever one came.
    let (_lang, body) = message.get_best_body(vec![])?;
    let from = message.from.as_ref()?.to_bare();
    let endpoint = EndpointId::new("xmpp", from.to_string()).ok()?;
    Some(Incoming {
        endpoint,
        text: body.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_xmpp::Stanza;

    fn message(from: &str, body: &str, kind: MessageType) -> Stanza {
        let mut m = Message::new(Some("bot@example.org".parse::<Jid>().unwrap()));
        m.from = Some(from.parse().unwrap());
        m.type_ = kind;
        m.bodies.insert(Lang::default(), body.to_string());
        m.into()
    }

    #[test]
    fn chat_messages_become_commands() {
        let got = interpret(Some(message(
            "marcus@example.org/phone",
            "signal",
            MessageType::Chat,
        )))
        .unwrap();
        assert_eq!(
            got.endpoint.to_string(),
            "xmpp:marcus@example.org",
            "the resource must be stripped so the roster survives reconnects"
        );
        assert_eq!(got.text, "signal");
    }

    #[test]
    fn groupchat_and_error_stanzas_are_ignored() {
        for kind in [MessageType::Groupchat, MessageType::Error] {
            assert!(interpret(Some(message("room@muc.example.org", "signal", kind))).is_none());
        }
    }

    #[test]
    fn a_body_tagged_with_a_language_is_still_a_command() {
        // What ejabberd actually sends: the server puts xml:lang on the
        // <message/>, the <body/> inherits it, and the body is filed under
        // "en" instead of under the empty string. Reading only the empty
        // string dropped every one of these.
        let mut m = Message::new(Some("bot@example.org".parse::<Jid>().unwrap()));
        m.from = Some("marcus@example.org/phone".parse().unwrap());
        m.type_ = MessageType::Chat;
        m.bodies.insert(Lang::from("en"), "signal".to_string());

        let got = interpret(Some(m.into())).unwrap();
        assert_eq!(got.text, "signal");
        assert_eq!(got.endpoint.to_string(), "xmpp:marcus@example.org");
    }

    #[test]
    fn bodyless_stanzas_are_ignored() {
        let mut m = Message::new(Some("bot@example.org".parse::<Jid>().unwrap()));
        m.from = Some("marcus@example.org".parse().unwrap());
        m.type_ = MessageType::Chat;
        assert!(interpret(Some(m.into())).is_none());
    }

    fn adapter() -> Xmpp {
        Xmpp::new(&config::Xmpp {
            enabled: true,
            jid: "bot@example.org".into(),
            password: "x".into(),
            tls: true,
        })
        .unwrap()
    }

    #[tokio::test]
    async fn sending_while_disconnected_fails_immediately() {
        let xmpp = adapter();

        // Nothing is draining the outbox because run() never started. Without
        // the connection flag this would queue and then wait forever for an
        // acknowledgement, hanging the fanout slot.
        let err = tokio::time::timeout(
            Duration::from_secs(1),
            xmpp.send("marcus@example.org", &OutMessage::plain("hi")),
        )
        .await
        .expect("send must not block while disconnected")
        .unwrap_err()
        .to_string();

        assert!(err.contains("not connected"), "{err}");
    }

    #[tokio::test]
    async fn a_queued_message_is_failed_rather_than_stranded_when_the_link_drops() {
        let xmpp = adapter();
        xmpp.draining.store(true, Ordering::SeqCst);

        // Queue one message, then simulate the connection task exiting: it
        // closes the channel and fails everything still in flight.
        let sending = {
            let (ack, wait) = oneshot::channel();
            xmpp.outbox
                .lock()
                .await
                .try_send(Outgoing {
                    to: "marcus@example.org".into(),
                    body: "hi".into(),
                    ack,
                })
                .unwrap();
            wait
        };

        let mut inbox = xmpp.inbox.lock().await.take().unwrap();
        inbox.close();
        while let Some(stranded) = inbox.recv().await {
            let _ = stranded.ack.send(Err(anyhow!("disconnected")));
        }

        let result = tokio::time::timeout(Duration::from_secs(1), sending)
            .await
            .expect("the waiter must be released, not stranded")
            .unwrap();
        assert!(result.is_err());
    }
}
