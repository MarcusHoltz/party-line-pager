//! Every string a subscriber ever sees.
//!
//! Kept pure and in one module so the wording can be tested, and so no adapter
//! is tempted to invent its own phrasing for a rejection.
//!
//! Everything here returns a [`Doc`] rather than a `String`. This module owns
//! what a message *says* and what its parts *mean*; each adapter owns what that
//! looks like on its own network. Before the split, the aligned blocks were
//! built with spaces and shipped to every transport identically, which reads
//! correctly in IRC and email and falls apart everywhere with a proportional
//! font.

use std::time::Duration;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;

use crate::config::{Policy, ProviderKind, Tier};
use crate::doc::Doc;
use crate::policy::{CloseRejection, OpenRejection, SubscribeOutcome};
use crate::state::{Room, RoomKind, Subscriber, SubscriberStatus};

/// A message ready for Apprise and email, both of which take a title and a body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Broadcast {
    pub title: String,
    pub body: Doc,
}

/// The invite itself. In v1 the credentials travel inline, which is the DM
/// delivery mode: the subscriber's chat service sees the address and the secret.
/// Link mode, where the broadcast carries only a token URL, is the v2 mode
/// selected by `delivery`.
///
/// `tz` is the recipient's timezone, so this is rendered once per subscriber
/// rather than once per broadcast. That costs a `Doc` per recipient and buys a
/// closing time somebody can act on without doing arithmetic.
pub fn broadcast(
    policy: &Policy,
    room: &Room,
    opened_by_name: Option<&str>,
    tz: Option<&str>,
    now: DateTime<Utc>,
    delivery: CredsDelivery,
) -> Broadcast {
    let name = &policy.instance.name;

    let host = opened_by_name
        .map(str::to_string)
        .unwrap_or_else(|| room.opened_by.to_string());
    let mut lead = format!("{host} opened the room.");
    if let Some(note) = &room.note {
        lead.push_str(&format!("\nNote: {note}"));
    }

    let body = Doc::new()
        .para(lead)
        .append(creds_block(room, delivery))
        .append(closing(room, tz, now))
        .append(client_note(room))
        // Short, and last. An opt-out belongs in every message, especially the
        // ones that arrive by email, but it is not what the reader is here for.
        .note("unsub to stop.");

    Broadcast {
        title: format!("{name}: the room is open"),
        body,
    }
}

/// How this room is joined, formatted the same way everywhere it appears.
///
/// A party line shows the address under whatever its network calls one, plus
/// the shared secret. No port: `partyline.sh` accepts only a bare address on
/// all three networks, and a pasted "address:port" breaks its normalization
/// instead of being ignored.
///
/// For a web room there is no second factor at all, so the line that says so
/// travels with the URL rather than being left to the README, which nobody
/// reads at 1am.
pub fn creds_block(room: &Room, delivery: CredsDelivery) -> Doc {
    match delivery {
        // The mint already happened (impure, in partylinepagerd, before this
        // module ever runs); we only ever see the resulting URL here.
        CredsDelivery::Link(url) => Doc::new()
            .fields(vec![("Room", url.to_string())])
            .note("One-time link: opening it burns it. Only you got this one."),
        CredsDelivery::Inline => match &room.kind {
            RoomKind::Partyline {
                transport,
                address,
                secret,
            } => Doc::new().fields(vec![
                (transport.label(), address.clone()),
                ("Secret", secret.clone()),
            ]),
            // The warning stays welded to the URL rather than moving to the
            // footnotes with the client link: it is the only access control
            // this kind of room has, and it is not a footnote.
            RoomKind::Web { url } => Doc::new()
                .fields(vec![("Room", url.clone())])
                .note("No password: anyone with this link can join, so do not forward it."),
        },
    }
}

/// How a room's join credentials should appear in a rendered message. The
/// caller (impure, in partylinepagerd) decides which one applies and, for `Link`,
/// has already minted the URL, so this module never does I/O to get one.
#[derive(Clone, Copy, Debug)]
pub enum CredsDelivery<'a> {
    /// v1: the address+secret, or room URL, appear in the message body.
    Inline,
    /// v2: a pre-minted, recipient-specific one-time link stands in for them.
    Link(&'a str),
}

/// The plaintext a Yopass link encrypts, for Link mode. Deliberately terse:
/// whoever opens the link already knows what the partyline pager is and why
/// they got it.
pub fn creds_plaintext(room: &Room) -> String {
    match &room.kind {
        RoomKind::Partyline {
            transport,
            address,
            secret,
        } => format!("{}: {address}\nSecret: {secret}", transport.label()),
        RoomKind::Web { url } => format!("Room: {url}"),
    }
}

/// Sent instead of a room's credentials when `CredsDelivery::Link` was
/// selected but minting the link itself failed. Never falls back to the
/// inline form: that would silently undo the reason Link mode was turned on.
pub fn link_unavailable() -> Doc {
    Doc::new().para(
        "Could not prepare a link for the room's credentials right now. Try again shortly.",
    )
}

/// Where to get the client, if this room needs one.
fn client_note(room: &Room) -> Doc {
    match &room.kind {
        RoomKind::Partyline { transport, .. } => {
            Doc::new().note(format!("Client: {}", transport.client_url()))
        }
        RoomKind::Web { .. } => Doc::new(),
    }
}

/// When the room closes, and what "closes" actually means for this kind.
fn closing(room: &Room, tz: Option<&str>, now: DateTime<Utc>) -> Doc {
    let at = local_time(room.expires_at, tz);

    let mut line = match room.expires_at.signed_duration_since(now).to_std() {
        Ok(left) => format!("Closes {at}, in {}.", humanize(left)),
        // Already past. Says so rather than promising a deadline in the past.
        Err(_) => format!("Closed at {at}."),
    };

    // Being honest beats being tidy: nothing here can evict anybody from a
    // room hosted somewhere else. All that happens at the deadline is that
    // this bot stops offering the room and lets somebody open the next one.
    if matches!(room.kind, RoomKind::Web { .. }) {
        line.push_str(" The link itself keeps working, so hang up when you are done.");
    }

    Doc::new().para(line)
}

/// A timestamp in the reader's own zone, falling back to UTC.
///
/// The daemon has always stored each subscriber's timezone, because quiet
/// hours are meaningless without one, and until now rendered every deadline in
/// UTC anyway. "Closes at 03:00 UTC" is not something a reader can act on.
fn local_time(at: DateTime<Utc>, tz: Option<&str>) -> String {
    match tz.and_then(|name| name.parse::<Tz>().ok()) {
        Some(zone) => at.with_timezone(&zone).format("%-I:%M %p %Z").to_string(),
        None => at.format("%H:%M UTC").to_string(),
    }
}

/// Sent to the host immediately, before the provider hook returns.
///
/// Only for the kinds slow enough to need it, where rotating the key forces a
/// fresh bootstrap and the bot would otherwise look dead for a minute or
/// more. A web room is minted outright, so the same ack would be overtaken
/// by the broadcast it promises and read as a bug.
/// [`Transport::bootstrap_hint`] returns `None` for web and this returns
/// `None` in turn.
pub fn ack_provisioning(policy: &Policy, kind: ProviderKind) -> Option<Doc> {
    let hint = kind.transport()?.bootstrap_hint()?;
    Some(Doc::new().para(format!(
        "{}: opening the room. Everyone gets the invite as soon as the address is up, {hint}.",
        policy.instance.name
    )))
}

pub fn already_live(
    room: &Room,
    tz: Option<&str>,
    now: DateTime<Utc>,
    delivery: CredsDelivery,
) -> Doc {
    Doc::new()
        .para("The room is already open, so this did not count against your quota.")
        .append(creds_block(room, delivery))
        .append(closing(room, tz, now))
        .append(client_note(room))
}

/// A second request from somebody who already has one queued. Their first
/// is untouched: this only tells them it is still there.
pub fn already_queued() -> Doc {
    Doc::new().para("You already have a request queued for an admin. It is still waiting.")
}

pub fn held_for_approval() -> Doc {
    Doc::new().para("Your request is queued for an admin to release. Nothing has gone out yet.")
}

pub fn rejection(reason: &OpenRejection) -> Doc {
    Doc::new().para(match reason {
        OpenRejection::NotSubscribed => "You are not subscribed. Send sub first.".to_string(),
        OpenRejection::AwaitingApproval => {
            "Your subscription is still waiting for an admin.".to_string()
        }
        OpenRejection::Banned => "You cannot use this service.".to_string(),
        OpenRejection::TierForbids => {
            "Your tier can receive invites but not open a room.".to_string()
        }
        OpenRejection::QuotaExhausted { retry_after } => format!(
            "You have used every room your tier gets. You can open another in {}.",
            humanize(*retry_after)
        ),
        OpenRejection::Paused => "Invites are paused right now.".to_string(),
    })
}

pub fn close_rejection(reason: &CloseRejection) -> Doc {
    match reason {
        CloseRejection::NotSubscribed => not_subscribed(),
        CloseRejection::AwaitingApproval => {
            Doc::new().para("Your subscription is still waiting for an admin.")
        }
        CloseRejection::Banned => Doc::new().para("You cannot use this service."),
        CloseRejection::TierForbids => Doc::new().para("Your tier cannot close a room."),
        CloseRejection::NothingLive => Doc::new().para("Nothing is live right now."),
        CloseRejection::NotYourRoom => {
            Doc::new().para("Only whoever opened this room can close it.")
        }
    }
}

pub fn closed() -> Doc {
    Doc::new().para("Closed. The room is coming down now.")
}

pub fn subscribe_reply(policy: &Policy, outcome: SubscribeOutcome) -> Doc {
    let name = &policy.instance.name;
    match outcome {
        // The one moment a new subscriber is paying attention. Three
        // instructions in one sentence is what shipped first; this spends the
        // attention on the two settings that change what they receive.
        SubscribeOutcome::Added => Doc::new()
            .heading(format!("Subscribed to {name}."))
            .para("Two things worth setting now:")
            .fields(vec![
                ("tz America/Denver", "so times read in your own zone"),
                ("quiet 23:00-07:00", "so nothing wakes you"),
            ])
            .note("Send help for everything else."),
        SubscribeOutcome::HeldForApproval => Doc::new().para(format!(
            "Request recorded. An admin has to approve it before {name} reaches you."
        )),
        SubscribeOutcome::Closed => Doc::new().para("Signups are closed on this instance."),
        SubscribeOutcome::AlreadyActive => Doc::new().para("You are already subscribed."),
        SubscribeOutcome::AlreadyPending => {
            Doc::new().para("You are already in the queue, waiting on an admin.")
        }
        SubscribeOutcome::Banned => Doc::new().para("You cannot subscribe to this service."),
    }
}

pub fn unsubscribed(policy: &Policy) -> Doc {
    Doc::new().para(format!(
        "Removed from {}. Send sub if you change your mind.",
        policy.instance.name
    ))
}

pub fn not_subscribed() -> Doc {
    Doc::new().para("You are not subscribed.")
}

pub fn tz_set(zone: &str) -> Doc {
    Doc::new().para(format!(
        "Timezone set to {zone}. Quiet hours and closing times both read in that zone."
    ))
}

pub fn tz_invalid(zone: &str) -> Doc {
    Doc::new().para(format!(
        "{zone} is not an IANA timezone. Try something like tz America/Denver."
    ))
}

pub fn quiet_set(window: &crate::quiet::QuietWindow, tz: Option<&str>) -> Doc {
    Doc::new().para(match tz {
        Some(tz) => format!(
            "Quiet hours set to {window} ({tz}). Invites inside that window are dropped, not queued."
        ),
        None => format!(
            "Quiet hours set to {window}, but you have no timezone yet so nothing is being silenced. Send tz America/Denver."
        ),
    })
}

pub fn quiet_cleared() -> Doc {
    Doc::new().para("Quiet hours cleared. You will be invited at any hour.")
}

pub fn quiet_show(subscriber: &Subscriber) -> Doc {
    Doc::new().para(match (subscriber.quiet, subscriber.tz.as_deref()) {
        (Some(window), Some(tz)) => format!("Quiet hours: {window} ({tz})."),
        (Some(window), None) => format!(
            "Quiet hours: {window}, but no timezone is set so nothing is silenced. Send tz America/Denver."
        ),
        (None, _) => "No quiet hours set. Use quiet 23:00-07:00, or quiet off to clear.".to_string(),
    })
}

/// The `status` reply: tier, quota, quiet window, and whether a room is up.
pub fn status(
    subscriber: &Subscriber,
    tier: &Tier,
    room: Option<&Room>,
    now: DateTime<Utc>,
    delivery: CredsDelivery,
) -> Doc {
    let doc = Doc::new().fields(vec![
        ("Endpoint", subscriber.endpoint.to_string()),
        ("Status", status_word(subscriber.status).to_string()),
        ("Tier", tier.name.clone()),
        ("Quota", quota_line(tier, subscriber, now)),
        (
            "Timezone",
            subscriber.tz.clone().unwrap_or_else(|| "unset".to_string()),
        ),
        (
            "Quiet",
            subscriber
                .quiet
                .map(|w| w.to_string())
                .unwrap_or_else(|| "none".to_string()),
        ),
    ]);

    match room {
        Some(room) if room.is_live_at(now) => doc
            .para("The room is open now.")
            .append(creds_block(room, delivery))
            .append(closing(room, subscriber.tz.as_deref(), now))
            .append(client_note(room)),
        _ => doc.para("Nothing is live right now."),
    }
}

/// Spelled out rather than derived from `Debug`, so renaming a variant is a
/// deliberate copy change instead of a silent one.
fn status_word(status: SubscriberStatus) -> &'static str {
    match status {
        SubscriberStatus::Pending => "pending",
        SubscriberStatus::Active => "active",
        SubscriberStatus::Banned => "banned",
    }
}

fn quota_line(tier: &Tier, subscriber: &Subscriber, now: DateTime<Utc>) -> String {
    if !tier.may_open {
        return "receive only".to_string();
    }
    if tier.window.is_zero() {
        return "unlimited".to_string();
    }

    let used = crate::policy::opens_used(tier, &subscriber.open_log, now);
    match crate::policy::quota_remaining(tier, &subscriber.open_log, now) {
        Some(remaining) => format!(
            "spent ({used}/{} used), resets in {}",
            tier.max_rooms,
            humanize(remaining)
        ),
        None => format!(
            "ready ({used}/{} used this {})",
            tier.max_rooms,
            humanize(tier.window)
        ),
    }
}

/// The `help` reply: the list of commands, or one command in detail when the
/// sender named one. Note that no administrative verb appears, because none
/// exists in the parser: approve, ban and pause live in `partylinepagerctl`, which
/// is reachable only over SSH.
///
/// Only the verbs this instance can actually answer are listed. An
/// unconfigured provider is treated as unrecognized input, so advertising one
/// would tell somebody to type a word and then ignore them.
pub fn help(policy: &Policy, topic: Option<&str>) -> Doc {
    match topic.and_then(|verb| help_for(policy, verb)) {
        Some(doc) => doc,
        // Includes every unrecognized word. Saying "no such command" would
        // answer a stranger's question about what this instance runs; the
        // list already says everything we are willing to say.
        None => help_index(policy),
    }
}

/// One command in detail. Returns `None` for unknown verbs.
fn help_for(policy: &Policy, verb: &str) -> Option<Doc> {
    if let Some(kind) = policy
        .provider
        .configured()
        .into_iter()
        .find(|kind| kind.verb() == verb)
    {
        return Some(provider_help(kind));
    }

    let doc = match verb {
        "close" => Doc::new()
            .heading("close")
            .para(
                "Closes the room you opened, ahead of its timer. \
                 Only the person who opened it can close it, and only if their tier allows it.",
            )
            .code("close"),
        "sub" | "subscribe" => Doc::new()
            .heading("sub")
            .para(
                "Puts you on the subscriber list. Every room opened after this reaches you, \
                 except during your quiet hours. Also spelled subscribe.",
            )
            .code("sub"),
        "unsub" | "unsubscribe" | "stop" => Doc::new()
            .heading("unsub")
            .para(
                "Removes you from the list. Nothing reaches you after that. \
                 Also spelled unsubscribe or stop. Send sub to rejoin.",
            )
            .code("unsub"),
        "tz" | "timezone" => Doc::new()
            .heading("tz <zone>")
            .para(
                "Sets your timezone using an IANA name. \
                 Closing times and quiet hours both use it, so set this first.",
            )
            .code("tz America/Denver"),
        "quiet" => Doc::new()
            .heading("quiet <window>")
            .para(
                "Sets a do-not-disturb window. Broadcasts inside it are dropped, not queued, \
                 so there is no catch-up when you wake up. \
                 Send quiet by itself to see your current window, or quiet off to clear it.",
            )
            .code("quiet 23:00-07:00"),
        "status" => Doc::new()
            .heading("status")
            .para(
                "Shows your tier, how much of your quota is left, \
                 your timezone and quiet hours, and the live room if one is open.",
            )
            .code("status"),
        "help" => Doc::new()
            .heading("help [command]")
            .para(
                "By itself, lists every command. Name one to get its detail page. \
                 For longer reference topics (audio modes, connecting, security), try wiki.",
            )
            .code("help close"),
        "wiki" | "guide" => Doc::new()
            .heading("wiki [topic]")
            .para(
                "Reference pages on how the pager and party lines work. \
                 By itself, lists every topic. Name one to read it. Also spelled guide.",
            )
            .code("wiki audio"),
        _ => return None,
    };
    Some(doc)
}

/// Help page for one room-opening verb.
fn provider_help(kind: ProviderKind) -> Doc {
    let body = match kind {
        ProviderKind::Tor => {
            "Opens a Tor party line with a fresh .onion address and shared secret. \
             Every subscriber gets both. Takes one to three minutes to come up."
        }
        ProviderKind::I2p => {
            "Opens an I2P party line with a fresh .b32.i2p address and shared secret. \
             Every subscriber gets both. Up in about 23 seconds."
        }
        ProviderKind::Rns => {
            "Opens a Reticulum party line with a fresh destination hash and shared secret. \
             Fastest of the three, works through NAT at both ends. \
             Not onion-routed: the relay sees every caller's IP."
        }
        ProviderKind::Web => "Opens a video room in the browser. Nothing to install. The link is it.",
    };

    Doc::new()
        .heading(format!("{} [note]", kind.verb()))
        .para(body)
        .code(format!("{} poker night", kind.verb()))
        .note("The [note] is an optional, single line, that rides along in the invite.")
}

/// The list. Every command this instance answers, one line each.
fn help_index(policy: &Policy) -> Doc {
    let verbs: Vec<(String, String)> = policy
        .provider
        .configured()
        .into_iter()
        .map(|kind| {
            (
                format!("{} [note]", kind.verb()),
                kind.help_line().to_string(),
            )
        })
        .collect();

    Doc::new()
        .heading(policy.instance.name.clone())
        .fields(verbs)
        .fields(vec![
            ("sub", "Subscribe"),
            ("unsub", "Unsubscribe"),
            ("tz <zone>", "Set timezone, e.g. tz America/Denver"),
            ("quiet <window>", "Quiet hours; quiet off clears, quiet shows"),
            ("status", "Tier, quota, quiet hours"),
            ("close", "Close room you opened"),
            ("help [command]", "Detail one command"),
            ("wiki [topic]", "Deep-dive reference pages"),
        ])
}


/// Deep-dive reference. Lists topics, or shows one.
pub fn wiki(topic: Option<&str>) -> Doc {
    match topic.and_then(wiki_topic) {
        Some(doc) => doc,
        None => wiki_index(),
    }
}

fn wiki_index() -> Doc {
    Doc::new()
        .heading("Wiki")
        .fields(vec![
            ("wiki audio", "Full-duplex vs half-duplex audio modes"),
            ("wiki connect", "How to connect with the party-line client"),
            ("wiki mobile", "Using the party-line client on your phone"),
            ("wiki security", "What is encrypted, what is not"),
            ("wiki rooms", "How rooms work: opening, closing, expiry"),
            ("wiki platforms", "Supported messaging services"),
            ("wiki selfhost", "Run your own Party Line Pager"),
        ])
}

fn wiki_topic(topic: &str) -> Option<Doc> {
    let doc = match topic {
        "audio" | "duplex" | "fullduplex" | "halfduplex" => Doc::new()
            .heading("Audio modes")
            .para(
                "Half-duplex is the default. It works like a walkie-talkie: hold a key \
                 to transmit, release to listen. One person speaks at a time.",
            )
            .para(
                "Full-duplex works like a phone call. Your mic is always open and \
                 everyone hears everyone at once. You can enable it in the client's \
                 settings menu.",
            )
            .para(
                "Both modes use the same relay and the same room, so half-duplex and \
                 full-duplex callers can be in the same room at the same time. No \
                 configuration is needed on the relay side.",
            ),
        "connect" | "client" | "setup" => Doc::new()
            .heading("Connecting")
            .para("Each transport has its own client. Grab the one that matches your room:")
            .fields(vec![
                ("Tor", "gitlab.com/MarcusHoltz/tor-party-line"),
                ("I2P", "gitlab.com/MarcusHoltz/i2p-party-line"),
                ("Reticulum", "gitlab.com/MarcusHoltz/reticulum-party-line"),
            ])
            .para(
                "Clone the repo and run partyline.sh. When a room opens, paste the \
                 address and shared secret from the invite. The script takes care of \
                 dependencies, audio setup, and encryption.",
            )
            .para(
                "Web rooms do not need a client at all. Just open the link in any browser.",
            ),
        "security" | "encryption" | "privacy" => Doc::new()
            .heading("Security")
            .para(
                "Tor rooms are onion-routed. The relay never sees caller IP addresses, \
                 and audio is encrypted in transit through the .onion tunnel.",
            )
            .para(
                "I2P rooms are garlic-routed. The relay never sees caller IPs, and \
                 audio is encrypted in transit through I2P tunnels.",
            )
            .para(
                "Reticulum rooms are not onion-routed. The relay does see caller IP \
                 addresses. Audio is encrypted in transit via RNS link encryption.",
            )
            .para(
                "Web rooms use browser WebRTC with SRTP. Connections are peer-to-peer \
                 when possible, through a TURN relay otherwise. The room URL is the \
                 only access control.",
            )
            .para(
                "On all transports, audio is encrypted with the shared secret before it \
                 leaves your device (AES-256-CBC, PBKDF2). The relay only sees ciphertext.",
            )
            .para(
                "When link mode is enabled, room credentials never appear in the \
                 message. Each subscriber gets a one-time Yopass link instead \
                 (share.yopass.se by default, or your own instance). The secret is \
                 encrypted client-side before it reaches the server, the link burns \
                 after one open, and its expiry matches the room lifetime. Miss the \
                 message? The link self-destructs. Nothing is revealed.",
            ),
        "mobile" | "phone" | "android" | "ios" | "termux" => Doc::new()
            .heading("Mobile")
            .para(
                "The party-line client runs on Android phones through Termux. Install \
                 Termux from F-Droid (not the Play Store version, which is outdated), \
                 then clone the client repo and run partyline.sh inside Termux.",
            )
            .para(
                "Full walkthrough with screenshots: \
                 blog.holtzweb.com/posts/tor-party-line-encrypted-push-to-talk-over-terminal-group-chat",
            )
            .para(
                "iOS has terminal apps but none of them will be able to access the \
                 microphone or speakers from the iOS environment. Termux on Android \
                 bridges this gap with termux-api; no iOS equivalent exists.",
            ),
        "platforms" | "services" | "messengers" => Doc::new()
            .heading("Supported platforms")
            .para(
                "The pager delivers invites over any of these services. You can subscribe \
                 from whichever one you use, and you will get room credentials there when \
                 someone opens a room.",
            )
            .fields(vec![
                ("Telegram", "Bot DM"),
                ("Signal", "Via signal-cli"),
                ("Discord", "Bot DM"),
                ("Matrix", "Bot DM"),
                ("Mattermost", "Bot DM or channel"),
                ("IRC", "Private message"),
                ("XMPP", "Chat message"),
                ("Mastodon", "Direct mention"),
                ("Email", "Inbox delivery"),
            ])
            .para(
                "Each service is a separate adapter. Your admin chooses which ones to \
                 enable. You can subscribe from more than one service if the admin has \
                 them configured, but each subscription is independent (each address on \
                 each service is its own subscriber).",
            ),
        "selfhost" | "self-host" | "hosting" | "install" => Doc::new()
            .heading("Run your own")
            .para(
                "PartylinePager is open source. You can run your own instance and \
                 control who subscribes, which transports are available, and which \
                 messaging platforms deliver invites.",
            )
            .para("Source and docs: gitlab.com/MarcusHoltz/party-line-pager")
            .para(
                "The project ships as a Docker image. A single Docker Compose file \
                 brings up the daemon, whichever adapters you enable, and the transport \
                 hooks. Configuration is two TOML files: policy.toml for rules and tiers, \
                 adapters.toml for service credentials.",
            ),
        "rooms" | "room" => Doc::new()
            .heading("Rooms")
            .para(
                "Opening a room generates a fresh address and secret, then sends both \
                 to every subscriber. Only one room can be open at a time per instance.",
            )
            .para(
                "A room closes in one of three ways: the timer expires, the person \
                 who opened it sends close, or an admin closes it with partylinepagerctl. \
                 Once closed, the address is destroyed and no one can reconnect.",
            )
            .para(
                "Each tier has a quota: a number of rooms per time window. Opening a \
                 room costs one from your quota. Joining someone else's room costs nothing.",
            ),
        _ => return None,
    };
    Some(doc)
}

/// Renders a duration the way a person would say it: the two largest units.
pub fn humanize(d: Duration) -> String {
    let total = d.as_secs();
    if total == 0 {
        return "0s".to_string();
    }
    let days = total / 86_400;
    let hours = (total % 86_400) / 3_600;
    let minutes = (total % 3_600) / 60;
    let seconds = total % 60;

    let parts = [(days, "d"), (hours, "h"), (minutes, "m"), (seconds, "s")];
    let rendered: Vec<String> = parts
        .iter()
        .filter(|(value, _)| *value > 0)
        .take(2)
        .map(|(value, unit)| format!("{value}{unit}"))
        .collect();
    rendered.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Transport;
    use crate::doc::Style;
    use crate::endpoint::EndpointId;
    use chrono::TimeZone;

    fn policy_with(providers: &str) -> Policy {
        Policy::parse_str(&format!(
            r#"
            [instance]
            name = "TestPager"
            default_tier = "weekly"
            {providers}
            [[tier]]
            name = "weekly"
            window = "168h"
            "#,
        ))
        .unwrap()
    }

    fn policy() -> Policy {
        policy_with("[provider.tor]\nup = \"u\"\ndown = \"d\"")
    }

    fn both_policy() -> Policy {
        policy_with(
            "[provider.tor]\nup = \"u\"\ndown = \"d\"\n\
             [provider.web]\nup = \"u\"\ndown = \"d\"",
        )
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 11, 12, 0, 0).unwrap()
    }

    fn room() -> Room {
        Room {
            kind: RoomKind::Partyline {
                transport: Transport::Tor,
                address: "batcave7xyzabcdefghijklmnopqrstuvwxyz234567abcdefghijklm.onion".into(),
                secret: "AAAABBBBCCCC".into(),
            },
            opened_by: "telegram:1".parse().unwrap(),
            note: Some("poker night".into()),
            started_at: now(),
            expires_at: now() + chrono::Duration::hours(2),
        }
    }

    fn web_room() -> Room {
        Room {
            kind: RoomKind::Web {
                url: "https://p2p.mirotalk.com/join/deadbeef".into(),
            },
            ..room()
        }
    }

    /// The broadcast as an unconfigured subscriber sees it: no timezone.
    fn utc_broadcast(policy: &Policy, room: &Room) -> Broadcast {
        broadcast(policy, room, None, None, now(), CredsDelivery::Inline)
    }

    #[test]
    fn broadcast_carries_the_address_the_secret_and_the_note() {
        let b = utc_broadcast(&policy(), &room());
        let body = b.body.plain();
        assert!(b.title.contains("TestPager"));
        assert!(body.contains("batcave7xyzabcdefghijklmnopqrstuvwxyz234567abcdefghijklm.onion"));
        assert!(body.contains("AAAABBBBCCCC"));
        assert!(body.contains("poker night"));
        assert!(body.contains("telegram:1"), "host is named");
        assert!(body.contains("unsub"), "opt-out in every message");
    }

    #[test]
    fn broadcast_leads_with_the_opener_s_display_name_when_one_is_set() {
        let b = broadcast(&policy(), &room(), Some("Doug"), None, now(), CredsDelivery::Inline);
        let body = b.body.plain();
        assert!(body.contains("Doug opened the room."), "{body}");
        assert!(!body.contains("telegram:1"), "raw id should not leak once a name is set: {body}");
    }

    #[test]
    fn broadcast_carries_the_partyline_sh_link_so_joiners_know_how_to_connect() {
        let b = utc_broadcast(&policy(), &room());
        assert!(
            b.body.plain().contains(Transport::Tor.client_url()),
            "a party line must link to the client that dials it: {}",
            b.body.plain()
        );
    }

    #[test]
    fn a_web_broadcast_carries_no_partyline_sh_link() {
        let b = utc_broadcast(&both_policy(), &web_room());
        assert!(
            !b.body.plain().contains("partyline.sh"),
            "a web room has nothing to do with tor-party-line: {}",
            b.body.plain()
        );
    }

    #[test]
    fn broadcast_without_a_note_omits_the_note_line() {
        let mut r = room();
        r.note = None;
        assert!(!utc_broadcast(&policy(), &r).body.plain().contains("Note:"));
    }

    #[test]
    fn a_closing_time_is_rendered_in_the_subscribers_own_zone() {
        // The room expires at 14:00 UTC, which is 08:00 in Denver.
        let denver = broadcast(
            &policy(),
            &room(),
            None,
            Some("America/Denver"),
            now(),
            CredsDelivery::Inline,
        )
        .body
        .plain();
        assert!(denver.contains("8:00 AM MDT"), "{denver}");
        assert!(denver.contains("in 2h"), "{denver}");
        assert!(!denver.contains("UTC"), "a known zone never falls back: {denver}");

        // Without a zone there is nothing to convert to, so UTC is stated
        // rather than guessed at.
        let unset = utc_broadcast(&policy(), &room()).body.plain();
        assert!(unset.contains("14:00 UTC"), "{unset}");
    }

    #[test]
    fn an_unknown_timezone_falls_back_instead_of_failing() {
        let text = broadcast(
            &policy(),
            &room(),
            None,
            Some("Mars/Olympus"),
            now(),
            CredsDelivery::Inline,
        )
        .body
        .plain();
        assert!(text.contains("14:00 UTC"), "{text}");
    }

    #[test]
    fn the_credentials_survive_a_proportional_font() {
        // The whole point of the Doc split: on the transports that render a
        // proportional font, the address and the secret have to arrive inside
        // something monospace or the columns collapse.
        let b = utc_broadcast(&policy(), &room());

        let markdown = b.body.render(Style::Markdown);
        assert!(markdown.contains("```"), "{markdown}");
        assert!(markdown.contains("batcave7xyzabcdefghijklmnopqrstuvwxyz234567abcdefghijklm.onion"), "{markdown}");

        for style in [Style::TelegramHtml, Style::MatrixHtml] {
            let html = b.body.render(style);
            assert!(html.contains("<pre>"), "{html}");
            assert!(html.contains("AAAABBBBCCCC"), "{html}");
        }
    }

    #[test]
    fn link_mode_carries_the_url_and_neither_the_address_nor_the_secret() {
        let link = "https://yopass.se/#/s/x/y";
        let b = broadcast(&policy(), &room(), None, None, now(), CredsDelivery::Link(link));
        let body = b.body.plain();
        assert!(body.contains(link), "{body}");
        assert!(!body.contains("batcave7xyzabcdefghijklmnopqrstuvwxyz234567abcdefghijklm.onion"), "{body}");
        assert!(!body.contains("AAAABBBBCCCC"), "{body}");
    }

    #[test]
    fn quota_rejection_states_the_wait() {
        let text = rejection(&OpenRejection::QuotaExhausted {
            retry_after: Duration::from_secs(6 * 86_400 + 23 * 3_600),
        });
        assert!(text.plain().contains("6d 23h"), "{}", text.plain());
    }

    #[test]
    fn humanize_uses_two_units() {
        assert_eq!(humanize(Duration::from_secs(0)), "0s");
        assert_eq!(humanize(Duration::from_secs(45)), "45s");
        assert_eq!(humanize(Duration::from_secs(3_600)), "1h");
        assert_eq!(humanize(Duration::from_secs(3_661)), "1h 1m");
        assert_eq!(humanize(Duration::from_secs(168 * 3_600)), "7d");
    }

    #[test]
    fn help_lists_the_user_verbs_and_no_admin_verbs() {
        let text = help(&both_policy(), None).plain();
        for verb in [
            "tor", "web", "sub", "unsub", "tz", "quiet", "status", "close", "help", "wiki",
        ] {
            assert!(text.contains(verb), "help is missing {verb}");
        }
        for admin in ["approve", "ban", "pause", "who", "deny"] {
            assert!(!text.contains(admin), "help leaks the admin verb {admin}");
        }
    }

    #[test]
    fn help_only_offers_verbs_the_instance_can_answer() {
        let tor_only = help(&policy(), None).plain();
        assert!(tor_only.contains("tor [note]"));
        assert!(
            !tor_only.contains("web [note]"),
            "a verb the engine ignores must not be advertised: {tor_only}"
        );

        let web_only = help(&policy_with("[provider.web]\nup = \"u\"\ndown = \"d\""), None).plain();
        assert!(web_only.contains("web [note]"));
        assert!(!web_only.contains("tor"), "{web_only}");
        assert!(!web_only.contains("signal"), "{web_only}");

        // The removed generic word must not resurface even with both
        // providers configured, where it used to be offered as an alias.
        assert!(!help(&both_policy(), None).plain().contains("signal"));
    }

    #[test]
    fn every_command_the_parser_accepts_has_a_page_of_its_own() {
        let policy = both_policy();
        for verb in [
            "tor", "web", "close", "sub", "subscribe", "unsub", "unsubscribe", "stop", "tz",
            "timezone", "quiet", "status", "help", "wiki", "guide",
        ] {
            let text = help(&policy, Some(verb)).plain();
            assert!(
                !text.contains("Set timezone, e.g."),
                "help {verb} fell through to the list: {text}"
            );
        }
    }

    #[test]
    fn a_page_says_what_to_type_what_it_does_and_shows_one_example() {
        let text = help(&both_policy(), Some("close")).plain();
        assert!(text.starts_with("close"), "{text}");
        // The example stands on its own line, unlabelled, so it is the thing
        // a phone offers to copy rather than a row in a table.
        assert!(text.ends_with("\n\nclose"), "{text}");
    }

    #[test]
    fn an_unanswerable_verb_gets_the_list_rather_than_a_denial() {
        // Unknown words, and a provider this instance does not offer, are the
        // same answer: enumerating either one would confirm what exists.
        for verb in ["banana", "i2p", "approve"] {
            let text = help(&both_policy(), Some(verb)).plain();
            assert!(text.contains("help [command]"), "help {verb} gave: {text}");
            assert!(
                !text.contains("Bare help lists the commands"),
                "help {verb} gave: {text}"
            );
        }
    }

    #[test]
    fn wiki_index_lists_available_topics() {
        let text = wiki(None).plain();
        for topic in ["audio", "connect", "mobile", "security", "rooms", "platforms", "selfhost"] {
            assert!(text.contains(topic), "wiki index is missing {topic}");
        }
    }

    #[test]
    fn wiki_topic_returns_a_page() {
        let text = wiki(Some("audio")).plain();
        assert!(text.contains("Half-duplex"), "{text}");
        assert!(text.contains("Full-duplex"), "{text}");
    }

    #[test]
    fn wiki_unknown_topic_falls_back_to_index() {
        let text = wiki(Some("banana")).plain();
        assert!(text.contains("wiki audio"), "{text}");
    }

    #[test]
    fn a_web_broadcast_carries_the_url_and_no_secret() {
        let b = utc_broadcast(&both_policy(), &web_room());
        let body = b.body.plain();
        assert!(body.contains("Room  https://p2p.mirotalk.com/join/deadbeef"), "{body}");
        assert!(!body.contains("Onion"), "{body}");
        assert!(!body.contains("Secret"), "{body}");
        assert!(
            body.contains("anyone with this link can join"),
            "the only access control has to be stated: {body}"
        );
        assert!(
            body.contains("The link itself keeps working"),
            "expiry must not be oversold: {body}"
        );
        assert!(body.contains("poker night"));
    }

    #[test]
    fn status_lines_its_labels_up() {
        let p = policy();
        let mut s = Subscriber::new("telegram:1".parse::<EndpointId>().unwrap(), "weekly");
        s.status = SubscriberStatus::Active;
        let text = status(&s, p.tier("weekly").unwrap(), None, now(), CredsDelivery::Inline)
            .plain();
        assert!(text.contains("Status    active"), "{text}");
        assert!(text.contains("Endpoint  telegram:1"), "{text}");
    }

    #[test]
    fn status_reports_quota_and_live_room() {
        let p = policy();
        let mut s = Subscriber::new("telegram:1".parse::<EndpointId>().unwrap(), "weekly");
        s.status = SubscriberStatus::Active;
        let tier = p.tier("weekly").unwrap();

        let ready = status(&s, tier, None, now(), CredsDelivery::Inline).plain();
        assert!(ready.contains("ready"), "{ready}");
        assert!(ready.contains("Nothing is live"), "{ready}");

        s.open_log = vec![now() - chrono::Duration::hours(1)];
        let spent = status(&s, tier, Some(&room()), now(), CredsDelivery::Inline).plain();
        assert!(spent.contains("1/1 used"), "{spent}");
        assert!(spent.contains("resets in 6d 23h"), "{spent}");
        assert!(spent.contains("batcave7xyzabcdefghijklmnopqrstuvwxyz234567abcdefghijklm.onion"), "{spent}");
    }

    #[test]
    fn status_uses_the_subscribers_zone_for_the_live_room() {
        let p = policy();
        let mut s = Subscriber::new("telegram:1".parse::<EndpointId>().unwrap(), "weekly");
        s.status = SubscriberStatus::Active;
        s.tz = Some("America/Denver".to_string());

        let text = status(
            &s,
            p.tier("weekly").unwrap(),
            Some(&room()),
            now(),
            CredsDelivery::Inline,
        )
        .plain();
        assert!(text.contains("8:00 AM MDT"), "{text}");
    }

    #[test]
    fn quiet_without_a_timezone_says_so() {
        let window = "23:00-07:00".parse().unwrap();
        assert!(quiet_set(&window, None).plain().contains("no timezone"));
        assert!(quiet_set(&window, Some("America/Denver"))
            .plain()
            .contains("America/Denver"));
    }

    #[test]
    fn an_invalid_timezone_is_quoted_without_debug_formatting() {
        let text = tz_invalid("Denver").plain();
        assert!(text.starts_with("Denver is not"), "{text}");
        assert!(!text.contains('"'), "Debug formatting leaked: {text}");
    }
}
