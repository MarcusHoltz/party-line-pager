//! Live IRC adapter tests, run against a real ircd.
//!
//! The unit tests in `adapters/irc.rs` cover the pure functions. Everything
//! that made IRC worth testing at all is on the other side of a socket: the
//! SASL exchange, which nick the server actually gives us, whether a broadcast
//! survives line wrapping, and whether channel traffic really is ignored. None
//! of that can be asserted against a mock, because a mock agrees with whatever
//! the code already does.
//!
//! `docker compose run --rm test` starts the ircd (see the `irc-test` service)
//! and sets `PARTYLINEPAGER_IRC_TEST_SERVER`. Without that variable these tests
//! print a skip line and pass, so a bare `cargo test` on the host stays green.
//!
//! Every test picks its own nicknames through [`unique`]. The tests share one
//! server and cargo runs them in parallel, so names have to be unique within a
//! run. They also have to be unique *between* runs: a registered account
//! outlives the test that made it, and the ircd container is not torn down
//! between `docker compose run` invocations.

use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use partylinepagerd::adapters::irc::Irc;
use partylinepagerd::config;
use partylinepagerd::transport::{Incoming, OutMessage, Transport};
use futures_util::StreamExt;
use irc::client::prelude::{Client, Command, Config, Message, Response};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;

const SERVER_ENV: &str = "PARTYLINEPAGER_IRC_TEST_SERVER";

/// Long enough that a loaded CI box does not flake, short enough that a real
/// hang still fails the run rather than hitting cargo's own timeout.
const WAIT: Duration = Duration::from_secs(20);

/// One round of "has it happened yet" polling.
const TICK: Duration = Duration::from_secs(1);

/// The ircd under test, or `None` when these tests should skip themselves.
fn test_server() -> Option<(String, u16)> {
    let raw = std::env::var(SERVER_ENV).ok()?;
    let (host, port) = raw.rsplit_once(':')?;
    Some((host.to_string(), port.parse().ok()?))
}

/// Skips the test, with a reason, when no ircd was configured.
macro_rules! ircd {
    () => {
        match test_server() {
            Some(server) => {
                wait_for_listener(&server);
                server
            }
            None => {
                eprintln!("skipped: {SERVER_ENV} is unset");
                return;
            }
        }
    };
}

/// Blocks until the ircd accepts connections.
///
/// Compose starts the server and the test runner together and has no way to
/// know when the first one is ready, so the first test out of the gate can
/// arrive before the socket is listening.
fn wait_for_listener(server: &(String, u16)) {
    use std::net::{TcpStream, ToSocketAddrs};

    let deadline = Instant::now() + WAIT;
    loop {
        let connected = (server.0.as_str(), server.1)
            .to_socket_addrs()
            .ok()
            .and_then(|mut addrs| addrs.next())
            .is_some_and(|addr| TcpStream::connect_timeout(&addr, TICK).is_ok());
        if connected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no ircd listening on {}:{}",
            server.0,
            server.1
        );
        std::thread::sleep(TICK);
    }
}

/// `name` with a per-run suffix, short enough to stay inside the 32-character
/// nickname limit.
fn unique(name: &str) -> String {
    static RUN: OnceLock<String> = OnceLock::new();
    let run = RUN.get_or_init(|| {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        format!("{:x}{:x}", std::process::id(), nanos)
    });
    format!("{name}-{run}")
}

fn adapter_config(server: &(String, u16), nick: &str, password: Option<&str>) -> config::Irc {
    config::Irc {
        enabled: true,
        server: server.0.clone(),
        port: server.1,
        tls: false,
        nick: nick.to_string(),
        account: None,
        password: password.map(str::to_string),
        channels: vec![],
    }
}

/// Starts an adapter in the background, the way `transport::supervise` would.
fn start(cfg: config::Irc) -> (Arc<Irc>, mpsc::Receiver<Incoming>, JoinHandle<anyhow::Result<()>>) {
    let irc = Arc::new(Irc::new(&cfg).expect("adapter config rejected"));
    let (tx, rx) = mpsc::channel(8);
    let running = irc.clone();
    let handle = tokio::spawn(async move { running.run(tx).await });
    (irc, rx, handle)
}

/// An ordinary IRC client standing in for a subscriber.
///
/// The inbound stream is pumped by a background task on purpose. The `irc`
/// crate flushes the outbound half only while the stream is being polled, so a
/// peer that just sends and then waits on something else never actually sends.
struct Peer {
    client: Client,
    inbox: mpsc::UnboundedReceiver<Message>,
}

impl Peer {
    async fn connect(server: &(String, u16), nick: &str) -> Self {
        let mut client = Client::from_config(Config {
            nickname: Some(nick.to_string()),
            server: Some(server.0.clone()),
            port: Some(server.1),
            use_tls: Some(false),
            ..Config::default()
        })
        .await
        .unwrap_or_else(|e| panic!("peer {nick} could not connect: {e}"));

        let mut stream = client.stream().expect("peer stream");
        let (tx, inbox) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(Ok(message)) = stream.next().await {
                if tx.send(message).is_err() {
                    break;
                }
            }
        });

        client.identify().expect("peer identify");
        let mut peer = Peer { client, inbox };
        peer.expect(WAIT, |m| {
            matches!(&m.command, Command::Response(Response::RPL_WELCOME, _)).then_some(())
        })
        .await
        .unwrap_or_else(|| panic!("peer {nick} was never welcomed"));
        peer
    }

    /// Waits for the first message the predicate accepts.
    async fn expect<T>(
        &mut self,
        within: Duration,
        mut want: impl FnMut(&Message) -> Option<T>,
    ) -> Option<T> {
        let inbox = &mut self.inbox;
        timeout(within, async {
            while let Some(message) = inbox.recv().await {
                if let Some(found) = want(&message) {
                    return Some(found);
                }
            }
            None
        })
        .await
        .ok()
        .flatten()
    }

    fn say(&self, target: &str, text: &str) {
        self.client.send_privmsg(target, text).expect("peer privmsg");
    }

    fn join(&self, channel: &str) {
        self.client.send_join(channel).expect("peer join");
    }

    fn whois(&self, nick: &str) {
        self.client
            .send(Command::WHOIS(None, nick.to_string()))
            .expect("peer whois");
    }

    /// Creates a services account named after this peer's own nickname.
    async fn register_account(&mut self, password: &str) {
        self.say("NickServ", &format!("REGISTER {password}"));
        self.expect(WAIT, |m| match &m.command {
            Command::NOTICE(_, text) if text.contains("logged in") => Some(()),
            _ => None,
        })
        .await
        .expect("NickServ never confirmed the registration");
    }

    fn quit(&self) {
        let _ = self.client.send_quit("done");
    }

    /// Blocks until the named nickname is visible on the network.
    async fn wait_for_nick(&mut self, nick: &str) {
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            self.whois(nick);
            let seen = self
                .expect(TICK, |m| match &m.command {
                    Command::Response(Response::RPL_WHOISUSER, args) => args
                        .get(1)
                        .is_some_and(|found| found.eq_ignore_ascii_case(nick))
                        .then_some(()),
                    _ => None,
                })
                .await;
            if seen.is_some() {
                return;
            }
        }
        panic!("{nick} never appeared on the server");
    }

    /// The services account the named nickname is logged in as, if any.
    ///
    /// `RPL_WHOISACCOUNT` is numeric 330, which this IRC library does not name,
    /// so it arrives raw.
    async fn account_of(&mut self, nick: &str) -> Option<String> {
        self.whois(nick);
        self.expect(WAIT, |m| match &m.command {
            Command::Raw(code, args) if code == "330" => args.get(2).cloned(),
            Command::Response(Response::RPL_ENDOFWHOIS, _) => Some(String::new()),
            _ => None,
        })
        .await
        .filter(|account| !account.is_empty())
    }

    /// Every PRIVMSG addressed to this peer that arrives within `within`,
    /// stopping early only when the connection drops.
    async fn collect_privmsgs(&mut self, within: Duration) -> Vec<String> {
        let mut lines = Vec::new();
        let inbox = &mut self.inbox;
        let _ = timeout(within, async {
            while let Some(message) = inbox.recv().await {
                if let Command::PRIVMSG(_, text) = &message.command {
                    lines.push(text.clone());
                }
            }
        })
        .await;
        lines
    }
}

#[tokio::test]
async fn a_direct_message_becomes_an_incoming_command() {
    let server = ircd!();
    let (bot, sub) = (unique("bat-dm"), unique("sub-dm"));
    let (_irc, mut rx, _handle) = start(adapter_config(&server, &bot, None));
    let mut peer = Peer::connect(&server, &sub).await;

    peer.wait_for_nick(&bot).await;
    peer.say(&bot, "help");

    let incoming = timeout(WAIT, rx.recv())
        .await
        .expect("the adapter never reported the direct message")
        .expect("the adapter closed its channel");

    assert_eq!(incoming.endpoint.to_string(), format!("irc:{sub}"));
    assert_eq!(incoming.text, "help");
    peer.quit();
}

#[tokio::test]
async fn channel_traffic_is_never_treated_as_a_command() {
    let server = ircd!();
    let (bot, sub) = (unique("bat-chan"), unique("sub-chan"));
    let channel = format!("#{}", unique("lobby"));
    let mut cfg = adapter_config(&server, &bot, None);
    cfg.channels = vec![channel.clone()];
    let (_irc, mut rx, _handle) = start(cfg);
    let mut peer = Peer::connect(&server, &sub).await;

    peer.wait_for_nick(&bot).await;
    peer.join(&channel);
    // The bot joins on its own connect, so whether we see it arrive or find it
    // already there is a race we do not care about.
    peer.expect(WAIT, |m| match &m.command {
        Command::JOIN(joined, _, _) if *joined == channel => m
            .source_nickname()
            .is_some_and(|who| who.eq_ignore_ascii_case(&bot))
            .then_some(()),
        Command::Response(Response::RPL_NAMREPLY, args) => args
            .last()
            .is_some_and(|names| {
                names
                    .split_whitespace()
                    .any(|name| name.trim_start_matches(['~', '&', '@', '%', '+']) == bot)
            })
            .then_some(()),
        _ => None,
    })
    .await
    .expect("the bot never joined the channel");

    peer.say(&channel, "signal");
    assert!(
        timeout(Duration::from_secs(3), rx.recv()).await.is_err(),
        "a channel message reached the engine"
    );

    // Proves the silence above was the filter and not a dead connection.
    peer.say(&bot, "status");
    let incoming = timeout(WAIT, rx.recv())
        .await
        .expect("the bot stopped listening entirely")
        .expect("the adapter closed its channel");
    assert_eq!(incoming.text, "status");
    peer.quit();
}

#[tokio::test]
async fn a_broadcast_is_wrapped_into_lines_and_nothing_is_lost() {
    let server = ircd!();
    let (bot, sub) = (unique("bat-out"), unique("sub-out"));
    let (irc, _rx, _handle) = start(adapter_config(&server, &bot, None));
    let mut peer = Peer::connect(&server, &sub).await;
    peer.wait_for_nick(&bot).await;

    // Longer than one IRC line, which is the case that loses half an onion
    // address if the wrapping is wrong.
    let onion = "x".repeat(900);
    let body = format!("PartylinePager is up\n\n{onion}\nsecret: hunter2");
    deliver(&irc, &sub, &OutMessage::plain(body.clone())).await;

    let lines = peer.collect_privmsgs(Duration::from_secs(5)).await;
    assert!(lines.len() > 3, "long line was not wrapped: {lines:?}");
    assert_eq!(
        lines.concat(),
        format!("PartylinePager is up{onion}secret: hunter2"),
        "the delivered text does not reassemble to what was sent"
    );
    peer.quit();
}

#[tokio::test]
async fn sasl_login_claims_a_nickname_that_is_reserved_to_the_account() {
    let server = ircd!();
    let password = "correct-horse-battery";
    let (bot, sub) = (unique("bat-sasl"), unique("sub-sasl"));

    // Own the nickname first, then hand it back.
    let mut owner = Peer::connect(&server, &bot).await;
    owner.register_account(password).await;
    owner.quit();

    let (_irc, _rx, handle) = start(adapter_config(&server, &bot, Some(password)));
    let mut peer = Peer::connect(&server, &sub).await;
    peer.wait_for_nick(&bot).await;

    assert!(!handle.is_finished(), "the adapter died during login");
    assert_eq!(
        peer.account_of(&bot).await.as_deref(),
        Some(bot.as_str()),
        "the bot holds the nickname but is not logged in to the account"
    );
    peer.quit();
}

#[tokio::test]
async fn a_reserved_nickname_without_a_password_fails_loudly() {
    let server = ircd!();

    let bot = unique("bat-nosasl");
    let mut owner = Peer::connect(&server, &bot).await;
    owner.register_account("correct-horse-battery").await;
    owner.quit();

    let (_irc, _rx, handle) = start(adapter_config(&server, &bot, None));

    let outcome = timeout(WAIT, handle)
        .await
        .expect("the adapter hung instead of failing")
        .expect("the adapter panicked");
    let err = outcome.expect_err("the adapter claimed a nickname it does not own");
    // What the supervisor logs before backing off and retrying.
    eprintln!("adapter reported: {err:#}");
}

/// Retries `send` until the adapter has finished connecting.
///
/// `Transport::send` is fallible by design while the socket is down, and the
/// engine treats that as one failed delivery. A test that raced the connection
/// would fail for that reason rather than for the reason it exists.
async fn deliver(irc: &Arc<Irc>, address: &str, msg: &OutMessage) {
    let deadline = Instant::now() + WAIT;
    loop {
        match irc.send(address, msg).await {
            Ok(()) => return,
            Err(e) if Instant::now() >= deadline => panic!("never delivered: {e:#}"),
            Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    }
}
