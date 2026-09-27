//! `party_line_pagerctl`: the admin surface.
//!
//! Deliberately not reachable from any chat network. The daemon's command
//! parser has eight verbs and none of them are privileged, so approving,
//! banning, pausing and closing all happen here, over SSH, as a user with
//! access to the state directory.
//!
//! Every mutation takes the same `flock` the daemon takes, so an admin decision
//! cannot interleave with a fanout in progress. The daemon picks up decisions on
//! its next poll (five seconds) without needing a restart or a signal.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use party_line_pager_core::state::{Runtime, SubscriberStatus};
use party_line_pager_core::{EndpointId, Store, Subscriber, Subscribers};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "party-line-pagerctl", version, about = "Administer a PartyLinePager instance")]
struct Args {
    /// State directory, matching the daemon's --state.
    #[arg(long, default_value = "/etc/party-line-pager/state", global = true)]
    state: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List the roster.
    Who {
        /// Only show this status: pending, active or banned.
        #[arg(long)]
        status: Option<String>,
    },
    /// Move a pending subscriber onto the roster.
    Approve { endpoint: String },
    /// Remove a subscriber. They may subscribe again.
    Deny { endpoint: String },
    /// Refuse a subscriber for good. Survives unsubscribe and resubscribe.
    Ban { endpoint: String },
    /// Undo a ban, leaving the endpoint off the roster.
    Unban { endpoint: String },
    /// Move a subscriber to another tier.
    Tier { endpoint: String, tier: String },
    /// Set (or, with an empty name, clear) an endpoint's display name.
    Name { endpoint: String, name: String },
    /// Add an endpoint by hand, active immediately.
    ///
    /// Use `apprise:<url>` to reach any of the services Apprise supports
    /// without an adapter, for example `apprise:ntfy://ntfy.sh/party-line-pager`.
    Add {
        endpoint: String,
        #[arg(long, default_value = "weekly")]
        tier: String,
    },
    /// Clear a subscriber's rolling quota so they may open a room again now.
    ResetQuota { endpoint: String },
    /// List requests held for approval.
    Pending,
    /// Release a held request. The daemon provisions it within five seconds.
    ApproveRequest { id: String },
    /// Discard a held request.
    DenyRequest { id: String },
    /// Refuse new rooms instance-wide.
    Pause,
    /// Undo `pause`.
    Resume,
    /// Show what is live right now.
    Status,
    /// Tear the live room down early.
    Close,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let store = Store::open(&args.state)
        .with_context(|| format!("could not open {}", args.state.display()))?;
    let output = run(&store, args.command)?;
    print!("{output}");
    Ok(())
}

/// Runs one command and returns what should be printed.
///
/// Split out from `main` so the tests can drive every subcommand against a
/// temporary state directory.
fn run(store: &Store, command: Command) -> Result<String> {
    match command {
        Command::Who { status } => {
            let filter = status
                .as_deref()
                .map(parse_status)
                .transpose()?;
            let subs = store.subscribers()?;
            Ok(render_roster(&subs, filter))
        }

        Command::Approve { endpoint } => {
            set_status(store, &endpoint, SubscriberStatus::Active, true)?;
            Ok(format!("{endpoint} is on the roster\n"))
        }

        Command::Ban { endpoint } => {
            set_status(store, &endpoint, SubscriberStatus::Banned, false)?;
            Ok(format!("{endpoint} is banned\n"))
        }

        Command::Unban { endpoint } => {
            let id = parse_endpoint(&endpoint)?;
            let _flock = store.lock()?;
            let mut subs = store.subscribers()?;
            match subs.get(&id).map(|s| s.status) {
                Some(SubscriberStatus::Banned) => {
                    subs.remove(&id);
                    store.save_subscribers(&subs)?;
                    Ok(format!("{endpoint} is no longer banned, and is not subscribed\n"))
                }
                Some(_) => bail!("{endpoint} is not banned"),
                None => bail!("{endpoint} is not on the roster"),
            }
        }

        Command::Deny { endpoint } => {
            let id = parse_endpoint(&endpoint)?;
            let _flock = store.lock()?;
            let mut subs = store.subscribers()?;
            if subs.remove(&id).is_none() {
                bail!("{endpoint} is not on the roster");
            }
            store.save_subscribers(&subs)?;
            Ok(format!("{endpoint} removed\n"))
        }

        Command::Tier { endpoint, tier } => {
            let id = parse_endpoint(&endpoint)?;
            let _flock = store.lock()?;
            let mut subs = store.subscribers()?;
            let subscriber = subs
                .get_mut(&id)
                .with_context(|| format!("{endpoint} is not on the roster"))?;
            subscriber.tier = tier.clone();
            store.save_subscribers(&subs)?;
            Ok(format!("{endpoint} is now tier {tier}\n"))
        }

        Command::Name { endpoint, name } => {
            let id = parse_endpoint(&endpoint)?;
            let trimmed = name.trim();
            if trimmed.chars().any(|c| c.is_control()) {
                bail!("name contains control characters");
            }
            if trimmed.len() > 128 {
                bail!("name longer than 128 bytes");
            }
            let _flock = store.lock()?;
            let mut subs = store.subscribers()?;
            let subscriber = subs
                .get_mut(&id)
                .with_context(|| format!("{endpoint} is not on the roster"))?;
            subscriber.name = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
            let msg = match &subscriber.name {
                Some(n) => format!("{endpoint} is now named {n}\n"),
                None => format!("{endpoint}'s name was cleared\n"),
            };
            store.save_subscribers(&subs)?;
            Ok(msg)
        }

        Command::Add { endpoint, tier } => {
            let id = parse_endpoint(&endpoint)?;
            let _flock = store.lock()?;
            let mut subs = store.subscribers()?;
            if subs.get(&id).is_some() {
                bail!("{endpoint} is already on the roster");
            }
            let mut subscriber = Subscriber::new(id, tier);
            subscriber.status = SubscriberStatus::Active;
            subs.insert(subscriber);
            store.save_subscribers(&subs)?;
            Ok(format!("{endpoint} added and active\n"))
        }

        Command::ResetQuota { endpoint } => {
            let id = parse_endpoint(&endpoint)?;
            let _flock = store.lock()?;
            let mut subs = store.subscribers()?;
            let subscriber = subs
                .get_mut(&id)
                .with_context(|| format!("{endpoint} is not on the roster"))?;
            subscriber.open_log.clear();
            store.save_subscribers(&subs)?;
            Ok(format!("{endpoint} may open a room again now\n"))
        }

        Command::Pending => {
            let pending = store.pending()?;
            if pending.0.is_empty() {
                return Ok("nothing is waiting\n".to_string());
            }
            let mut rows = vec![[
                "ID".to_string(),
                "REQUESTED".to_string(),
                "KIND".to_string(),
                "ENDPOINT".to_string(),
                "NOTE".to_string(),
            ]];
            for signal in &pending.0 {
                rows.push([
                    signal.id.clone(),
                    signal.requested_at.format("%Y-%m-%d %H:%M").to_string(),
                    // Which room they asked for, so releasing one is never a
                    // surprise about which backend is about to be started.
                    signal.kind.to_string(),
                    signal.endpoint.to_string(),
                    signal.note.as_deref().unwrap_or("-").to_string(),
                ]);
            }
            Ok(table(&rows))
        }

        Command::ApproveRequest { id } => {
            let _flock = store.lock()?;
            let mut pending = store.pending()?;
            let signal = pending
                .0
                .iter_mut()
                .find(|p| p.id == id)
                .with_context(|| format!("no held signal with id {id}"))?;
            signal.approved = true;
            let endpoint = signal.endpoint.clone();
            store.save_pending(&pending)?;
            Ok(format!(
                "released {id} from {endpoint}; the daemon provisions it within five seconds\n"
            ))
        }

        Command::DenyRequest { id } => {
            let _flock = store.lock()?;
            let mut pending = store.pending()?;
            let before = pending.0.len();
            pending.0.retain(|p| p.id != id);
            if pending.0.len() == before {
                bail!("no held signal with id {id}");
            }
            store.save_pending(&pending)?;
            Ok(format!("discarded {id}\n"))
        }

        Command::Pause => {
            set_runtime(store, |r| r.paused = true)?;
            Ok("paused: no new rooms will be accepted\n".to_string())
        }

        Command::Resume => {
            set_runtime(store, |r| r.paused = false)?;
            Ok("resumed\n".to_string())
        }

        Command::Close => {
            if store.room()?.is_none() {
                return Ok("nothing is live\n".to_string());
            }
            set_runtime(store, |r| r.close_requested = true)?;
            Ok("close requested; the daemon tears it down within five seconds\n".to_string())
        }

        Command::Status => {
            let runtime = store.runtime()?;
            let subs = store.subscribers()?;
            let active = subs
                .iter()
                .filter(|s| s.status == SubscriberStatus::Active)
                .count();
            let pending_subs = subs
                .iter()
                .filter(|s| s.status == SubscriberStatus::Pending)
                .count();

            let mut out = String::new();
            out.push_str(&format!(
                "roster:  {active} active, {pending_subs} pending, {} total\n",
                subs.len()
            ));
            out.push_str(&format!("held:    {} requests\n", store.pending()?.0.len()));
            out.push_str(&format!(
                "paused:  {}\n",
                if runtime.paused { "yes" } else { "no" }
            ));
            match store.room() {
                Ok(Some(room)) => {
                    // Formatted rather than printed straight: a chrono
                    // `DateTime<Utc>` displays to the nanosecond, and nine
                    // digits of precision on a two-hour deadline is noise in
                    // the one report an admin reads while something is wrong.
                    out.push_str(&format!(
                        "live:    {} room, {} until {} UTC\n",
                        room.provider_kind(),
                        room.id(),
                        room.expires_at.format("%Y-%m-%d %H:%M")
                    ));
                    match subs.get(&room.opened_by).and_then(|s| s.name.as_deref()) {
                        Some(n) => {
                            out.push_str(&format!("host:    {} ({n})\n", room.opened_by))
                        }
                        None => out.push_str(&format!("host:    {}\n", room.opened_by)),
                    }
                }
                Ok(None) => out.push_str("live:    nothing\n"),
                // A room.json written before the provider split cannot be
                // parsed. Say so plainly instead of failing the whole status
                // report, which is the one command an admin runs when
                // something is already wrong.
                Err(e) => out.push_str(&format!(
                    "live:    unreadable room.json ({e}). The daemon clears this at startup; \n\
                     \t or delete it by hand and stop any leftover room yourself.\n"
                )),
            }
            Ok(out)
        }
    }
}

fn parse_endpoint(raw: &str) -> Result<EndpointId> {
    raw.parse()
        .with_context(|| format!("{raw:?} is not a transport:address endpoint"))
}

fn parse_status(raw: &str) -> Result<SubscriberStatus> {
    match raw.to_ascii_lowercase().as_str() {
        "pending" => Ok(SubscriberStatus::Pending),
        "active" => Ok(SubscriberStatus::Active),
        "banned" => Ok(SubscriberStatus::Banned),
        other => bail!("unknown status {other:?}, expected pending, active or banned"),
    }
}

/// Sets a status, optionally requiring that the endpoint already exists.
fn set_status(
    store: &Store,
    endpoint: &str,
    status: SubscriberStatus,
    must_exist: bool,
) -> Result<()> {
    let id = parse_endpoint(endpoint)?;
    let _flock = store.lock()?;
    let mut subs = store.subscribers()?;

    match subs.get_mut(&id) {
        Some(subscriber) => subscriber.status = status,
        None if must_exist => bail!("{endpoint} is not on the roster"),
        None => {
            // Banning somebody who never subscribed is legitimate: it blocks
            // them before they ever ask.
            let mut subscriber = Subscriber::new(id, "banned");
            subscriber.status = status;
            subs.insert(subscriber);
        }
    }
    store.save_subscribers(&subs)?;
    Ok(())
}

fn set_runtime(store: &Store, edit: impl FnOnce(&mut Runtime)) -> Result<()> {
    let _flock = store.lock()?;
    let mut runtime = store.runtime()?;
    edit(&mut runtime);
    store.save_runtime(&runtime)?;
    Ok(())
}

/// Lays rows out in columns sized to their own content.
///
/// Row 0 is the header. Widths are measured rather than hardcoded because an
/// `apprise:` endpoint is routinely longer than any fixed guess: the previous
/// `{:<28}` was blown out by `apprise:ntfy://ntfy.sh/party-line-pager` and took the
/// rest of the line's alignment with it. The last column is never padded, so
/// nothing trails whitespace.
fn table<const N: usize>(rows: &[[String; N]]) -> String {
    let mut widths = [0usize; N];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }

    let mut out = String::new();
    for row in rows {
        let line: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                if i == N - 1 {
                    cell.clone()
                } else {
                    format!("{cell:<width$}", width = widths[i])
                }
            })
            .collect();
        out.push_str(line.join("  ").trim_end());
        out.push('\n');
    }
    out
}

fn render_roster(subs: &Subscribers, filter: Option<SubscriberStatus>) -> String {
    let mut rows = vec![[
        "STATUS".to_string(),
        "TIER".to_string(),
        "ENDPOINT".to_string(),
        "TZ".to_string(),
        "QUIET".to_string(),
        "ROOMS".to_string(),
        "LAST ROOM".to_string(),
        "NAME".to_string(),
    ]];

    for subscriber in subs.iter() {
        if filter.is_some_and(|want| want != subscriber.status) {
            continue;
        }
        rows.push([
            status_word(subscriber.status).to_string(),
            subscriber.tier.clone(),
            subscriber.endpoint.to_string(),
            subscriber.tz.as_deref().unwrap_or("-").to_string(),
            subscriber
                .quiet
                .map(|w| w.to_string())
                .unwrap_or_else(|| "-".into()),
            subscriber.open_log.len().to_string(),
            subscriber
                .open_log
                .last()
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "never".into()),
            subscriber.name.as_deref().unwrap_or("-").to_string(),
        ]);
    }

    // Only the header survived the filter, so there is nothing to head.
    if rows.len() == 1 {
        return "nobody\n".to_string();
    }
    table(&rows)
}

/// Spelled out rather than derived from `Debug`, matching `render` in the core
/// crate: renaming a variant should not silently rewrite admin output that
/// scripts and `party-line-pager.sh` both parse.
fn status_word(status: SubscriberStatus) -> &'static str {
    match status {
        SubscriberStatus::Pending => "pending",
        SubscriberStatus::Active => "active",
        SubscriberStatus::Banned => "banned",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use party_line_pager_core::state::{PendingRequest, Pending, Room};
    use chrono::Utc;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    fn subscribe(store: &Store, endpoint: &str, status: SubscriberStatus) {
        let mut subs = store.subscribers().unwrap();
        let mut s = Subscriber::new(endpoint.parse().unwrap(), "weekly");
        s.status = status;
        subs.insert(s);
        store.save_subscribers(&subs).unwrap();
    }

    fn status_of(store: &Store, endpoint: &str) -> Option<SubscriberStatus> {
        store
            .subscribers()
            .unwrap()
            .get(&endpoint.parse().unwrap())
            .map(|s| s.status)
    }

    #[test]
    fn approve_moves_a_pending_subscriber_onto_the_roster() {
        let (_d, store) = store();
        subscribe(&store, "telegram:1", SubscriberStatus::Pending);

        let out = run(
            &store,
            Command::Approve {
                endpoint: "telegram:1".into(),
            },
        )
        .unwrap();

        assert!(out.contains("on the roster"));
        assert_eq!(status_of(&store, "telegram:1"), Some(SubscriberStatus::Active));
    }

    #[test]
    fn approving_a_stranger_is_an_error() {
        let (_d, store) = store();
        assert!(run(
            &store,
            Command::Approve {
                endpoint: "telegram:404".into()
            }
        )
        .is_err());
    }

    #[test]
    fn ban_works_even_for_somebody_who_never_subscribed() {
        let (_d, store) = store();
        run(
            &store,
            Command::Ban {
                endpoint: "telegram:9".into(),
            },
        )
        .unwrap();
        assert_eq!(status_of(&store, "telegram:9"), Some(SubscriberStatus::Banned));
    }

    #[test]
    fn unban_removes_the_entry_rather_than_activating_it() {
        let (_d, store) = store();
        subscribe(&store, "telegram:1", SubscriberStatus::Banned);

        run(
            &store,
            Command::Unban {
                endpoint: "telegram:1".into(),
            },
        )
        .unwrap();

        assert_eq!(
            status_of(&store, "telegram:1"),
            None,
            "unban must not silently subscribe somebody"
        );
    }

    #[test]
    fn add_creates_an_active_apprise_endpoint() {
        let (_d, store) = store();
        let out = run(
            &store,
            Command::Add {
                endpoint: "apprise:ntfy://ntfy.sh/party-line-pager".into(),
                tier: "trusted".into(),
            },
        )
        .unwrap();

        assert!(out.contains("active"));
        let subs = store.subscribers().unwrap();
        let added = subs
            .get(&"apprise:ntfy://ntfy.sh/party-line-pager".parse().unwrap())
            .unwrap();
        assert_eq!(added.status, SubscriberStatus::Active);
        assert_eq!(added.tier, "trusted");
    }

    #[test]
    fn add_refuses_duplicates_and_malformed_endpoints() {
        let (_d, store) = store();
        subscribe(&store, "telegram:1", SubscriberStatus::Active);
        assert!(run(
            &store,
            Command::Add {
                endpoint: "telegram:1".into(),
                tier: "weekly".into()
            }
        )
        .is_err());
        assert!(run(
            &store,
            Command::Add {
                endpoint: "no-transport".into(),
                tier: "weekly".into()
            }
        )
        .is_err());
    }

    #[test]
    fn tier_and_reset_quota_edit_one_subscriber() {
        let (_d, store) = store();
        subscribe(&store, "telegram:1", SubscriberStatus::Active);
        {
            let mut subs = store.subscribers().unwrap();
            subs.get_mut(&"telegram:1".parse().unwrap()).unwrap().open_log = vec![Utc::now()];
            store.save_subscribers(&subs).unwrap();
        }

        run(
            &store,
            Command::Tier {
                endpoint: "telegram:1".into(),
                tier: "trusted".into(),
            },
        )
        .unwrap();
        run(
            &store,
            Command::ResetQuota {
                endpoint: "telegram:1".into(),
            },
        )
        .unwrap();

        let subs = store.subscribers().unwrap();
        let s = subs.get(&"telegram:1".parse().unwrap()).unwrap();
        assert_eq!(s.tier, "trusted");
        assert!(s.open_log.is_empty());
    }

    #[test]
    fn who_lists_and_filters() {
        let (_d, store) = store();
        subscribe(&store, "telegram:1", SubscriberStatus::Active);
        subscribe(&store, "telegram:2", SubscriberStatus::Pending);

        let all = run(&store, Command::Who { status: None }).unwrap();
        assert!(all.contains("telegram:1") && all.contains("telegram:2"));

        let pending = run(
            &store,
            Command::Who {
                status: Some("pending".into()),
            },
        )
        .unwrap();
        assert!(pending.contains("telegram:2"));
        assert!(!pending.contains("telegram:1"));

        assert!(run(
            &store,
            Command::Who {
                status: Some("asleep".into())
            }
        )
        .is_err());
    }

    #[test]
    fn who_says_nobody_on_an_empty_roster() {
        let (_d, store) = store();
        assert_eq!(run(&store, Command::Who { status: None }).unwrap(), "nobody\n");
    }

    #[test]
    fn the_roster_stays_in_columns_when_an_apprise_endpoint_is_long() {
        // The old fixed `{:<28}` endpoint column was narrower than a routine
        // apprise: URL, so one long row pushed every column after it out of
        // alignment for the whole table.
        let (_d, store) = store();
        subscribe(&store, "telegram:1", SubscriberStatus::Active);
        subscribe(
            &store,
            "apprise:ntfy://ntfy.sh/a-deliberately-long-topic-name",
            SubscriberStatus::Active,
        );

        let out = run(&store, Command::Who { status: None }).unwrap();
        let lines: Vec<&str> = out.lines().collect();

        assert!(lines[0].starts_with("STATUS"), "a header row: {out}");
        assert_eq!(lines.len(), 3, "header plus both subscribers: {out}");

        // Every data row's tz value has to begin exactly where the header's
        // TZ column begins, whatever the endpoint above it did to the width.
        let tz_column = lines[0].find("TZ").expect("header names the tz column");
        for line in &lines[1..] {
            let (before, from_tz) = line.split_at(tz_column);
            assert!(
                before.ends_with("  ") && !from_tz.starts_with(' '),
                "tz column does not start at {tz_column} in {line:?}\n{out}"
            );
        }
    }

    #[test]
    fn approve_signal_flips_the_flag_the_daemon_polls_for() {
        let (_d, store) = store();
        store
            .save_pending(&Pending(vec![PendingRequest {
                id: "abc123".into(),
                endpoint: "telegram:1".parse().unwrap(),
                kind: party_line_pager_core::ProviderKind::Web,
                note: Some("after work".into()),
                requested_at: Utc::now(),
                approved: false,
            }]))
            .unwrap();

        let listed = run(&store, Command::Pending).unwrap();
        assert!(listed.contains("abc123") && listed.contains("after work"));
        assert!(listed.contains("web"), "the held verb is shown: {listed}");

        run(
            &store,
            Command::ApproveRequest {
                id: "abc123".into(),
            },
        )
        .unwrap();
        assert!(store.pending().unwrap().0[0].approved);
    }

    #[test]
    fn deny_signal_drops_it() {
        let (_d, store) = store();
        store
            .save_pending(&Pending(vec![PendingRequest {
                id: "abc123".into(),
                endpoint: "telegram:1".parse().unwrap(),
                kind: party_line_pager_core::ProviderKind::Tor,
                note: None,
                requested_at: Utc::now(),
                approved: false,
            }]))
            .unwrap();

        run(&store, Command::DenyRequest { id: "abc123".into() }).unwrap();
        assert!(store.pending().unwrap().0.is_empty());
        assert!(run(&store, Command::DenyRequest { id: "abc123".into() }).is_err());
    }

    #[test]
    fn pause_and_resume_toggle_the_runtime_switch() {
        let (_d, store) = store();
        run(&store, Command::Pause).unwrap();
        assert!(store.runtime().unwrap().paused);
        run(&store, Command::Resume).unwrap();
        assert!(!store.runtime().unwrap().paused);
    }

    #[test]
    fn close_only_requests_when_something_is_live() {
        let (_d, store) = store();
        let out = run(&store, Command::Close).unwrap();
        assert_eq!(out, "nothing is live\n");
        assert!(!store.runtime().unwrap().close_requested);

        store
            .save_room(&Room {
                kind: party_line_pager_core::RoomKind::Partyline {
                    transport: party_line_pager_core::Transport::Tor,
                    address: "abc.onion".into(),
                    secret: "S".into(),
                },
                opened_by: "telegram:1".parse().unwrap(),
                note: None,
                started_at: Utc::now(),
                expires_at: Utc::now() + chrono::Duration::hours(1),
            })
            .unwrap();

        run(&store, Command::Close).unwrap();
        assert!(store.runtime().unwrap().close_requested);
    }

    #[test]
    fn status_summarizes_the_instance() {
        let (_d, store) = store();
        subscribe(&store, "telegram:1", SubscriberStatus::Active);
        subscribe(&store, "telegram:2", SubscriberStatus::Pending);
        run(&store, Command::Pause).unwrap();

        let out = run(&store, Command::Status).unwrap();
        assert!(out.contains("1 active, 1 pending, 2 total"), "{out}");
        assert!(out.contains("paused:  yes"), "{out}");
        assert!(out.contains("live:    nothing"), "{out}");
    }
}
