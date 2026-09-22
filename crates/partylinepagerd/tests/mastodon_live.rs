//! Live Mastodon adapter tests, run against a real fediverse server.
//!
//! The unit tests in `adapters/mastodon.rs` cover the two pure functions,
//! `extract` and `strip_html`, against JSON written by hand. What they cannot
//! cover is everything that only exists once a server is involved: whether a
//! direct status posted by one account arrives as a `mention` notification on
//! another, what a real instance's HTML actually looks like once it has
//! rewritten a mention into an `h-card`, whether `since_id` really stops the
//! same toot being delivered twice, and whether the priming poll really does
//! swallow the backlog. Hand-written JSON agrees with whatever the code already
//! expects; a server does not.
//!
//! Requires a GoToSocial (or Mastodon-compatible) server at the URL in
//! `PARTYLINEPAGER_MASTODON_TEST_URL`. Without that variable these tests print a skip
//! line and pass, so a bare `cargo test` on the host stays green.

use std::sync::Arc;
use std::time::{Duration, Instant};

use partylinepagerd::adapters::mastodon::Mastodon;
use partylinepagerd::config;
use partylinepagerd::transport::{Incoming, OutMessage, Transport};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;

const URL_ENV: &str = "PARTYLINEPAGER_MASTODON_TEST_URL";

/// Every throwaway account on the throwaway instance uses this. It has to
/// satisfy the server's own password rules, which is the only reason it looks
/// like that.
const PASSWORD: &str = "Testpassword1!";

/// Long enough that a loaded box does not flake, short enough that a real hang
/// still fails the run rather than hitting cargo's own timeout. Mastodon is a
/// polling transport, so every round trip here costs at least one
/// [`POLL_INTERVAL`].
const WAIT: Duration = Duration::from_secs(45);

/// One round of "has it happened yet" polling.
const TICK: Duration = Duration::from_millis(250);

/// How long to watch a channel that is supposed to stay empty. It has to
/// outlast several poll intervals, or the test passes because the adapter had
/// not got round to looking yet.
const SILENCE: Duration = Duration::from_secs(8);

/// Much faster than any real deployment would use. A live test should not spend
/// its time waiting for a timer that exists to be polite to somebody else's
/// server.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// The instance under test, or `None` when these tests should skip themselves.
fn instance() -> Option<String> {
    std::env::var(URL_ENV).ok()
}

/// A per-run marker, appended to everything these tests post.
///
/// The accounts are fixed, and the container outlives a single
/// `docker compose run`, so an instance that has been tested against before
/// still holds every toot from every earlier run. Without a marker a test that
/// waits for "signal from yesterday" is satisfied by yesterday's *actual* run,
/// and passes or fails on which one it happened to see first.
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

/// An HTTP client that the instance will actually answer.
///
/// The `user_agent` is not decoration. GoToSocial replies **418 I'm a teapot**
/// to a request with an empty one, before it looks at anything else, and
/// `reqwest` sends none unless told to. The adapter had exactly this bug, which
/// is how it was found: writing these tests meant hitting the same wall the
/// adapter had been sitting behind.
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("partylinepager-live-tests")
        .timeout(Duration::from_secs(30))
        .build()
        .expect("could not build an HTTP client")
}

/// Skips the test, with a reason, when no instance was configured.
macro_rules! mastodon {
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

/// Blocks until the instance answers, and until its accounts exist.
///
/// Compose starts the server and the test runner together. GoToSocial also runs
/// database migrations on a cold container and only creates the test accounts
/// after it is already answering, so "the port is open" is not the same as
/// "you can log in".
async fn wait_for_instance(url: &str) {
    let http = client();
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut last;

    loop {
        match http.get(format!("{url}/api/v1/instance")).send().await {
            Ok(response) if response.status().is_success() => return,
            Ok(response) => last = format!("HTTP {}", response.status()),
            // Worth keeping rather than collapsing into a bool: when this
            // times out, the reason is the only thing that tells you whether
            // the server is slow, missing, or answering something unexpected.
            Err(e) => last = e.to_string(),
        }
        assert!(
            Instant::now() < deadline,
            "no instance answering at {url}: {last}"
        );
        tokio::time::sleep(TICK).await;
    }
}

/// An account on the instance, with a token that can act as it.
struct Account {
    /// The `acct` handle, which is what the roster stores and what a reply is
    /// addressed to. Bare for a local account, `user@instance` for a remote
    /// one.
    acct: String,
    token: String,
    url: String,
    http: reqwest::Client,
}

impl Account {
    /// Logs in as `username` and returns a token that can read and post.
    ///
    /// This is the whole OAuth dance, by hand, because there is no shortcut:
    /// the server supports `authorization_code` and `client_credentials` and
    /// not the password grant, and a `client_credentials` token belongs to no
    /// account, so it cannot post. Five requests, in this order, and the order
    /// is the part that is easy to get wrong: the first `/oauth/authorize` is
    /// what puts the request's parameters into the session, and signing in
    /// before that has happened just bounces back to the sign-in page.
    async fn login(url: &str, username: &str) -> Self {
        // Not the shared client: this one must not follow redirects, because
        // the authorization code arrives *in* a redirect's Location header and
        // following it throws the code away.
        let http = reqwest::Client::builder()
            .user_agent("partylinepager-live-tests")
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()
            .expect("could not build an HTTP client");

        let app: serde_json::Value = http
            .post(format!("{url}/api/v1/apps"))
            .json(&serde_json::json!({
                "client_name": "partylinepager-live-tests",
                "redirect_uris": "urn:ietf:wg:oauth:2.0:oob",
                "scopes": "read write",
            }))
            .send()
            .await
            .expect("could not register an application")
            .json()
            .await
            .expect("the instance sent unparseable JSON for the application");

        let client_id = app["client_id"].as_str().expect("no client_id").to_string();
        let client_secret = app["client_secret"]
            .as_str()
            .expect("no client_secret")
            .to_string();

        let authorize = format!(
            "{url}/oauth/authorize?client_id={client_id}\
             &redirect_uri=urn:ietf:wg:oauth:2.0:oob&response_type=code&scope=read+write"
        );

        // The session is one cookie, carried by hand. Pulling in reqwest's
        // cookie store for it would add a feature to the daemon's own
        // dependency for the sake of a test.
        let mut session = String::new();

        let first = http.get(&authorize).send().await.expect("authorize failed");
        remember_cookie(&first, &mut session);

        let signed_in = http
            .post(format!("{url}/auth/sign_in"))
            .header("Cookie", &session)
            .form(&[
                ("username", format!("{username}@example.org")),
                ("password", PASSWORD.to_string()),
            ])
            .send()
            .await
            .expect("sign in failed");
        assert_eq!(
            signed_in.status(),
            302,
            "sign in as {username} was rejected"
        );
        remember_cookie(&signed_in, &mut session);

        // Again, with the parameters: the consent page needs them a second
        // time, and answers 400 without them.
        let consent = http
            .get(&authorize)
            .header("Cookie", &session)
            .send()
            .await
            .expect("consent page failed");
        remember_cookie(&consent, &mut session);

        let granted = http
            .post(format!("{url}/oauth/authorize"))
            .header("Cookie", &session)
            .send()
            .await
            .expect("granting failed");
        let location = granted
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let code = location
            .split_once("code=")
            .map(|(_, code)| code.to_string())
            .unwrap_or_else(|| {
                panic!("no authorization code for {username}; the server said {location:?}")
            });

        let token: serde_json::Value = http
            .post(format!("{url}/oauth/token"))
            .json(&serde_json::json!({
                "redirect_uri": "urn:ietf:wg:oauth:2.0:oob",
                "client_id": client_id,
                "client_secret": client_secret,
                "grant_type": "authorization_code",
                "code": code,
            }))
            .send()
            .await
            .expect("token exchange failed")
            .json()
            .await
            .expect("the instance sent unparseable JSON for the token");

        Account {
            acct: username.to_string(),
            token: token["access_token"]
                .as_str()
                .unwrap_or_else(|| panic!("no access_token for {username}: {token}"))
                .to_string(),
            url: url.to_string(),
            http,
        }
    }

    /// Posts a status, and returns nothing: what matters is what the *other*
    /// side sees.
    async fn post(&self, status: &str, visibility: &str) {
        let response = self
            .http
            .post(format!("{}/api/v1/statuses", self.url))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "status": status, "visibility": visibility }))
            .send()
            .await
            .expect("posting a status failed");
        assert!(
            response.status().is_success(),
            "posting a {visibility} status failed: {}",
            response.text().await.unwrap_or_default()
        );
    }

    /// Every status this account can see in its own notifications and home,
    /// as raw JSON. Used to check what the *bot* sent.
    async fn direct_statuses_from(&self, sender: &str) -> Vec<serde_json::Value> {
        let notifications: Vec<serde_json::Value> = self
            .http
            .get(format!("{}/api/v1/notifications", self.url))
            .bearer_auth(&self.token)
            .query(&[("types[]", "mention"), ("limit", "40")])
            .send()
            .await
            .expect("reading notifications failed")
            .json()
            .await
            .expect("the instance sent unparseable notifications");

        notifications
            .into_iter()
            .filter_map(|n| n.get("status").cloned())
            .filter(|s| s["account"]["acct"] == sender)
            .collect()
    }

    /// Waits for a status from `sender` whose content satisfies `want`.
    async fn expect_direct_from(
        &self,
        sender: &str,
        within: Duration,
        want: impl Fn(&str) -> bool,
    ) -> Option<serde_json::Value> {
        let deadline = Instant::now() + within;
        loop {
            for status in self.direct_statuses_from(sender).await {
                if want(status["content"].as_str().unwrap_or_default()) {
                    return Some(status);
                }
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(TICK).await;
        }
    }
}

/// Keeps the newest `Set-Cookie` value, in the `name=value` form a request
/// wants it back in.
fn remember_cookie(response: &reqwest::Response, session: &mut String) {
    if let Some(set) = response
        .headers()
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
    {
        *session = set.to_string();
    }
}

fn adapter_config(url: &str, token: &str) -> config::Mastodon {
    config::Mastodon {
        enabled: true,
        base_url: url.to_string(),
        access_token: token.to_string(),
        poll_interval: POLL_INTERVAL,
    }
}

/// Starts an adapter in the background, the way `transport::supervise` would.
fn start(
    cfg: config::Mastodon,
) -> (
    Arc<Mastodon>,
    mpsc::Receiver<Incoming>,
    JoinHandle<anyhow::Result<()>>,
) {
    let mastodon = Arc::new(Mastodon::new(&cfg).expect("adapter config rejected"));
    let (tx, rx) = mpsc::channel(8);
    let running = mastodon.clone();
    let handle = tokio::spawn(async move { running.run(tx).await });
    (mastodon, rx, handle)
}

async fn next_command(rx: &mut mpsc::Receiver<Incoming>, within: Duration) -> Option<Incoming> {
    timeout(within, rx.recv()).await.ok().flatten()
}

/// The inbound path, against HTML a real instance actually produced.
///
/// The interesting half is `strip_html`. The unit test feeds it markup somebody
/// typed into a test file; this one feeds it what the server made of
/// `@botmention signal poker night`, which is a nested `h-card` span wrapping an
/// anchor wrapping another span.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_direct_mention_becomes_an_incoming_command() {
    let url = mastodon!();
    let bot = Account::login(&url, "botmention").await;
    let human = Account::login(&url, "humanmention").await;

    let (_adapter, mut rx, _running) = start(adapter_config(&url, &bot.token));
    // The adapter primes itself before its first real poll, so anything posted
    // before that is deliberately discarded. Give it that first poll.
    tokio::time::sleep(POLL_INTERVAL * 2).await;

    let said = format!("@botmention signal poker night {}", tag());
    human.post(&said, "direct").await;

    let got = next_command(&mut rx, WAIT)
        .await
        .expect("the bot never saw a direct mention");
    assert_eq!(
        got.endpoint.to_string(),
        format!("mastodon:{}", human.acct),
        "the endpoint must be the acct handle, which is what a reply is addressed to"
    );
    assert_eq!(
        got.text, said,
        "the instance's h-card markup has to come out as plain text the parser can read"
    );
}

/// A public mention must never be actionable, because the reply to it would be
/// a public post naming an onion address and a shared secret.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_public_mention_is_never_treated_as_a_command() {
    let url = mastodon!();
    let bot = Account::login(&url, "botpublic").await;
    let human = Account::login(&url, "humanpublic").await;

    let (_adapter, mut rx, _running) = start(adapter_config(&url, &bot.token));
    tokio::time::sleep(POLL_INTERVAL * 2).await;

    let said = format!("@botpublic signal {}", tag());
    human.post(&said, "public").await;
    assert!(
        next_command(&mut rx, SILENCE).await.is_none(),
        "a public mention was treated as a command"
    );

    // ...and the same words sent directly still work, so the assertion above
    // is not passing because delivery is broken outright.
    human.post(&said, "direct").await;
    assert_eq!(
        next_command(&mut rx, WAIT)
            .await
            .expect("a direct mention stopped working")
            .text,
        said
    );
}

/// The outbound path. A broadcast has to leave as a direct status addressed to
/// the subscriber, for the same reason: it carries the way into the room.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_broadcast_is_sent_as_a_direct_status_to_the_subscriber() {
    let url = mastodon!();
    let bot = Account::login(&url, "botcast").await;
    let human = Account::login(&url, "humancast").await;

    let (adapter, _rx, _running) = start(adapter_config(&url, &bot.token));
    adapter
        .send(
            &human.acct,
            &OutMessage::titled("Signal up", format!("onion + secret {}", tag())),
        )
        .await
        .expect("the bot could not broadcast");

    let status = human
        .expect_direct_from(&bot.acct, WAIT, |content| content.contains(tag()))
        .await
        .expect("the subscriber never got the broadcast");

    assert_eq!(
        status["visibility"], "direct",
        "a broadcast carries the way into the room; it must never be public"
    );
    let content = status["content"].as_str().unwrap_or_default();
    assert!(
        content.contains("Signal up") && content.contains("onion + secret"),
        "the title and body both have to survive the round trip: {content}"
    );
}

/// Priming, against a real backlog.
///
/// A daemon that comes back after an outage must not act on a `signal` from
/// hours ago: it would wake everybody for a party that is long over. The
/// adapter's first poll therefore exists only to move `since_id` past whatever
/// is already there.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn priming_skips_what_arrived_before_the_daemon_started() {
    let url = mastodon!();
    let bot = Account::login(&url, "botprime").await;
    let human = Account::login(&url, "humanprime").await;

    // The backlog: this is the toot from before the daemon existed.
    let backlog = format!("@botprime signal from yesterday {}", tag());
    human.post(&backlog, "direct").await;
    // Make sure the server has it before the adapter looks, or this test
    // proves nothing at all.
    bot.expect_direct_from(&human.acct, WAIT, |c| c.contains(tag()))
        .await
        .expect("the backlog toot never landed on the instance");

    let (_adapter, mut rx, _running) = start(adapter_config(&url, &bot.token));

    assert!(
        next_command(&mut rx, SILENCE).await.is_none(),
        "the backlog was replayed instead of skipped"
    );

    // Anything after startup is live traffic and must arrive.
    let live = format!("@botprime signal now {}", tag());
    human.post(&live, "direct").await;
    assert_eq!(
        next_command(&mut rx, WAIT)
            .await
            .expect("a mention after startup was skipped too")
            .text,
        live
    );
}

/// `since_id`, against a server rather than against a fixture.
///
/// The adapter polls a list endpoint on a timer. Without `since_id` advancing
/// correctly every poll would re-read the same mention, and one `signal` would
/// open a room every second until the quota stopped it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_same_mention_is_never_delivered_twice() {
    let url = mastodon!();
    let bot = Account::login(&url, "botonce").await;
    let human = Account::login(&url, "humanonce").await;

    let (_adapter, mut rx, _running) = start(adapter_config(&url, &bot.token));
    tokio::time::sleep(POLL_INTERVAL * 2).await;

    let said = format!("@botonce signal {}", tag());
    human.post(&said, "direct").await;
    assert_eq!(
        next_command(&mut rx, WAIT)
            .await
            .expect("the bot never saw the mention")
            .text,
        said
    );

    // Several more polls go by. The same mention is still the newest thing in
    // the account's notifications, and must not come round again.
    assert!(
        next_command(&mut rx, SILENCE).await.is_none(),
        "the same mention was delivered more than once"
    );
}
