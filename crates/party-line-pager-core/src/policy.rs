//! Policy evaluation: who may open a room, who receives the invite, and when.
//!
//! Every function here is pure. Given the same policy, roster, room and clock
//! they return the same decision, which is what makes the rules testable
//! without a chat network, a Tor daemon, or a real clock.

use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::config::{Policy, Signups, Tier};
use crate::endpoint::EndpointId;
use crate::state::{Room, Subscriber, SubscriberStatus, Subscribers};

/// What the daemon should do with a `signal` command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenOutcome {
    /// Run the provider hook and fan out.
    Provision,
    /// A room is already up. Re-send the creds to the host only, and do not
    /// spend their quota: they asked for a room that already exists.
    AlreadyLive(Box<Room>),
    /// Park it in `pending.json` for `party_line_pagerctl approve-request`.
    Hold,
    Reject(OpenRejection),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenRejection {
    /// Not on the roster at all.
    NotSubscribed,
    /// On the roster, waiting for an admin.
    AwaitingApproval,
    Banned,
    /// The tier is receive-only.
    TierForbids,
    /// Rolling window has not elapsed.
    QuotaExhausted { retry_after: Duration },
    /// Instance-wide kill switch.
    Paused,
}

/// Decides what happens when `endpoint` sends `signal`.
///
/// Checks run in this order, and the first one that fires wins:
/// roster status, pause switch, tier rights, an already-live room, quota, then
/// hold-for-approval. A banned endpoint therefore learns nothing about whether
/// a room is up.
pub fn decide_open(
    policy: &Policy,
    subscriber: Option<&Subscriber>,
    room: Option<&Room>,
    paused: bool,
    now: DateTime<Utc>,
) -> OpenOutcome {
    let Some(subscriber) = subscriber else {
        return OpenOutcome::Reject(OpenRejection::NotSubscribed);
    };

    match subscriber.status {
        SubscriberStatus::Banned => return OpenOutcome::Reject(OpenRejection::Banned),
        SubscriberStatus::Pending => {
            return OpenOutcome::Reject(OpenRejection::AwaitingApproval)
        }
        SubscriberStatus::Active => {}
    }

    if paused {
        return OpenOutcome::Reject(OpenRejection::Paused);
    }

    let tier = policy.tier_or_default(&subscriber.tier);
    if !tier.may_open {
        return OpenOutcome::Reject(OpenRejection::TierForbids);
    }

    if let Some(room) = room {
        if room.is_live_at(now) {
            return OpenOutcome::AlreadyLive(Box::new(room.clone()));
        }
    }

    if let Some(retry_after) = quota_remaining(tier, &subscriber.open_log, now) {
        return OpenOutcome::Reject(OpenRejection::QuotaExhausted { retry_after });
    }

    if tier.hold_for_approval {
        return OpenOutcome::Hold;
    }

    OpenOutcome::Provision
}

/// What the daemon should do with a `close` command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloseOutcome {
    /// Tear the live room down.
    Torn,
    Reject(CloseRejection),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloseRejection {
    /// Not on the roster at all.
    NotSubscribed,
    /// On the roster, waiting for an admin.
    AwaitingApproval,
    Banned,
    /// The tier does not allow closing a room it opened.
    TierForbids,
    /// Nothing is live to close.
    NothingLive,
    /// Live, but opened by somebody else.
    NotYourRoom,
}

/// Decides what happens when `endpoint` sends `close`.
///
/// Checks run in this order, and the first one that fires wins: roster
/// status, tier rights, whether anything is live, then ownership. Only the
/// endpoint that opened a room may close it early; there is no moderator
/// override and no transfer of that right to anybody else.
pub fn decide_close(
    policy: &Policy,
    subscriber: Option<&Subscriber>,
    room: Option<&Room>,
    endpoint: &EndpointId,
    now: DateTime<Utc>,
) -> CloseOutcome {
    let Some(subscriber) = subscriber else {
        return CloseOutcome::Reject(CloseRejection::NotSubscribed);
    };

    match subscriber.status {
        SubscriberStatus::Banned => return CloseOutcome::Reject(CloseRejection::Banned),
        SubscriberStatus::Pending => {
            return CloseOutcome::Reject(CloseRejection::AwaitingApproval)
        }
        SubscriberStatus::Active => {}
    }

    let tier = policy.tier_or_default(&subscriber.tier);
    if !tier.may_close {
        return CloseOutcome::Reject(CloseRejection::TierForbids);
    }

    let Some(room) = room.filter(|r| r.is_live_at(now)) else {
        return CloseOutcome::Reject(CloseRejection::NothingLive);
    };

    if &room.opened_by != endpoint {
        return CloseOutcome::Reject(CloseRejection::NotYourRoom);
    }

    CloseOutcome::Torn
}

/// The entries of `open_log` that still fall inside the tier's window,
/// oldest first. Assumes `open_log` is stored in ascending order, which
/// [`record_open`] guarantees.
fn opens_in_window(window: Duration, open_log: &[DateTime<Utc>], now: DateTime<Utc>) -> &[DateTime<Utc>] {
    let Ok(window) = chrono::Duration::from_std(window) else {
        return &[];
    };
    let cutoff = now - window;
    let first_recent = open_log.partition_point(|t| *t <= cutoff);
    &open_log[first_recent..]
}

/// How many of this subscriber's rooms still count against the tier's
/// window right now. Always `0` on an unlimited (`window = "0s"`) tier.
pub fn opens_used(tier: &Tier, open_log: &[DateTime<Utc>], now: DateTime<Utc>) -> u32 {
    if tier.window.is_zero() {
        return 0;
    }
    opens_in_window(tier.window, open_log, now).len() as u32
}

/// Time left on a rolling quota window, or `None` when opening is allowed.
///
/// The window is rolling, not calendar-aligned: "up to `max_rooms` rooms per
/// 168h since the oldest one still in the window", so there is no thundering
/// herd every Monday at 00:00. A zero window means unlimited.
pub fn quota_remaining(
    tier: &Tier,
    open_log: &[DateTime<Utc>],
    now: DateTime<Utc>,
) -> Option<Duration> {
    if tier.window.is_zero() {
        return None;
    }
    let in_window = opens_in_window(tier.window, open_log, now);
    if (in_window.len() as u32) < tier.max_rooms {
        return None;
    }
    let window = chrono::Duration::from_std(tier.window).ok()?;
    let oldest = *in_window.first()?;
    let ready_at = oldest.checked_add_signed(window)?;
    if now >= ready_at {
        None
    } else {
        (ready_at - now).to_std().ok()
    }
}

/// Records a successfully opened room against the rolling window, trimming
/// that can no longer affect a future decision. Called only once a room has
/// actually come up: a held or rejected request spends nothing.
pub fn record_open(tier: &Tier, open_log: &mut Vec<DateTime<Utc>>, now: DateTime<Utc>) {
    if tier.window.is_zero() {
        // Nothing to track: this tier never consults the log.
        open_log.clear();
        return;
    }
    open_log.push(now);
    let Ok(window) = chrono::Duration::from_std(tier.window) else {
        return;
    };
    let cutoff = now - window;
    open_log.retain(|t| *t > cutoff);
}

/// What the daemon should do with a `sub` command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubscribeOutcome {
    /// Added and active immediately.
    Added,
    /// Recorded, awaiting `party_line_pagerctl approve`.
    HeldForApproval,
    /// Signups are closed.
    Closed,
    AlreadyActive,
    AlreadyPending,
    Banned,
}

pub fn decide_subscribe(policy: &Policy, existing: Option<&Subscriber>) -> SubscribeOutcome {
    match existing.map(|s| s.status) {
        Some(SubscriberStatus::Banned) => return SubscribeOutcome::Banned,
        Some(SubscriberStatus::Active) => return SubscribeOutcome::AlreadyActive,
        Some(SubscriberStatus::Pending) => return SubscribeOutcome::AlreadyPending,
        None => {}
    }

    match policy.instance.signups {
        Signups::Open => SubscribeOutcome::Added,
        Signups::Approval => SubscribeOutcome::HeldForApproval,
        Signups::Closed => SubscribeOutcome::Closed,
    }
}

/// Everyone who should receive this broadcast: active, allowed to receive by
/// their tier, and not inside their quiet window.
///
/// Subscribers filtered out for quiet hours are dropped silently. Nothing is
/// recorded about them, so the daemon never accumulates a list of who was
/// asleep at 03:00.
pub fn recipients<'a>(
    policy: &Policy,
    subs: &'a Subscribers,
    now: DateTime<Utc>,
) -> Vec<&'a Subscriber> {
    subs.iter()
        .filter(|s| s.status == SubscriberStatus::Active)
        .filter(|s| policy.tier_or_default(&s.tier).may_receive)
        .filter(|s| !s.is_quiet_at(now))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::EndpointId;
    use chrono::TimeZone;

    const POLICY: &str = r#"
        [instance]
        default_tier = "weekly"
        signups = "open"

        [provider.tor]
        up = "/hooks/up.sh"
        down = "/hooks/down.sh"

        [[tier]]
        name = "lurker"
        may_open = false

        [[tier]]
        name = "weekly"
        window = "168h"

        [[tier]]
        name = "held"
        window = "24h"
        hold_for_approval = true

        [[tier]]
        name = "trusted"
        window = "0s"

        [[tier]]
        name = "writeonly"
        may_receive = false

        [[tier]]
        name = "twice-daily"
        window = "24h"
        max_rooms = 2

        [[tier]]
        name = "no-close"
        may_close = false
    "#;

    fn policy() -> Policy {
        Policy::parse_str(POLICY).unwrap()
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 11, 12, 0, 0).unwrap()
    }

    fn sub(id: &str, tier: &str) -> Subscriber {
        let mut s = Subscriber::new(id.parse::<EndpointId>().unwrap(), tier);
        s.status = SubscriberStatus::Active;
        s
    }

    fn room_until(hours: i64) -> Room {
        Room {
            kind: crate::state::RoomKind::Partyline {
                transport: crate::config::Transport::Tor,
                address: "abc.onion".into(),
                secret: "S".into(),
            },
            opened_by: "telegram:9".parse().unwrap(),
            note: None,
            started_at: now(),
            expires_at: now() + chrono::Duration::hours(hours),
        }
    }

    #[test]
    fn a_live_room_blocks_an_open_whatever_kind_either_one_is() {
        // The one-room-at-a-time rule is deliberately provider-blind: these
        // decisions only ever ask whether something is live, never what it is.
        let p = policy();
        let s = sub("telegram:1", "weekly");
        let web = Room {
            kind: crate::state::RoomKind::Web {
                url: "https://p2p.mirotalk.com/join/abc".into(),
            },
            ..room_until(1)
        };
        assert!(matches!(
            decide_open(&p, Some(&s), Some(&web), false, now()),
            OpenOutcome::AlreadyLive(_)
        ));
    }

    #[test]
    fn active_subscriber_with_fresh_quota_provisions() {
        let p = policy();
        let s = sub("telegram:1", "weekly");
        assert_eq!(
            decide_open(&p, Some(&s), None, false, now()),
            OpenOutcome::Provision
        );
    }

    #[test]
    fn strangers_pending_and_banned_are_refused() {
        let p = policy();
        assert_eq!(
            decide_open(&p, None, None, false, now()),
            OpenOutcome::Reject(OpenRejection::NotSubscribed)
        );

        let mut pending = sub("telegram:2", "weekly");
        pending.status = SubscriberStatus::Pending;
        assert_eq!(
            decide_open(&p, Some(&pending), None, false, now()),
            OpenOutcome::Reject(OpenRejection::AwaitingApproval)
        );

        let mut banned = sub("telegram:3", "weekly");
        banned.status = SubscriberStatus::Banned;
        assert_eq!(
            decide_open(&p, Some(&banned), None, false, now()),
            OpenOutcome::Reject(OpenRejection::Banned)
        );
    }

    #[test]
    fn a_banned_endpoint_learns_nothing_about_a_live_room() {
        let p = policy();
        let mut banned = sub("telegram:3", "weekly");
        banned.status = SubscriberStatus::Banned;
        assert_eq!(
            decide_open(&p, Some(&banned), Some(&room_until(1)), false, now()),
            OpenOutcome::Reject(OpenRejection::Banned)
        );
    }

    #[test]
    fn receive_only_tiers_cannot_open() {
        let p = policy();
        let s = sub("telegram:4", "lurker");
        assert_eq!(
            decide_open(&p, Some(&s), None, false, now()),
            OpenOutcome::Reject(OpenRejection::TierForbids)
        );
    }

    #[test]
    fn pause_stops_everyone() {
        let p = policy();
        let s = sub("telegram:5", "trusted");
        assert_eq!(
            decide_open(&p, Some(&s), None, true, now()),
            OpenOutcome::Reject(OpenRejection::Paused)
        );
    }

    #[test]
    fn quota_is_rolling_and_reports_exact_remaining_time() {
        let p = policy();
        let mut s = sub("telegram:6", "weekly");

        s.open_log = vec![now() - chrono::Duration::hours(167)];
        let outcome = decide_open(&p, Some(&s), None, false, now());
        assert_eq!(
            outcome,
            OpenOutcome::Reject(OpenRejection::QuotaExhausted {
                retry_after: Duration::from_secs(3600)
            })
        );

        s.open_log = vec![now() - chrono::Duration::hours(168)];
        assert_eq!(
            decide_open(&p, Some(&s), None, false, now()),
            OpenOutcome::Provision
        );
    }

    #[test]
    fn unlimited_tier_ignores_the_open_log() {
        let p = policy();
        let mut s = sub("telegram:7", "trusted");
        s.open_log = vec![now() - chrono::Duration::seconds(1)];
        assert_eq!(
            decide_open(&p, Some(&s), None, false, now()),
            OpenOutcome::Provision
        );
    }

    #[test]
    fn a_tier_may_allow_more_than_one_room_per_window() {
        let p = policy();
        let tier = p.tier("twice-daily").unwrap();
        let mut s = sub("telegram:12", "twice-daily");

        // Nothing spent yet: both rooms are available.
        assert_eq!(
            decide_open(&p, Some(&s), None, false, now()),
            OpenOutcome::Provision
        );

        // One room nine hours ago still leaves the second available.
        s.open_log = vec![now() - chrono::Duration::hours(9)];
        assert_eq!(
            decide_open(&p, Some(&s), None, false, now()),
            OpenOutcome::Provision
        );

        // Both slots spent: blocked until the older of the two ages out.
        s.open_log = vec![
            now() - chrono::Duration::hours(20),
            now() - chrono::Duration::hours(1),
        ];
        assert_eq!(
            decide_open(&p, Some(&s), None, false, now()),
            OpenOutcome::Reject(OpenRejection::QuotaExhausted {
                retry_after: Duration::from_secs(4 * 3600)
            })
        );
        assert_eq!(opens_used(tier, &s.open_log, now()), 2);

        // The older one falls out of the 24h window: one slot frees up.
        s.open_log = vec![
            now() - chrono::Duration::hours(25),
            now() - chrono::Duration::hours(1),
        ];
        assert_eq!(
            decide_open(&p, Some(&s), None, false, now()),
            OpenOutcome::Provision
        );
        assert_eq!(opens_used(tier, &s.open_log, now()), 1);
    }

    #[test]
    fn record_open_trims_entries_that_left_the_window() {
        let tier = policy().tier("twice-daily").unwrap().clone();
        let mut log = vec![now() - chrono::Duration::hours(25)];
        record_open(&tier, &mut log, now());
        assert_eq!(log, vec![now()], "the stale entry should have been dropped");
    }

    #[test]
    fn record_open_on_an_unlimited_tier_keeps_no_history() {
        let tier = policy().tier("trusted").unwrap().clone();
        let mut log = vec![now() - chrono::Duration::seconds(1)];
        record_open(&tier, &mut log, now());
        assert!(log.is_empty());
    }

    #[test]
    fn a_live_room_is_returned_instead_of_provisioning_a_second_one() {
        let p = policy();
        let s = sub("telegram:8", "weekly");
        let room = room_until(1);
        assert_eq!(
            decide_open(&p, Some(&s), Some(&room), false, now()),
            OpenOutcome::AlreadyLive(Box::new(room))
        );
    }

    #[test]
    fn an_expired_room_does_not_block_a_new_one() {
        let p = policy();
        let s = sub("telegram:8", "weekly");
        let stale = room_until(-1);
        assert_eq!(
            decide_open(&p, Some(&s), Some(&stale), false, now()),
            OpenOutcome::Provision
        );
    }

    #[test]
    fn a_live_room_does_not_burn_quota_but_an_exhausted_quota_still_gets_the_creds() {
        // Quota is checked after the live-room shortcut, so somebody who
        // already spent their room this week can still be told where the
        // party is.
        let p = policy();
        let mut s = sub("telegram:8", "weekly");
        s.open_log = vec![now() - chrono::Duration::hours(1)];
        let room = room_until(1);
        assert_eq!(
            decide_open(&p, Some(&s), Some(&room), false, now()),
            OpenOutcome::AlreadyLive(Box::new(room))
        );
    }

    #[test]
    fn held_tiers_park_for_approval_only_when_quota_allows() {
        let p = policy();
        let mut s = sub("telegram:9", "held");
        assert_eq!(
            decide_open(&p, Some(&s), None, false, now()),
            OpenOutcome::Hold
        );

        s.open_log = vec![now() - chrono::Duration::hours(1)];
        assert!(matches!(
            decide_open(&p, Some(&s), None, false, now()),
            OpenOutcome::Reject(OpenRejection::QuotaExhausted { .. })
        ));
    }

    #[test]
    fn a_deleted_tier_falls_back_to_the_default_tier() {
        let p = policy();
        let s = sub("telegram:10", "tier-that-was-deleted");
        // The default tier is "weekly", which may open.
        assert_eq!(
            decide_open(&p, Some(&s), None, false, now()),
            OpenOutcome::Provision
        );
    }

    #[test]
    fn subscribe_honours_the_signups_switch() {
        let mut p = policy();
        assert_eq!(decide_subscribe(&p, None), SubscribeOutcome::Added);

        p.instance.signups = Signups::Approval;
        assert_eq!(decide_subscribe(&p, None), SubscribeOutcome::HeldForApproval);

        p.instance.signups = Signups::Closed;
        assert_eq!(decide_subscribe(&p, None), SubscribeOutcome::Closed);
    }

    #[test]
    fn a_ban_survives_a_resubscribe_even_with_open_signups() {
        let p = policy();
        let mut banned = sub("telegram:11", "weekly");
        banned.status = SubscriberStatus::Banned;
        assert_eq!(
            decide_subscribe(&p, Some(&banned)),
            SubscribeOutcome::Banned
        );
    }

    #[test]
    fn recipients_exclude_pending_banned_quiet_and_write_only() {
        let p = policy();
        let mut subs = Subscribers::default();

        subs.insert(sub("telegram:active", "weekly"));

        let mut pending = sub("telegram:pending", "weekly");
        pending.status = SubscriberStatus::Pending;
        subs.insert(pending);

        let mut banned = sub("telegram:banned", "weekly");
        banned.status = SubscriberStatus::Banned;
        subs.insert(banned);

        subs.insert(sub("telegram:writeonly", "writeonly"));

        let mut asleep = sub("telegram:asleep", "weekly");
        asleep.tz = Some("America/Denver".into());
        asleep.quiet = Some("23:00-07:00".parse().unwrap());
        subs.insert(asleep);

        // 2026-08-11 08:00Z is 02:00 in Denver.
        let at = Utc.with_ymd_and_hms(2026, 8, 11, 8, 0, 0).unwrap();
        let got: Vec<String> = recipients(&p, &subs, at)
            .iter()
            .map(|s| s.endpoint.to_string())
            .collect();
        assert_eq!(got, vec!["telegram:active"]);

        // Six hours later Denver is awake and gets the same signal.
        let later = Utc.with_ymd_and_hms(2026, 8, 11, 14, 0, 0).unwrap();
        let got: Vec<String> = recipients(&p, &subs, later)
            .iter()
            .map(|s| s.endpoint.to_string())
            .collect();
        assert_eq!(got, vec!["telegram:active", "telegram:asleep"]);
    }

    #[test]
    fn the_host_may_close_their_own_live_room() {
        let p = policy();
        let s = sub("telegram:9", "weekly");
        assert_eq!(
            decide_close(&p, Some(&s), Some(&room_until(1)), &"telegram:9".parse().unwrap(), now()),
            CloseOutcome::Torn
        );
    }

    #[test]
    fn nobody_else_may_close_it() {
        let p = policy();
        let s = sub("telegram:1", "weekly");
        assert_eq!(
            decide_close(&p, Some(&s), Some(&room_until(1)), &"telegram:1".parse().unwrap(), now()),
            CloseOutcome::Reject(CloseRejection::NotYourRoom)
        );
    }

    #[test]
    fn closing_with_nothing_live_is_refused() {
        let p = policy();
        let s = sub("telegram:9", "weekly");
        assert_eq!(
            decide_close(&p, Some(&s), None, &"telegram:9".parse().unwrap(), now()),
            CloseOutcome::Reject(CloseRejection::NothingLive)
        );

        // An expired room does not count as live for closing either.
        assert_eq!(
            decide_close(
                &p,
                Some(&s),
                Some(&room_until(-1)),
                &"telegram:9".parse().unwrap(),
                now()
            ),
            CloseOutcome::Reject(CloseRejection::NothingLive)
        );
    }

    #[test]
    fn a_tier_can_be_denied_close_rights_even_over_its_own_room() {
        let p = policy();
        let s = sub("telegram:9", "no-close");
        assert_eq!(
            decide_close(&p, Some(&s), Some(&room_until(1)), &"telegram:9".parse().unwrap(), now()),
            CloseOutcome::Reject(CloseRejection::TierForbids)
        );
    }

    #[test]
    fn strangers_pending_and_banned_cannot_close_either() {
        let p = policy();
        let endpoint: EndpointId = "telegram:9".parse().unwrap();
        assert_eq!(
            decide_close(&p, None, Some(&room_until(1)), &endpoint, now()),
            CloseOutcome::Reject(CloseRejection::NotSubscribed)
        );

        let mut pending = sub("telegram:9", "weekly");
        pending.status = SubscriberStatus::Pending;
        assert_eq!(
            decide_close(&p, Some(&pending), Some(&room_until(1)), &endpoint, now()),
            CloseOutcome::Reject(CloseRejection::AwaitingApproval)
        );

        let mut banned = sub("telegram:9", "weekly");
        banned.status = SubscriberStatus::Banned;
        assert_eq!(
            decide_close(&p, Some(&banned), Some(&room_until(1)), &endpoint, now()),
            CloseOutcome::Reject(CloseRejection::Banned)
        );
    }
}
