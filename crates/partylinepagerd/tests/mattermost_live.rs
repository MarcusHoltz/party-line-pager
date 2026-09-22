//! Live Mattermost adapter tests, run against a real server.
//!
//! The unit tests in `adapters/mattermost.rs` cover `extract` and
//! `websocket_url` against JSON written by hand. What they cannot cover is
//! everything that only exists once a server is involved: whether the
//! `authentication_challenge` handshake this crate sends is actually the one
//! a real server wants, whether a direct post from one account really arrives
//! as a `channel_type: "D"` event on the other's socket, and whether the
//! bot's own broadcast really comes back over its own connection and gets
//! filtered rather than re-entering the parser.
//!
//! Requires a Mattermost server at the URL in
//! `PARTYLINEPAGER_MATTERMOST_TEST_URL`. Without that variable these tests print a
//! skip line and pass, so a bare `cargo test` on the host stays green.
//!
//! Every fixture account is created here, over the plain REST API.

use std::sync::Arc;
use std::time::{Duration, Instant};

use partylinepagerd::adapters::mattermost::Mattermost;
use partylinepagerd::config;
use partylinepagerd::transport::{Incoming, OutMessage, Transport};
use tokio::sync::{mpsc, OnceCell};
use tokio::task::JoinHandle;
use tokio::time::timeout;

const URL_ENV: &str = "PARTYLINEPAGER_MATTERMOST_TEST_URL";

/// Every throwaway account on the throwaway server uses this. It has to
/// satisfy the server's own password rules, which is the only reason it
/// looks like that.
const PASSWORD: &str = "Testpassword1!";

/// Long enough that a loaded box does not flake, short enough that a real
/// hang still fails the run rather than hitting cargo's own timeout.
const WAIT: Duration = Duration::from_secs(45);

/// One round of "has it happened yet" polling.
const TICK: Duration = Duration::from_millis(250);

/// How long to watch a channel that is supposed to stay empty. It has to
/// outlast the websocket's own connect time, or the test passes because the
/// adapter had not finished connecting yet.
const SILENCE: Duration = Duration::from_secs(8);

/// Give `run()`'s websocket time to connect and authenticate before anything
/// is posted, so the events under test are not lost to a socket that was not
/// open yet.
const SETTLE: Duration = Duration::from_secs(1);

/// The server under test, or `None` when these tests should skip themselves.
fn instance() -> Option<String> {
    std::env::var(URL_ENV).ok()
}

/// A per-run marker, appended to everything these tests post.
///
/// The accounts are fixed, and the container outlives a single
/// `docker compose run`, so a server that has been tested against before
/// still holds every post from every earlier run. Without a marker a test
/// that waits for "signal from yesterday" is satisfied by yesterday's
/// *actual* run, and passes or fails on which one it happened to see first.
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

/// An HTTP client that speaks JSON with a sane timeout.
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("could not build an HTTP client")
}

/// Skips the test, with a reason, when no server was configured.
macro_rules! mattermost {
    () => {
        match instance() {
            Some(url) => {
                wait_for_instance(&url).await;
                url
            }
            None => {
                eprintln!("skipped: {URL_ENV} is unset");
                return;
            }
        }
    };
}

/// Blocks until the server answers `/api/v4/system/ping`.
async fn wait_for_instance(url: &str) {
    let http = client();
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut last;

    loop {
        match http.get(format!("{url}/api/v4/system/ping")).send().await {
            Ok(response) if response.status().is_success() => return,
            Ok(response) => last = format!("HTTP {}", response.status()),
            Err(e) => last = e.to_string(),
        }
        assert!(
            Instant::now() < deadline,
            "no server answering at {url}: {last}"
        );
        tokio::time::sleep(TICK).await;
    }
}

/// An account on the server, with a personal access token that can act as it.
struct Account {
    id: String,
    token: String,
    url: String,
    http: reqwest::Client,
}

/// Logs in as `username` and returns its session token, plus its own user id
/// from the login response body.
async fn login(url: &str, http: &reqwest::Client, username: &str) -> Account {
    let response = http
        .post(format!("{url}/api/v4/users/login"))
        .json(&serde_json::json!({"login_id": username, "password": PASSWORD}))
        .send()
        .await
        .expect("login request failed");
    assert!(
        response.status().is_success(),
        "login as {username} failed: HTTP {}",
        response.status()
    );
    // The session token comes back in a header, not the body. The body is
    // the logged-in User object.
    let token = response
        .headers()
        .get("token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_else(|| panic!("no Token header in the login response for {username}"))
        .to_string();
    let user: serde_json::Value = response
        .json()
        .await
        .expect("the server sent unparseable JSON for the logged-in user");

    Account {
        id: user["id"].as_str().expect("no id").to_string(),
        token,
        url: url.to_string(),
        http: http.clone(),
    }
}

/// Just enough of the bootstrap admin to rebuild an [`Account`] later:
/// deliberately no [`reqwest::Client`], which is tied to the tokio runtime
/// that built it. `#[tokio::test(flavor = "multi_thread")]` gives every test
/// its own runtime, so a client cached in a `static` alongside one test's
/// runtime stops working the moment that runtime shuts down and the *next*
/// test tries to reuse it.
struct AdminCreds {
    id: String,
    token: String,
}

/// The one bootstrap admin, created and logged in exactly once per process no
/// matter how many tests run concurrently.
///
/// The first account ever created on an empty server becomes System Admin
/// automatically; there is no other way in with `EnableUserAccessTokens` and
/// no other session to create fixture accounts with. Every later account is
/// created and minted a token *by* this one, rather than by repeating the
/// same race for each of them.
async fn admin(url: &str) -> Account {
    static CREDS: OnceCell<AdminCreds> = OnceCell::const_new();
    let creds = CREDS
        .get_or_init(|| async {
            let http = client();
            // Ignored on purpose: on a fresh container this creates the
            // account and becomes System Admin; on a reused one it fails
            // because the account already exists. Either way, the login
            // right after is what actually has to succeed.
            let _ = http
                .post(format!("{url}/api/v4/users"))
                .json(&serde_json::json!({
                    "email": "partylinepager-admin@example.org",
                    "username": "partylinepager-admin",
                    "password": PASSWORD,
                }))
                .send()
                .await;
            let account = login(url, &http, "partylinepager-admin").await;
            AdminCreds {
                id: account.id,
                token: account.token,
            }
        })
        .await;

    Account {
        id: creds.id.clone(),
        token: creds.token.clone(),
        url: url.to_string(),
        http: client(),
    }
}

/// Creates (or reuses, on a container from an earlier run) a fixture account
/// and mints it a fresh personal access token.
async fn register(admin: &Account, username: &str) -> Account {
    let create = admin
        .http
        .post(format!("{}/api/v4/users", admin.url))
        .bearer_auth(&admin.token)
        .json(&serde_json::json!({
            "email": format!("{username}@example.org"),
            "username": username,
            "password": PASSWORD,
        }))
        .send()
        .await
        .expect("could not reach the server to create a fixture user");

    let user: serde_json::Value = if create.status().is_success() {
        create.json().await.expect("unparseable user JSON")
    } else {
        admin
            .http
            .get(format!("{}/api/v4/users/username/{username}", admin.url))
            .bearer_auth(&admin.token)
            .send()
            .await
            .expect("could not look up the existing fixture user")
            .error_for_status()
            .unwrap_or_else(|e| panic!("creating {username} failed and it does not already exist either: {e}"))
            .json()
            .await
            .expect("unparseable user JSON")
    };
    let id = user["id"].as_str().expect("no id").to_string();

    let token: serde_json::Value = admin
        .http
        .post(format!("{}/api/v4/users/{id}/tokens", admin.url))
        .bearer_auth(&admin.token)
        .json(&serde_json::json!({"description": "partylinepager-live-tests"}))
        .send()
        .await
        .expect("could not mint a personal access token")
        .error_for_status()
        .unwrap_or_else(|e| panic!("minting a token for {username} failed: {e}"))
        .json()
        .await
        .expect("unparseable token JSON");

    Account {
        id,
        token: token["token"].as_str().expect("no token field").to_string(),
        url: admin.url.clone(),
        http: admin.http.clone(),
    }
}

/// The id of an open team, created if it does not already exist.
async fn team_id(admin: &Account, name: &str) -> String {
    let existing = admin
        .http
        .get(format!("{}/api/v4/teams/name/{name}", admin.url))
        .bearer_auth(&admin.token)
        .send()
        .await
        .expect("could not look up the test team");
    let team: serde_json::Value = if existing.status().is_success() {
        existing.json().await.expect("unparseable team JSON")
    } else {
        admin
            .http
            .post(format!("{}/api/v4/teams", admin.url))
            .bearer_auth(&admin.token)
            .json(&serde_json::json!({"name": name, "display_name": name, "type": "O"}))
            .send()
            .await
            .expect("could not create the test team")
            .error_for_status()
            .unwrap_or_else(|e| panic!("creating team {name} failed: {e}"))
            .json()
            .await
            .expect("unparseable team JSON")
    };
    team["id"].as_str().expect("no id").to_string()
}

/// Adds `account` to the team, which auto-joins it to Town Square too.
///
/// Done as `admin` rather than a self-join: a plain member does not have
/// `join_public_teams` by default on this server, and granting it is a
/// System Console change with no REST equivalent worth scripting for a
/// throwaway fixture. A System Admin can add anyone to any team regardless.
async fn join_team(admin: &Account, account: &Account, team_id: &str) {
    let response = admin
        .http
        .post(format!("{}/api/v4/teams/{team_id}/members", admin.url))
        .bearer_auth(&admin.token)
        .json(&serde_json::json!({"team_id": team_id, "user_id": account.id}))
        .send()
        .await
        .expect("could not join the test team");
    // Idempotent from this test's point of view: already being a member (a
    // reused container from an earlier run) is not a failure.
    assert!(
        response.status().is_success() || response.status() == reqwest::StatusCode::BAD_REQUEST,
        "joining the test team failed: HTTP {}",
        response.status()
    );
}

async fn town_square(account: &Account, team_id: &str) -> String {
    let channel: serde_json::Value = account
        .http
        .get(format!(
            "{}/api/v4/teams/{team_id}/channels/name/town-square",
            account.url
        ))
        .bearer_auth(&account.token)
        .send()
        .await
        .expect("could not fetch town-square")
        .error_for_status()
        .unwrap_or_else(|e| panic!("fetching town-square failed: {e}"))
        .json()
        .await
        .expect("unparseable channel JSON");
    channel["id"].as_str().expect("no id").to_string()
}

/// The id of the direct channel between two accounts, opening it if it does
/// not exist yet. Deterministic: opening an already-open direct channel just
/// returns the same id.
async fn direct_channel_id(a: &Account, b: &Account) -> String {
    let channel: serde_json::Value = a
        .http
        .post(format!("{}/api/v4/channels/direct", a.url))
        .bearer_auth(&a.token)
        .json(&[a.id.as_str(), b.id.as_str()])
        .send()
        .await
        .expect("could not open the direct channel")
        .error_for_status()
        .unwrap_or_else(|e| panic!("opening the direct channel failed: {e}"))
        .json()
        .await
        .expect("unparseable channel JSON");
    channel["id"].as_str().expect("no id").to_string()
}

async fn post_into(account: &Account, channel_id: &str, message: &str) {
    account
        .http
        .post(format!("{}/api/v4/posts", account.url))
        .bearer_auth(&account.token)
        .json(&serde_json::json!({"channel_id": channel_id, "message": message}))
        .send()
        .await
        .expect("could not post")
        .error_for_status()
        .unwrap_or_else(|e| panic!("posting failed: {e}"));
}

/// Waits for a post containing `needle` to appear in a channel.
async fn expect_post_containing(
    account: &Account,
    channel_id: &str,
    within: Duration,
    needle: &str,
) -> Option<String> {
    let deadline = Instant::now() + within;
    loop {
        let posts: serde_json::Value = account
            .http
            .get(format!("{}/api/v4/channels/{channel_id}/posts", account.url))
            .bearer_auth(&account.token)
            .send()
            .await
            .expect("could not list posts")
            .json()
            .await
            .expect("unparseable posts JSON");
        if let Some(order) = posts["order"].as_array() {
            for id in order {
                let id = id.as_str().unwrap_or_default();
                if let Some(message) = posts["posts"][id]["message"].as_str() {
                    if message.contains(needle) {
                        return Some(message.to_string());
                    }
                }
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(TICK).await;
    }
}

fn adapter_config(url: &str, token: &str) -> config::Mattermost {
    config::Mattermost {
        enabled: true,
        base_url: url.to_string(),
        access_token: token.to_string(),
    }
}

/// Starts an adapter in the background, the way `transport::supervise` would.
async fn start(
    cfg: config::Mattermost,
) -> (
    Arc<Mattermost>,
    mpsc::Receiver<Incoming>,
    JoinHandle<anyhow::Result<()>>,
) {
    let mattermost = Arc::new(Mattermost::new(&cfg).await.expect("adapter config rejected"));
    let (tx, rx) = mpsc::channel(8);
    let running = mattermost.clone();
    let handle = tokio::spawn(async move { running.run(tx).await });
    (mattermost, rx, handle)
}

async fn next_command(rx: &mut mpsc::Receiver<Incoming>, within: Duration) -> Option<Incoming> {
    timeout(within, rx.recv()).await.ok().flatten()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_direct_message_becomes_an_incoming_command() {
    let url = mattermost!();
    let admin = admin(&url).await;
    let bot = register(&admin, "botmsg").await;
    let peer = register(&admin, "peermsg").await;

    let (_adapter, mut rx, _running) = start(adapter_config(&url, &bot.token)).await;
    tokio::time::sleep(SETTLE).await;

    let channel_id = direct_channel_id(&peer, &bot).await;
    let said = format!("signal poker night {}", tag());
    post_into(&peer, &channel_id, &said).await;

    let got = next_command(&mut rx, WAIT)
        .await
        .expect("the bot never saw a direct message");
    assert_eq!(
        got.endpoint.to_string(),
        format!("mattermost:{}", peer.id),
        "the endpoint must be the sender's user id, which is what a reply is addressed to"
    );
    assert_eq!(got.text, said);
}

/// Traffic in a channel both accounts belong to must never be actionable: the
/// reply to it would be a public post naming an onion address and a shared
/// secret.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn channel_traffic_is_never_treated_as_a_command() {
    let url = mattermost!();
    let admin = admin(&url).await;
    let bot = register(&admin, "botpublic").await;
    let peer = register(&admin, "peerpublic").await;

    let team = team_id(&admin, "partylinepager-test").await;
    join_team(&admin, &bot, &team).await;
    join_team(&admin, &peer, &team).await;
    let channel_id = town_square(&bot, &team).await;

    let (_adapter, mut rx, _running) = start(adapter_config(&url, &bot.token)).await;
    tokio::time::sleep(SETTLE).await;

    let said = format!("signal {}", tag());
    post_into(&peer, &channel_id, &said).await;
    assert!(
        next_command(&mut rx, SILENCE).await.is_none(),
        "public channel traffic was treated as a command"
    );

    // ...and the same words sent directly still work, so the assertion above
    // is not passing because delivery is broken outright.
    let dm = direct_channel_id(&peer, &bot).await;
    post_into(&peer, &dm, &said).await;
    assert_eq!(
        next_command(&mut rx, WAIT)
            .await
            .expect("a direct message stopped working")
            .text,
        said
    );
}

/// The outbound path. A broadcast has to arrive as a direct post, for the
/// same reason: it carries the way into the room.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_broadcast_is_delivered_as_a_direct_post() {
    let url = mattermost!();
    let admin = admin(&url).await;
    let bot = register(&admin, "botcast").await;
    let peer = register(&admin, "peercast").await;

    let (adapter, _rx, _running) = start(adapter_config(&url, &bot.token)).await;

    let body = format!("onion + secret {}", tag());
    adapter
        .send(&peer.id, &OutMessage::titled("Signal up", body.clone()))
        .await
        .expect("the bot could not broadcast");

    let channel_id = direct_channel_id(&peer, &bot).await;
    let message = expect_post_containing(&peer, &channel_id, WAIT, tag())
        .await
        .expect("the subscriber never got the broadcast");
    assert!(
        message.contains("Signal up") && message.contains("onion + secret"),
        "the title and body both have to survive the round trip: {message}"
    );
}

/// The bot's own broadcast comes back over its own websocket connection too,
/// because it is a member of the channel it just posted into. It must never
/// be re-ingested as a command.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_bots_own_post_is_never_treated_as_a_command() {
    let url = mattermost!();
    let admin = admin(&url).await;
    let bot = register(&admin, "botecho").await;
    let peer = register(&admin, "peerecho").await;

    let (adapter, mut rx, _running) = start(adapter_config(&url, &bot.token)).await;
    tokio::time::sleep(SETTLE).await;

    adapter
        .send(&peer.id, &OutMessage::plain(format!("onion + secret {}", tag())))
        .await
        .expect("the bot could not send its own message");

    assert!(
        next_command(&mut rx, SILENCE).await.is_none(),
        "the bot's own post was treated as an incoming command"
    );
}
