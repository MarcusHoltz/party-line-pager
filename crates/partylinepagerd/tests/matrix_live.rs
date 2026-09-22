// `tokio::spawn` has to prove the whole sync future is `Send`, and matrix-sdk's
// sync future nests deeply enough that the auto-trait solver hits the default
// limit before it gets to the bottom. The daemon never trips this because it
// awaits sync in place instead of spawning it.
#![recursion_limit = "256"]

//! Live Matrix adapter tests, run against a real Synapse.
//!
//! The unit tests in `adapters/matrix.rs` cover nothing worth covering, because
//! on Matrix there are no pure functions to cover: every interesting decision is
//! made by state the homeserver owns. Whether an invitation is accepted, whether
//! a joined room counts as direct, whether a body survives the trip through
//! end-to-end encryption -- all three live on the far side of a sync loop, and a
//! mock would only ever agree with whatever the code already does.
//!
//! Two of these tests failed the first time they were run, which is the whole
//! argument for having them. The adapter never accepted invitations, so a
//! first-time subscriber's opening message went nowhere; and `Room::is_direct`
//! reads *our own* `m.direct` account data, which the person inviting us cannot
//! write, so even a joined direct room looked ordinary and every command in it
//! was dropped.
//!
//! Requires a Synapse homeserver at the URL in
//! `PARTYLINEPAGER_MATRIX_TEST_HOMESERVER`. Without that variable these tests print a
//! skip line and pass, so a bare `cargo test` on the host stays green.
//!
//! Every test registers its own accounts through [`unique`]. The tests share one
//! homeserver and cargo runs them in parallel, so names have to be unique within
//! a run, and because the container outlives a single `docker compose run`, they
//! have to be unique between runs too.

use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use partylinepagerd::adapters::matrix::Matrix;
use partylinepagerd::config;
use partylinepagerd::transport::{Incoming, OutMessage, Transport};
use matrix_sdk::config::SyncSettings;
use matrix_sdk::ruma::events::room::create::RoomCreateEventContent;
use matrix_sdk::ruma::events::room::member::StrippedRoomMemberEvent;
use matrix_sdk::ruma::events::room::message::{
    MessageType, RoomMessageEventContent, SyncRoomMessageEvent,
};
use matrix_sdk::ruma::{api::client::room::create_room, OwnedUserId, UserId};
use matrix_sdk::{Client, Room, RoomMemberships, RoomState};
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;

const SERVER_ENV: &str = "PARTYLINEPAGER_MATRIX_TEST_HOMESERVER";

/// The homeserver's `server_name`, which is the second half of every user id on
/// it. Fixed by tests/matrix/homeserver.yaml.
const SERVER_NAME: &str = "matrix-test";

/// One password for every throwaway account on a throwaway server.
const PASSWORD: &str = "partylinepager-test-password";

/// Long enough that a loaded box does not flake, short enough that a real hang
/// still fails the run rather than hitting cargo's own timeout.
///
/// More generous than the IRC suite's: a Matrix round trip is a sync cycle, and
/// an encrypted one also waits on a key query and a key share.
const WAIT: Duration = Duration::from_secs(60);

/// One round of "has it happened yet" polling.
const TICK: Duration = Duration::from_millis(250);

/// How long to watch an inbox that is supposed to stay empty.
///
/// Proving a negative costs real time: it has to outlast a sync cycle, or the
/// test passes because nothing had arrived *yet*.
const SILENCE: Duration = Duration::from_secs(10);

/// The homeserver under test, or `None` when these tests should skip themselves.
fn homeserver() -> Option<String> {
    std::env::var(SERVER_ENV).ok()
}

/// Skips the test, with a reason, when no homeserver was configured.
macro_rules! synapse {
    () => {
        match homeserver() {
            Some(url) => {
                wait_for_homeserver(&url).await;
                url
            }
            None => {
                eprintln!("skipped: {SERVER_ENV} is unset");
                return;
            }
        }
    };
}

/// Blocks until the homeserver answers the client API.
///
/// Compose starts Synapse and the test runner together and has no way to know
/// when the first one is ready. Synapse also takes several seconds to run its
/// database migrations on a fresh container, so this is a longer wait than the
/// equivalent for the ircd.
async fn wait_for_homeserver(url: &str) {
    let versions = format!("{}/_matrix/client/versions", url.trim_end_matches('/'));
    let http = reqwest::Client::new();
    let deadline = Instant::now() + WAIT;

    loop {
        if http
            .get(&versions)
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            return;
        }
        assert!(Instant::now() < deadline, "no homeserver answering at {url}");
        tokio::time::sleep(TICK).await;
    }
}

/// `name` with a per-run suffix, lowercased because Matrix localparts are.
fn unique(name: &str) -> String {
    static RUN: OnceLock<String> = OnceLock::new();
    let run = RUN.get_or_init(|| {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        format!("{:x}{:x}", std::process::id(), nanos)
    });
    format!("{name}{run}")
}

/// Creates an account through the client API.
///
/// Registering over HTTP rather than by shelling into the container keeps the
/// test self-contained: `docker compose run` never has to reach sideways into
/// another service to set up its own fixtures. `m.login.dummy` is the only
/// flow the test homeserver offers, since it has no email and no captcha.
async fn register(url: &str, localpart: &str) -> OwnedUserId {
    let response = reqwest::Client::new()
        .post(format!(
            "{}/_matrix/client/v3/register",
            url.trim_end_matches('/')
        ))
        .json(&serde_json::json!({
            "username": localpart,
            "password": PASSWORD,
            "auth": { "type": "m.login.dummy" },
            "inhibit_login": true,
        }))
        .send()
        .await
        .expect("registration request failed");

    assert!(
        response.status().is_success(),
        "could not register {localpart}: {}",
        response.text().await.unwrap_or_default()
    );

    UserId::parse(format!("@{localpart}:{SERVER_NAME}")).expect("built a malformed user id")
}

/// Starts the adapter in the background, the way `transport::supervise` would.
///
/// The returned [`TempDir`] is the E2EE store, and has to outlive the adapter:
/// dropping it early pulls the device identity and the room keys out from under
/// a running client.
async fn start(
    url: &str,
    user: &OwnedUserId,
) -> (
    Arc<Matrix>,
    mpsc::Receiver<Incoming>,
    JoinHandle<anyhow::Result<()>>,
    TempDir,
) {
    let store = TempDir::new().expect("could not make an E2EE store");
    let cfg = config::Matrix {
        enabled: true,
        homeserver: url.to_string(),
        user: user.localpart().to_string(),
        password: PASSWORD.to_string(),
        store_path: store.path().to_path_buf(),
    };

    let matrix = Arc::new(Matrix::new(&cfg).await.expect("adapter could not log in"));
    let (tx, rx) = mpsc::channel(8);
    let running = matrix.clone();
    let handle = tokio::spawn(async move { running.run(tx).await });
    (matrix, rx, handle, store)
}

/// An ordinary Matrix client standing in for a subscriber.
///
/// It accepts its own invitations and marks direct ones, because that is what
/// every real client does and the adapter's outbound path depends on it: a
/// broadcast to somebody who never wrote first arrives as an invitation.
struct Peer {
    client: Client,
    inbox: mpsc::UnboundedReceiver<(String, String)>,
    _store: TempDir,
    _sync: JoinHandle<()>,
}

impl Peer {
    async fn login(url: &str, user_id: &OwnedUserId) -> Self {
        let store = TempDir::new().expect("could not make a peer store");
        let client = Client::builder()
            .homeserver_url(url)
            .sqlite_store(store.path(), None)
            .build()
            .await
            .expect("peer could not reach the homeserver");

        client
            .matrix_auth()
            .login_username(user_id.localpart(), PASSWORD)
            .initial_device_display_name("peer")
            .await
            .expect("peer login failed");

        let me = client.user_id().expect("peer has no user id").to_owned();

        let (tx, inbox) = mpsc::unbounded_channel();
        let sender = me.clone();
        client.add_event_handler(move |event: SyncRoomMessageEvent, room: Room| {
            let tx = tx.clone();
            let sender = sender.clone();
            async move {
                let Some(original) = event.as_original() else {
                    return;
                };
                if original.sender == sender {
                    return;
                }
                if let MessageType::Text(text) = &original.content.msgtype {
                    let _ = tx.send((room.room_id().to_string(), text.body.clone()));
                }
            }
        });

        let joiner = me.clone();
        client.add_event_handler(move |event: StrippedRoomMemberEvent, room: Room| {
            let joiner = joiner.clone();
            async move {
                if event.state_key != joiner || room.state() != RoomState::Invited {
                    return;
                }
                for _ in 0..5 {
                    if room.join().await.is_ok() {
                        break;
                    }
                    tokio::time::sleep(TICK).await;
                }
                if event.content.is_direct == Some(true) {
                    let _ = room.set_is_direct(true).await;
                }
            }
        });

        let syncing = client.clone();
        let sync = tokio::spawn(async move {
            let _ = syncing.sync(SyncSettings::default()).await;
        });

        Peer {
            client,
            inbox,
            _store: store,
            _sync: sync,
        }
    }

    /// Opens an encrypted direct room with `other` and waits for them to join.
    ///
    /// The wait is not politeness. Room keys are shared with the members a room
    /// has at the moment of sending, so a message sent before the far side
    /// joins is one it can never decrypt, and the test would fail for a reason
    /// that has nothing to do with the adapter.
    async fn direct_room_with(&self, other: &UserId) -> Room {
        let room = self
            .client
            .create_dm(other)
            .await
            .expect("could not create a direct room");
        wait_for_join(&room, other).await;
        room
    }

    /// Opens an ordinary, unencrypted, non-direct room and invites `other`.
    async fn room_with(&self, other: &UserId) -> Room {
        let mut request = create_room::v3::Request::new();
        request.invite = vec![other.to_owned()];
        request.is_direct = false;
        request.preset = Some(create_room::v3::RoomPreset::PublicChat);
        request.creation_content = Some(
            matrix_sdk::ruma::serde::Raw::new(&RoomCreateEventContent::new_v11())
                .expect("could not build room creation content")
                .cast_unchecked(),
        );

        let room = self
            .client
            .create_room(request)
            .await
            .expect("could not create a room");
        wait_for_join(&room, other).await;
        room
    }

    async fn say(&self, room: &Room, text: &str) {
        room.send(RoomMessageEventContent::text_plain(text))
            .await
            .expect("peer could not send");
    }

    /// Waits for the first message the predicate accepts.
    async fn expect(
        &mut self,
        within: Duration,
        want: impl FnMut(&str, &str) -> bool,
    ) -> Option<String> {
        self.expect_full(within, want)
            .await
            .map(|(_room, body)| body)
    }

    /// As [`Peer::expect`], but keeps the room the message arrived in.
    async fn expect_full(
        &mut self,
        within: Duration,
        mut want: impl FnMut(&str, &str) -> bool,
    ) -> Option<(String, String)> {
        let inbox = &mut self.inbox;
        timeout(within, async {
            while let Some((room_id, body)) = inbox.recv().await {
                if want(&room_id, &body) {
                    return (room_id, body);
                }
            }
            unreachable!("the peer's sync task outlives every test")
        })
        .await
        .ok()
    }
}

/// Blocks until `user` has actually joined `room`.
async fn wait_for_join(room: &Room, user: &UserId) {
    let deadline = Instant::now() + WAIT;
    loop {
        let joined = room
            .members(RoomMemberships::JOIN)
            .await
            .map(|members| members.iter().any(|m| m.user_id() == user))
            .unwrap_or(false);
        if joined {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{user} never joined {}",
            room.room_id()
        );
        tokio::time::sleep(TICK).await;
    }
}

/// Waits for one [`Incoming`] the adapter considered a command.
async fn next_command(rx: &mut mpsc::Receiver<Incoming>, within: Duration) -> Option<Incoming> {
    timeout(within, rx.recv()).await.ok().flatten()
}

/// The first-contact path, which is the one that was broken.
///
/// A stranger invites the bot to a direct room and says something. Every step
/// of that -- accepting the invitation, recording the room as direct,
/// decrypting the body -- has to work, or nothing reaches the command parser.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_direct_message_becomes_an_incoming_command() {
    let url = synapse!();
    let bot_id = register(&url, &unique("bot")).await;
    let human_id = register(&url, &unique("human")).await;

    let (_bot, mut rx, _running, _store) = start(&url, &bot_id).await;
    let human = Peer::login(&url, &human_id).await;

    let room = human.direct_room_with(&bot_id).await;
    human.say(&room, "signal").await;

    let got = next_command(&mut rx, WAIT)
        .await
        .expect("the bot never saw a direct message");
    assert_eq!(got.text, "signal");
    assert_eq!(
        got.endpoint.to_string(),
        format!("matrix:{human_id}"),
        "the endpoint must be the sender's user id, which survives a display name change"
    );
}

/// The outbound path to somebody who has never written, which is what a
/// broadcast to a freshly approved subscriber is.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_broadcast_reaches_a_subscriber_who_never_wrote_first() {
    let url = synapse!();
    let bot_id = register(&url, &unique("bcbot")).await;
    let human_id = register(&url, &unique("bchuman")).await;

    let (bot, _rx, _running, _store) = start(&url, &bot_id).await;
    let mut human = Peer::login(&url, &human_id).await;

    bot.send(
        human_id.as_str(),
        &OutMessage::titled("Signal up", "onion + secret"),
    )
    .await
    .expect("the bot could not broadcast");

    let body = human
        .expect(WAIT, |_, body| body.contains("onion + secret"))
        .await
        .expect("the subscriber never got the broadcast");
    assert_eq!(
        body, "Signal up\n\nonion + secret",
        "Matrix has no subject line, so the title has to arrive flattened into the body"
    );
}

/// The bot idles in rooms so people can find it, exactly as on IRC. Somebody
/// shouting in one of those rooms must not be able to drive it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn room_traffic_is_never_treated_as_a_command() {
    let url = synapse!();
    let bot_id = register(&url, &unique("roombot")).await;
    let human_id = register(&url, &unique("roomhuman")).await;

    let (_bot, mut rx, _running, _store) = start(&url, &bot_id).await;
    let human = Peer::login(&url, &human_id).await;

    let room = human.room_with(&bot_id).await;
    human.say(&room, "signal").await;

    assert!(
        next_command(&mut rx, SILENCE).await.is_none(),
        "a message in an ordinary room was treated as a command"
    );
}

/// A broadcast is echoed back by the bot's own sync. Treating it as input would
/// let the bot answer itself, and one careless reply template becomes a loop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_bots_own_broadcast_does_not_come_back_as_a_command() {
    let url = synapse!();
    let bot_id = register(&url, &unique("echobot")).await;
    let human_id = register(&url, &unique("echohuman")).await;

    let (bot, mut rx, _running, _store) = start(&url, &bot_id).await;
    let mut human = Peer::login(&url, &human_id).await;

    bot.send(human_id.as_str(), &OutMessage::plain("signal"))
        .await
        .expect("the bot could not broadcast");

    // Wait for the round trip to have definitely happened before asserting the
    // bot did not hear itself, so this cannot pass by being early.
    human
        .expect(WAIT, |_, body| body == "signal")
        .await
        .expect("the subscriber never got the broadcast");

    assert!(
        next_command(&mut rx, SILENCE).await.is_none(),
        "the bot treated its own broadcast as a command"
    );
}

/// Matrix is the transport a room secret is meant to arrive on, and the only
/// reason for that is encryption. If the room the bot answers in were not
/// encrypted, the homeserver operator would read every secret it ever sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_direct_room_is_end_to_end_encrypted_in_both_directions() {
    let url = synapse!();
    let bot_id = register(&url, &unique("e2ebot")).await;
    let human_id = register(&url, &unique("e2ehuman")).await;

    let (bot, mut rx, _running, _store) = start(&url, &bot_id).await;
    let mut human = Peer::login(&url, &human_id).await;

    let room = human.direct_room_with(&bot_id).await;
    assert!(
        room.latest_encryption_state()
            .await
            .expect("could not read the room's encryption state")
            .is_encrypted(),
        "a direct room must be encrypted before a secret is sent through it"
    );

    // Inbound: the bot decrypted it, or `text` would be missing entirely.
    human.say(&room, "signal").await;
    let got = next_command(&mut rx, WAIT)
        .await
        .expect("the bot could not decrypt a message in an encrypted room");
    assert_eq!(got.text, "signal");

    // Outbound: the subscriber decrypted the reply, and it arrived in the room
    // they wrote in rather than a second one. A bot that opens a fresh room per
    // broadcast leaves a subscriber with a screenful of them.
    let want = room.room_id().to_string();
    for reply in ["onion + secret", "still the same room"] {
        bot.send(human_id.as_str(), &OutMessage::plain(reply))
            .await
            .expect("the bot could not reply");

        let (room_id, body) = human
            .expect_full(WAIT, |_, body| body == reply)
            .await
            .expect("the subscriber could not decrypt the bot's reply");
        assert_eq!(body, reply);
        assert_eq!(
            room_id, want,
            "the bot opened a second direct room instead of reusing the first"
        );
    }
}
