//! Live XMPP adapter tests, run against a real ejabberd.
//!
//! The unit tests in `adapters/xmpp.rs` cover the stanza-to-command mapping and
//! the outbox's behaviour while disconnected. What they cannot cover is
//! *routing*, and routing is where XMPP differs from every other transport
//! here. A broadcast is addressed to a bare JID, and which of a subscriber's
//! logged-in clients that reaches -- one, all, or none -- is the server's
//! decision, made from presence the bot has to have published first. A mock has
//! no opinion about any of that.
//!
//! Requires an ejabberd server at the domain in
//! `PARTY_LINE_PAGER_XMPP_TEST_DOMAIN`. Without that variable these tests print a skip
//! line and pass, so a bare `cargo test` on the host stays green.
//!
//! Accounts are pre-registered on the server, one pair per test, so no two
//! tests share a login and a session left behind by one cannot flake the next.

use std::sync::Arc;
use std::time::{Duration, Instant};

use party_line_pagerd::adapters::xmpp::Xmpp;
use party_line_pagerd::config;
use party_line_pagerd::transport::{Incoming, OutMessage, Transport};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_xmpp::connect::DnsConfig;
use tokio_xmpp::parsers::jid::Jid;
use tokio_xmpp::parsers::message::{Lang, Message, MessageType};
use tokio_xmpp::parsers::presence::Presence;
use tokio_xmpp::parsers::stanza::Stanza;
use tokio_xmpp::xmlstream::Timeouts;
use tokio_xmpp::{Client, Event};

const DOMAIN_ENV: &str = "PARTY_LINE_PAGER_XMPP_TEST_DOMAIN";

/// Pre-registered on the test server with one of these two.
const BOT_PASSWORD: &str = "botpassword";
const HUMAN_PASSWORD: &str = "humanpassword";

/// Long enough that a loaded box does not flake, short enough that a real hang
/// still fails the run rather than hitting cargo's own timeout.
const WAIT: Duration = Duration::from_secs(30);

/// One round of "has it happened yet" polling.
const TICK: Duration = Duration::from_millis(250);

/// How long to watch an inbox that is supposed to stay empty. Proving a
/// negative has to outlast a delivery, or the test passes on nothing having
/// arrived *yet*.
const SILENCE: Duration = Duration::from_secs(5);

/// The XMPP domain under test, or `None` when these tests should skip
/// themselves.
fn domain() -> Option<String> {
    std::env::var(DOMAIN_ENV).ok()
}

/// Skips the test, with a reason, when no server was configured.
macro_rules! xmpp {
    () => {
        match domain() {
            Some(domain) => {
                wait_for_listener(&domain);
                domain
            }
            None => {
                eprintln!("skipped: {DOMAIN_ENV} is unset");
                return;
            }
        }
    };
}

/// Blocks until the server accepts connections.
///
/// Compose starts ejabberd and the test runner together and has no way to know
/// when the first one is ready.
fn wait_for_listener(domain: &str) {
    use std::net::{TcpStream, ToSocketAddrs};

    let deadline = Instant::now() + WAIT;
    loop {
        let connected = (domain, 5222)
            .to_socket_addrs()
            .ok()
            .and_then(|mut addrs| addrs.next())
            .is_some_and(|addr| TcpStream::connect_timeout(&addr, TICK).is_ok());
        if connected {
            return;
        }
        assert!(Instant::now() < deadline, "no XMPP server on {domain}:5222");
        std::thread::sleep(TICK);
    }
}

fn adapter_config(domain: &str, localpart: &str, password: &str) -> config::Xmpp {
    config::Xmpp {
        enabled: true,
        jid: format!("{localpart}@{domain}"),
        password: password.to_string(),
        // The test server has no certificate, which is the entire reason this
        // knob exists. See adapters/xmpp.rs.
        tls: false,
    }
}

/// Starts an adapter in the background, the way `transport::supervise` would.
fn start(cfg: config::Xmpp) -> (Arc<Xmpp>, mpsc::Receiver<Incoming>, JoinHandle<anyhow::Result<()>>)
{
    let xmpp = Arc::new(Xmpp::new(&cfg).expect("adapter config rejected"));
    let (tx, rx) = mpsc::channel(8);
    let running = xmpp.clone();
    let handle = tokio::spawn(async move { running.run(tx).await });
    (xmpp, rx, handle)
}

/// Starts an adapter and waits until it is actually logged in.
///
/// `send` is the readiness probe rather than anything cleverer, because it is
/// the only thing the adapter exposes that knows: it refuses immediately while
/// the connection task is not draining the outbox. The probe messages go to
/// `probe_target`, whose peer ignores anything it is not waiting for.
async fn start_online(
    cfg: config::Xmpp,
    probe_target: &str,
) -> (Arc<Xmpp>, mpsc::Receiver<Incoming>, JoinHandle<anyhow::Result<()>>) {
    let (xmpp, rx, handle) = start(cfg);

    let deadline = Instant::now() + WAIT;
    loop {
        if xmpp
            .send(probe_target, &OutMessage::plain("__probe__"))
            .await
            .is_ok()
        {
            return (xmpp, rx, handle);
        }
        assert!(Instant::now() < deadline, "the adapter never came online");
        tokio::time::sleep(TICK).await;
    }
}

/// An ordinary XMPP client standing in for a subscriber.
///
/// The connection lives in its own task because `tokio_xmpp::Client` cannot be
/// shared, the same constraint the adapter works around, so stanzas travel in
/// and out through channels.
struct Peer {
    outbox: mpsc::Sender<Stanza>,
    inbox: mpsc::UnboundedReceiver<(Jid, String)>,
}

impl Peer {
    /// Connects, and does not return until the server has bound the session and
    /// accepted the initial presence.
    ///
    /// Publishing presence is not optional politeness. A message addressed to a
    /// bare JID is delivered to that account's *available* resources, and an
    /// account with none is offline as far as the server is concerned.
    async fn connect(domain: &str, localpart: &str, password: &str) -> Self {
        let jid = format!("{localpart}@{domain}");
        let mut client = Client::new_plaintext(
            jid.parse::<Jid>().expect("built a malformed JID"),
            password.to_string(),
            DnsConfig::no_srv(domain, 5222),
            Timeouts::default(),
        );

        let (outbox, mut to_send) = mpsc::channel::<Stanza>(16);
        let (received, inbox) = mpsc::unbounded_channel();
        let (online, is_online) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            let mut online = Some(online);
            loop {
                tokio::select! {
                    event = client.next() => {
                        let Some(event) = event else { return };
                        match event {
                            Event::Online { .. } => {
                                // Available, priority 0, which is what any
                                // client sends the moment it finishes binding.
                                let _ = client
                                    .send_stanza(Presence::available().into())
                                    .await;
                                if let Some(online) = online.take() {
                                    let _ = online.send(());
                                }
                            }
                            Event::Stanza(stanza) => {
                                if let Some(pair) = body_of(stanza) {
                                    if received.send(pair).is_err() {
                                        return;
                                    }
                                }
                            }
                            Event::Disconnected(_) => return,
                        }
                    }
                    Some(stanza) = to_send.recv() => {
                        let _ = client.send_stanza(stanza).await;
                    }
                }
            }
        });

        timeout(WAIT, is_online)
            .await
            .unwrap_or_else(|_| panic!("peer {jid} never came online"))
            .expect("peer connection task died before binding");

        Peer { outbox, inbox }
    }

    /// Sends a one-to-one chat message, which is what a subscriber's client
    /// does.
    async fn say(&self, to: &str, text: &str) {
        self.send_typed(to, text, MessageType::Chat).await;
    }

    async fn send_typed(&self, to: &str, text: &str, kind: MessageType) {
        let mut message = Message::new(Some(to.parse::<Jid>().expect("malformed recipient")));
        message.type_ = kind;
        message.bodies.insert(Lang::default(), text.to_string());
        self.outbox
            .send(message.into())
            .await
            .expect("peer connection task is gone");
    }

    /// Waits for the first message body the predicate accepts, ignoring the
    /// readiness probes [`start_online`] scatters around.
    async fn expect(&mut self, within: Duration, mut want: impl FnMut(&str) -> bool) -> Option<String> {
        let inbox = &mut self.inbox;
        timeout(within, async {
            while let Some((_from, body)) = inbox.recv().await {
                if body != "__probe__" && want(&body) {
                    return body;
                }
            }
            unreachable!("the peer connection task outlives every test")
        })
        .await
        .ok()
    }
}

/// Sender and body of a message stanza, or `None` for anything else.
fn body_of(stanza: Stanza) -> Option<(Jid, String)> {
    let message = Message::try_from(stanza).ok()?;
    // Same trap the adapter fell into: the server stamps xml:lang on the
    // <message/> and the <body/> inherits it, so the body is not filed under
    // the empty-string language. See `interpret` in adapters/xmpp.rs.
    let (_lang, body) = message.get_best_body(vec![])?;
    Some((message.from.clone()?, body.to_string()))
}

/// Waits for one [`Incoming`] the adapter considered a command, ignoring the
/// readiness probes.
async fn next_command(rx: &mut mpsc::Receiver<Incoming>, within: Duration) -> Option<Incoming> {
    let deadline = Instant::now() + within;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let incoming = timeout(left, rx.recv()).await.ok()??;
        if incoming.text != "__probe__" {
            return Some(incoming);
        }
    }
}

/// The inbound path: somebody types at the bot from their own client.
///
/// The assertion that matters is the endpoint. A subscriber's client picks a
/// new resource on every reconnect, so a roster keyed on the full JID would
/// lose them the first time their phone changed networks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chat_message_becomes_an_incoming_command() {
    let domain = xmpp!();
    let human_jid = format!("humanchat@{domain}");

    let (_bot, mut rx, _running) =
        start_online(adapter_config(&domain, "botchat", BOT_PASSWORD), &human_jid).await;
    let _human_online = Peer::connect(&domain, "humanchat", HUMAN_PASSWORD).await;

    let human = Peer::connect(&domain, "humanchat", HUMAN_PASSWORD).await;
    human.say(&format!("botchat@{domain}"), "signal").await;

    let got = next_command(&mut rx, WAIT)
        .await
        .expect("the bot never saw a chat message");
    assert_eq!(got.text, "signal");
    assert_eq!(
        got.endpoint.to_string(),
        format!("xmpp:{human_jid}"),
        "the resource has to be stripped, or a reconnect loses the subscriber"
    );
}

/// The outbound path, addressed the only way the roster can address it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_broadcast_reaches_a_subscriber_at_their_bare_jid() {
    let domain = xmpp!();
    let human_jid = format!("humancast@{domain}");

    let mut human = Peer::connect(&domain, "humancast", HUMAN_PASSWORD).await;
    let (bot, _rx, _running) =
        start_online(adapter_config(&domain, "botcast", BOT_PASSWORD), &human_jid).await;

    bot.send(&human_jid, &OutMessage::titled("Signal up", "onion + secret"))
        .await
        .expect("the bot could not broadcast");

    let body = human
        .expect(WAIT, |body| body.contains("onion + secret"))
        .await
        .expect("the subscriber never got the broadcast");
    assert_eq!(
        body, "Signal up\n\nonion + secret",
        "XMPP has no subject line for chat, so the title has to arrive flattened into the body"
    );
}

/// A subscriber logged in on a laptop and a phone has to get the invite on both.
///
/// This is the test a mock cannot write. Nothing in the adapter fans anything
/// out: it sends one stanza to one bare JID, and the *server* decides that both
/// available resources get a copy. Getting presence wrong turns that into one
/// copy, or none.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_broadcast_reaches_every_resource_the_subscriber_has_online() {
    let domain = xmpp!();
    let human_jid = format!("humanfanout@{domain}");

    let mut laptop = Peer::connect(&domain, "humanfanout", HUMAN_PASSWORD).await;
    let mut phone = Peer::connect(&domain, "humanfanout", HUMAN_PASSWORD).await;

    let (bot, _rx, _running) = start_online(
        adapter_config(&domain, "botfanout", BOT_PASSWORD),
        &human_jid,
    )
    .await;

    bot.send(&human_jid, &OutMessage::plain("signal is up"))
        .await
        .expect("the bot could not broadcast");

    for (name, peer) in [("laptop", &mut laptop), ("phone", &mut phone)] {
        assert_eq!(
            peer.expect(WAIT, |body| body == "signal is up")
                .await
                .unwrap_or_else(|| panic!("the subscriber's {name} never got the broadcast")),
            "signal is up"
        );
    }
}

/// The bot may sit in a room so people can find it. Somebody shouting in that
/// room must not be able to drive it, which is the same rule IRC channels get.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn groupchat_stanzas_are_never_treated_as_a_command() {
    let domain = xmpp!();
    let human_jid = format!("humanmuc@{domain}");
    let bot_jid = format!("botmuc@{domain}");

    let (_bot, mut rx, _running) =
        start_online(adapter_config(&domain, "botmuc", BOT_PASSWORD), &human_jid).await;
    let human = Peer::connect(&domain, "humanmuc", HUMAN_PASSWORD).await;

    // A room relays what people say as type="groupchat", so this is the stanza
    // the bot would actually receive from a MUC it had joined.
    human
        .send_typed(&bot_jid, "signal", MessageType::Groupchat)
        .await;

    assert!(
        next_command(&mut rx, SILENCE).await.is_none(),
        "a groupchat message was treated as a command"
    );

    // ...and the same words as a chat message still work, so the test above is
    // not passing because delivery is broken outright.
    human.say(&bot_jid, "signal").await;
    assert_eq!(
        next_command(&mut rx, WAIT)
            .await
            .expect("a chat message stopped working")
            .text,
        "signal"
    );
}

/// A wrong password has to end the run, not spin quietly.
///
/// `transport::supervise` restarts an adapter that returns, with backoff, and
/// logs why. What it cannot do is notice a client that keeps retrying a login
/// the server will never accept, and that is exactly what tokio-xmpp does: the
/// rejection is a debug line inside the library and then another attempt, with
/// nothing on the stream to say so. The operator's view is an adapter that is
/// simply silent, on a daemon that reports itself healthy.
///
/// Nothing can distinguish that from a slow server except time, so the adapter
/// gives a login `LOGIN_TIMEOUT` to finish and then hands the failure up. This
/// test is deliberately slower than every other one here for that reason.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_login_with_the_wrong_password_fails_rather_than_retrying_silently() {
    let domain = xmpp!();
    let (_bot, _rx, running) = start(adapter_config(&domain, "botauth", "not-the-password"));

    // Has to outlast the adapter's own LOGIN_TIMEOUT, or this measures the
    // test's patience instead of the adapter's.
    let outcome = timeout(Duration::from_secs(90), running)
        .await
        .expect("the adapter neither failed nor gave up after a rejected login")
        .expect("the adapter task panicked");

    let error = outcome
        .expect_err("a rejected login has to surface as an error the supervisor can log")
        .to_string();
    assert!(
        error.contains("logging in"),
        "the error has to name the login as the problem, or it is no better than silence: {error}"
    );
}
