//! `adapters.toml`: credentials for the chat networks.
//!
//! Kept separate from `policy.toml` on purpose. Policy is meant to be readable,
//! diffable, and arguably committable. This file holds bot tokens and passwords,
//! wants mode 0600, and never belongs in version control.
//!
//! Any secret may be written as `env:NAME` to read it from the environment
//! instead, which is how the compose file passes tokens without putting them
//! on disk.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Adapters {
    pub telegram: Option<Telegram>,
    pub matrix: Option<Matrix>,
    pub irc: Option<Irc>,
    pub xmpp: Option<Xmpp>,
    pub mastodon: Option<Mastodon>,
    pub email: Option<Email>,
    pub signal: Option<Signal>,
    pub mattermost: Option<Mattermost>,
    pub discord: Option<Discord>,
}

fn enabled() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Telegram {
    #[serde(default = "enabled")]
    pub enabled: bool,
    /// Bot token from @BotFather.
    pub token: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Matrix {
    #[serde(default = "enabled")]
    pub enabled: bool,
    pub homeserver: String,
    pub user: String,
    pub password: String,
    /// Where the encryption store and session live. Deleting it forces a fresh
    /// login and loses the device's E2EE keys.
    #[serde(default = "default_matrix_store")]
    pub store_path: PathBuf,
}

fn default_matrix_store() -> PathBuf {
    PathBuf::from("/var/lib/party-line-pager/matrix")
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Irc {
    #[serde(default = "enabled")]
    pub enabled: bool,
    pub server: String,
    #[serde(default = "default_irc_port")]
    pub port: u16,
    #[serde(default = "enabled")]
    pub tls: bool,
    pub nick: String,
    /// Services account to log in as, when it differs from `nick`. Blank means
    /// the two are the same, which is the normal case.
    #[serde(default)]
    pub account: Option<String>,
    /// The services account password, sent over SASL PLAIN during connection
    /// registration. Unset means connect without logging in at all.
    #[serde(default)]
    pub password: Option<String>,
    /// Channels to sit in. Not required: the partyline pager only ever answers
    /// direct messages, but idling in a channel is how people find the bot.
    #[serde(default)]
    pub channels: Vec<String>,
}

impl Irc {
    /// The services account to authenticate as, defaulting to the nickname.
    pub fn account(&self) -> &str {
        self.account.as_deref().unwrap_or(&self.nick)
    }
}

fn default_irc_port() -> u16 {
    6697
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Xmpp {
    #[serde(default = "enabled")]
    pub enabled: bool,
    pub jid: String,
    pub password: String,
    /// Negotiate STARTTLS before logging in. Leave it alone.
    ///
    /// The only reason this exists is the throwaway server the live tests run
    /// against, which has no certificate anybody could verify. Turning it off
    /// against a real server sends the password in the clear, and SASL does not
    /// save you: PLAIN is base64, not encryption. Same knob, same reason, as
    /// `[irc] tls`.
    #[serde(default = "enabled")]
    pub tls: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mastodon {
    #[serde(default = "enabled")]
    pub enabled: bool,
    /// Instance base URL, for example `https://mastodon.social`.
    pub base_url: String,
    pub access_token: String,
    #[serde(with = "humantime_serde", default = "default_poll")]
    pub poll_interval: Duration,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Email {
    #[serde(default = "enabled")]
    pub enabled: bool,
    pub imap_host: String,
    #[serde(default = "default_imaps_port")]
    pub imap_port: u16,
    pub imap_user: String,
    pub imap_password: String,
    #[serde(default = "default_mailbox")]
    pub mailbox: String,

    pub smtp_host: String,
    #[serde(default = "default_submission_port")]
    pub smtp_port: u16,
    pub smtp_user: String,
    pub smtp_password: String,
    /// Envelope and header From.
    pub from: String,

    /// Encrypt both legs. Leave it alone.
    ///
    /// Covers IMAP and SMTP together, because the case where one is encrypted
    /// and the other is not does not come up: either the mail server is out on
    /// the internet, where both must be, or it is a throwaway on a private
    /// network, where the live tests need neither. False sends the mailbox
    /// password in the clear on both. Same knob, same reason, as `[irc] tls`
    /// and `[xmpp] tls`.
    #[serde(default = "enabled")]
    pub tls: bool,

    #[serde(with = "humantime_serde", default = "default_poll")]
    pub poll_interval: Duration,
}

fn default_imaps_port() -> u16 {
    993
}

fn default_submission_port() -> u16 {
    587
}

fn default_mailbox() -> String {
    "INBOX".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signal {
    #[serde(default = "enabled")]
    pub enabled: bool,
    /// Base URL of a signal-cli-rest-api container, for example
    /// `http://signal-cli-rest-api:8080`.
    pub rest_url: String,
    /// The registered number this bot sends from, in E.164.
    pub number: String,
}

fn default_poll() -> Duration {
    Duration::from_secs(30)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mattermost {
    #[serde(default = "enabled")]
    pub enabled: bool,
    /// Server base URL, for example `https://mattermost.example.org`.
    pub base_url: String,
    /// A personal or bot access token. Needs `create_user_access_token` and
    /// `EnableUserAccessTokens` switched on for the account it belongs to.
    pub access_token: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Discord {
    #[serde(default = "enabled")]
    pub enabled: bool,
    /// Bot token from the Discord Developer Portal.
    pub token: String,
}

impl Adapters {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        warn_if_world_readable(path);

        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("could not read {}", path.display()))?;
        let mut adapters: Adapters = toml::from_str(&raw)
            .with_context(|| format!("could not parse {}", path.display()))?;
        adapters.resolve_env()?;
        Ok(adapters)
    }

    pub fn parse_str(raw: &str) -> Result<Self> {
        let mut adapters: Adapters = toml::from_str(raw)?;
        adapters.resolve_env()?;
        Ok(adapters)
    }

    /// Replaces every `env:NAME` value with the environment variable's contents.
    fn resolve_env(&mut self) -> Result<()> {
        let mut fields: Vec<&mut String> = Vec::new();

        if let Some(t) = &mut self.telegram {
            fields.push(&mut t.token);
        }
        if let Some(m) = &mut self.matrix {
            fields.push(&mut m.password);
        }
        if let Some(i) = &mut self.irc {
            if let Some(password) = &mut i.password {
                fields.push(password);
            }
        }
        if let Some(x) = &mut self.xmpp {
            fields.push(&mut x.password);
        }
        if let Some(m) = &mut self.mastodon {
            fields.push(&mut m.access_token);
        }
        if let Some(e) = &mut self.email {
            fields.push(&mut e.imap_password);
            fields.push(&mut e.smtp_password);
        }
        if let Some(m) = &mut self.mattermost {
            fields.push(&mut m.access_token);
        }
        if let Some(d) = &mut self.discord {
            fields.push(&mut d.token);
        }

        for field in fields {
            if let Some(name) = field.strip_prefix("env:") {
                let value = std::env::var(name)
                    .with_context(|| format!("adapters.toml refers to ${name}, which is unset"))?;
                if value.is_empty() {
                    bail!("${name} is set but empty");
                }
                *field = value;
            }
        }
        Ok(())
    }

    /// Names of the adapters that are present and switched on.
    pub fn enabled_names(&self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if self.telegram.as_ref().is_some_and(|c| c.enabled) {
            names.push("telegram");
        }
        if self.matrix.as_ref().is_some_and(|c| c.enabled) {
            names.push("matrix");
        }
        if self.irc.as_ref().is_some_and(|c| c.enabled) {
            names.push("irc");
        }
        if self.xmpp.as_ref().is_some_and(|c| c.enabled) {
            names.push("xmpp");
        }
        if self.mastodon.as_ref().is_some_and(|c| c.enabled) {
            names.push("mastodon");
        }
        if self.email.as_ref().is_some_and(|c| c.enabled) {
            names.push("email");
        }
        if self.signal.as_ref().is_some_and(|c| c.enabled) {
            names.push("signal");
        }
        if self.mattermost.as_ref().is_some_and(|c| c.enabled) {
            names.push("mattermost");
        }
        if self.discord.as_ref().is_some_and(|c| c.enabled) {
            names.push("discord");
        }
        names
    }
}

/// Bot tokens in a file anyone on the box can read is worth a loud line in the
/// log, but not a refusal to start: some deployments run everything as one user
/// in a container.
fn warn_if_world_readable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    let mode = meta.permissions().mode() & 0o077;
    if mode != 0 {
        tracing::warn!(
            path = %path.display(),
            mode = format!("{:o}", meta.permissions().mode() & 0o777),
            "credentials file is readable beyond its owner, chmod 600 it"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_section_is_optional() {
        let adapters = Adapters::parse_str("").unwrap();
        assert!(adapters.enabled_names().is_empty());
    }

    #[test]
    fn typos_are_rejected_rather_than_silently_ignored() {
        let err = Adapters::parse_str(
            r#"
            [telegram]
            tokne = "oops"
            "#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("tokne") || err.contains("unknown field"), "{err}");
    }

    #[test]
    fn enabled_defaults_to_true_and_can_be_switched_off() {
        let adapters = Adapters::parse_str(
            r#"
            [telegram]
            token = "t"

            [signal]
            enabled = false
            rest_url = "http://signal:8080"
            number = "+15551234567"
            "#,
        )
        .unwrap();
        assert_eq!(adapters.enabled_names(), vec!["telegram"]);
    }

    #[test]
    fn secrets_can_come_from_the_environment() {
        // SAFETY: single-threaded test, no other thread reads the environment.
        unsafe { std::env::set_var("PARTY_LINE_PAGER_TEST_TOKEN", "from-env") };
        let adapters = Adapters::parse_str(
            r#"
            [telegram]
            token = "env:PARTY_LINE_PAGER_TEST_TOKEN"
            "#,
        )
        .unwrap();
        assert_eq!(adapters.telegram.unwrap().token, "from-env");
    }

    #[test]
    fn a_missing_environment_variable_is_a_startup_error() {
        let err = Adapters::parse_str(
            r#"
            [telegram]
            token = "env:PARTY_LINE_PAGER_DEFINITELY_UNSET"
            "#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("PARTY_LINE_PAGER_DEFINITELY_UNSET"), "{err}");
    }

    #[test]
    fn defaults_match_the_documented_ports() {
        let adapters = Adapters::parse_str(
            r#"
            [irc]
            server = "irc.libera.chat"
            nick = "party-line-pager"

            [email]
            imap_host = "imap.example.org"
            imap_user = "bot"
            imap_password = "x"
            smtp_host = "smtp.example.org"
            smtp_user = "bot"
            smtp_password = "x"
            from = "bot@example.org"
            "#,
        )
        .unwrap();

        let irc = adapters.irc.unwrap();
        assert_eq!(irc.port, 6697);
        assert!(irc.tls);

        let email = adapters.email.unwrap();
        assert_eq!(email.imap_port, 993);
        assert_eq!(email.smtp_port, 587);
        assert_eq!(email.mailbox, "INBOX");
        assert_eq!(email.poll_interval, Duration::from_secs(30));
    }
}
