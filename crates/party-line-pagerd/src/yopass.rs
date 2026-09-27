//! Minting one-time Yopass links for Link mode credential delivery.
//!
//! Yopass encrypts client-side with OpenPGP (`openpgp.SymmetricallyEncrypt`,
//! ProtonMail's go-crypto) before it ever reaches the server. Byte-matching
//! that in Rust well enough to interoperate with Yopass's own web decryptor
//! would be a crypto project on its own, for no benefit: the official `yopass`
//! CLI already does it correctly. This module runs that binary the same way
//! `provider.rs` runs an admin hook: plaintext on stdin, one line of stdout
//! back, everything about the process itself (piping, timeout, draining
//! stderr) following that same shape.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

/// Shells out to the `yopass` CLI to encrypt a secret and mint a share URL.
pub struct YopassClient {
    bin: PathBuf,
    url: String,
    api: String,
    timeout: Duration,
}

/// Yopass's CLI only accepts `--expiration` as one of these three literal
/// tokens (`pkg/yopass/yopass.go`'s `expirations` map: `{"1h": 3600, "1d":
/// 86400, "1w": 604800}`), not an arbitrary duration — an earlier version of
/// `mint` sent `"{seconds}s"`, which the real CLI rejected outright with
/// "Expiration can only be 1 hour (1h), 1 day (1d), or 1 week (1w)". Only
/// caught by `yopass_live.rs` actually running the real binary; the unit
/// tests below use a fake script that ignores its arguments entirely.
///
/// Rounds up to the smallest bucket that covers `ttl`, so a link never
/// expires before the room does — the tradeoff is it can outlive the room by
/// up to a week, which is harmless since the room itself is already gone by
/// then.
fn expiration_flag(ttl: Duration) -> &'static str {
    const HOUR: Duration = Duration::from_secs(3600);
    const DAY: Duration = Duration::from_secs(86_400);
    if ttl <= HOUR {
        "1h"
    } else if ttl <= DAY {
        "1d"
    } else {
        "1w"
    }
}

impl YopassClient {
    pub fn new(
        bin: impl Into<PathBuf>,
        url: impl Into<String>,
        api: impl Into<String>,
        timeout: Duration,
    ) -> Self {
        Self {
            bin: bin.into(),
            url: url.into(),
            api: api.into(),
            timeout,
        }
    }

    /// Encrypts `plaintext` and returns the one-time share URL. `ttl` is
    /// rounded to the CLI's `--expiration` grammar by [`expiration_flag`];
    /// its own `--one-time` default (`true`) is left alone, since that is
    /// exactly the property Link mode wants.
    pub async fn mint(&self, plaintext: &str, ttl: Duration) -> Result<String> {
        let mut cmd = Command::new(&self.bin);
        cmd.arg("--url")
            .arg(&self.url)
            .arg("--api")
            .arg(&self.api)
            .arg("--expiration")
            .arg(expiration_flag(ttl))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = cmd
            .spawn()
            .with_context(|| format!("could not run yopass at {}", self.bin.display()))?;

        // Always close stdin, same reasoning as the provider hooks: a
        // `yopass` build that somehow blocked on stdin must not hang forever.
        {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| anyhow!("yopass stdin was not piped"))?;
            stdin.write_all(plaintext.as_bytes()).await?;
            stdin.shutdown().await?;
        }

        // Stdout and stderr are drained on separate tasks, independent of the
        // timeout below, so a `yopass` invocation that is killed for running
        // too long still gets whatever it had already printed onto the
        // daemon's own log.
        let mut stdout_pipe = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("yopass stdout was not piped"))?;
        let stderr_pipe = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("yopass stderr was not piped"))?;

        let stdout_task = tokio::spawn(async move {
            let mut buf = String::new();
            let _ = stdout_pipe.read_to_string(&mut buf).await;
            buf
        });

        let stderr_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr_pipe).lines();
            let mut collected = String::new();
            while let Ok(Some(line)) = lines.next_line().await {
                collected.push_str(line.trim());
                collected.push('\n');
            }
            collected
        });

        let status = match tokio::time::timeout(self.timeout, child.wait()).await {
            Ok(res) => res.context("yopass process failed")?,
            Err(_) => {
                let _ = child.start_kill();
                let _ = child.wait().await;
                stdout_task.abort();
                stderr_task.abort();
                bail!("yopass exceeded {}s", self.timeout.as_secs());
            }
        };

        let stdout = stdout_task.await.unwrap_or_default();
        let stderr = stderr_task.await.unwrap_or_default();

        if !status.success() {
            bail!("yopass exited with {}: {}", status, stderr.trim());
        }

        let url = stdout.trim();
        if url.is_empty() {
            bail!("yopass printed no URL on stdout");
        }
        Ok(url.to_string())
    }
}

/// Per-(room, recipient) cache of minted links.
///
/// Yopass links are one-time-view, but a room's credentials are rendered
/// from three separate places (the initial broadcast, `already_live`, and
/// repeated `status` checks). Minting fresh every time would mean a
/// subscriber who checks `status` twice sees a dead link on the second look,
/// if they had already opened the first one. Mint once per (room, recipient),
/// reuse for the room's lifetime, and drop the entries when the room closes.
#[derive(Default)]
pub struct LinkCache {
    entries: std::sync::Mutex<std::collections::HashMap<(String, party_line_pager_core::EndpointId), String>>,
}

impl LinkCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the cached link for `(room_id, recipient)`, minting one first
    /// if this room's credentials have not been rendered for them yet.
    pub async fn get_or_mint(
        &self,
        room_id: &str,
        recipient: &party_line_pager_core::EndpointId,
        yopass: &YopassClient,
        plaintext: &str,
        ttl: Duration,
    ) -> Result<String> {
        let key = (room_id.to_string(), recipient.clone());
        if let Some(url) = self.entries.lock().unwrap().get(&key) {
            return Ok(url.clone());
        }
        let url = yopass.mint(plaintext, ttl).await?;
        self.entries.lock().unwrap().insert(key, url.clone());
        Ok(url)
    }

    /// Drops every cached link for a room. Called when it tears down, so
    /// entries do not outlive the room they belong to.
    pub fn clear_room(&self, room_id: &str) {
        self.entries.lock().unwrap().retain(|(id, _), _| id != room_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn script(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[tokio::test]
    async fn mint_sends_the_plaintext_on_stdin_and_returns_the_url() {
        let dir = tempfile::tempdir().unwrap();
        let seen = dir.path().join("seen");
        let bin = script(
            dir.path(),
            "yopass",
            &format!(
                "#!/bin/sh\ncat > {}\necho 'https://yopass.se/#/s/x/y'\n",
                seen.display()
            ),
        );
        let client = YopassClient::new(&bin, "https://yopass.se", "https://api.yopass.se", Duration::from_secs(5));

        let url = client.mint("Onion: abc.onion\nSecret: S", Duration::from_secs(60)).await.unwrap();

        assert_eq!(url, "https://yopass.se/#/s/x/y");
        assert_eq!(
            std::fs::read_to_string(seen).unwrap(),
            "Onion: abc.onion\nSecret: S"
        );
    }

    #[tokio::test]
    async fn ttl_is_rounded_to_a_token_the_real_cli_accepts() {
        let dir = tempfile::tempdir().unwrap();
        let seen = dir.path().join("seen");
        let bin = script(
            dir.path(),
            "yopass",
            &format!(
                "#!/bin/sh\ncat >/dev/null\necho \"$@\" > {}\necho 'https://yopass.se/#/s/x/y'\n",
                seen.display()
            ),
        );
        let client = YopassClient::new(&bin, "https://yopass.se", "https://api.yopass.se", Duration::from_secs(5));

        for (ttl, want) in [
            (Duration::from_secs(60), "1h"),
            (Duration::from_secs(3600), "1h"),
            (Duration::from_secs(3601), "1d"),
            (Duration::from_secs(86_400), "1d"),
            (Duration::from_secs(86_401), "1w"),
            (Duration::from_secs(30 * 86_400), "1w"),
        ] {
            client.mint("secret", ttl).await.unwrap();
            let argv = std::fs::read_to_string(&seen).unwrap();
            assert!(
                argv.contains(&format!("--expiration {want}")),
                "ttl {ttl:?} should map to --expiration {want}, got: {argv}"
            );
        }
    }

    #[tokio::test]
    async fn a_nonzero_exit_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let bin = script(dir.path(), "yopass", "#!/bin/sh\necho 'server unreachable' >&2\nexit 1\n");
        let client = YopassClient::new(&bin, "https://yopass.se", "https://api.yopass.se", Duration::from_secs(5));

        let err = client.mint("secret", Duration::from_secs(60)).await.unwrap_err().to_string();
        assert!(err.contains("server unreachable"), "{err}");
    }

    #[tokio::test]
    async fn a_hanging_process_hits_the_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let bin = script(dir.path(), "yopass", "#!/bin/sh\nsleep 30\n");
        let client = YopassClient::new(&bin, "https://yopass.se", "https://api.yopass.se", Duration::from_millis(200));

        let err = client.mint("secret", Duration::from_secs(60)).await.unwrap_err().to_string();
        assert!(err.contains("exceeded"), "{err}");
    }

    #[tokio::test]
    async fn link_cache_reuses_a_url_instead_of_minting_twice() {
        let dir = tempfile::tempdir().unwrap();
        let calls = dir.path().join("calls");
        let bin = script(
            dir.path(),
            "yopass",
            &format!(
                "#!/bin/sh\ncat >/dev/null\nprintf x >> {}\necho https://yopass.se/#/s/x/y\n",
                calls.display()
            ),
        );
        let client = YopassClient::new(&bin, "https://yopass.se", "https://api.yopass.se", Duration::from_secs(5));
        let cache = LinkCache::new();
        let recipient: party_line_pager_core::EndpointId = "telegram:1".parse().unwrap();

        let first = cache
            .get_or_mint("room-a", &recipient, &client, "secret", Duration::from_secs(60))
            .await
            .unwrap();
        let second = cache
            .get_or_mint("room-a", &recipient, &client, "secret", Duration::from_secs(60))
            .await
            .unwrap();

        assert_eq!(first, second);
        assert_eq!(std::fs::read_to_string(calls).unwrap(), "x", "minted only once");
    }

    #[tokio::test]
    async fn clear_room_drops_only_that_rooms_entries() {
        let dir = tempfile::tempdir().unwrap();
        let bin = script(dir.path(), "yopass", "#!/bin/sh\ncat >/dev/null\necho https://yopass.se/#/s/x/y\n");
        let client = YopassClient::new(&bin, "https://yopass.se", "https://api.yopass.se", Duration::from_secs(5));
        let cache = LinkCache::new();
        let recipient: party_line_pager_core::EndpointId = "telegram:1".parse().unwrap();

        cache.get_or_mint("room-a", &recipient, &client, "secret", Duration::from_secs(60)).await.unwrap();
        cache.get_or_mint("room-b", &recipient, &client, "secret", Duration::from_secs(60)).await.unwrap();
        cache.clear_room("room-a");

        assert_eq!(cache.entries.lock().unwrap().len(), 1);
        assert!(cache.entries.lock().unwrap().contains_key(&("room-b".to_string(), recipient)));
    }
}
