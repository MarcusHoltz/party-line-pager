//! Live email adapter tests, run against a real mail server.
//!
//! The unit tests in `adapters/email.rs` cover `interpret`, which turns one
//! RFC822 blob into a command, against blobs written by hand. Everything around
//! it is protocol: logging in over IMAP, `SEARCH UNSEEN`, fetching, setting
//! `\Seen`, and getting a message out through SMTP with the title in the
//! subject line. None of that has a pure function to test, and a mock would
//! agree with whatever the adapter already believes about IMAP.
//!
//! The property most worth having a server for is the one the adapter's own
//! comment claims: *every* unseen message is marked seen, parsed or not, so one
//! malformed mail cannot be re-read forever. That is a claim about a flag on a
//! server, and only a server can settle it.
//!
//! Requires a GreenMail server at the host/port in
//! `PARTY_LINE_PAGER_EMAIL_TEST_HOST`. Without that variable these tests print a skip
//! line and pass, so a bare `cargo test` on the host stays green.

use std::sync::Arc;
use std::time::{Duration, Instant};

use party_line_pagerd::adapters::email::Email;
use party_line_pagerd::config;
use party_line_pagerd::transport::{Incoming, OutMessage, Transport};
use futures_util::TryStreamExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;

const HOST_ENV: &str = "PARTY_LINE_PAGER_EMAIL_TEST_HOST";

/// GreenMail's unencrypted ports, from `-Dgreenmail.setup.test.all`.
const SMTP_PORT: u16 = 3025;
const IMAP_PORT: u16 = 3143;

/// The domain every mailbox on the throwaway server lives in.
const DOMAIN: &str = "example.org";

const BOT_PASSWORD: &str = "botpass";
const HUMAN_PASSWORD: &str = "humanpass";

/// Long enough that a loaded box does not flake, short enough that a real hang
/// still fails the run rather than hitting cargo's own timeout. Email is a
/// polling transport, so every round trip costs at least one [`POLL_INTERVAL`].
const WAIT: Duration = Duration::from_secs(45);

/// One round of "has it happened yet" polling.
const TICK: Duration = Duration::from_millis(250);

/// How long to watch a channel that is supposed to stay empty. It has to
/// outlast several poll intervals, or the test passes because the adapter had
/// not looked yet.
const SILENCE: Duration = Duration::from_secs(8);

/// Much faster than any real mailbox would be polled.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// The mail server under test, or `None` when these tests should skip
/// themselves.
fn host() -> Option<String> {
    std::env::var(HOST_ENV).ok()
}

/// A per-run marker, put in the body of everything these tests send.
///
/// The mailboxes are fixed and the container outlives a single
/// `docker compose run`, so a mailbox may already hold mail from an earlier
/// run. Without a marker, a test waiting for "signal" can be satisfied by last
/// run's.
fn tag() -> &'static str {
    static RUN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    RUN.get_or_init(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        format!("r{:x}{:x}", std::process::id(), nanos)
    })
}

/// Skips the test, with a reason, when no mail server was configured.
macro_rules! greenmail {
    () => {
        match host() {
            Some(host) => {
                wait_for_listener(&host, SMTP_PORT);
                wait_for_listener(&host, IMAP_PORT);
                host
            }
            None => {
                eprintln!("skipped: {HOST_ENV} is unset");
                return;
            }
        }
    };
}

/// Blocks until the server accepts connections on a port.
fn wait_for_listener(host: &str, port: u16) {
    use std::net::{TcpStream as StdTcpStream, ToSocketAddrs};

    let deadline = Instant::now() + WAIT;
    loop {
        let connected = (host, port)
            .to_socket_addrs()
            .ok()
            .and_then(|mut addrs| addrs.next())
            .is_some_and(|addr| StdTcpStream::connect_timeout(&addr, TICK).is_ok());
        if connected {
            return;
        }
        assert!(Instant::now() < deadline, "nothing listening on {host}:{port}");
        std::thread::sleep(TICK);
    }
}

fn adapter_config(host: &str, mailbox: &str) -> config::Email {
    config::Email {
        enabled: true,
        imap_host: host.to_string(),
        imap_port: IMAP_PORT,
        // The login and the address are not the same string here, and that is
        // worth exercising rather than papering over: GreenMail's login is the
        // bare name, and plenty of real providers are the same way round. An
        // adapter that quietly assumed "the login is the address" would work
        // against the ones that do and fail against the ones that do not.
        imap_user: mailbox.to_string(),
        imap_password: BOT_PASSWORD.to_string(),
        mailbox: "INBOX".to_string(),
        smtp_host: host.to_string(),
        smtp_port: SMTP_PORT,
        smtp_user: mailbox.to_string(),
        smtp_password: BOT_PASSWORD.to_string(),
        from: format!("{mailbox}@{DOMAIN}"),
        // The server has no certificate worth verifying, which is the entire
        // reason this knob exists. See adapters/email.rs.
        tls: false,
        poll_interval: POLL_INTERVAL,
    }
}

/// Starts an adapter in the background, the way `transport::supervise` would.
fn start(
    cfg: config::Email,
) -> (
    Arc<Email>,
    mpsc::Receiver<Incoming>,
    JoinHandle<anyhow::Result<()>>,
) {
    let email = Arc::new(Email::new(&cfg).expect("adapter config rejected"));
    let (tx, rx) = mpsc::channel(8);
    let running = email.clone();
    let handle = tokio::spawn(async move { running.run(tx).await });
    (email, rx, handle)
}

/// Waits for one [`Incoming`] that *this* run produced.
///
/// Mailboxes survive the container, and an earlier run that failed part way
/// through leaves unread mail behind. Without the tag filter the first poll
/// hands back somebody else's message and every assertion below fails
/// instantly, which is a confusing way to be told the mailbox is dirty.
async fn next_command(rx: &mut mpsc::Receiver<Incoming>, within: Duration) -> Option<Incoming> {
    let deadline = Instant::now() + within;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let incoming = timeout(left, rx.recv()).await.ok()??;
        if incoming.text.contains(tag()) {
            return Some(incoming);
        }
    }
}

/// Sends one message by talking SMTP directly.
///
/// Deliberately not lettre, which is what the adapter uses. A test that sends
/// with the same library the code under test sends with only proves the library
/// agrees with itself, and two of these tests need to put something on the wire
/// that no sane library would build: a message with no `From` header at all.
async fn deliver(host: &str, from: &str, to: &str, message: &str) {
    let mut socket = TcpStream::connect((host, SMTP_PORT))
        .await
        .expect("could not reach the SMTP port");

    // GreenMail answers every command with a single line, so reading is not
    // needed to keep the conversation in order: it is pipelined and checked at
    // the end by whether the mail arrived.
    let conversation = format!(
        "HELO party-line-pager-tests\r\n\
         MAIL FROM:<{from}>\r\n\
         RCPT TO:<{to}>\r\n\
         DATA\r\n\
         {message}\r\n\
         .\r\n\
         QUIT\r\n"
    );
    socket
        .write_all(conversation.as_bytes())
        .await
        .expect("could not send the message");
    socket.flush().await.expect("could not flush");

    // Give the server a moment to accept it before the connection drops.
    tokio::time::sleep(TICK).await;
}

/// Sends an ordinary, well-formed message from a human.
async fn say(host: &str, from_mailbox: &str, to_mailbox: &str, body: &str) -> String {
    let from = format!("{from_mailbox}@{DOMAIN}");
    let body = format!("{body} {}", tag());
    deliver(
        host,
        &from,
        &format!("{to_mailbox}@{DOMAIN}"),
        &format!(
            "From: {from}\r\n\
             To: {to_mailbox}@{DOMAIN}\r\n\
             Subject: hello\r\n\
             \r\n\
             {body}"
        ),
    )
    .await;
    body
}

/// One message in a mailbox.
struct Mail {
    subject: String,
    body: String,
}

/// Reads a mailbox over IMAP, without marking anything seen.
///
/// `BODY.PEEK[]` rather than `RFC822`: fetching normally sets `\Seen`, and a
/// test that changes the flags it is about to assert on is a test that passes
/// once.
async fn inbox(host: &str, mailbox: &str, password: &str) -> Vec<Mail> {
    let tcp = TcpStream::connect((host, IMAP_PORT))
        .await
        .expect("could not reach the IMAP port");

    let mut client = async_imap::Client::new(tcp);
    client.read_response().await.expect("no IMAP greeting");

    let mut session = client
        .login(mailbox, password)
        .await
        .map_err(|(e, _)| e)
        .expect("IMAP login failed");

    session.select("INBOX").await.expect("could not select INBOX");
    let all = session.search("ALL").await.expect("search failed");

    let mut found = Vec::new();
    if !all.is_empty() {
        let set = all
            .iter()
            .map(|seq| seq.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let fetches: Vec<_> = session
            .fetch(&set, "BODY.PEEK[]")
            .await
            .expect("fetch failed")
            .try_collect()
            .await
            .expect("fetch stream failed");

        for fetch in fetches {
            let Some(raw) = fetch.body() else { continue };
            let Ok(parsed) = mailparse::parse_mail(raw) else {
                continue;
            };
            use mailparse::MailHeaderMap;
            found.push(Mail {
                subject: parsed.headers.get_first_value("Subject").unwrap_or_default(),
                body: parsed.get_body().unwrap_or_default(),
            });
        }
    }

    session.logout().await.ok();
    found
}

/// Waits for a message in `mailbox` that the predicate accepts.
async fn expect_mail(
    host: &str,
    mailbox: &str,
    within: Duration,
    want: impl Fn(&Mail) -> bool,
) -> Option<Mail> {
    let deadline = Instant::now() + within;
    loop {
        for mail in inbox(host, mailbox, HUMAN_PASSWORD).await {
            if want(&mail) {
                return Some(mail);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(TICK).await;
    }
}

/// The inbound path, end to end: SMTP in, IMAP out, one command.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mail_becomes_an_incoming_command() {
    let host = greenmail!();
    let (_adapter, mut rx, _running) = start(adapter_config(&host, "botread"));

    let said = say(&host, "humanread", "botread", "signal poker night").await;

    let got = next_command(&mut rx, WAIT)
        .await
        .expect("the bot never read the mail");
    assert_eq!(
        got.endpoint.to_string(),
        format!("email:humanread@{DOMAIN}"),
        "the endpoint is the sender's address, which is what a reply goes back to"
    );
    assert!(
        got.text.contains(&said),
        "the body has to reach the parser intact: {:?}",
        got.text
    );
}

/// The outbound path. Email is the one transport with a subject line, so the
/// title belongs there rather than flattened into the body.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_broadcast_arrives_as_mail_with_the_title_as_the_subject() {
    let host = greenmail!();
    let (adapter, _rx, _running) = start(adapter_config(&host, "botsend"));

    let body = format!("onion + secret {}", tag());
    adapter
        .send(
            &format!("humansend@{DOMAIN}"),
            &OutMessage::titled("Signal up", body.clone()),
        )
        .await
        .expect("the bot could not broadcast");

    let mail = expect_mail(&host, "humansend", WAIT, |m| m.body.contains(tag()))
        .await
        .expect("the subscriber never got the broadcast");

    assert_eq!(
        mail.subject, "Signal up",
        "the title is the subject line, not the first line of the body"
    );
    assert!(
        mail.body.contains(&body),
        "the body has to survive the round trip: {:?}",
        mail.body
    );
}

/// `\Seen`, against a server.
///
/// The adapter reads `UNSEEN` and then sets `\Seen`. If the flag did not stick,
/// every poll would re-read the same mail and one `tor` would open a room
/// every second until the quota stopped it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_message_is_read_once_and_then_marked_seen() {
    let host = greenmail!();
    let (_adapter, mut rx, _running) = start(adapter_config(&host, "botonce"));

    let said = say(&host, "humanonce", "botonce", "signal").await;
    assert!(
        next_command(&mut rx, WAIT)
            .await
            .expect("the bot never read the mail")
            .text
            .contains(&said)
    );

    assert!(
        next_command(&mut rx, SILENCE).await.is_none(),
        "the same message was delivered more than once"
    );
}

/// The malformed-mail guarantee, which is the reason this suite exists.
///
/// A message with no `From` header cannot become a command: there is nobody to
/// attribute it to. The adapter still has to mark it seen, or it sits at the
/// top of `UNSEEN` forever and every poll rediscovers it. That costs a fetch a
/// second for as long as nobody notices, and it is the sort of thing that only
/// shows up against a server, because a mock has no flags.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreadable_message_is_marked_seen_rather_than_read_forever() {
    let host = greenmail!();
    let (_adapter, mut rx, _running) = start(adapter_config(&host, "botjunk"));

    // No From header at all. `interpret` returns None for this, by design.
    deliver(
        &host,
        &format!("humanjunk@{DOMAIN}"),
        &format!("botjunk@{DOMAIN}"),
        &format!(
            "To: botjunk@{DOMAIN}\r\n\
             Subject: no sender\r\n\
             \r\n\
             signal {}",
            tag()
        ),
    )
    .await;

    assert!(
        next_command(&mut rx, SILENCE).await.is_none(),
        "a message with no sender was turned into a command"
    );

    // The real assertion: the adapter did not get stuck on it. A well-formed
    // message sent afterwards still arrives, which it could not if the poll
    // were still chewing on the first one.
    let said = say(&host, "humanjunk", "botjunk", "signal").await;
    assert!(
        next_command(&mut rx, WAIT)
            .await
            .expect("the adapter got stuck on a message it could not read")
            .text
            .contains(&said)
    );
}

/// One human, one subscriber, whatever case they type.
///
/// Mail addresses are case-insensitive in practice and mail clients do not
/// normalise them, so the same person can arrive as `Marcus@Example.ORG` on
/// Monday and `marcus@example.org` on Tuesday. Two rows on the roster means
/// two subscribers, two quotas, and an admin approving the same person twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sender_who_shouts_their_address_is_still_one_subscriber() {
    let host = greenmail!();
    let (_adapter, mut rx, _running) = start(adapter_config(&host, "botcase"));

    let body = format!("signal {}", tag());
    deliver(
        &host,
        &format!("humancase@{DOMAIN}"),
        &format!("botcase@{DOMAIN}"),
        &format!(
            "From: Marcus <HumanCase@Example.ORG>\r\n\
             To: botcase@{DOMAIN}\r\n\
             Subject: shouting\r\n\
             \r\n\
             {body}"
        ),
    )
    .await;

    let got = next_command(&mut rx, WAIT)
        .await
        .expect("the bot never read the mail");
    assert_eq!(
        got.endpoint.to_string(),
        format!("email:humancase@{DOMAIN}"),
        "a shouted address has to fold onto the same subscriber as a quiet one"
    );
}
