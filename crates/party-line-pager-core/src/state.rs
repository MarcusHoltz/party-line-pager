//! Mutable runtime state, stored as JSON files in the state directory.
//!
//! There is no database. Both the daemon and `party_line_pagerctl` read and write these
//! files, so every mutation happens under an exclusive `flock` on `state/.lock`
//! and every write is a write-to-temp plus `rename`, which is atomic on POSIX.
//! A reader therefore sees either the old file or the new one, never a partial
//! one, and `jq` works on all of it.
//!
//! Files:
//! - `subscribers.json`: the roster, including pending and banned entries
//! - `pending.json`: requests held for admin approval
//! - `room.json`: the currently live room, absent when nothing is live
//! - `runtime.json`: the pause switch

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::config::{ProviderKind, Transport};
use crate::endpoint::EndpointId;
use crate::error::{Error, Result};
use crate::quiet::{self, QuietWindow};

const SUBSCRIBERS: &str = "subscribers.json";
const PENDING: &str = "pending.json";
const ROOM: &str = "room.json";
const RUNTIME: &str = "runtime.json";
const LOCK: &str = ".lock";

/// Where a subscriber sits with respect to the roster.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SubscriberStatus {
    /// Asked to join while `signups = "approval"`. Receives nothing.
    #[default]
    Pending,
    /// On the roster.
    Active,
    /// Refused. Kept rather than deleted so a ban survives a re-subscribe.
    Banned,
}

/// How a broadcast reaches one subscriber.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// Hand the message back to the adapter the subscriber arrived on. It
    /// already holds an authenticated session, and it works for transports
    /// Apprise cannot reach (IRC has no Apprise plugin at all).
    Native { transport: String, address: String },
    /// POST to the Apprise sidecar with this target URL. Used by endpoints an
    /// admin adds by hand, which is how the other 130+ services are reached
    /// without writing an adapter for each.
    Apprise { url: String },
}

/// Endpoints on this pseudo-transport carry an Apprise URL as their address,
/// for example `apprise:ntfy://ntfy.sh/party-line-pager`.
pub const APPRISE_TRANSPORT: &str = "apprise";

impl Delivery {
    /// Delivery path for any endpoint, subscribed or not. Used for replies to
    /// strangers (`help` works before you subscribe) as well as for broadcasts.
    pub fn for_endpoint(endpoint: &EndpointId) -> Self {
        if endpoint.transport() == APPRISE_TRANSPORT {
            Delivery::Apprise {
                url: endpoint.address().to_string(),
            }
        } else {
            Delivery::Native {
                transport: endpoint.transport().to_string(),
                address: endpoint.address().to_string(),
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Subscriber {
    pub endpoint: EndpointId,

    pub tier: String,

    /// Admin-assigned display name. Subscribers cannot set their own; this is
    /// purely a label so `party_line_pagerctl`/`party-line-pager.sh` and
    /// broadcasts read as "Doug opened the room" instead of the raw endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    #[serde(default)]
    pub status: SubscriberStatus,

    /// IANA timezone name. Quiet hours mean nothing without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tz: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet: Option<QuietWindow>,

    /// Timestamps of this endpoint's opened rooms that may still count
    /// against their tier's rolling window. Drives the quota. Trimmed to the
    /// window on every write, so it never grows without bound.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_log: Vec<DateTime<Utc>>,

    pub created_at: DateTime<Utc>,
}

impl Subscriber {
    pub fn new(endpoint: EndpointId, tier: impl Into<String>) -> Self {
        Self {
            endpoint,
            tier: tier.into(),
            name: None,
            status: SubscriberStatus::Pending,
            tz: None,
            quiet: None,
            open_log: Vec::new(),
            created_at: Utc::now(),
        }
    }

    /// Where a broadcast for this subscriber should be sent.
    ///
    /// Derived from the endpoint rather than stored, so it can never drift out
    /// of sync with the address it describes.
    pub fn delivery(&self) -> Delivery {
        Delivery::for_endpoint(&self.endpoint)
    }

    /// True when this subscriber is inside their quiet window right now.
    ///
    /// A window without a timezone silences nothing: we refuse to guess an
    /// offset and wake somebody at 3am because of it.
    pub fn is_quiet_at(&self, now: DateTime<Utc>) -> bool {
        let (Some(window), Some(tz_name)) = (self.quiet, self.tz.as_deref()) else {
            return false;
        };
        match quiet::parse_timezone(tz_name) {
            Ok(tz) => window.is_quiet_at(tz, now),
            Err(_) => false,
        }
    }
}

/// The roster, keyed by endpoint so an address can appear only once.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(transparent)]
pub struct Subscribers(pub BTreeMap<EndpointId, Subscriber>);

impl Subscribers {
    pub fn get(&self, id: &EndpointId) -> Option<&Subscriber> {
        self.0.get(id)
    }

    pub fn get_mut(&mut self, id: &EndpointId) -> Option<&mut Subscriber> {
        self.0.get_mut(id)
    }

    pub fn insert(&mut self, s: Subscriber) {
        self.0.insert(s.endpoint.clone(), s);
    }

    pub fn remove(&mut self, id: &EndpointId) -> Option<Subscriber> {
        self.0.remove(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Subscriber> {
        self.0.values()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A request parked for admin approval.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PendingRequest {
    pub id: String,
    pub endpoint: EndpointId,

    /// Which verb was used. Defaulted rather than required so a `pending.json`
    /// written before the `web` provider existed still parses, as a party
    /// line, which is what it was.
    #[serde(default)]
    pub kind: ProviderKind,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub requested_at: DateTime<Utc>,

    /// Flipped by `party_line_pagerctl approve-request`. The daemon polls for this
    /// rather than exposing a socket, so approval is a file edit and nothing
    /// on the network can trigger it.
    #[serde(default)]
    pub approved: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(transparent)]
pub struct Pending(pub Vec<PendingRequest>);

/// What kind of room is up, and the fields only that kind has.
///
/// Internally tagged, so `room.json` stays one flat object that `jq` reads the
/// same way it always did, and so an impossible combination (an onion *and* a
/// URL) cannot be represented at all.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "provider", rename_all = "lowercase")]
pub enum RoomKind {
    /// A party line: which network it is on, the address, and the secret
    /// needed to be let in.
    ///
    /// One variant for all three networks, because a room is the same thing on
    /// each of them. The port a hook may know about is deliberately not here:
    /// the transport script only ever accepts a bare address on all three, and a
    /// pasted "address:port" breaks its normalization instead of being
    /// ignored.
    Partyline {
        transport: Transport,
        address: String,
        secret: String,
    },
    /// A WebRTC room. No secret exists: whoever has the URL is in.
    Web { url: String },
}

/// The live room. Deleted at teardown, so its absence means nothing is up.
///
/// Note for anyone reading an old `room.json`: files written before the `i2p`
/// and `reticulum` providers existed name the address `"onion"` and carry no
/// `"transport"` key, and files older still have no `"provider"` key. Neither
/// parses. The daemon clears one of those at startup rather than refusing to
/// boot, see `Engine::recover`, so the only cost is that a room live across
/// that one upgrade is forgotten.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Room {
    #[serde(flatten)]
    pub kind: RoomKind,
    pub opened_by: EndpointId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub started_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl Room {
    pub fn is_live_at(&self, now: DateTime<Utc>) -> bool {
        now < self.expires_at
    }

    /// The string the teardown hook is handed to identify this room, and the
    /// value a stale teardown timer compares itself against. Unique per room
    /// for both kinds: a fresh onion, or a fresh random slug.
    pub fn id(&self) -> &str {
        match &self.kind {
            RoomKind::Partyline { address, .. } => address,
            RoomKind::Web { url } => url,
        }
    }

    pub fn provider_kind(&self) -> ProviderKind {
        match &self.kind {
            RoomKind::Partyline { transport, .. } => match transport {
                Transport::Tor => ProviderKind::Tor,
                Transport::I2p => ProviderKind::I2p,
                Transport::Reticulum => ProviderKind::Rns,
            },
            RoomKind::Web { .. } => ProviderKind::Web,
        }
    }
}

/// Switches an admin can flip without editing `policy.toml`.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Runtime {
    #[serde(default)]
    pub paused: bool,

    /// Set by `party_line_pagerctl close`. The daemon notices on its next poll, tears
    /// the room down, and clears the flag. A file rather than a socket, because
    /// the daemon must not listen on anything an attacker could reach.
    #[serde(default)]
    pub close_requested: bool,
}

/// Handle on the state directory.
#[derive(Clone, Debug)]
pub struct Store {
    dir: PathBuf,
}

/// Held for the duration of a read-modify-write. Releases on drop.
#[derive(Debug)]
pub struct StoreGuard {
    _file: File,
}

impl Store {
    /// Opens (creating if needed) the state directory.
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Takes the exclusive advisory lock guarding every file in the directory.
    ///
    /// `party_line_pagerctl` and the daemon both call this before a read-modify-write,
    /// so an admin approving a subscriber cannot lose a `open_log` entry
    /// written by a fanout happening at the same moment.
    pub fn lock(&self) -> Result<StoreGuard> {
        let path = self.dir.join(LOCK);
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| Error::io(&path, e))?;
        hand_to_host_user(&path);
        file.lock().map_err(|e| Error::io(&path, e))?;
        Ok(StoreGuard { _file: file })
    }

    pub fn subscribers(&self) -> Result<Subscribers> {
        self.load(SUBSCRIBERS)
    }

    pub fn save_subscribers(&self, subs: &Subscribers) -> Result<()> {
        self.save(SUBSCRIBERS, subs)
    }

    pub fn pending(&self) -> Result<Pending> {
        self.load(PENDING)
    }

    pub fn save_pending(&self, pending: &Pending) -> Result<()> {
        self.save(PENDING, pending)
    }

    pub fn runtime(&self) -> Result<Runtime> {
        self.load(RUNTIME)
    }

    pub fn save_runtime(&self, runtime: &Runtime) -> Result<()> {
        self.save(RUNTIME, runtime)
    }

    /// The live room, if any. A room whose file exists but whose TTL has passed
    /// is reported as absent: the file is a cache of a fact, not the fact.
    pub fn room(&self) -> Result<Option<Room>> {
        let path = self.dir.join(ROOM);
        if !path.exists() {
            return Ok(None);
        }
        let raw = fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
        if raw.trim().is_empty() {
            return Ok(None);
        }
        serde_json::from_str(&raw)
            .map(Some)
            .map_err(|source| Error::Json { path, source })
    }

    pub fn save_room(&self, room: &Room) -> Result<()> {
        self.save(ROOM, room)
    }

    /// True when `room.json` exists but cannot be read as a [`Room`].
    ///
    /// Only the daemon's startup recovery uses this, to clear a file left by an
    /// older version rather than refuse to boot forever. Everything else keeps
    /// treating unreadable state as an error: a corrupt roster must never be
    /// silently reset.
    pub fn room_is_unreadable(&self) -> bool {
        self.dir.join(ROOM).exists() && self.room().is_err()
    }

    /// Removes the room file. Idempotent, because teardown may race a restart.
    pub fn clear_room(&self) -> Result<()> {
        let path = self.dir.join(ROOM);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::io(&path, e)),
        }
    }

    fn load<T: DeserializeOwned + Default>(&self, name: &str) -> Result<T> {
        let path = self.dir.join(name);
        match fs::read_to_string(&path) {
            Ok(raw) if raw.trim().is_empty() => Ok(T::default()),
            Ok(raw) => serde_json::from_str(&raw).map_err(|source| Error::Json { path, source }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
            Err(e) => Err(Error::io(&path, e)),
        }
    }

    /// Write-to-temp, fsync, rename. The temp file lives in the same directory
    /// so the rename cannot cross a filesystem boundary.
    fn save<T: Serialize>(&self, name: &str, value: &T) -> Result<()> {
        let final_path = self.dir.join(name);
        let tmp_path = self.dir.join(format!(".{name}.tmp"));

        let json = serde_json::to_vec_pretty(value).map_err(|source| Error::Json {
            path: final_path.clone(),
            source,
        })?;

        {
            let mut tmp = File::create(&tmp_path).map_err(|e| Error::io(&tmp_path, e))?;
            tmp.write_all(&json).map_err(|e| Error::io(&tmp_path, e))?;
            tmp.write_all(b"\n").map_err(|e| Error::io(&tmp_path, e))?;
            tmp.sync_all().map_err(|e| Error::io(&tmp_path, e))?;
        }

        hand_to_host_user(&tmp_path);
        fs::rename(&tmp_path, &final_path).map_err(|e| Error::io(&final_path, e))?;
        Ok(())
    }
}

/// Hands a state file to the host user, when this process happens to be root.
///
/// The full image runs the daemon as root, and has to: the tor, i2p and rns
/// relay hooks chown their own state to a service user and setuid into it, so a
/// daemon without privileges cannot bring up three of the four room types. But
/// the state directory is a bind mount of the host's `./config/state`, so a
/// root-owned file in here is one the host user cannot read with their own
/// tools, and the modular image, whose daemon runs as `HOST_UID`, cannot open
/// at all.
///
/// Doing it once at startup would not help, because every write is
/// write-to-temp plus rename: the inode is new each time, so the ownership is
/// root again on the next save. It is applied to the temp file before the
/// rename, so the final path never exists root-owned.
///
/// No-op unless this process is root, which covers the host and the modular
/// image, and no-op when `HOST_UID` is unset or zero, which is what an operator
/// who genuinely wants root-owned state gets.
#[cfg(unix)]
fn hand_to_host_user(path: &Path) {
    use std::os::unix::fs::chown;

    let Some(uid) = std::env::var("HOST_UID")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
    else {
        return;
    };
    if uid == 0 {
        return;
    }
    let gid = std::env::var("HOST_GID")
        .ok()
        .and_then(|g| g.parse::<u32>().ok())
        .unwrap_or(uid);
    // Best effort, and no check on whether this process is root, because the
    // kernel already draws that line: a process that is not root gets EPERM
    // here unless the file is already its own, which is the modular image and
    // the normal host, so this costs them one failed syscall per write. The data
    // is written and renamed by the time we get here, so a refusal is not a
    // reason to fail a save that already worked.
    let _ = chown(path, Some(uid), Some(gid));
}

#[cfg(not(unix))]
fn hand_to_host_user(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    fn subscriber(id: &str) -> Subscriber {
        Subscriber::new(id.parse::<EndpointId>().unwrap(), "weekly")
    }

    #[test]
    fn delivery_is_derived_from_the_endpoint() {
        assert_eq!(
            subscriber("telegram:12345").delivery(),
            Delivery::Native {
                transport: "telegram".into(),
                address: "12345".into()
            }
        );
        assert_eq!(
            subscriber("apprise:ntfy://ntfy.sh/party-line-pager").delivery(),
            Delivery::Apprise {
                url: "ntfy://ntfy.sh/party-line-pager".into()
            }
        );
    }

    #[test]
    fn missing_files_read_as_empty() {
        let (_d, store) = store();
        assert!(store.subscribers().unwrap().is_empty());
        assert!(store.pending().unwrap().0.is_empty());
        assert!(store.room().unwrap().is_none());
        assert!(!store.runtime().unwrap().paused);
    }

    #[test]
    fn subscribers_round_trip() {
        let (_d, store) = store();
        let mut subs = Subscribers::default();
        let mut s = subscriber("telegram:1");
        s.status = SubscriberStatus::Active;
        s.tz = Some("America/Denver".into());
        s.quiet = Some("23:00-07:00".parse().unwrap());
        subs.insert(s.clone());

        store.save_subscribers(&subs).unwrap();
        let loaded = store.subscribers().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.get(&s.endpoint).unwrap(), &s);
    }

    #[test]
    fn state_files_are_human_readable_json() {
        let (dir, store) = store();
        let mut subs = Subscribers::default();
        subs.insert(subscriber("irc:nick"));
        store.save_subscribers(&subs).unwrap();

        let raw = fs::read_to_string(dir.path().join("subscribers.json")).unwrap();
        assert!(raw.contains("\"irc:nick\""), "{raw}");
        assert!(raw.contains('\n'), "pretty printed for jq and humans");
        assert!(raw.ends_with('\n'));
    }

    #[test]
    fn saving_leaves_no_temp_files_behind() {
        let (dir, store) = store();
        store.save_subscribers(&Subscribers::default()).unwrap();
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    fn partyline_room() -> Room {
        Room {
            kind: RoomKind::Partyline {
                transport: Transport::Tor,
                address: "abc.onion".into(),
                secret: "SECRET".into(),
            },
            opened_by: "telegram:1".parse().unwrap(),
            note: Some("poker".into()),
            started_at: Utc.with_ymd_and_hms(2026, 8, 11, 0, 0, 0).unwrap(),
            expires_at: Utc.with_ymd_and_hms(2026, 8, 11, 2, 0, 0).unwrap(),
        }
    }

    #[test]
    fn room_lifecycle() {
        let (_d, store) = store();
        let room = partyline_room();
        store.save_room(&room).unwrap();
        assert_eq!(store.room().unwrap().unwrap(), room);

        assert!(room.is_live_at(Utc.with_ymd_and_hms(2026, 8, 11, 1, 0, 0).unwrap()));
        assert!(!room.is_live_at(Utc.with_ymd_and_hms(2026, 8, 11, 3, 0, 0).unwrap()));

        store.clear_room().unwrap();
        assert!(store.room().unwrap().is_none());
        store.clear_room().unwrap(); // idempotent
    }

    #[test]
    fn a_room_is_one_flat_json_object_tagged_with_its_provider() {
        let (dir, store) = store();
        store.save_room(&partyline_room()).unwrap();
        let raw = fs::read_to_string(dir.path().join("room.json")).unwrap();
        // Flat, so `jq '.address'` reads it without digging through a nested
        // object, with two keys saying which kind of room this is and which
        // network it is on.
        assert!(raw.contains("\"provider\": \"partyline\""), "{raw}");
        assert!(raw.contains("\"transport\": \"tor\""), "{raw}");
        assert!(raw.contains("\"address\": \"abc.onion\""), "{raw}");
        assert!(raw.contains("\"secret\""), "{raw}");
    }

    #[test]
    fn a_web_room_carries_no_secret_and_no_address_at_all() {
        let (dir, store) = store();
        let room = Room {
            kind: RoomKind::Web {
                url: "https://p2p.mirotalk.com/join/abc123".into(),
            },
            opened_by: "telegram:1".parse().unwrap(),
            note: None,
            started_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::hours(2),
        };
        store.save_room(&room).unwrap();
        assert_eq!(store.room().unwrap().unwrap(), room);

        let raw = fs::read_to_string(dir.path().join("room.json")).unwrap();
        assert!(raw.contains("\"provider\": \"web\""), "{raw}");
        assert!(!raw.contains("secret"), "no null secret key: {raw}");
        assert!(!raw.contains("onion"), "no null onion key: {raw}");
    }

    #[test]
    fn room_id_and_kind_identify_the_room_for_teardown() {
        assert_eq!(partyline_room().id(), "abc.onion");
        assert_eq!(
            partyline_room().provider_kind(),
            crate::config::ProviderKind::Tor
        );

        let web = Room {
            kind: RoomKind::Web {
                url: "https://meet.example.org/join/x".into(),
            },
            ..partyline_room()
        };
        assert_eq!(web.id(), "https://meet.example.org/join/x");
        assert_eq!(web.provider_kind(), crate::config::ProviderKind::Web);
    }

    #[test]
    fn a_pre_upgrade_room_file_is_detected_rather_than_guessed_at() {
        // Written by a version that had no `provider` key. The daemon clears
        // this at startup instead of refusing to boot; see Engine::recover.
        let (dir, store) = store();
        fs::write(
            dir.path().join("room.json"),
            r#"{"onion":"abc.onion","secret":"S","opened_by":"telegram:1",
                "started_at":"2026-08-11T00:00:00Z","expires_at":"2026-08-11T02:00:00Z"}"#,
        )
        .unwrap();
        assert!(store.room().is_err());
        assert!(store.room_is_unreadable());

        store.clear_room().unwrap();
        assert!(!store.room_is_unreadable());
        assert!(store.room().unwrap().is_none());
    }

    #[test]
    fn a_room_file_from_before_the_other_networks_existed_is_detected_too() {
        // Written when a party line was always Tor: the address was called
        // `onion` and there was no `transport` key. Same treatment as any
        // other unreadable room file, so the upgrade costs at most one live
        // room rather than a daemon that will not start.
        let (dir, store) = store();
        fs::write(
            dir.path().join("room.json"),
            r#"{"provider":"partyline","onion":"abc.onion","port":7777,"secret":"S",
                "opened_by":"telegram:1","started_at":"2026-08-11T00:00:00Z",
                "expires_at":"2026-08-11T02:00:00Z"}"#,
        )
        .unwrap();
        assert!(store.room().is_err());
        assert!(store.room_is_unreadable());
    }

    #[test]
    fn a_held_request_remembers_which_verb_asked_for_it() {
        let (dir, store) = store();
        let pending = Pending(vec![PendingRequest {
            id: "abc".into(),
            endpoint: "telegram:1".parse().unwrap(),
            kind: crate::config::ProviderKind::Web,
            note: None,
            requested_at: Utc::now(),
            approved: false,
        }]);
        store.save_pending(&pending).unwrap();
        assert_eq!(
            store.pending().unwrap().0[0].kind,
            crate::config::ProviderKind::Web
        );

        // A file written before `web` existed still parses, as a party line.
        fs::write(
            dir.path().join("pending.json"),
            r#"[{"id":"old","endpoint":"telegram:2","requested_at":"2026-08-11T00:00:00Z"}]"#,
        )
        .unwrap();
        assert_eq!(
            store.pending().unwrap().0[0].kind,
            crate::config::ProviderKind::Tor
        );
    }

    #[test]
    fn lock_is_exclusive() {
        let (dir, store) = store();
        let other = Store::open(dir.path()).unwrap();

        let guard = store.lock().unwrap();
        let path = dir.path().join(".lock");
        let contender = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        assert!(
            contender.try_lock().is_err(),
            "a second holder must not get the lock while it is held"
        );

        drop(guard);
        let _second = other.lock().expect("lock is available once released");
    }

    #[test]
    fn quiet_hours_need_a_timezone() {
        let mut s = subscriber("telegram:1");
        s.quiet = Some("23:00-07:00".parse().unwrap());
        let three_am_denver = Utc.with_ymd_and_hms(2026, 8, 11, 9, 0, 0).unwrap();

        assert!(
            !s.is_quiet_at(three_am_denver),
            "no timezone means no guessing"
        );

        s.tz = Some("America/Denver".into());
        assert!(s.is_quiet_at(three_am_denver));

        s.tz = Some("Mars/Olympus".into());
        assert!(!s.is_quiet_at(three_am_denver), "a bad zone must not silence");
    }

    #[test]
    fn corrupt_json_is_an_error_not_a_silent_reset() {
        let (dir, store) = store();
        fs::write(dir.path().join("subscribers.json"), "{not json").unwrap();
        assert!(store.subscribers().is_err());
    }
}
