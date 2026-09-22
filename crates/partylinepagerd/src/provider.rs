//! Room provisioning through admin-owned hook scripts.
//!
//! The partyline pager never speaks Docker, Tor, or any backend's protocol. It
//! runs two scripts named in `policy.toml` and reads one JSON object back.
//! That keeps the privileged half of the system in shell an admin can audit,
//! and it means swapping the Docker party line for something else later is a
//! hook change rather than a rewrite.
//!
//! The shared secret is written to the hook's **stdin**, never to argv or the
//! environment, because `/proc/<pid>/cmdline` and `/proc/<pid>/environ` are
//! readable by any process of the same user.
//!
//! There are two hook contracts, because their outputs genuinely differ: a
//! party line returns an address and takes a secret, a web room returns a URL
//! and takes nothing. All three party-line networks share the first contract.
//! Everything around that (stdin handling, environment, timeout, finding the
//! JSON object in stdout) is shared, so [`ProviderRunner::up`] is generic over
//! what it deserializes.
//!
//! Validation deliberately lives at the **caller**, not inside `up()`: an
//! address can only be judged against the network it is supposed to be on, and
//! a web URL only against the `base_url` from policy, neither of which the
//! runner has any business knowing. Both call sites in `engine.rs` validate
//! before anything is persisted or sent.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use partylinepager_core::Transport;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

/// Longest URL a web hook may return. Comfortably past any real room link and
/// far short of what would wreck an SMS or an IRC line.
const MAX_URL_LEN: usize = 2048;
/// Longest room slug, measured after the configured base URL.
const MAX_SLUG_LEN: usize = 200;

/// What a party-line provider hook prints on stdout.
///
/// One shape for all three networks. Which network the address belongs to is
/// decided by which `[provider.*]` section ran the hook, not by anything the
/// hook says, so a hook pointed at the wrong compose file is caught by
/// [`Transport::validate`] rather than believed.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct PartylineOutput {
    /// The address joiners dial. No port travels with it: `partyline.sh`
    /// accepts only a bare address on all three networks.
    pub address: String,
    /// Optional override for how long the room should live. Lets a backend that
    /// knows better than policy shorten the window.
    #[serde(default)]
    pub ttl_secs: Option<u64>,
}

/// What a `web` provider hook prints on stdout. No port, no secret: a room here
/// is a URL and nothing else.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct WebOutput {
    /// The room URL subscribers open.
    pub url: String,
    #[serde(default)]
    pub ttl_secs: Option<u64>,
}

impl WebOutput {
    /// Rejects anything that would be unsafe to paste into a broadcast.
    ///
    /// `base_url` is passed in rather than stored, so this stays a pure
    /// function of the hook's output and the admin's policy. Requiring the URL
    /// to start with the configured base is the whole check that matters: it
    /// means a hook that goes wrong cannot mail the roster a link to somewhere
    /// the admin never chose.
    pub fn validate(&self, base_url: &str) -> Result<()> {
        let url = self.url.trim();
        let base = base_url.trim_end_matches('/');

        if url.is_empty() {
            bail!("web hook returned an empty url");
        }
        if url.len() > MAX_URL_LEN {
            bail!("web hook returned an implausibly long url ({} bytes)", url.len());
        }
        if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
            bail!("room url contains whitespace or control characters");
        }
        let slug = url.strip_prefix(base).ok_or_else(|| {
            anyhow!("room url {url:?} does not start with the configured base_url {base:?}")
        })?;
        let slug = slug.trim_start_matches('/');
        if slug.is_empty() {
            bail!("room url {url:?} is the bare base_url with no room in it");
        }
        if slug.len() > MAX_SLUG_LEN {
            bail!("room url has an implausibly long path");
        }
        // No regex, same as the address check: an explicit character set is
        // easier to be sure about at 3am.
        if !slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '/'))
        {
            bail!("room url path {slug:?} has characters outside [A-Za-z0-9_/-]");
        }
        Ok(())
    }
}

impl PartylineOutput {
    /// Rejects an address that is not well-formed for `transport`.
    ///
    /// The real rules live on [`Transport`] in `partylinepager-core`, so the daemon
    /// and anything else that has to judge an address agree by construction.
    pub fn validate(&self, transport: Transport) -> Result<()> {
        transport
            .validate(self.address.trim())
            .map_err(|e| anyhow!("{e}"))
    }
}

pub struct ProviderRunner {
    up: PathBuf,
    down: PathBuf,
    timeout: Duration,
    /// Extra environment handed to both hooks. Lets `main.rs` give the web
    /// hook its `PARTYLINEPAGER_WEB_BASE_URL` without this type ever learning that
    /// web providers exist.
    env: Vec<(String, String)>,
}

impl ProviderRunner {
    pub fn new(up: impl Into<PathBuf>, down: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            up: up.into(),
            down: down.into(),
            timeout,
            env: Vec::new(),
        }
    }

    /// Adds one environment variable to every run of these hooks.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Brings a room up and deserializes whatever the hook printed.
    ///
    /// **Does not validate.** The caller must call `validate()` on the result
    /// before persisting or broadcasting it; see the module docs for why.
    ///
    /// `secret` is `None` for providers that have no secret, in which case
    /// nothing is written to the hook's stdin at all. Stdin is closed either
    /// way, so a hook that reads it never blocks.
    pub async fn up<O: DeserializeOwned>(
        &self,
        secret: Option<&str>,
        ttl: Duration,
        note: Option<&str>,
    ) -> Result<O> {
        let stdout = self
            .run(&self.up, secret, |cmd| {
                cmd.env("PARTYLINEPAGER_TTL_SECS", ttl.as_secs().to_string());
                cmd.env("PARTYLINEPAGER_NOTE", note.unwrap_or_default());
            })
            .await?;

        let json = stdout
            .lines()
            .rev()
            .find(|line| line.trim_start().starts_with('{'))
            .ok_or_else(|| {
                anyhow!("provider hook printed no JSON object on stdout: {stdout:?}")
            })?;

        serde_json::from_str(json)
            .with_context(|| format!("provider hook printed invalid JSON: {json:?}"))
    }

    /// Tears a room down. Best effort: a failure here is logged, not fatal,
    /// because the alternative is a daemon that refuses to accept new signals
    /// because an old container will not die.
    ///
    /// `room_id` is the address for a party line and the room URL for a web
    /// room. Deliberately one variable for both, so the engine's teardown path
    /// needs no idea which kind of room it is retiring.
    pub async fn down(&self, room_id: &str) -> Result<()> {
        self.run(&self.down, None, |cmd| {
            cmd.env("PARTYLINEPAGER_ROOM_ID", room_id);
        })
        .await
        .map(|_| ())
    }

    async fn run(
        &self,
        script: &Path,
        stdin_secret: Option<&str>,
        configure: impl FnOnce(&mut Command),
    ) -> Result<String> {
        let mut cmd = Command::new(script);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (key, value) in &self.env {
            cmd.env(key, value);
        }
        configure(&mut cmd);

        let mut child = cmd
            .spawn()
            .with_context(|| format!("could not run provider hook {}", script.display()))?;

        // Always close stdin, even when there is no secret, so a hook that
        // reads it does not block forever. BrokenPipe is ignored: the child
        // may exit before the write completes, and the exit-code path below
        // reports the real failure.
        {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| anyhow!("provider hook stdin was not piped"))?;
            let r: std::io::Result<()> = async {
                if let Some(secret) = stdin_secret {
                    stdin.write_all(secret.as_bytes()).await?;
                    stdin.write_all(b"\n").await?;
                }
                stdin.shutdown().await
            }
            .await;
            if let Err(e) = r {
                if e.kind() != std::io::ErrorKind::BrokenPipe {
                    return Err(e.into());
                }
            }
        }

        // Stdout and stderr are drained on separate tasks, independent of the
        // timeout below, so a hook that is killed for running too long still
        // gets whatever it had already printed onto the daemon's own log —
        // stderr is traced line-by-line as it arrives, not buffered until
        // the process exits. `wait_with_output()` used to buffer everything
        // and hand it back only on a clean exit, so a timeout silently threw
        // away every diagnostic the hook had written.
        let mut stdout_pipe = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("provider hook stdout was not piped"))?;
        let stderr_pipe = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("provider hook stderr was not piped"))?;

        let stdout_task = tokio::spawn(async move {
            let mut buf = String::new();
            let _ = stdout_pipe.read_to_string(&mut buf).await;
            buf
        });

        let hook_display = script.display().to_string();
        let stderr_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr_pipe).lines();
            let mut collected = String::new();
            while let Ok(Some(line)) = lines.next_line().await {
                if !line.trim().is_empty() {
                    tracing::info!(hook = %hook_display, "{}", line.trim());
                }
                collected.push_str(line.trim());
                collected.push('\n');
            }
            collected
        });

        let status = match tokio::time::timeout(self.timeout, child.wait()).await {
            Ok(res) => {
                res.with_context(|| format!("provider hook {} failed", script.display()))?
            }
            Err(_) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                stdout_task.abort();
                stderr_task.abort();
                bail!(
                    "provider hook {} exceeded {}s",
                    script.display(),
                    self.timeout.as_secs()
                );
            }
        };

        let stdout = stdout_task.await.unwrap_or_default();
        let stderr = stderr_task.await.unwrap_or_default();

        if !status.success() {
            bail!(
                "provider hook {} exited with {}: {}",
                script.display(),
                status,
                stderr.trim()
            );
        }

        Ok(stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// A real-shaped v3 onion: 56 base32 characters plus the suffix.
    const ONION: &str = "abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrstuvwx.onion";
    /// A real-shaped I2P address: 52 base32 characters plus the suffix.
    const B32: &str = "abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrst.b32.i2p";
    /// A real-shaped Reticulum destination hash: 32 lowercase hex characters.
    const HASH: &str = "3a1c9d4e07b21f88c2a04e7d612b0f4e";

    const BASE: &str = "https://p2p.mirotalk.com";

    fn web(url: &str) -> WebOutput {
        WebOutput {
            url: url.into(),
            ttl_secs: None,
        }
    }

    #[test]
    fn validation_accepts_a_real_address_on_every_network() {
        for (transport, address) in [
            (Transport::Tor, ONION),
            (Transport::I2p, B32),
            (Transport::Reticulum, HASH),
        ] {
            let ok = PartylineOutput {
                address: address.into(),
                ttl_secs: None,
            };
            assert!(ok.validate(transport).is_ok(), "{transport} {address}");
        }
    }

    #[test]
    fn validation_rejects_junk() {
        for bad in [
            "",
            "example.com",
            "not base32!.onion",
            ".onion",
            "has space.onion",
            // A perfectly good address, but on the wrong network.
            B32,
            HASH,
        ] {
            let out = PartylineOutput {
                address: bad.into(),
                ttl_secs: None,
            };
            assert!(
                out.validate(Transport::Tor).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn a_room_url_must_live_under_the_configured_base_url() {
        assert!(web("https://p2p.mirotalk.com/join/abc123DEF-_/x")
            .validate(BASE)
            .is_ok());
        // A trailing slash in policy is cosmetic and must not fail a good URL.
        assert!(web("https://p2p.mirotalk.com/join/abc")
            .validate("https://p2p.mirotalk.com/")
            .is_ok());

        for bad in [
            "",
            "https://evil.example.com/join/abc",
            // Prefix-only is not enough: a lookalike host must not pass.
            "https://p2p.mirotalk.com.evil.example/join/abc",
            "https://p2p.mirotalk.com",
            "https://p2p.mirotalk.com/",
            "https://p2p.mirotalk.com/join/abc def",
            "https://p2p.mirotalk.com/join/<script>",
            "https://p2p.mirotalk.com/join/a?b=c",
        ] {
            assert!(web(bad).validate(BASE).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn a_room_url_cannot_be_absurdly_long() {
        let long = format!("{BASE}/join/{}", "a".repeat(MAX_SLUG_LEN + 1));
        assert!(web(&long).validate(BASE).is_err());
        assert!(web(&format!("{BASE}/join/{}", "a".repeat(MAX_URL_LEN)))
            .validate(BASE)
            .is_err());
    }

    #[tokio::test]
    async fn up_reads_the_secret_from_stdin_and_parses_json() {
        let dir = tempfile::tempdir().unwrap();
        let seen = dir.path().join("seen");
        let up = script(
            dir.path(),
            "up.sh",
            &format!(
                "#!/bin/sh\nread -r SECRET\nprintf '%s' \"$SECRET\" > {}\n\
                 echo \"bootstrapping tor\" >&2\n\
                 echo '{{\"address\":\"{ONION}\"}}'\n",
                seen.display()
            ),
        );
        let runner = ProviderRunner::new(&up, dir.path().join("down.sh"), Duration::from_secs(10));

        let out = runner
            .up::<PartylineOutput>(Some("SUPERSECRET"), Duration::from_secs(7200), Some("poker"))
            .await
            .unwrap();

        assert_eq!(out.address, ONION);
        assert_eq!(std::fs::read_to_string(seen).unwrap(), "SUPERSECRET");
    }

    #[tokio::test]
    async fn the_secret_never_appears_in_argv_or_the_environment() {
        let dir = tempfile::tempdir().unwrap();
        let dump = dir.path().join("dump");
        let up = script(
            dir.path(),
            "up.sh",
            &format!(
                "#!/bin/sh\nread -r SECRET\n{{ echo \"argv=$*\"; env; }} > {}\n\
                 echo '{{\"address\":\"{ONION}\"}}'\n",
                dump.display()
            ),
        );
        let runner = ProviderRunner::new(&up, dir.path().join("down.sh"), Duration::from_secs(10));
        runner
            .up::<PartylineOutput>(Some("LEAKCANARY"), Duration::from_secs(60), None)
            .await
            .unwrap();

        let dumped = std::fs::read_to_string(dump).unwrap();
        assert!(
            !dumped.contains("LEAKCANARY"),
            "secret leaked into argv or env: {dumped}"
        );
        assert!(dumped.contains("PARTYLINEPAGER_TTL_SECS=60"));
    }

    #[tokio::test]
    async fn ttl_and_note_reach_the_hook() {
        let dir = tempfile::tempdir().unwrap();
        let up = script(
            dir.path(),
            "up.sh",
            &format!(
                "#!/bin/sh\nread -r _\n\
                 echo \"{{\\\"address\\\":\\\"{ONION}\\\",\\\"ttl_secs\\\":$PARTYLINEPAGER_TTL_SECS}}\"\n"
            ),
        );
        let runner = ProviderRunner::new(&up, dir.path().join("down.sh"), Duration::from_secs(10));
        let out = runner
            .up::<PartylineOutput>(Some("s"), Duration::from_secs(1234), Some("note"))
            .await
            .unwrap();
        assert_eq!(out.ttl_secs, Some(1234));
    }

    #[tokio::test]
    async fn a_failing_hook_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let up = script(
            dir.path(),
            "up.sh",
            "#!/bin/sh\necho 'tor refused to start' >&2\nexit 3\n",
        );
        let runner = ProviderRunner::new(&up, dir.path().join("down.sh"), Duration::from_secs(10));
        let err = runner
            .up::<PartylineOutput>(Some("s"), Duration::from_secs(60), None)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("tor refused to start"), "{err}");
    }

    #[tokio::test]
    async fn a_hook_that_prints_no_json_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let up = script(dir.path(), "up.sh", "#!/bin/sh\necho hello\n");
        let runner = ProviderRunner::new(&up, dir.path().join("down.sh"), Duration::from_secs(10));
        assert!(runner.up::<PartylineOutput>(Some("s"), Duration::from_secs(60), None).await.is_err());
    }

    #[tokio::test]
    async fn a_hook_that_returns_a_bogus_address_parses_but_fails_validation() {
        // `up()` deliberately does not validate, because an address can only
        // be judged against the network it should be on and a web URL only
        // against policy. The runner's job stops at "this
        // was JSON of the right shape"; the engine calls validate() before
        // anything is persisted or sent, and there is an end-to-end test for
        // that in engine.rs.
        let dir = tempfile::tempdir().unwrap();
        let up = script(
            dir.path(),
            "up.sh",
            "#!/bin/sh\necho '{\"address\":\"evil.example.com\"}'\n",
        );
        let runner = ProviderRunner::new(&up, dir.path().join("down.sh"), Duration::from_secs(10));
        let out: PartylineOutput = runner
            .up(Some("s"), Duration::from_secs(60), None)
            .await
            .unwrap();
        assert!(out.validate(Transport::Tor).is_err());
    }

    #[tokio::test]
    async fn a_web_hook_gets_its_base_url_and_no_secret_on_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let seen = dir.path().join("seen");
        let up = script(
            dir.path(),
            "up.sh",
            &format!(
                "#!/bin/sh\nSTDIN=\"$(cat)\"\nprintf '[%s]' \"$STDIN\" > {}\n\
                 printf '{{\"url\":\"%s/join/abc123\"}}\\n' \"$PARTYLINEPAGER_WEB_BASE_URL\"\n",
                seen.display()
            ),
        );
        let runner = ProviderRunner::new(&up, dir.path().join("down.sh"), Duration::from_secs(10))
            .with_env("PARTYLINEPAGER_WEB_BASE_URL", BASE);

        let out: WebOutput = runner.up(None, Duration::from_secs(60), None).await.unwrap();

        assert_eq!(out.url, format!("{BASE}/join/abc123"));
        assert!(out.validate(BASE).is_ok());
        assert_eq!(
            std::fs::read_to_string(seen).unwrap(),
            "[]",
            "a provider with no secret must be handed nothing at all"
        );
    }

    #[tokio::test]
    async fn a_hanging_hook_hits_the_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let up = script(dir.path(), "up.sh", "#!/bin/sh\nsleep 30\n");
        let runner =
            ProviderRunner::new(&up, dir.path().join("down.sh"), Duration::from_millis(200));
        let err = runner
            .up::<PartylineOutput>(Some("s"), Duration::from_secs(60), None)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("exceeded"), "{err}");
    }

    #[tokio::test]
    async fn down_receives_the_room_id_whatever_kind_of_room_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let seen = dir.path().join("seen");
        let down = script(
            dir.path(),
            "down.sh",
            &format!(
                "#!/bin/sh\nprintf '%s' \"$PARTYLINEPAGER_ROOM_ID\" > {}\n",
                seen.display()
            ),
        );
        let runner = ProviderRunner::new(dir.path().join("up.sh"), &down, Duration::from_secs(10));

        runner.down(ONION).await.unwrap();
        assert_eq!(std::fs::read_to_string(&seen).unwrap(), ONION);

        let url = format!("{BASE}/join/abc123");
        runner.down(&url).await.unwrap();
        assert_eq!(std::fs::read_to_string(&seen).unwrap(), url);
    }

    #[tokio::test]
    async fn a_missing_hook_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let runner = ProviderRunner::new(
            dir.path().join("nope.sh"),
            dir.path().join("down.sh"),
            Duration::from_secs(5),
        );
        let err = runner
            .up::<PartylineOutput>(Some("s"), Duration::from_secs(60), None)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("could not run provider hook"), "{err}");
    }
}
