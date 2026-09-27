//! `policy.toml`: the admin-owned, daemon-read-only half of the configuration.
//!
//! TOML rather than YAML on purpose. `serde_yaml` was archived by its author in
//! March 2024 and the surviving fork is not something to put underneath a
//! security policy file.
//!
//! The daemon never writes this file. Mutable runtime state (subscribers,
//! quotas, the live room) lives in the state directory instead, so an admin
//! editing policy can never be clobbered by a fanout in progress.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Policy {
    pub instance: Instance,
    #[serde(default)]
    pub fanout: Fanout,
    #[serde(default)]
    pub provider: Provider,
    /// Written as repeated `[[tier]]` tables.
    #[serde(rename = "tier", default)]
    pub tiers: Vec<Tier>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Instance {
    /// Shown in broadcasts and help text.
    #[serde(default = "default_name")]
    pub name: String,

    /// Whether strangers may add themselves to the roster.
    #[serde(default)]
    pub signups: Signups,

    /// Tier assigned to a new subscriber.
    pub default_tier: String,

    /// How long a room lives before the teardown hook runs.
    #[serde(with = "humantime_serde", default = "default_room_ttl")]
    pub room_ttl: Duration,

    /// Ceiling on the provider hook. Generous by default: wiping the onion key
    /// forces a fresh Tor bootstrap, which takes one to three minutes.
    #[serde(with = "humantime_serde", default = "default_hook_timeout")]
    pub hook_timeout: Duration,

    /// Global kill switch. `partylinepagerctl pause` flips this without a redeploy
    /// by writing the state directory, so this is only the boot default.
    #[serde(default)]
    pub paused: bool,

    /// Whether a room's onion+secret (or URL) travel inline in the broadcast,
    /// or as a one-time Yopass link minted per recipient. See render.rs.
    #[serde(default)]
    pub creds_delivery: CredsMode,

    /// Yopass instance the share link points subscribers at. Only read when
    /// `creds_delivery = "link"`. Defaults to the public share.yopass.se.
    #[serde(default = "default_yopass_url")]
    pub yopass_url: String,

    /// Yopass API endpoint the `yopass` CLI mints against. Only read when
    /// `creds_delivery = "link"`. Defaults to the public api2.yopass.se.
    #[serde(default = "default_yopass_api")]
    pub yopass_api: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Signups {
    /// Anyone who messages the bot is subscribed immediately.
    Open,
    /// Requests land in `pending.json` for `partylinepagerctl approve`.
    #[default]
    Approval,
    /// New subscribers are refused.
    Closed,
}

/// How a room's join credentials reach a subscriber. In v1 this was always
/// inline; `Link` is the v2 mode described in render.rs's `creds_block` doc.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CredsMode {
    /// The onion+secret, or room URL, appear in the message body.
    #[default]
    Inline,
    /// The message carries a one-time Yopass link instead.
    Link,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Fanout {
    /// Stateless Apprise endpoint. Reachable only on the internal network.
    #[serde(default = "default_apprise_url")]
    pub apprise_url: String,

    /// Per-request timeout for one delivery.
    #[serde(with = "humantime_serde", default = "default_fanout_timeout")]
    pub timeout: Duration,

    /// How many deliveries are in flight at once.
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
}

/// Which anonymity network a party line runs on.
///
/// All three party lines are the same program over a different network. A
/// caller always gets an address and a shared secret and dials them with
/// the transport script. Only four things differ between the three, and all four are
/// on this enum: what a valid address looks like, where the client is
/// downloaded from, what to call the address in a message, and whether the
/// room takes long enough to come up that the host needs an "on it" reply.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    /// Onion routed. Slowest to come up, strongest anonymity.
    #[default]
    Tor,
    /// Garlic routed. Comes up faster than Tor and needs no open port either.
    I2p,
    /// Neither. Callers are cryptographically authenticated and the link to
    /// the relay is encrypted, but the relay sees every caller's IP address
    /// and the address is announced across the network.
    Reticulum,
}

impl Transport {
    /// Lowercase name, used in logs, in `--check`, and as the serialized form.
    pub fn name(self) -> &'static str {
        match self {
            Transport::Tor => "tor",
            Transport::I2p => "i2p",
            Transport::Reticulum => "reticulum",
        }
    }

    /// What this network calls an address, for the label beside it in a
    /// broadcast. A reader who has used one of these before recognizes the
    /// word; a reader who has not is about to follow the client link anyway.
    pub fn label(self) -> &'static str {
        match self {
            Transport::Tor => "Onion",
            Transport::I2p => "I2P address",
            Transport::Reticulum => "Relay address",
        }
    }

    /// Where a subscriber downloads the client that dials this network.
    ///
    /// Travels in every broadcast: a party line needs software most people do
    /// not have lying around, and looking it up at 1am is how a room ends up
    /// empty.
    pub fn client_url(self) -> &'static str {
        match self {
            Transport::Tor => "https://gitlab.com/MarcusHoltz/tor-party-line",
            Transport::I2p => "https://gitlab.com/MarcusHoltz/i2p-party-line",
            Transport::Reticulum => "https://gitlab.com/MarcusHoltz/reticulum-party-line",
        }
    }

    /// How long the host should expect to wait, when that is long enough to
    /// be worth saying.
    ///
    /// `None` means the room comes up fast enough that an "on it" reply would
    /// be overtaken by the broadcast it promises, and would read as a bug.
    /// Measured relay bring-up: Tor one to three minutes, I2P 23s from a cold
    /// container, Reticulum a few seconds once the image is local (but the
    /// first cold start pulls the image, which can take much longer).
    pub fn bootstrap_hint(self) -> Option<&'static str> {
        match self {
            Transport::Tor => Some("usually one to three minutes"),
            Transport::I2p => Some("usually well under a minute"),
            Transport::Reticulum => Some("usually a few seconds"),
        }
    }

    /// The suffix every address on this network ends with, if any.
    fn suffix(self) -> &'static str {
        match self {
            Transport::Tor => ".onion",
            Transport::I2p => ".b32.i2p",
            Transport::Reticulum => "",
        }
    }

    /// How many characters the address is, before any suffix.
    ///
    /// Tor v3 is base32 of a 35-byte blob, I2P is base32 of a SHA-256 digest,
    /// and Reticulum is a 16-byte truncated hash written as hex. All three are
    /// fixed width, so a wrong length is a broken hook rather than an unusual
    /// address.
    fn body_len(self) -> usize {
        match self {
            Transport::Tor => 56,
            Transport::I2p => 52,
            Transport::Reticulum => 32,
        }
    }

    /// True if `c` can appear in an address on this network.
    ///
    /// Spelled out rather than done with a regex, for the same reason the rest
    /// of this file avoids them: an explicit character set is easier to be sure
    /// about at 3am.
    fn allows(self, c: char) -> bool {
        match self {
            // Base32, RFC 4648 lowercase alphabet.
            Transport::Tor | Transport::I2p => c.is_ascii_lowercase() || ('2'..='7').contains(&c),
            // Lowercase hex. The transport script accepts uppercase and folds it, but
            // the bridge only ever emits lowercase, so anything else here means
            // the hook is misbehaving.
            Transport::Reticulum => c.is_ascii_digit() || ('a'..='f').contains(&c),
        }
    }

    /// Rejects any address that would be unsafe or useless to broadcast.
    ///
    /// The hook is admin-owned, but its output reaches every subscriber
    /// verbatim, so a backend that prints a stray log line where the address
    /// should be must fail loudly rather than mail nonsense to the roster.
    pub fn validate(self, address: &str) -> crate::Result<()> {
        let reject = |why: String| Err(crate::Error::Address(why));

        if address.is_empty() {
            return reject("hook returned an empty address".into());
        }
        if address.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return reject(format!(
                "{address:?} contains whitespace or control characters"
            ));
        }

        let body = match address.strip_suffix(self.suffix()) {
            Some(body) => body,
            None => {
                return reject(format!(
                    "{address:?} does not end in {:?}, which every {} address does",
                    self.suffix(),
                    self.name()
                ))
            }
        };

        if body.chars().count() != self.body_len() {
            return reject(format!(
                "{address:?} is {} characters long where a {} address is {}",
                body.chars().count(),
                self.name(),
                self.body_len()
            ));
        }
        if !body.chars().all(|c| self.allows(c)) {
            return reject(format!(
                "{address:?} has characters that cannot appear in a {} address",
                self.name()
            ));
        }
        Ok(())
    }
}

impl std::fmt::Display for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(self.name())
    }
}

/// Which kind of room is asked for. One verb each, see `command.rs`.
///
/// Lives here rather than in `command.rs` because `state.rs` needs it too, for
/// [`crate::state::Room`] and for a held request, and `state` importing from
/// `command` would have the layering backwards.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    /// A Tor party line, provisioned by a privileged hook.
    #[default]
    Tor,
    /// An I2P party line, provisioned by a privileged hook.
    I2p,
    /// A Reticulum party line, provisioned by a privileged hook.
    Rns,
    /// A plain WebRTC room URL. No secret: the unguessable URL is the access
    /// control.
    Web,
}

/// Every kind, in the order they appear in `help` and `--check`. The three
/// party lines first, cheapest-to-join last.
const ALL_KINDS: [ProviderKind; 4] = [
    ProviderKind::Tor,
    ProviderKind::I2p,
    ProviderKind::Rns,
    ProviderKind::Web,
];

impl ProviderKind {
    /// The verb a subscriber types for this kind. Every provider gets a
    /// three-letter code, `tor`/`rns`/`i2p`/`web`, so no one of them reads as
    /// "the" party line the way bare `partyline` used to when Tor was the
    /// only network this bot spoke.
    pub fn verb(self) -> &'static str {
        match self {
            ProviderKind::Tor => "tor",
            ProviderKind::I2p => "i2p",
            ProviderKind::Rns => "rns",
            ProviderKind::Web => "web",
        }
    }

    /// The `[provider.<name>]` section that configures it.
    pub fn section(self) -> &'static str {
        match self {
            ProviderKind::Tor => "[provider.tor]",
            ProviderKind::I2p => "[provider.i2p]",
            ProviderKind::Rns => "[provider.rns]",
            ProviderKind::Web => "[provider.web]",
        }
    }

    /// The network a room of this kind runs on, or `None` for a web room,
    /// which has no address to validate and no client to download.
    pub fn transport(self) -> Option<Transport> {
        match self {
            ProviderKind::Tor => Some(Transport::Tor),
            ProviderKind::I2p => Some(Transport::I2p),
            ProviderKind::Rns => Some(Transport::Reticulum),
            ProviderKind::Web => None,
        }
    }

    /// One line describing this verb, for `help`.
    pub fn help_line(self) -> &'static str {
        match self {
            ProviderKind::Tor => "Open Tor room",
            ProviderKind::I2p => "Open I2P room",
            ProviderKind::Rns => "Open Reticulum room",
            ProviderKind::Web => "Open video room (browser)",
        }
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `pad`, not `write_str`, so `{:<9}` in a table actually aligns.
        f.pad(self.verb())
    }
}

/// The hook pairs this instance is willing to run.
///
/// Every subsection is optional and each one is complete on its own: an
/// instance with only `[provider.i2p]` answers only `i2p`. That is the whole
/// "restrict this instance to one kind of room" switch, so there is no
/// separate enable flag to keep in sync with it.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Provider {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tor: Option<PartylineProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub i2p: Option<PartylineProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rns: Option<PartylineProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web: Option<WebProvider>,
}

/// `[provider.tor]`, `[provider.i2p]` and `[provider.rns]`: a real
/// party line on some anonymity network, provisioned by a privileged hook.
///
/// One type for all three, because the hook contract is identical. Which
/// network a section means is decided by the section name, see
/// [`ProviderKind::transport`].
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PartylineProvider {
    /// Executed to bring a room up. Reads the shared secret on **stdin** and
    /// receives `PARTYLINEPAGER_TTL_SECS` and `PARTYLINEPAGER_NOTE` in its environment.
    /// Must print one JSON object on stdout: `{"address":"..."}`.
    pub up: PathBuf,
    /// Executed at TTL to tear the room down. Receives `PARTYLINEPAGER_ROOM_ID`.
    pub down: PathBuf,
}

/// `[provider.web]`: a MiroTalk-style room URL. There is nothing to provision,
/// so the hook only pastes a slug onto [`WebProvider::base_url`], under
/// [`WebProvider::path`].
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WebProvider {
    /// Executed to mint a room URL. Receives `PARTYLINEPAGER_WEB_BASE_URL`,
    /// `PARTYLINEPAGER_WEB_PATH`, `PARTYLINEPAGER_WEB_STATIC_SLUG`, `PARTYLINEPAGER_TTL_SECS`
    /// and `PARTYLINEPAGER_NOTE`, and nothing on stdin. Must print one JSON object
    /// on stdout: `{"url":"..."}`.
    pub up: PathBuf,
    /// Executed at TTL. Receives `PARTYLINEPAGER_ROOM_ID`. Usually a no-op: a room
    /// URL is not something the partyline pager can revoke.
    pub down: PathBuf,
    /// The instance rooms are minted on: your own MiroTalk, Jitsi, or
    /// whatever else the hook targets. Point this at your own deployment to
    /// keep traffic off the public one.
    #[serde(default = "default_web_base_url")]
    pub base_url: String,
    /// Segment(s) between `base_url` and the room slug, e.g. `join` for
    /// MiroTalk's `/join/<room>` convention. May be several segments deep
    /// (`conf/rooms`), or empty for a backend that puts rooms straight under
    /// the domain, e.g. a self-hosted Jitsi's `/<room>`. Defaults to `join`,
    /// matching the historical hardcoded behavior of `provider-web.sh`.
    #[serde(default = "default_web_path")]
    pub path: String,
    /// When set, every room mints this exact slug instead of a random one,
    /// so the room URL never changes and regulars can join without waiting
    /// for a fresh link. Same access control as a random slug: whoever has
    /// the URL is in, so a static one is a standing invite to anyone who
    /// ever saw it.
    #[serde(default)]
    pub static_slug: Option<String>,
}

impl WebProvider {
    /// `base_url` without a trailing slash, which is what the hook emits and
    /// what the daemon validates the returned URL against.
    pub fn trimmed_base_url(&self) -> &str {
        self.base_url.trim_end_matches('/')
    }

    /// `path` without leading or trailing slashes, ready to be pasted between
    /// `base_url` and the slug.
    pub fn trimmed_path(&self) -> &str {
        self.path.trim_matches('/')
    }
}

impl Provider {
    pub fn supports(&self, kind: ProviderKind) -> bool {
        self.get(kind).is_some()
    }

    /// The hook pair configured for `kind`, if this instance offers it.
    ///
    /// Returns the two script paths rather than the section, so callers that
    /// only need "which scripts do I run" do not have to care that three of
    /// the four kinds share a section type and the fourth does not.
    pub fn get(&self, kind: ProviderKind) -> Option<(&PathBuf, &PathBuf)> {
        match kind {
            ProviderKind::Tor => self.tor.as_ref().map(|p| (&p.up, &p.down)),
            ProviderKind::I2p => self.i2p.as_ref().map(|p| (&p.up, &p.down)),
            ProviderKind::Rns => self.rns.as_ref().map(|p| (&p.up, &p.down)),
            ProviderKind::Web => self.web.as_ref().map(|w| (&w.up, &w.down)),
        }
    }

    /// Every configured kind, in verb order. Drives `help` and `--check`.
    pub fn configured(&self) -> Vec<ProviderKind> {
        ALL_KINDS
            .into_iter()
            .filter(|k| self.supports(*k))
            .collect()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Tier {
    pub name: String,

    /// May this tier open a room at all?
    #[serde(default = "yes")]
    pub may_open: bool,

    /// May this tier receive broadcasts? Setting this false with `may_open`
    /// true gives you a write-only role.
    #[serde(default = "yes")]
    pub may_receive: bool,

    /// Rolling quota window. `0s` means unlimited, which is the admin and
    /// journalist case.
    #[serde(with = "humantime_serde", default)]
    pub window: Duration,

    /// How many rooms this tier gets inside one `window` before it is
    /// exhausted. Ignored when `window` is `0s`. Must be at least 1.
    #[serde(default = "one_room")]
    pub max_rooms: u32,

    /// Park requests from this tier in `pending.json` until an admin releases
    /// them over SSH.
    #[serde(default)]
    pub hold_for_approval: bool,

    /// May this tier's host tear down a room they opened, before it expires
    /// on its own?
    #[serde(default = "yes")]
    pub may_close: bool,
}

fn yes() -> bool {
    true
}
fn one_room() -> u32 {
    1
}
fn default_name() -> String {
    "PartylinePager".to_string()
}
fn default_room_ttl() -> Duration {
    Duration::from_secs(2 * 60 * 60)
}
fn default_hook_timeout() -> Duration {
    Duration::from_secs(300)
}
fn default_apprise_url() -> String {
    "http://apprise:8000/notify".to_string()
}
fn default_yopass_url() -> String {
    "https://share.yopass.se".to_string()
}
fn default_yopass_api() -> String {
    "https://api2.yopass.se".to_string()
}
fn default_fanout_timeout() -> Duration {
    Duration::from_secs(30)
}
fn default_concurrency() -> usize {
    8
}
fn default_web_base_url() -> String {
    "https://p2p.mirotalk.com".to_string()
}
fn default_web_path() -> String {
    "join".to_string()
}

impl Default for Fanout {
    fn default() -> Self {
        Self {
            apprise_url: default_apprise_url(),
            timeout: default_fanout_timeout(),
            concurrency: default_concurrency(),
        }
    }
}

impl Policy {
    /// Reads and validates `policy.toml`.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
        let policy: Policy = toml::from_str(&raw).map_err(|source| Error::Toml {
            path: path.to_path_buf(),
            source,
        })?;
        policy.validate()?;
        Ok(policy)
    }

    pub fn parse_str(raw: &str) -> Result<Self> {
        let policy: Policy = toml::from_str(raw).map_err(|source| Error::Toml {
            path: PathBuf::from("<memory>"),
            source,
        })?;
        policy.validate()?;
        Ok(policy)
    }

    pub fn tier(&self, name: &str) -> Option<&Tier> {
        self.tiers.iter().find(|t| t.name == name)
    }

    /// The tier a subscriber is treated as when their recorded tier has been
    /// deleted from policy. Falling back to the default tier rather than
    /// erroring keeps a typo in `policy.toml` from silently granting rights.
    pub fn tier_or_default(&self, name: &str) -> &Tier {
        self.tier(name)
            .or_else(|| self.tier(&self.instance.default_tier))
            .expect("validate() guarantees the default tier exists")
    }

    /// Rejects configurations that would behave surprisingly at runtime.
    pub fn validate(&self) -> Result<()> {
        if self.tiers.is_empty() {
            return Err(Error::Policy("no [[tier]] tables defined".into()));
        }

        let mut seen = BTreeSet::new();
        for tier in &self.tiers {
            if tier.name.trim().is_empty() {
                return Err(Error::Policy("a tier has an empty name".into()));
            }
            if !seen.insert(tier.name.as_str()) {
                return Err(Error::Policy(format!("duplicate tier {:?}", tier.name)));
            }
            if tier.max_rooms == 0 {
                return Err(Error::Policy(format!(
                    "tier {:?} has max_rooms = 0; must be at least 1",
                    tier.name
                )));
            }
        }

        if self.tier(&self.instance.default_tier).is_none() {
            return Err(Error::Policy(format!(
                "default_tier {:?} is not one of {:?}",
                self.instance.default_tier,
                seen.iter().collect::<Vec<_>>()
            )));
        }

        if self.instance.room_ttl.is_zero() {
            return Err(Error::Policy("room_ttl must be greater than zero".into()));
        }
        if self.instance.hook_timeout.is_zero() {
            return Err(Error::Policy("hook_timeout must be greater than zero".into()));
        }
        if self.fanout.concurrency == 0 {
            return Err(Error::Policy("fanout.concurrency must be at least 1".into()));
        }

        // No provider means no way to open anything, which is a mistake rather
        // than a mode. Name the migration in the message: the old shape was a
        // flat `[provider] up/down`, and TOML happily ignores those keys now,
        // so an admin upgrading would otherwise see "no provider" while
        // looking straight at a `[provider]` table.
        if self.provider.configured().is_empty() {
            return Err(Error::Policy(
                "no [provider.*] section, so nothing could ever be opened. Add at least one of \
                 [provider.tor], [provider.i2p], [provider.rns] or [provider.web]. \
                 If you are upgrading, the old flat [provider] up/down table is now \
                 [provider.tor] up/down; see README, \"Upgrading\"."
                    .into(),
            ));
        }

        if let Some(web) = &self.provider.web {
            let url = web.base_url.trim();
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(Error::Policy(format!(
                    "provider.web.base_url {:?} must start with http:// or https://",
                    web.base_url
                )));
            }
            // The hook builds the room URL by concatenation, so anything after
            // the path would end up in the wrong half of the address.
            if url.contains('?') || url.contains('#') {
                return Err(Error::Policy(format!(
                    "provider.web.base_url {:?} must not contain a query string or fragment",
                    web.base_url
                )));
            }
            if url.len() > 512 {
                return Err(Error::Policy(
                    "provider.web.base_url is implausibly long".into(),
                ));
            }
            if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(Error::Policy(
                    "provider.web.base_url contains whitespace or control characters".into(),
                ));
            }

            // `path` is pasted between base_url and the slug verbatim, and
            // `static_slug` (when set) becomes the whole slug, so both get the
            // same treatment as a room URL's path in provider.rs's
            // WebOutput::validate: an explicit character set, no `/` for the
            // slug since it names one room rather than a directory.
            let path = web.trimmed_path();
            if !path.is_empty() {
                if path.len() > 200 {
                    return Err(Error::Policy(
                        "provider.web.path is implausibly long".into(),
                    ));
                }
                if !path
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '/'))
                {
                    return Err(Error::Policy(format!(
                        "provider.web.path {:?} has characters outside [A-Za-z0-9_/-]",
                        web.path
                    )));
                }
            }
            if let Some(slug) = &web.static_slug {
                let slug = slug.trim();
                if slug.is_empty() {
                    return Err(Error::Policy(
                        "provider.web.static_slug is set but empty; omit the key for a random \
                         slug instead"
                            .into(),
                    ));
                }
                if slug.len() > 200 {
                    return Err(Error::Policy(
                        "provider.web.static_slug is implausibly long".into(),
                    ));
                }
                if !slug
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
                {
                    return Err(Error::Policy(format!(
                        "provider.web.static_slug {slug:?} has characters outside [A-Za-z0-9_-]"
                    )));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
        [instance]
        default_tier = "weekly"

        [provider.tor]
        up = "/hooks/up.sh"
        down = "/hooks/down.sh"

        [[tier]]
        name = "weekly"
        window = "168h"
    "#;

    #[test]
    fn minimal_policy_loads_with_defaults() {
        let p = Policy::parse_str(MINIMAL).unwrap();
        assert_eq!(p.instance.name, "PartylinePager");
        assert_eq!(p.instance.signups, Signups::Approval);
        assert_eq!(p.instance.creds_delivery, CredsMode::Inline);
        assert_eq!(p.instance.room_ttl, Duration::from_secs(7200));
        assert_eq!(p.instance.hook_timeout, Duration::from_secs(300));
        assert_eq!(p.fanout.apprise_url, "http://apprise:8000/notify");
        assert_eq!(p.fanout.concurrency, 8);
        assert!(!p.instance.paused);
    }

    #[test]
    fn tier_defaults_are_permissive_but_quota_bound() {
        let p = Policy::parse_str(MINIMAL).unwrap();
        let t = p.tier("weekly").unwrap();
        assert!(t.may_open);
        assert!(t.may_receive);
        assert!(!t.hold_for_approval);
        assert_eq!(t.window, Duration::from_secs(168 * 3600));
    }

    #[test]
    fn durations_are_human_readable() {
        let p = Policy::parse_str(
            r#"
            [instance]
            default_tier = "t"
            room_ttl = "90m"
            hook_timeout = "45s"
            [provider.tor]
            up = "u"
            down = "d"
            [[tier]]
            name = "t"
            window = "0s"
            "#,
        )
        .unwrap();
        assert_eq!(p.instance.room_ttl, Duration::from_secs(5400));
        assert_eq!(p.instance.hook_timeout, Duration::from_secs(45));
        assert!(p.tier("t").unwrap().window.is_zero(), "0s means unlimited");
    }

    #[test]
    fn signups_modes_parse() {
        for (raw, want) in [
            ("open", Signups::Open),
            ("approval", Signups::Approval),
            ("closed", Signups::Closed),
        ] {
            let toml = MINIMAL.replace(
                "[instance]",
                &format!("[instance]\nsignups = \"{raw}\""),
            );
            assert_eq!(Policy::parse_str(&toml).unwrap().instance.signups, want);
        }
    }

    #[test]
    fn creds_delivery_modes_parse() {
        for (raw, want) in [("inline", CredsMode::Inline), ("link", CredsMode::Link)] {
            let toml = MINIMAL.replace(
                "[instance]",
                &format!("[instance]\ncreds_delivery = \"{raw}\""),
            );
            assert_eq!(
                Policy::parse_str(&toml).unwrap().instance.creds_delivery,
                want
            );
        }
    }

    #[test]
    fn rejects_unknown_default_tier() {
        let toml = MINIMAL.replace("default_tier = \"weekly\"", "default_tier = \"ghost\"");
        let err = Policy::parse_str(&toml).unwrap_err().to_string();
        assert!(err.contains("ghost"), "{err}");
    }

    #[test]
    fn rejects_duplicate_tiers() {
        let toml = format!("{MINIMAL}\n[[tier]]\nname = \"weekly\"\n");
        assert!(Policy::parse_str(&toml).is_err());
    }

    #[test]
    fn rejects_empty_tier_list_and_zero_ttl() {
        assert!(Policy::parse_str(
            r#"
            [instance]
            default_tier = "x"
            [provider.tor]
            up = "u"
            down = "d"
            "#
        )
        .is_err());

        let toml = MINIMAL.replace("[instance]", "[instance]\nroom_ttl = \"0s\"");
        assert!(Policy::parse_str(&toml).is_err());
    }

    #[test]
    fn unknown_tier_falls_back_to_default_rather_than_panicking() {
        let p = Policy::parse_str(MINIMAL).unwrap();
        assert_eq!(p.tier_or_default("deleted-tier").name, "weekly");
    }

    #[test]
    fn a_partyline_only_instance_supports_only_partyline() {
        let p = Policy::parse_str(MINIMAL).unwrap();
        assert!(p.provider.supports(ProviderKind::Tor));
        assert!(!p.provider.supports(ProviderKind::Web));
        assert_eq!(p.provider.configured(), vec![ProviderKind::Tor]);
    }

    #[test]
    fn a_web_only_instance_needs_no_partyline_hooks() {
        let p = Policy::parse_str(
            r#"
            [instance]
            default_tier = "weekly"
            [provider.web]
            up = "/hooks/provider-web.sh"
            down = "/hooks/teardown-web.sh"
            [[tier]]
            name = "weekly"
            "#,
        )
        .unwrap();
        assert!(!p.provider.supports(ProviderKind::Tor));
        assert!(p.provider.supports(ProviderKind::Web));
        assert_eq!(
            p.provider.web.as_ref().unwrap().base_url,
            "https://p2p.mirotalk.com",
            "the public instance is the default"
        );
    }

    #[test]
    fn both_providers_can_be_configured_together() {
        let toml = format!(
            "{MINIMAL}\n[provider.web]\nup = \"u\"\ndown = \"d\"\nbase_url = \"https://meet.example.org/\"\n"
        );
        let p = Policy::parse_str(&toml).unwrap();
        assert_eq!(
            p.provider.configured(),
            vec![ProviderKind::Tor, ProviderKind::Web]
        );
        assert_eq!(
            p.provider.web.as_ref().unwrap().trimmed_base_url(),
            "https://meet.example.org",
            "a trailing slash must not reach the hook or the validator"
        );
    }

    #[test]
    fn a_policy_with_no_provider_section_is_refused_and_names_the_migration() {
        let toml = r#"
            [instance]
            default_tier = "weekly"
            [[tier]]
            name = "weekly"
        "#;
        let err = Policy::parse_str(toml).unwrap_err().to_string();
        assert!(err.contains("[provider.tor]"), "{err}");
    }

    #[test]
    fn the_old_flat_provider_table_is_refused_with_a_migration_hint() {
        // TOML ignores the now-unknown up/down keys, so without this the admin
        // would be told "no provider" while looking at a [provider] table.
        let toml = r#"
            [instance]
            default_tier = "weekly"
            [provider]
            up = "/hooks/up.sh"
            down = "/hooks/down.sh"
            [[tier]]
            name = "weekly"
        "#;
        let err = Policy::parse_str(toml).unwrap_err().to_string();
        assert!(err.contains("[provider.tor]"), "{err}");
        assert!(err.contains("Upgrading"), "{err}");
    }

    #[test]
    fn a_bogus_web_base_url_is_refused_at_startup() {
        for bad in [
            "p2p.mirotalk.com",
            "ftp://p2p.mirotalk.com",
            "https://meet.example.org/?room=",
            "https://meet example.org",
        ] {
            let toml = format!(
                "{MINIMAL}\n[provider.web]\nup = \"u\"\ndown = \"d\"\nbase_url = \"{bad}\"\n"
            );
            assert!(
                Policy::parse_str(&toml).is_err(),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn web_path_defaults_to_join_and_trims_slashes() {
        let toml =
            format!("{MINIMAL}\n[provider.web]\nup = \"u\"\ndown = \"d\"\n");
        let p = Policy::parse_str(&toml).unwrap();
        let web = p.provider.web.as_ref().unwrap();
        assert_eq!(web.path, "join", "matches the historical hardcoded hook path");
        assert_eq!(web.trimmed_path(), "join");
        assert!(web.static_slug.is_none());
    }

    #[test]
    fn web_path_can_be_multi_segment_or_empty() {
        let toml = format!(
            "{MINIMAL}\n[provider.web]\nup = \"u\"\ndown = \"d\"\npath = \"/conf/rooms/\"\n"
        );
        let p = Policy::parse_str(&toml).unwrap();
        assert_eq!(
            p.provider.web.as_ref().unwrap().trimmed_path(),
            "conf/rooms"
        );

        let toml =
            format!("{MINIMAL}\n[provider.web]\nup = \"u\"\ndown = \"d\"\npath = \"\"\n");
        let p = Policy::parse_str(&toml).unwrap();
        assert_eq!(p.provider.web.as_ref().unwrap().trimmed_path(), "");
    }

    #[test]
    fn a_bogus_web_path_is_refused_at_startup() {
        for bad in ["join room", "join/<room>", "join?x=1"] {
            let toml = format!(
                "{MINIMAL}\n[provider.web]\nup = \"u\"\ndown = \"d\"\npath = \"{bad}\"\n"
            );
            assert!(
                Policy::parse_str(&toml).is_err(),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn a_static_slug_survives_a_round_trip() {
        let toml = format!(
            "{MINIMAL}\n[provider.web]\nup = \"u\"\ndown = \"d\"\nstatic_slug = \"movie-night\"\n"
        );
        let p = Policy::parse_str(&toml).unwrap();
        assert_eq!(
            p.provider.web.as_ref().unwrap().static_slug.as_deref(),
            Some("movie-night")
        );
    }

    #[test]
    fn a_bogus_static_slug_is_refused_at_startup() {
        for bad in ["", "  ", "has space", "has/slash", "a?b=c"] {
            let toml = format!(
                "{MINIMAL}\n[provider.web]\nup = \"u\"\ndown = \"d\"\nstatic_slug = \"{bad}\"\n"
            );
            assert!(
                Policy::parse_str(&toml).is_err(),
                "{bad:?} should be refused"
            );
        }
    }

    /// One real-shaped address per network, used as the base for the
    /// mangled variants below.
    const TOR_ADDRESS: &str = "batcave7xyzabcdefghijklmnopqrstuvwxyz234567abcdefghijklm.onion";
    const I2P_ADDRESS: &str = "jhqkryidp2cyfcilabcdefghijklmnopqrstuvwxyz234567abcd.b32.i2p";
    const RETICULUM_ADDRESS: &str = "3a1c9d4e07b21f88c2a04e7d612b0f4e";

    #[test]
    fn each_network_accepts_its_own_address() {
        assert!(Transport::Tor.validate(TOR_ADDRESS).is_ok());
        assert!(Transport::I2p.validate(I2P_ADDRESS).is_ok());
        assert!(Transport::Reticulum.validate(RETICULUM_ADDRESS).is_ok());
    }

    #[test]
    fn no_network_accepts_another_networks_address() {
        // The suffix alone separates Tor from I2P, and the whole shape
        // separates both from Reticulum. A hook wired to the wrong compose
        // file fails loudly here rather than mailing the roster an address
        // their client cannot dial.
        for (transport, wrong) in [
            (Transport::Tor, I2P_ADDRESS),
            (Transport::Tor, RETICULUM_ADDRESS),
            (Transport::I2p, TOR_ADDRESS),
            (Transport::I2p, RETICULUM_ADDRESS),
            (Transport::Reticulum, TOR_ADDRESS),
            (Transport::Reticulum, I2P_ADDRESS),
        ] {
            assert!(
                transport.validate(wrong).is_err(),
                "{transport} must not accept {wrong:?}"
            );
        }
    }

    #[test]
    fn junk_where_an_address_should_be_is_refused() {
        for (transport, bad) in [
            (Transport::Tor, ""),
            (Transport::Tor, ".onion"),
            (Transport::Tor, "example.com"),
            // Right suffix, wrong length: a truncated read of the hostname file.
            (Transport::Tor, "batcave7xyz.onion"),
            // Right length, uppercase: not the base32 alphabet Tor publishes.
            (Transport::Tor, "BATCAVE7XYZABCDEFGHIJKLMNOPQRSTUVWXYZ234567ABCDEFGHIJKLM.onion"),
            // Right length, but 0/1/8/9 are outside base32.
            (Transport::Tor, "batcave7xyz0189fghijklmnopqrstuvwxyz234567abcdefghijklmn.onion"),
            (Transport::I2p, ""),
            (Transport::I2p, ".b32.i2p"),
            (Transport::I2p, "jhqkryidp2cyfcil.b32.i2p"),
            (Transport::Reticulum, ""),
            (Transport::Reticulum, "3a1c9d4e"),
            // Uppercase hex: the transport script folds it on input, but the bridge
            // only ever emits lowercase, so this means a broken hook.
            (Transport::Reticulum, "3A1C9D4E07B21F88C2A04E7D612B0F4E"),
            // Hex has no letters past f.
            (Transport::Reticulum, "3a1c9d4e07b21f88c2a04e7d612b0fzz"),
        ] {
            assert!(
                transport.validate(bad).is_err(),
                "{transport} must refuse {bad:?}"
            );
        }
    }

    #[test]
    fn an_address_with_whitespace_or_control_characters_is_refused() {
        // A hook that prints a log line where the address should be, or one
        // whose output picked up a stray newline, must fail rather than send
        // something that breaks the message it lands in.
        for transport in [Transport::Tor, Transport::I2p, Transport::Reticulum] {
            for bad in ["has space", "two\nlines", "bell\u{7}"] {
                assert!(
                    transport.validate(bad).is_err(),
                    "{transport} must refuse {bad:?}"
                );
            }
        }
    }

    #[test]
    fn every_verb_maps_back_to_the_network_it_names() {
        assert_eq!(ProviderKind::Tor.transport(), Some(Transport::Tor));
        assert_eq!(ProviderKind::I2p.transport(), Some(Transport::I2p));
        assert_eq!(
            ProviderKind::Rns.transport(),
            Some(Transport::Reticulum)
        );
        // A web room has no address to validate and no client to download.
        assert_eq!(ProviderKind::Web.transport(), None);
    }

    #[test]
    fn only_the_configured_providers_are_offered() {
        let policy = Policy::parse_str(
            r#"
            [instance]
            default_tier = "t"
            [provider.i2p]
            up = "/hooks/up.sh"
            down = "/hooks/down.sh"
            [provider.web]
            up = "/hooks/web-up.sh"
            down = "/hooks/web-down.sh"
            [[tier]]
            name = "t"
            "#,
        )
        .unwrap();
        assert_eq!(
            policy.provider.configured(),
            vec![ProviderKind::I2p, ProviderKind::Web]
        );
        assert!(!policy.provider.supports(ProviderKind::Tor));
        assert!(!policy.provider.supports(ProviderKind::Rns));
    }
}
