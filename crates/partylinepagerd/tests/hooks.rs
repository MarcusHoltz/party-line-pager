//! Contract tests for the shipped provider hooks.
//!
//! The hooks are the privileged half of the partyline pager and the one part
//! written in shell, so their contract is pinned here: what they read from
//! stdin, what they print on stdout, what they do to the key that names the
//! room, and how they fail.
//!
//! Docker is replaced by a stub on `PATH`, so these run anywhere with a shell.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use partylinepager_core::Transport;
use partylinepagerd::provider::{PartylineOutput, WebOutput};

const FAKE_ONION: &str = "abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrstuvwx.onion";
const FAKE_B32: &str = "abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrst.b32.i2p";
const FAKE_HASH: &str = "3a1c9d4e07b21f88c2a04e7d612b0f4e";
const WEB_BASE: &str = "https://meet.example.org";

/// Where each party line keeps the state the hook reads back, as seen from
/// *inside* its container. The stub rewrites these to somewhere in the temp
/// dir, which is what the real bind mounts do for real.
const TOR_STATE: &str = "/var/lib/tor";
const I2P_STATE: &str = "/data/.partyline";
const RETICULUM_STATE: &str = "/app/data";

struct Harness {
    dir: tempfile::TempDir,
    bin: PathBuf,
    /// Stands in for the party-line checkout. One directory serves all three
    /// networks: each hook is handed it under its own variable, which is also
    /// the point of them having separate variables.
    checkout: PathBuf,
    /// Stands in for the container-side state directories.
    state: PathBuf,
}

impl Harness {
    /// `docker_behaviour` is shell run inside the stub when `compose up` is
    /// called, letting each test decide whether the relay comes up.
    fn new(docker_behaviour: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        let checkout = dir.path().join("party");
        let state = dir.path().join("state");
        for d in [&bin, &checkout, &state] {
            fs::create_dir_all(d).unwrap();
        }

        // One stub for all four hooks. A compose subcommand can arrive behind
        // leading -f and --profile flags, so the verb is found by scanning the
        // arguments rather than by position.
        write_exec(
            &bin.join("docker"),
            &format!(
                r#"#!/bin/sh
echo "$@" >> "{log}"
verb=""
for a in "$@"; do
  case "$a" in
    up|down|run|exec|logs) verb="$a"; break ;;
  esac
done
last=""
for a in "$@"; do last="$a"; done
case "$verb" in
  up) {behaviour} ;;
  # The relay's own output, which is how the i2p hook learns it is ready.
  logs) cat "$RELAY_LOG" 2>/dev/null ;;
  exec|run)
    # Emulate the bind mounts: a path inside the container maps to one under
    # $STATE_ROOT on the host. Take the trailing `-c '...'` snippet and run it
    # against the translated path.
    real=$(printf '%s' "$last" \
      | sed -e "s#{tor_state}#$STATE_ROOT/tor#g" \
            -e "s#{i2p_state}#$STATE_ROOT/i2p#g" \
            -e "s#{reticulum_state}#$STATE_ROOT/reticulum#g")
    sh -c "$real"
    exit $?
    ;;
esac
exit 0
"#,
                log = dir.path().join("docker.log").display(),
                behaviour = docker_behaviour,
                tor_state = TOR_STATE,
                i2p_state = I2P_STATE,
                reticulum_state = RETICULUM_STATE,
            ),
        );

        Harness {
            dir,
            bin,
            checkout,
            state,
        }
    }

    /// Tor's key directory, as the stub maps it.
    fn hidden_service_dir(&self) -> PathBuf {
        self.state.join("tor/hidden_service")
    }

    /// I2P's persistent destination key, as the stub maps it.
    fn i2p_key(&self) -> PathBuf {
        self.state.join("i2p/partyline-keys.dat")
    }

    /// Reticulum's persistent identity, as the stub maps it.
    fn reticulum_identity(&self) -> PathBuf {
        self.state.join("reticulum/identity")
    }

    /// Where the Tor hook writes the shared secret. The other two hooks must
    /// never create this.
    fn secret_file(&self) -> PathBuf {
        self.checkout.join("secrets/shared_secret.txt")
    }

    fn docker_log(&self) -> String {
        fs::read_to_string(self.dir.path().join("docker.log")).unwrap_or_default()
    }

    /// True if any compose call ended in the `down` verb.
    ///
    /// Checked as the trailing word rather than as a substring, because the
    /// calls carry different leading flags per network (`-f` for the override,
    /// `--profile` for Reticulum) and a substring match on "compose down"
    /// silently stops matching the moment one is added.
    fn stopped_the_relay(&self) -> bool {
        self.docker_log()
            .lines()
            .any(|line| line.split_whitespace().last() == Some("down"))
    }

    fn run(&self, hook: &str, secret: Option<&str>) -> (bool, String, String) {
        self.run_inner(hook, secret, Some(WEB_BASE), Some("join"), None)
    }

    /// `None` leaves `PARTYLINEPAGER_WEB_BASE_URL` unset, which is what an
    /// unconfigured instance looks like to the hook.
    fn run_with_base_url(&self, hook: &str, base_url: Option<&str>) -> (bool, String, String) {
        self.run_inner(hook, None, base_url, Some("join"), None)
    }

    /// Exercises `provider-web.sh` with a specific `path` (mirrors
    /// `provider.web.path`, `None` here behaves like the daemon's own
    /// unset-env case) and `static_slug` (mirrors `provider.web.static_slug`,
    /// `None` means "no static slug", matching an unset
    /// `PARTYLINEPAGER_WEB_STATIC_SLUG`).
    fn run_web(&self, path: Option<&str>, static_slug: Option<&str>) -> (bool, String, String) {
        self.run_inner("provider-web.sh", None, Some(WEB_BASE), path, static_slug)
    }

    fn run_inner(
        &self,
        hook: &str,
        secret: Option<&str>,
        base_url: Option<&str>,
        web_path: Option<&str>,
        static_slug: Option<&str>,
    ) -> (bool, String, String) {
        let mut child = Command::new(repo_root().join("hooks").join(hook))
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()))
            .env("STATE_ROOT", &self.state)
            .env("RELAY_LOG", self.dir.path().join("relay.log"))
            // Every party line is handed the same stand-in checkout, under its
            // own variable. Three names rather than one shared TOR_PARTYLINE_DIR,
            // because all four hooks run inside one partylinepagerd container and a
            // shared name would have the last compose overlay loaded win.
            .env("TOR_PARTYLINE_DIR", &self.checkout)
            .env("I2P_PARTYLINE_DIR", &self.checkout)
            .env("RETICULUM_PARTYLINE_DIR", &self.checkout)
            // Short enough that a failing test does not sit for four minutes.
            .env("BOOTSTRAP_TIMEOUT", "4")
            .env("I2P_BOOTSTRAP_TIMEOUT", "4")
            .env("RETICULUM_BOOTSTRAP_TIMEOUT", "4")
            // The shipped override lives at an absolute path that only exists
            // inside the deployment image. Empty means "shipped compose file
            // unmodified", which is what the stub expects.
            .env("I2P_RELAY_OVERRIDE", "")
            .env("PARTYLINEPAGER_TTL_SECS", "7200")
            .env("PARTYLINEPAGER_ROOM_ID", FAKE_ONION)
            .envs(base_url.map(|u| ("PARTYLINEPAGER_WEB_BASE_URL", u)))
            .envs(web_path.map(|p| ("PARTYLINEPAGER_WEB_PATH", p)))
            .envs(static_slug.map(|s| ("PARTYLINEPAGER_WEB_STATIC_SLUG", s)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("hook is executable");

        {
            use std::io::Write;
            let mut stdin = child.stdin.take().unwrap();
            if let Some(secret) = secret {
                writeln!(stdin, "{secret}").unwrap();
            }
        }

        let out = child.wait_with_output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

fn write_exec(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/partylinepagerd.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Stub behaviour: Tor publishes an address immediately.
fn tor_publishes() -> String {
    format!(
        "mkdir -p \"$STATE_ROOT/tor/hidden_service\" && \
         printf '{FAKE_ONION}\\n' > \"$STATE_ROOT/tor/hidden_service/hostname\""
    )
}

/// Stub behaviour: i2pd writes its address AND, separately, logs that the
/// destination is actually reachable. The two are deliberately distinct: the
/// address file appears long before the LeaseSet is published, so a hook that
/// trusted the file alone would broadcast an address nobody can dial yet.
fn i2p_publishes() -> String {
    format!(
        "mkdir -p \"$STATE_ROOT/i2p\" && \
         printf '{FAKE_B32}\\n' > \"$STATE_ROOT/i2p/address\" && \
         echo 'I2P destination active' >> \"$RELAY_LOG\""
    )
}

/// Stub behaviour: i2pd writes an address but never becomes reachable. What a
/// stalled reseed looks like from outside.
fn i2p_publishes_but_never_becomes_reachable() -> String {
    format!(
        "mkdir -p \"$STATE_ROOT/i2p\" && \
         printf '{FAKE_B32}\\n' > \"$STATE_ROOT/i2p/address\""
    )
}

/// Stub behaviour: the reflector announces its destination immediately.
fn reticulum_publishes() -> String {
    format!(
        "mkdir -p \"$STATE_ROOT/reticulum\" && \
         printf '{FAKE_HASH}\\n' > \"$STATE_ROOT/reticulum/destination\""
    )
}

/// The one JSON object a provider hook is allowed to print.
fn parse_partyline(stdout: &str) -> PartylineOutput {
    let json = stdout
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .expect("a provider hook prints one JSON object");
    serde_json::from_str(json).expect("stdout is one JSON object")
}

// --- the Tor party line -------------------------------------------------

#[test]
fn the_tor_provider_prints_an_address_the_daemon_accepts() {
    let h = Harness::new(&tor_publishes());
    let (ok, stdout, stderr) = h.run("provider-tor.sh", Some("SUPERSECRET"));

    assert!(ok, "hook failed: {stderr}");
    let parsed = parse_partyline(&stdout);
    assert_eq!(parsed.address, FAKE_ONION);
    parsed
        .validate(Transport::Tor)
        .expect("the daemon accepts what the hook printed");
}

#[test]
fn no_port_travels_with_any_address() {
    // partyline.sh accepts only a bare address on all three networks, and a
    // pasted "address:port" breaks its normalization instead of being ignored.
    for (hook, behaviour) in [
        ("provider-tor.sh", tor_publishes()),
        ("provider-i2p.sh", i2p_publishes()),
        ("provider-rns.sh", reticulum_publishes()),
    ] {
        let h = Harness::new(&behaviour);
        let (ok, stdout, stderr) = h.run(hook, Some("s"));
        assert!(ok, "{hook} failed: {stderr}");
        let json = stdout.lines().rev().find(|l| l.starts_with('{')).unwrap();
        assert!(!json.contains("port"), "{hook} printed a port: {json}");
    }
}

#[test]
fn the_secret_is_written_for_the_owner_only() {
    let h = Harness::new(&tor_publishes());
    h.run("provider-tor.sh", Some("SUPERSECRET"));

    let secret = h.secret_file();
    assert_eq!(fs::read_to_string(&secret).unwrap().trim(), "SUPERSECRET");
    let mode = fs::metadata(&secret).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "shared secret must not be group or world readable");
}

#[test]
fn a_fresh_onion_key_is_forced_for_every_signal() {
    let h = Harness::new(&tor_publishes());
    let stale = h.hidden_service_dir();
    fs::create_dir_all(&stale).unwrap();
    fs::write(stale.join("hs_ed25519_secret_key"), b"old key").unwrap();

    h.run("provider-tor.sh", Some("s"));

    assert!(
        !stale.join("hs_ed25519_secret_key").exists(),
        "the previous key must be discarded so two parties never share an address"
    );
}

#[test]
fn a_tor_that_never_publishes_fails_and_cleans_up() {
    // The stub starts "successfully" but never writes a hostname file.
    let h = Harness::new("true");
    let (ok, stdout, stderr) = h.run("provider-tor.sh", Some("s"));

    assert!(!ok, "the hook must fail rather than hang forever");
    assert!(stdout.trim().is_empty(), "no JSON on failure");
    assert!(stderr.contains("did not publish"), "{stderr}");
    assert!(
        h.stopped_the_relay(),
        "a half-started relay must be stopped: {}",
        h.docker_log()
    );
}

#[test]
fn teardown_stops_the_relay_and_shreds_the_secret() {
    let h = Harness::new(&tor_publishes());
    h.run("provider-tor.sh", Some("s"));
    assert!(h.secret_file().exists());

    let (ok, _stdout, stderr) = h.run("teardown-tor.sh", None);

    assert!(ok, "{stderr}");
    assert!(h.stopped_the_relay(), "{}", h.docker_log());
    assert!(!h.secret_file().exists(), "the secret must not outlive the room");
    assert!(
        !h.hidden_service_dir().exists(),
        "the onion key must not outlive the room"
    );
}

// --- the I2P party line -------------------------------------------------

#[test]
fn the_i2p_provider_prints_an_address_the_daemon_accepts() {
    let h = Harness::new(&i2p_publishes());
    let (ok, stdout, stderr) = h.run("provider-i2p.sh", Some("SUPERSECRET"));

    assert!(ok, "hook failed: {stderr}");
    let parsed = parse_partyline(&stdout);
    assert_eq!(parsed.address, FAKE_B32);
    parsed
        .validate(Transport::I2p)
        .expect("the daemon accepts what the hook printed");
}

#[test]
fn the_i2p_provider_waits_for_reachability_not_just_an_address() {
    // The address file appears as soon as the key exists; the destination is
    // not callable until its LeaseSet is published. A hook that returned on
    // the file alone would mail the roster an address that answers "not
    // found".
    let h = Harness::new(&i2p_publishes_but_never_becomes_reachable());
    let (ok, stdout, stderr) = h.run("provider-i2p.sh", Some("s"));

    assert!(
        !ok,
        "an address without reachability must not count as a room: {stdout}"
    );
    assert!(stdout.trim().is_empty(), "no JSON on failure");
    assert!(stderr.contains("did not publish a reachable destination"), "{stderr}");
    assert!(
        h.stopped_the_relay(),
        "a half-started relay must be stopped: {}",
        h.docker_log()
    );
}

#[test]
fn a_fresh_i2p_key_is_forced_for_every_signal() {
    let h = Harness::new(&i2p_publishes());
    let stale = h.i2p_key();
    fs::create_dir_all(stale.parent().unwrap()).unwrap();
    fs::write(&stale, b"old key").unwrap();

    h.run("provider-i2p.sh", Some("s"));

    assert!(
        !stale.exists(),
        "the previous destination key must be discarded so no two parties share an address"
    );
}

#[test]
fn the_i2p_teardown_stops_the_relay_and_shreds_the_key() {
    let h = Harness::new(&i2p_publishes());
    h.run("provider-i2p.sh", Some("s"));
    assert!(h.i2p_key().parent().unwrap().exists());
    fs::write(h.i2p_key(), b"key").unwrap();

    let (ok, _stdout, stderr) = h.run("teardown-i2p.sh", None);

    assert!(ok, "{stderr}");
    assert!(h.stopped_the_relay(), "{}", h.docker_log());
    assert!(!h.i2p_key().exists(), "the key must not outlive the room");
}

// --- the Reticulum party line -------------------------------------------

#[test]
fn the_reticulum_provider_prints_an_address_the_daemon_accepts() {
    let h = Harness::new(&reticulum_publishes());
    let (ok, stdout, stderr) = h.run("provider-rns.sh", Some("SUPERSECRET"));

    assert!(ok, "hook failed: {stderr}");
    let parsed = parse_partyline(&stdout);
    assert_eq!(parsed.address, FAKE_HASH);
    parsed
        .validate(Transport::Reticulum)
        .expect("the daemon accepts what the hook printed");
}

#[test]
fn the_reticulum_provider_names_its_compose_profile() {
    // Every service in that compose file sits behind a profile, so a call that
    // forgets --profile matches nothing and silently starts no reflector.
    let h = Harness::new(&reticulum_publishes());
    h.run("provider-rns.sh", Some("s"));
    assert!(
        h.docker_log().contains("--profile reflector"),
        "without the profile nothing starts: {}",
        h.docker_log()
    );
}

#[test]
fn the_reticulum_provider_creates_the_secret_file_compose_demands() {
    // The compose file declares a file-backed secret, and compose refuses to
    // start at all when the host-side file is missing. Empty is correct: a
    // relay needs no secret, and partyline.sh reads an empty file as "unset".
    let h = Harness::new(&reticulum_publishes());
    h.run("provider-rns.sh", Some("s"));

    let declared = h.checkout.join("secrets/shared_secret.txt");
    assert!(declared.exists(), "compose would refuse to start without it");
    assert!(
        fs::read_to_string(&declared).unwrap().is_empty(),
        "the relay must not be handed the secret"
    );
}

#[test]
fn a_reflector_that_never_announces_fails_and_cleans_up() {
    let h = Harness::new("true");
    let (ok, stdout, stderr) = h.run("provider-rns.sh", Some("s"));

    assert!(!ok, "the hook must fail rather than hang forever");
    assert!(stdout.trim().is_empty(), "no JSON on failure");
    assert!(stderr.contains("did not publish a destination"), "{stderr}");
    assert!(h.stopped_the_relay(), "{}", h.docker_log());
}

#[test]
fn a_fresh_reticulum_identity_is_forced_for_every_signal() {
    let h = Harness::new(&reticulum_publishes());
    let stale = h.reticulum_identity();
    fs::create_dir_all(stale.parent().unwrap()).unwrap();
    fs::write(&stale, b"old identity").unwrap();

    h.run("provider-rns.sh", Some("s"));

    assert!(
        !stale.exists(),
        "the previous identity must be discarded so no two parties share an address"
    );
}

#[test]
fn the_reticulum_teardown_stops_the_reflector_and_shreds_the_identity() {
    let h = Harness::new(&reticulum_publishes());
    h.run("provider-rns.sh", Some("s"));
    fs::write(h.reticulum_identity(), b"identity").unwrap();

    let (ok, _stdout, stderr) = h.run("teardown-rns.sh", None);

    assert!(ok, "{stderr}");
    assert!(h.stopped_the_relay(), "{}", h.docker_log());
    assert!(
        !h.reticulum_identity().exists(),
        "the identity must not outlive the room"
    );
}

// --- shared contract, every party line ----------------------------------

/// Every party-line hook pair, as (provider, teardown, stub behaviour).
fn every_partyline() -> Vec<(&'static str, &'static str, String)> {
    vec![
        ("provider-tor.sh", "teardown-tor.sh", tor_publishes()),
        ("provider-i2p.sh", "teardown-i2p.sh", i2p_publishes()),
        (
            "provider-rns.sh",
            "teardown-rns.sh",
            reticulum_publishes(),
        ),
    ]
}

#[test]
fn no_party_line_runs_without_a_secret() {
    for (provider, _, behaviour) in every_partyline() {
        let h = Harness::new(&behaviour);
        let (ok, _stdout, stderr) = h.run(provider, None);
        assert!(!ok, "{provider} ran without a secret");
        assert!(stderr.contains("no secret"), "{provider}: {stderr}");
        assert!(
            h.docker_log().is_empty(),
            "{provider} started something before checking: {}",
            h.docker_log()
        );
    }
}

#[test]
fn a_missing_checkout_fails_loudly_rather_than_starting_something_else() {
    for (provider, _, behaviour) in every_partyline() {
        let h = Harness::new(&behaviour);
        fs::remove_dir_all(&h.checkout).unwrap();
        let (ok, _stdout, stderr) = h.run(provider, Some("s"));
        assert!(!ok, "{provider} carried on without its checkout");
        assert!(stderr.contains("does not exist"), "{provider}: {stderr}");
    }
}

#[test]
fn teardown_is_safe_when_there_is_nothing_to_stop() {
    for (_, teardown, behaviour) in every_partyline() {
        let h = Harness::new(&behaviour);
        fs::remove_dir_all(&h.checkout).unwrap();
        let (ok, _stdout, stderr) = h.run(teardown, None);
        assert!(ok, "{teardown} must never block the next signal: {stderr}");
    }
}

#[test]
fn only_the_tor_hook_writes_the_secret_to_disk() {
    // A relay is a dumb fan-out for ciphertext it has no key for: "The relay
    // operator does NOT need a shared secret". The two newer hooks read the
    // secret off stdin and drop it, so it never lands on the relay host. The
    // Tor hook still writes one, which is existing behaviour and left alone.
    for (provider, _, behaviour) in every_partyline() {
        let h = Harness::new(&behaviour);
        h.run(provider, Some("LEAKCANARY"));

        let mut found = Vec::new();
        walk(&h.dir.path().to_path_buf(), &mut found);
        let leaked: Vec<_> = found
            .iter()
            .filter(|p| {
                fs::read_to_string(p)
                    .map(|c| c.contains("LEAKCANARY"))
                    .unwrap_or(false)
            })
            .collect();

        if provider == "provider-tor.sh" {
            assert_eq!(leaked.len(), 1, "{provider}: {leaked:?}");
            assert!(leaked[0].ends_with("shared_secret.txt"), "{leaked:?}");
        } else {
            assert!(
                leaked.is_empty(),
                "{provider} left the secret on the relay host: {leaked:?}"
            );
        }
    }
}

fn walk(dir: &PathBuf, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

// --- the web hooks ------------------------------------------------------
//
// Unprivileged and trivial by design: no Docker, no waiting, no state. What is
// worth pinning is that they satisfy the same contract the daemon enforces.

#[test]
fn the_web_provider_prints_a_url_the_daemon_accepts() {
    let h = Harness::new(&tor_publishes());
    let (ok, stdout, stderr) = h.run("provider-web.sh", None);

    assert!(ok, "hook failed: {stderr}");
    let json = stdout.lines().last().unwrap();
    let parsed: WebOutput = serde_json::from_str(json).expect("stdout is one JSON object");
    assert!(parsed.url.starts_with(&format!("{WEB_BASE}/join/")), "{}", parsed.url);
    parsed
        .validate(WEB_BASE)
        .expect("the daemon accepts what the hook printed");
}

#[test]
fn a_trailing_slash_in_the_base_url_does_not_produce_a_double_slash() {
    let h = Harness::new(&tor_publishes());
    let (ok, stdout, stderr) = h.run_with_base_url("provider-web.sh", Some("https://meet.example.org/"));
    assert!(ok, "{stderr}");
    let parsed: WebOutput = serde_json::from_str(stdout.lines().last().unwrap()).unwrap();
    assert!(!parsed.url.contains("//join"), "{}", parsed.url);
    parsed.validate(WEB_BASE).unwrap();
}

#[test]
fn every_web_room_gets_a_different_name() {
    // The URL is the only access control, so two rooms must never collide.
    let h = Harness::new(&tor_publishes());
    let (_, first, _) = h.run("provider-web.sh", None);
    let (_, second, _) = h.run("provider-web.sh", None);
    assert_ne!(first, second, "room names must not repeat");
}

#[test]
fn the_web_provider_refuses_to_guess_a_base_url() {
    let h = Harness::new(&tor_publishes());
    let (ok, _stdout, stderr) = h.run_with_base_url("provider-web.sh", None);
    assert!(!ok, "an unset base_url must fail loudly, not invent a host");
    assert!(stderr.contains("PARTYLINEPAGER_WEB_BASE_URL"), "{stderr}");
}

#[test]
fn the_web_teardown_always_succeeds() {
    let h = Harness::new(&tor_publishes());
    let (ok, _stdout, stderr) = h.run("teardown-web.sh", None);
    assert!(ok, "{stderr}");
    assert!(
        h.docker_log().is_empty(),
        "the web hooks must never touch Docker"
    );
}

#[test]
fn an_empty_path_puts_the_room_straight_under_the_domain() {
    // A self-hosted Jitsi that serves rooms at /<room> rather than
    // MiroTalk's /join/<room> sets provider.web.path = "".
    let h = Harness::new(&tor_publishes());
    let (ok, stdout, stderr) = h.run_web(Some(""), None);
    assert!(ok, "{stderr}");
    let parsed: WebOutput = serde_json::from_str(stdout.lines().last().unwrap()).unwrap();
    assert!(
        parsed.url.starts_with(&format!("{WEB_BASE}/")) && !parsed.url.contains("/join/"),
        "{}",
        parsed.url
    );
    parsed.validate(WEB_BASE).unwrap();
}

#[test]
fn a_multi_segment_path_is_pasted_through_verbatim() {
    let h = Harness::new(&tor_publishes());
    let (ok, stdout, stderr) = h.run_web(Some("conf/rooms"), None);
    assert!(ok, "{stderr}");
    let parsed: WebOutput = serde_json::from_str(stdout.lines().last().unwrap()).unwrap();
    assert!(
        parsed.url.starts_with(&format!("{WEB_BASE}/conf/rooms/")),
        "{}",
        parsed.url
    );
    parsed.validate(WEB_BASE).unwrap();
}

#[test]
fn a_static_slug_is_reused_instead_of_a_random_one() {
    // The opposite of `every_web_room_gets_a_different_name`: an admin who
    // sets provider.web.static_slug wants the same room name every time, so
    // regulars can join without waiting on a fresh broadcast.
    let h = Harness::new(&tor_publishes());
    let (ok, first, stderr) = h.run_web(Some("join"), Some("movie-night"));
    assert!(ok, "{stderr}");
    let (ok, second, stderr) = h.run_web(Some("join"), Some("movie-night"));
    assert!(ok, "{stderr}");

    let first: WebOutput = serde_json::from_str(first.lines().last().unwrap()).unwrap();
    let second: WebOutput = serde_json::from_str(second.lines().last().unwrap()).unwrap();
    assert_eq!(first.url, second.url, "a static slug must not vary between rooms");
    assert_eq!(first.url, format!("{WEB_BASE}/join/movie-night"));
    first.validate(WEB_BASE).unwrap();
}
