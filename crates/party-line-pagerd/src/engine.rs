//! Command handling, provisioning, and the TTL clock.
//!
//! The engine is the only place that mutates state. Adapters hand it messages
//! and it hands adapters replies, so an adapter never makes a policy decision.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use party_line_pager_core::command::{Command, QuietArg};
use party_line_pager_core::policy::{self, CloseOutcome, OpenOutcome, SubscribeOutcome};
use party_line_pager_core::state::{PendingRequest, Room, RoomKind, SubscriberStatus};
use party_line_pager_core::{
    config::Policy, quiet, render, secret, CredsMode, EndpointId, ProviderKind, Store, Subscriber,
    Subscribers,
};
use chrono::Utc;

use crate::fanout::Fanout;
use crate::provider::{PartylineOutput, ProviderRunner, WebOutput};
use crate::transport::{Incoming, OutMessage};
use crate::yopass::{LinkCache, YopassClient};

/// How often the daemon re-reads `pending.json` looking for admin decisions.
/// There is no socket and no API: an admin edits state over SSH and the daemon
/// notices.
pub const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// The hook pairs this daemon can run, one per provider kind.
///
/// A missing entry is not an error anywhere: it means this instance does not
/// offer that kind of room, which is the whole "restrict to one provider"
/// switch. Every lookup therefore returns an `Option` and every caller has to
/// say what it does when the answer is `None`, rather than unwrapping and
/// taking the daemon down over a config edit.
///
/// Held as a list rather than one field per kind so that offering a new
/// network is a line in `main.rs` and nothing else.
#[derive(Default)]
pub struct Providers(Vec<(ProviderKind, ProviderRunner)>);

impl Providers {
    pub fn get(&self, kind: ProviderKind) -> Option<&ProviderRunner> {
        self.0
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, runner)| runner)
    }

    pub fn insert(&mut self, kind: ProviderKind, runner: ProviderRunner) {
        self.0.push((kind, runner));
    }
}

impl FromIterator<(ProviderKind, ProviderRunner)> for Providers {
    fn from_iter<I: IntoIterator<Item = (ProviderKind, ProviderRunner)>>(iter: I) -> Self {
        Providers(iter.into_iter().collect())
    }
}

pub struct Engine {
    policy: Policy,
    store: Store,
    fanout: Arc<Fanout>,
    providers: Providers,
    /// Serializes read-modify-write cycles inside this process. The `flock` in
    /// [`Store`] serializes against `party_line_pagerctl` in another process.
    state_lock: tokio::sync::Mutex<()>,
    /// Guarantees one provisioning at a time, so two people raising a room
    /// simultaneously cannot start two of them. Shared across both providers on
    /// purpose: the rule is one room per instance, not one room per kind.
    provision_lock: tokio::sync::Mutex<()>,
    /// Minted Yopass links for the live room, one per recipient. See
    /// [`crate::yopass::LinkCache`] for why this has to be a cache and not a
    /// mint-on-every-render call.
    link_cache: Arc<LinkCache>,
}

impl Engine {
    pub fn new(
        policy: Policy,
        store: Store,
        fanout: Arc<Fanout>,
        providers: Providers,
    ) -> Arc<Self> {
        Arc::new(Self {
            policy,
            store,
            fanout,
            providers,
            state_lock: tokio::sync::Mutex::new(()),
            provision_lock: tokio::sync::Mutex::new(()),
            link_cache: Arc::new(LinkCache::new()),
        })
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Handles one inbound message. Never returns an error: a bad message from
    /// one stranger must not disturb anyone else.
    pub async fn handle(self: &Arc<Self>, incoming: Incoming) {
        let Some(command) = party_line_pager_core::command::parse(&incoming.text) else {
            tracing::trace!(endpoint = %incoming.endpoint, "ignored unrecognized message");
            return;
        };

        // A verb this instance has no hooks for is not a command here at
        // all, so it gets the same treatment as any other word a stranger
        // types: silence, and no log line implying somebody tried something.
        let command = match command {
            Command::Open(kind, note) => match self.resolve_target(kind) {
                Some(kind) => Command::Open(kind, note),
                None => {
                    tracing::trace!(
                        endpoint = %incoming.endpoint,
                        "ignored a request for a provider this instance does not offer"
                    );
                    return;
                }
            },
            other => other,
        };

        tracing::info!(endpoint = %incoming.endpoint, ?command, "command");

        let result = match command {
            Command::Help(topic) => {
                let doc = render::help(&self.policy, topic.as_deref());
                self.reply(&incoming.endpoint, doc).await;
                Ok(())
            }
            Command::Wiki(topic) => {
                let doc = render::wiki(topic.as_deref());
                self.reply(&incoming.endpoint, doc).await;
                Ok(())
            }
            Command::Subscribe => self.subscribe(&incoming.endpoint).await,
            Command::Unsubscribe => self.unsubscribe(&incoming.endpoint).await,
            Command::Tz(zone) => self.set_timezone(&incoming.endpoint, &zone).await,
            Command::Quiet(arg) => self.quiet(&incoming.endpoint, arg).await,
            Command::Status => self.status(&incoming.endpoint).await,
            Command::Close => self.close(&incoming.endpoint).await,
            Command::Open(kind, note) => self.open(&incoming.endpoint, kind, note).await,
        };

        if let Err(e) = result {
            tracing::error!(endpoint = %incoming.endpoint, error = ?e, "command failed");
            self.reply(
                &incoming.endpoint,
                "Something broke on my side. An admin has the logs.".to_string(),
            )
            .await;
        }
    }

    /// Whether this instance actually offers the named provider. `None` means
    /// "we do not do that here", which the caller treats as silence rather
    /// than an error.
    fn resolve_target(&self, kind: ProviderKind) -> Option<ProviderKind> {
        self.policy.provider.supports(kind).then_some(kind)
    }

    /// The subscriber's timezone, for the replies that quote a closing time.
    /// `None` for a stranger, or for somebody who never sent `tz`.
    ///
    /// Read without the flock, matching the neighbouring unlocked reads: a
    /// timezone that is one poll out of date costs a reader nothing, and
    /// taking the lock here would mean holding it across a reply that can run
    /// as long as the whole fanout timeout.
    fn tz_of(&self, endpoint: &EndpointId) -> Option<String> {
        self.store
            .subscribers()
            .ok()?
            .get(endpoint)
            .and_then(|subscriber| subscriber.tz.clone())
    }

    async fn reply(&self, endpoint: &EndpointId, text: impl Into<party_line_pager_core::Doc>) {
        if let Err(e) = self
            .fanout
            .send_to(endpoint, &OutMessage::plain(text))
            .await
        {
            tracing::warn!(%endpoint, error = ?e, "could not reply");
        }
    }

    fn yopass_client(&self) -> YopassClient {
        YopassClient::new(
            "yopass",
            &self.policy.instance.yopass_url,
            &self.policy.instance.yopass_api,
            self.policy.instance.hook_timeout,
        )
    }

    /// Resolves Link-mode delivery for one room and recipient: `Ok(None)`
    /// means `policy.instance.creds_delivery` is `Inline` and nothing needed
    /// minting; `Ok(Some(url))` is a cached or freshly minted one-time link;
    /// `Err` means Link mode wanted a link and minting failed, which the
    /// caller must turn into [`render::link_unavailable`] rather than falling
    /// back to plaintext.
    async fn mint_delivery(&self, room: &Room, recipient: &EndpointId) -> Result<Option<String>> {
        if self.policy.instance.creds_delivery != CredsMode::Link {
            return Ok(None);
        }
        // The link has to outlive the room, not the subprocess call that
        // mints it: this is `--expiration`, not `hook_timeout`. Falls back to
        // a minute on an already-expired room, which nothing should be
        // minting for in the first place (every call site checks
        // `is_live_at` first) but `Duration` cannot hold a negative value.
        let ttl = (room.expires_at - Utc::now())
            .to_std()
            .unwrap_or(Duration::from_secs(60));
        self.link_cache
            .get_or_mint(
                room.id(),
                recipient,
                &self.yopass_client(),
                &render::creds_plaintext(room),
                ttl,
            )
            .await
            .map(Some)
    }

    /// The `already_live` reply for one recipient, minting a Link-mode URL
    /// first when policy calls for one. Shared by the two places a request can
    /// land on a room that is already up: [`Self::open`]'s own check, and
    /// the race [`Self::provision`] re-checks after taking the lock.
    async fn already_live_reply(
        &self,
        room: &Room,
        recipient: &EndpointId,
        tz: Option<&str>,
    ) -> party_line_pager_core::Doc {
        let now = Utc::now();
        match self.mint_delivery(room, recipient).await {
            Ok(minted) => render::already_live(
                room,
                tz,
                now,
                minted
                    .as_deref()
                    .map(render::CredsDelivery::Link)
                    .unwrap_or(render::CredsDelivery::Inline),
            ),
            Err(e) => {
                tracing::warn!(%recipient, error = ?e, "could not mint a yopass link");
                render::link_unavailable()
            }
        }
    }

    async fn subscribe(&self, endpoint: &EndpointId) -> Result<()> {
        let _guard = self.state_lock.lock().await;
        let _flock = self.store.lock()?;
        let mut subs = self.store.subscribers()?;

        let outcome = policy::decide_subscribe(&self.policy, subs.get(endpoint));
        match outcome {
            SubscribeOutcome::Added | SubscribeOutcome::HeldForApproval => {
                let mut subscriber =
                    Subscriber::new(endpoint.clone(), &self.policy.instance.default_tier);
                subscriber.status = if outcome == SubscribeOutcome::Added {
                    SubscriberStatus::Active
                } else {
                    SubscriberStatus::Pending
                };
                subs.insert(subscriber);
                self.store.save_subscribers(&subs)?;
            }
            _ => {}
        }

        drop(_flock);
        drop(_guard);
        self.reply(endpoint, render::subscribe_reply(&self.policy, outcome))
            .await;
        Ok(())
    }

    async fn unsubscribe(&self, endpoint: &EndpointId) -> Result<()> {
        let removed = {
            let _guard = self.state_lock.lock().await;
            let _flock = self.store.lock()?;
            let mut subs = self.store.subscribers()?;

            match subs.get(endpoint).map(|s| s.status) {
                // A ban outlives an unsubscribe, otherwise leaving and
                // rejoining would launder it.
                Some(SubscriberStatus::Banned) | None => false,
                Some(_) => {
                    subs.remove(endpoint);
                    self.store.save_subscribers(&subs)?;
                    true
                }
            }
        };

        let text = if removed {
            render::unsubscribed(&self.policy)
        } else {
            render::not_subscribed()
        };
        self.reply(endpoint, text).await;
        Ok(())
    }

    async fn set_timezone(&self, endpoint: &EndpointId, zone: &str) -> Result<()> {
        let text = {
            let _guard = self.state_lock.lock().await;
            let _flock = self.store.lock()?;
            let mut subs = self.store.subscribers()?;

            match subs.get_mut(endpoint) {
                None => render::not_subscribed(),
                Some(subscriber) => match quiet::parse_timezone(zone) {
                    Err(_) => render::tz_invalid(zone),
                    Ok(tz) => {
                        subscriber.tz = Some(tz.name().to_string());
                        let confirmation = render::tz_set(tz.name());
                        self.store.save_subscribers(&subs)?;
                        confirmation
                    }
                },
            }
        };
        self.reply(endpoint, text).await;
        Ok(())
    }

    async fn quiet(&self, endpoint: &EndpointId, arg: QuietArg) -> Result<()> {
        let text = {
            let _guard = self.state_lock.lock().await;
            let _flock = self.store.lock()?;
            let mut subs = self.store.subscribers()?;

            // Every branch computes its reply inside the lock and sends it
            // outside: a reply can take as long as the fanout timeout, and
            // holding the state lock across it would stall every other command.
            match subs.get_mut(endpoint) {
                None => render::not_subscribed(),
                Some(subscriber) => match arg {
                    QuietArg::Show => render::quiet_show(subscriber),
                    QuietArg::Clear => {
                        subscriber.quiet = None;
                        let text = render::quiet_cleared();
                        self.store.save_subscribers(&subs)?;
                        text
                    }
                    QuietArg::Set(window) => {
                        subscriber.quiet = Some(window);
                        let text = render::quiet_set(&window, subscriber.tz.as_deref());
                        self.store.save_subscribers(&subs)?;
                        text
                    }
                },
            }
        };
        self.reply(endpoint, text).await;
        Ok(())
    }

    async fn status(&self, endpoint: &EndpointId) -> Result<()> {
        let (subscriber, room) = {
            let _guard = self.state_lock.lock().await;
            let _flock = self.store.lock()?;
            let subs = self.store.subscribers()?;
            (subs.get(endpoint).cloned(), self.store.room()?)
        };

        let text = match subscriber {
            None => render::not_subscribed(),
            Some(subscriber) => {
                let tier = self.policy.tier_or_default(&subscriber.tier).clone();
                let now = Utc::now();
                match room.as_ref().filter(|r| r.is_live_at(now)) {
                    Some(live) => match self.mint_delivery(live, endpoint).await {
                        Ok(minted) => render::status(
                            &subscriber,
                            &tier,
                            room.as_ref(),
                            now,
                            minted
                                .as_deref()
                                .map(render::CredsDelivery::Link)
                                .unwrap_or(render::CredsDelivery::Inline),
                        ),
                        Err(e) => {
                            tracing::warn!(%endpoint, error = ?e, "could not mint a yopass link");
                            render::link_unavailable()
                        }
                    },
                    None => render::status(
                        &subscriber,
                        &tier,
                        room.as_ref(),
                        now,
                        render::CredsDelivery::Inline,
                    ),
                }
            }
        };
        self.reply(endpoint, text).await;
        Ok(())
    }

    /// The `close` command. Only whoever opened the live room may tear it
    /// down early; everybody else gets told why not.
    async fn close(&self, endpoint: &EndpointId) -> Result<()> {
        let (outcome, room_id) = {
            let _guard = self.state_lock.lock().await;
            let _flock = self.store.lock()?;
            let subs = self.store.subscribers()?;
            let room = self.store.room()?;

            let outcome = policy::decide_close(
                &self.policy,
                subs.get(endpoint),
                room.as_ref(),
                endpoint,
                Utc::now(),
            );
            let room_id = room.as_ref().map(|r| r.id().to_string());
            (outcome, room_id)
        };

        match outcome {
            CloseOutcome::Reject(reason) => {
                self.reply(endpoint, render::close_rejection(&reason)).await;
            }
            CloseOutcome::Torn => {
                // decide_close only returns Torn when a live room exists and
                // endpoint is its host, so room_id is always Some here.
                if let Some(room_id) = room_id {
                    tracing::info!(%endpoint, room = %room_id, "owner closed their room early");
                    self.teardown(&room_id).await;
                }
                self.reply(endpoint, render::closed()).await;
            }
        }
        Ok(())
    }

    /// The `tor` and `web` commands.
    ///
    /// `kind` is already known to be configured: [`Self::resolve_target`] took
    /// care of that before this was ever called.
    async fn open(
        self: &Arc<Self>,
        endpoint: &EndpointId,
        kind: ProviderKind,
        note: Option<String>,
    ) -> Result<()> {
        let mut already_queued = false;
        let outcome = {
            let _guard = self.state_lock.lock().await;
            let _flock = self.store.lock()?;
            let subs = self.store.subscribers()?;
            let room = self.store.room()?;
            let paused = self.policy.instance.paused || self.store.runtime()?.paused;

            let outcome = policy::decide_open(
                &self.policy,
                subs.get(endpoint),
                room.as_ref(),
                paused,
                Utc::now(),
            );

            // Parking a held request happens under the same lock that decided it.
            if outcome == OpenOutcome::Hold {
                let mut pending = self.store.pending()?;
                // One queued request per endpoint. Holding does not spend quota,
                // so without this a held-tier subscriber could bury the admin
                // under thousands of identical requests.
                if pending.0.iter().any(|p| &p.endpoint == endpoint) {
                    already_queued = true;
                } else {
                    pending.0.push(PendingRequest {
                        id: secret::generate_id()?,
                        endpoint: endpoint.clone(),
                        // Remembered so an admin releasing this next week gets
                        // the kind of room that was actually asked for.
                        kind,
                        note: note.clone(),
                        requested_at: Utc::now(),
                        approved: false,
                    });
                    self.store.save_pending(&pending)?;
                }
            }
            outcome
        };

        match outcome {
            OpenOutcome::Reject(reason) => {
                self.reply(endpoint, render::rejection(&reason)).await;
            }
            OpenOutcome::AlreadyLive(room) => {
                let tz = self.tz_of(endpoint);
                let text = self.already_live_reply(&room, endpoint, tz.as_deref()).await;
                self.reply(endpoint, text).await;
            }
            OpenOutcome::Hold => {
                let text = if already_queued {
                    render::already_queued()
                } else {
                    render::held_for_approval()
                };
                self.reply(endpoint, text).await;
            }
            OpenOutcome::Provision => {
                // Acknowledge before the hook runs, but only for the kinds
                // slow enough to need it: rotating the key forces a fresh
                // bootstrap and the bot would otherwise look dead for a minute
                // or more. A web or Reticulum room comes up fast enough that
                // the same ack would be overtaken by the broadcast it promises
                // and read as a bug, so `ack_provisioning` returns None for
                // those and nothing is sent.
                if let Some(ack) = render::ack_provisioning(&self.policy, kind) {
                    self.reply(endpoint, ack).await;
                }
                self.provision(endpoint, kind, note).await?;
            }
        }
        Ok(())
    }

    /// Runs the up hook, records the room, fans out, and arms the teardown.
    pub async fn provision(
        self: &Arc<Self>,
        host: &EndpointId,
        kind: ProviderKind,
        note: Option<String>,
    ) -> Result<()> {
        let _provisioning = self.provision_lock.lock().await;

        // Somebody may have won the race while we waited for the lock. Note
        // that this does not care which kind of room won: one room per
        // instance, whichever verb started it.
        if let Some(room) = self.store.room()? {
            if room.is_live_at(Utc::now()) {
                let tz = self.tz_of(host);
                let text = self.already_live_reply(&room, host, tz.as_deref()).await;
                self.reply(host, text).await;
                return Ok(());
            }
        }

        let Some(runner) = self.providers.get(kind) else {
            // Only reachable when an admin deleted a provider section while a
            // request for it was still held in pending.json. Say so plainly
            // rather than dropping the request into a log nobody reads.
            tracing::warn!(%kind, "a request arrived for a provider that is no longer configured");
            self.reply(
                host,
                format!(
                    "The {kind} option is not available on this instance any more, so nothing was opened. Send help to see what is."
                ),
            )
            .await;
            return Ok(());
        };

        let policy_ttl = self.policy.instance.room_ttl;

        // The one place the two contracts genuinely diverge: a party line needs
        // a secret minted and validates its address against the network it is
        // supposed to be on, a web room needs neither and validates a URL
        // against the configured base. All three networks take the first path,
        // and everything after this match is shared.
        let (kind_state, hook_ttl) = match kind.transport() {
            Some(transport) => {
                let secret = secret::generate_secret()?;
                let output: PartylineOutput = match self
                    .run_up(runner, Some(&secret), policy_ttl, note.as_deref())
                    .await
                {
                    Ok(output) => output,
                    Err(e) => return self.provisioning_failed(host, e).await,
                };
                if let Err(e) = output.validate(transport) {
                    return self.provisioning_failed(host, e).await;
                }
                (
                    RoomKind::Partyline {
                        transport,
                        address: output.address.trim().to_string(),
                        secret,
                    },
                    output.ttl_secs,
                )
            }
            None => {
                // validate() needs the admin's base_url, which is why
                // validation lives here and not inside the runner.
                let base_url = self
                    .policy
                    .provider
                    .web
                    .as_ref()
                    .map(|w| w.trimmed_base_url().to_string())
                    .unwrap_or_default();
                let output: WebOutput =
                    match self.run_up(runner, None, policy_ttl, note.as_deref()).await {
                        Ok(output) => output,
                        Err(e) => return self.provisioning_failed(host, e).await,
                    };
                if let Err(e) = output.validate(&base_url) {
                    return self.provisioning_failed(host, e).await;
                }
                (RoomKind::Web { url: output.url }, output.ttl_secs)
            }
        };

        let ttl = hook_ttl.map(Duration::from_secs).unwrap_or(policy_ttl);
        let now = Utc::now();
        let room = Room {
            kind: kind_state,
            opened_by: host.clone(),
            note,
            started_at: now,
            expires_at: now + chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::hours(2)),
        };

        let (recipients, host_name) = {
            let _guard = self.state_lock.lock().await;
            let _flock = self.store.lock()?;
            let mut subs = self.store.subscribers()?;

            // Quota is spent only on a room that actually came up.
            if let Some(subscriber) = subs.get_mut(host) {
                let tier = self.policy.tier_or_default(&subscriber.tier).clone();
                policy::record_open(&tier, &mut subscriber.open_log, now);
            }
            self.store.save_subscribers(&subs)?;
            self.store.save_room(&room)?;

            let host_name = subs.get(host).and_then(|s| s.name.clone());
            let recipients = policy::recipients(&self.policy, &subs, now)
                .into_iter()
                .cloned()
                .collect::<Vec<_>>();
            (recipients, host_name)
        };

        // Arm the teardown before fanning out, not after. The room exists from
        // the moment the hook returned, and a transport that hangs must not be
        // able to leave an onion up forever.
        self.arm_teardown(room.id().to_string(), ttl);

        // Rendered per recipient rather than once: the closing time is only
        // actionable in the reader's own timezone, and the daemon has always
        // known each subscriber's zone because quiet hours need it. In Link
        // mode this is also where each recipient's own one-time link gets
        // minted, fail-closed: a recipient whose mint fails is dropped from
        // the fanout entirely rather than falling back to plaintext.
        let mut addressed = Vec::with_capacity(recipients.len());
        let mut mint_failures = 0usize;
        for subscriber in recipients {
            let minted = match self.mint_delivery(&room, &subscriber.endpoint).await {
                Ok(minted) => minted,
                Err(e) => {
                    tracing::warn!(
                        endpoint = %subscriber.endpoint,
                        error = ?e,
                        "could not mint a yopass link, skipping recipient"
                    );
                    mint_failures += 1;
                    continue;
                }
            };
            let delivery = minted
                .as_deref()
                .map(render::CredsDelivery::Link)
                .unwrap_or(render::CredsDelivery::Inline);
            let broadcast = render::broadcast(
                &self.policy,
                &room,
                host_name.as_deref(),
                subscriber.tz.as_deref(),
                now,
                delivery,
            );
            let msg = OutMessage::titled(broadcast.title, broadcast.body);
            addressed.push((subscriber, msg));
        }

        let report = self.fanout.broadcast_each(addressed).await;
        tracing::info!(
            provider = %kind,
            room = %room.id(),
            host = %host,
            host_name = host_name.as_deref().unwrap_or("-"),
            delivered = report.delivered,
            failed = report.failures.len() + mint_failures,
            "room opened"
        );

        Ok(())
    }

    /// Runs an up hook and deserializes its output. Split out only so the two
    /// arms of `provision()` share the timeout and JSON handling.
    async fn run_up<O: serde::de::DeserializeOwned>(
        &self,
        runner: &ProviderRunner,
        secret: Option<&str>,
        ttl: Duration,
        note: Option<&str>,
    ) -> Result<O> {
        runner
            .up(secret, ttl, note)
            .await
            .context("provider hook failed")
    }

    /// One answer for every way provisioning can fail: tell the host, keep
    /// their quota, send nothing to anybody else.
    async fn provisioning_failed(&self, host: &EndpointId, e: anyhow::Error) -> Result<()> {
        // The host keeps their quota: a broken backend is not their fault and
        // must not cost them a week.
        tracing::error!(error = ?e, "provisioning failed");
        self.reply(
            host,
            "The line would not come up, so nothing was sent and your quota is untouched."
                .to_string(),
        )
        .await;
        Ok(())
    }

    /// Sleeps out the room's life, then tears it down.
    fn arm_teardown(self: &Arc<Self>, room_id: String, ttl: Duration) {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(ttl).await;
            me.teardown(&room_id).await;
        });
    }

    /// Stops the room and forgets it. Safe to call twice.
    ///
    /// A teardown timer can outlive the room that armed it: an admin closes the
    /// line early, somebody opens a new room, and then the old timer fires.
    /// Tearing down unconditionally at that point would kill the new party and
    /// delete its state, so a teardown only ever acts on the room that is
    /// actually live.
    pub async fn teardown(&self, room_id: &str) {
        // Read the kind back off the persisted room rather than remembering it
        // in the timer: the room on disk is the only thing that knows what is
        // actually live right now.
        let kind = {
            let _guard = self.state_lock.lock().await;
            let _flock = self.store.lock();
            match self.store.room() {
                Ok(Some(room)) if room.id() != room_id => {
                    tracing::info!(
                        stale = %room_id,
                        live = %room.id(),
                        "ignoring a teardown for a room that has already been replaced"
                    );
                    return;
                }
                Ok(Some(room)) => Some(room.provider_kind()),
                Ok(None) => {
                    tracing::debug!(%room_id, "teardown skipped, nothing is live");
                    return;
                }
                // Unreadable state: clear it so the instance is not wedged
                // behind a room nobody can retire, but run no hook. There is no
                // way to know which backend this belonged to, and guessing
                // could take down a healthy room of the other kind.
                Err(e) => {
                    tracing::error!(error = ?e, "could not read room state, clearing it without running a hook");
                    None
                }
            }
        };

        tracing::info!(%room_id, ?kind, "tearing down");
        match kind.and_then(|k| self.providers.get(k).map(|runner| (k, runner))) {
            Some((_, runner)) => {
                if let Err(e) = runner.down(room_id).await {
                    tracing::error!(%room_id, error = ?e, "teardown hook failed, clearing state anyway");
                }
            }
            None => {
                if let Some(kind) = kind {
                    // The admin deleted this provider's section while its room
                    // was live. Nothing can be run, but the state still has to
                    // go or the instance stays blocked forever.
                    tracing::warn!(
                        %room_id, %kind,
                        "no hooks configured for this room's provider any more, clearing state without a teardown"
                    );
                }
            }
        }

        let _guard = self.state_lock.lock().await;
        match self.store.lock().and_then(|_flock| self.store.clear_room()) {
            Ok(()) => {}
            Err(e) => tracing::error!(error = ?e, "could not clear room state"),
        }
        self.link_cache.clear_room(room_id);
    }

    /// Restores the TTL clock after a restart, and cleans up a room that
    /// outlived the daemon.
    ///
    /// Also the one place that forgives an unreadable `room.json`. A file
    /// written by a version from before the provider split has no `provider`
    /// key and cannot be parsed; refusing to boot over it would need an admin
    /// with shell access to rescue a daemon that is otherwise perfectly fine,
    /// so it is logged and cleared instead. Every other reader still treats
    /// unreadable state as an error, because a corrupt roster must never be
    /// silently reset.
    pub async fn recover(self: &Arc<Self>) -> Result<()> {
        if self.store.room_is_unreadable() {
            tracing::error!(
                "room.json could not be read (a file from before the provider split has no \
                 \"provider\" key). Clearing it: any room it described is no longer tracked, \
                 and if it was a party line you may need to stop it by hand."
            );
            let _guard = self.state_lock.lock().await;
            let _flock = self.store.lock()?;
            self.store.clear_room()?;
            return Ok(());
        }

        let Some(room) = self.store.room()? else {
            return Ok(());
        };

        let now = Utc::now();
        let id = room.id().to_string();
        if room.is_live_at(now) {
            let remaining = (room.expires_at - now).to_std().unwrap_or(Duration::ZERO);
            tracing::info!(room = %id, seconds = remaining.as_secs(), "re-arming teardown");
            self.arm_teardown(id, remaining);
        } else {
            tracing::info!(room = %id, "found an expired room, tearing it down");
            self.teardown(&id).await;
        }
        Ok(())
    }

    /// Polls the state directory for decisions an admin made over SSH:
    /// released signals, and subscribers who were approved.
    pub async fn poll_admin_decisions(self: &Arc<Self>) {
        let mut known_active = self.active_endpoints().unwrap_or_default();

        loop {
            tokio::time::sleep(POLL_INTERVAL).await;

            if let Err(e) = self.honour_close_request().await {
                tracing::error!(error = ?e, "could not honour a close request");
            }

            match self.take_approved_signals() {
                Ok(approved) => {
                    for signal in approved {
                        tracing::info!(
                            endpoint = %signal.endpoint,
                            provider = %signal.kind,
                            "admin released a held signal"
                        );
                        if let Err(e) = self
                            .provision(&signal.endpoint, signal.kind, signal.note)
                            .await
                        {
                            tracing::error!(error = ?e, "releasing a held signal failed");
                        }
                    }
                }
                Err(e) => tracing::error!(error = ?e, "could not read pending signals"),
            }

            // Tell people the moment an admin lets them in, rather than leaving
            // them wondering until the next broadcast.
            if let Ok(active) = self.active_endpoints() {
                for endpoint in active.iter() {
                    if !known_active.contains(endpoint) {
                        self.reply(
                            endpoint,
                            format!(
                                "An admin approved you. You are on the {} roster now. Send help for the commands.",
                                self.policy.instance.name
                            ),
                        )
                        .await;
                    }
                }
                known_active = active;
            }
        }
    }

    /// Acts on `party_line_pagerctl close`.
    async fn honour_close_request(self: &Arc<Self>) -> Result<()> {
        {
            let _flock = self.store.lock()?;
            let mut runtime = self.store.runtime()?;
            if !runtime.close_requested {
                return Ok(());
            }
            runtime.close_requested = false;
            self.store.save_runtime(&runtime)?;
        }

        match self.store.room()? {
            Some(room) => {
                tracing::info!(room = %room.id(), "admin closed the line early");
                let id = room.id().to_string();
                self.teardown(&id).await;
            }
            None => tracing::info!("close requested but nothing is live"),
        }
        Ok(())
    }

    fn active_endpoints(&self) -> Result<std::collections::BTreeSet<EndpointId>> {
        Ok(self
            .store
            .subscribers()?
            .iter()
            .filter(|s| s.status == SubscriberStatus::Active)
            .map(|s| s.endpoint.clone())
            .collect())
    }

    /// Removes and returns every signal an admin approved.
    fn take_approved_signals(&self) -> Result<Vec<PendingRequest>> {
        let _flock = self.store.lock()?;
        let mut pending = self.store.pending()?;
        if !pending.0.iter().any(|p| p.approved) {
            return Ok(Vec::new());
        }
        let (approved, rest): (Vec<_>, Vec<_>) = pending.0.into_iter().partition(|p| p.approved);
        pending.0 = rest;
        self.store.save_pending(&pending)?;
        Ok(approved)
    }

    /// Test and boot helper: the current roster.
    pub fn subscribers(&self) -> Result<Subscribers> {
        Ok(self.store.subscribers()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{write_script, MockTransport};
    use crate::transport::Transport;
    use std::collections::HashMap;

    // `crate::transport::Transport` is a chat network and already imported
    // above; the room's network is spelled out in full where it is needed, so
    // the two never get confused.
    use party_line_pager_core::Transport as Network;

    const ONION: &str = "abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrstuvwx.onion";
    const B32: &str = "abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrst.b32.i2p";
    const HASH: &str = "3a1c9d4e07b21f88c2a04e7d612b0f4e";
    const WEB_BASE: &str = "https://p2p.mirotalk.com";
    const WEB_URL: &str = "https://p2p.mirotalk.com/join/deadbeef";

    /// Which hooks an instance in a test has, and what they do. `Setup::default()`
    /// is the everything-configured instance most tests want.
    struct Setup<'a> {
        signups: &'a str,
        extra_tiers: &'a str,
        ttl: &'a str,
        /// `(up body, down body)`, or `None` to leave the provider unconfigured.
        tor: Option<(String, String)>,
        i2p: Option<(String, String)>,
        rns: Option<(String, String)>,
        web: Option<(String, String)>,
    }

    impl Setup<'_> {
        /// An instance that offers `web` and no party line at all.
        fn web_only() -> Self {
            Setup {
                tor: None,
                i2p: None,
                rns: None,
                ..Setup::default()
            }
        }

        /// The party-line hooks, paired with the section each one configures.
        /// One list so `build` writes the policy and registers the runners in
        /// a single pass, whatever mix of networks a test asks for.
        fn partylines(&self) -> Vec<(ProviderKind, &(String, String))> {
            [
                (ProviderKind::Tor, self.tor.as_ref()),
                (ProviderKind::I2p, self.i2p.as_ref()),
                (ProviderKind::Rns, self.rns.as_ref()),
            ]
            .into_iter()
            .filter_map(|(kind, hooks)| hooks.map(|h| (kind, h)))
            .collect()
        }
    }

    /// The secret of a party line room, for tests that assert it travelled.
    fn secret_of(room: &Room) -> String {
        match &room.kind {
            RoomKind::Partyline { secret, .. } => secret.clone(),
            RoomKind::Web { .. } => panic!("a web room has no secret"),
        }
    }

    fn partyline_room(address: &str, secret: &str, opened_by: &str, expires_in: i64) -> Room {
        let now = Utc::now();
        Room {
            kind: RoomKind::Partyline {
                transport: Network::Tor,
                address: address.into(),
                secret: secret.into(),
            },
            opened_by: opened_by.parse().unwrap(),
            note: None,
            started_at: now,
            expires_at: now + chrono::Duration::hours(expires_in),
        }
    }

    fn torn_down_line() -> String {
        "#!/bin/sh\necho \"$PARTY_LINE_PAGER_ROOM_ID\" >> \"$(dirname \"$0\")/torn-down\"\n".to_string()
    }

    /// An up hook that publishes `address`, and a down hook that records what
    /// it was asked to tear down.
    fn hooks_publishing(address: &str) -> Option<(String, String)> {
        Some((
            format!("#!/bin/sh\nread -r SECRET\necho '{{\"address\":\"{address}\"}}'\n"),
            torn_down_line(),
        ))
    }

    fn web_hooks() -> Option<(String, String)> {
        Some((
            "#!/bin/sh\ncat >/dev/null\nprintf '{\"url\":\"%s/join/deadbeef\"}\\n' \"$PARTY_LINE_PAGER_WEB_BASE_URL\"\n"
                .to_string(),
            torn_down_line(),
        ))
    }

    impl Default for Setup<'_> {
        fn default() -> Self {
            Setup {
                signups: "open",
                extra_tiers: "",
                ttl: "2h",
                tor: hooks_publishing(ONION),
                i2p: hooks_publishing(B32),
                rns: hooks_publishing(HASH),
                web: web_hooks(),
            }
        }
    }

    struct Harness {
        engine: Arc<Engine>,
        telegram: Arc<MockTransport>,
        dir: tempfile::TempDir,
    }

    impl Harness {
        fn new(signups: &str, extra_tiers: &str, ttl: &str) -> Self {
            Self::build(Setup {
                signups,
                extra_tiers,
                ttl,
                ..Setup::default()
            })
        }

        /// A tor room whose hooks misbehave in whatever way the test needs.
        fn with_hooks(
            signups: &str,
            extra_tiers: &str,
            ttl: &str,
            up_body: &str,
            down_body: &str,
        ) -> Self {
            Self::build(Setup {
                signups,
                extra_tiers,
                ttl,
                tor: Some((up_body.to_string(), down_body.to_string())),
                ..Setup::default()
            })
        }

        fn build(setup: Setup<'_>) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let mut sections = String::new();
            let mut providers = Providers::default();

            for (kind, (up_body, down_body)) in setup.partylines() {
                let up = write_script(dir.path(), &format!("{}-up.sh", kind.verb()), up_body);
                let down = write_script(dir.path(), &format!("{}-down.sh", kind.verb()), down_body);
                sections.push_str(&format!(
                    "{}\nup = \"{}\"\ndown = \"{}\"\n",
                    kind.section(),
                    up.display(),
                    down.display()
                ));
                providers.insert(kind, ProviderRunner::new(&up, &down, Duration::from_secs(10)));
            }

            if let Some((up_body, down_body)) = &setup.web {
                let up = write_script(dir.path(), "web-up.sh", up_body);
                let down = write_script(dir.path(), "web-down.sh", down_body);
                sections.push_str(&format!(
                    "[provider.web]\nup = \"{}\"\ndown = \"{}\"\nbase_url = \"{WEB_BASE}\"\n",
                    up.display(),
                    down.display()
                ));
                providers.insert(
                    ProviderKind::Web,
                    ProviderRunner::new(&up, &down, Duration::from_secs(10))
                        .with_env("PARTY_LINE_PAGER_WEB_BASE_URL", WEB_BASE),
                );
            }

            let policy = Policy::parse_str(&format!(
                r#"
                [instance]
                name = "TestPager"
                signups = "{signups}"
                default_tier = "weekly"
                room_ttl = "{ttl}"
                hook_timeout = "10s"

                {sections}

                [[tier]]
                name = "weekly"
                window = "168h"

                [[tier]]
                name = "lurker"
                may_open = false

                {extra_tiers}
                "#,
                signups = setup.signups,
                ttl = setup.ttl,
                extra_tiers = setup.extra_tiers,
            ))
            .unwrap();

            let store = Store::open(dir.path().join("state")).unwrap();
            let telegram = MockTransport::new("telegram");
            let mut transports: HashMap<String, Arc<dyn Transport>> = HashMap::new();
            transports.insert("telegram".into(), telegram.clone());
            let fanout = Arc::new(Fanout::new(transports, None, 4, Duration::from_secs(5)));

            Harness {
                engine: Engine::new(policy, store, fanout, providers),
                telegram,
                dir,
            }
        }

        async fn send(&self, from: &str, text: &str) {
            self.engine
                .handle(Incoming {
                    endpoint: from.parse().unwrap(),
                    text: text.to_string(),
                })
                .await;
        }

        fn replies_to(&self, address: &str) -> Vec<String> {
            self.telegram.bodies_to(address)
        }

        fn last_reply_to(&self, address: &str) -> String {
            self.replies_to(address)
                .last()
                .cloned()
                .unwrap_or_else(|| panic!("no reply was sent to {address}"))
        }

        fn subscriber(&self, id: &str) -> Subscriber {
            self.engine
                .subscribers()
                .unwrap()
                .get(&id.parse().unwrap())
                .cloned()
                .unwrap_or_else(|| panic!("{id} is not on the roster"))
        }

        fn set_status(&self, id: &str, status: SubscriberStatus) {
            let mut subs = self.engine.subscribers().unwrap();
            subs.get_mut(&id.parse().unwrap()).unwrap().status = status;
            self.engine.store().save_subscribers(&subs).unwrap();
        }

        fn torn_down(&self) -> String {
            std::fs::read_to_string(self.dir.path().join("torn-down")).unwrap_or_default()
        }
    }

    #[tokio::test]
    async fn unknown_messages_get_no_reply_at_all() {
        let h = Harness::new("open", "", "2h");
        h.send("telegram:1", "hey what is this bot").await;
        h.send("telegram:1", "approve telegram:1").await;
        h.send("telegram:1", "pause").await;
        assert!(h.telegram.sent().is_empty(), "the daemon must stay silent");
    }

    #[tokio::test]
    async fn help_works_before_subscribing() {
        let h = Harness::new("open", "", "2h");
        h.send("telegram:1", "help").await;
        assert!(h.last_reply_to("1").contains("tor [note]"));
    }

    #[tokio::test]
    async fn open_signups_activate_immediately() {
        let h = Harness::new("open", "", "2h");
        h.send("telegram:1", "sub").await;
        assert_eq!(h.subscriber("telegram:1").status, SubscriberStatus::Active);
        assert!(h.last_reply_to("1").contains("Subscribed"));
    }

    #[tokio::test]
    async fn approval_signups_park_the_request() {
        let h = Harness::new("approval", "", "2h");
        h.send("telegram:1", "sub").await;
        assert_eq!(h.subscriber("telegram:1").status, SubscriberStatus::Pending);
        assert!(h.last_reply_to("1").contains("admin"));

        h.send("telegram:1", "tor").await;
        assert!(h.last_reply_to("1").contains("waiting for an admin"));
        assert!(h.engine.store().room().unwrap().is_none());
    }

    #[tokio::test]
    async fn closed_signups_refuse() {
        let h = Harness::new("closed", "", "2h");
        h.send("telegram:1", "sub").await;
        assert!(h.last_reply_to("1").contains("closed"));
        assert!(h.engine.subscribers().unwrap().is_empty());
    }

    #[tokio::test]
    async fn timezone_and_quiet_hours_round_trip() {
        let h = Harness::new("open", "", "2h");
        h.send("telegram:1", "sub").await;

        h.send("telegram:1", "tz Mars/Olympus").await;
        assert!(h.last_reply_to("1").contains("not an IANA timezone"));

        h.send("telegram:1", "tz America/Denver").await;
        assert_eq!(h.subscriber("telegram:1").tz.as_deref(), Some("America/Denver"));

        h.send("telegram:1", "quiet 23:00-07:00").await;
        assert_eq!(
            h.subscriber("telegram:1").quiet.unwrap().to_string(),
            "23:00-07:00"
        );

        h.send("telegram:1", "quiet").await;
        assert!(h.last_reply_to("1").contains("23:00-07:00"));

        h.send("telegram:1", "quiet off").await;
        assert!(h.subscriber("telegram:1").quiet.is_none());
    }

    #[tokio::test]
    async fn settings_require_a_subscription() {
        let h = Harness::new("open", "", "2h");
        for command in ["tz America/Denver", "quiet 23:00-07:00", "status"] {
            h.send("telegram:99", command).await;
            assert_eq!(h.last_reply_to("99"), "You are not subscribed.");
        }
    }

    #[tokio::test]
    async fn raising_provisions_a_room_and_tells_everyone() {
        let h = Harness::new("open", "", "2h");
        h.send("telegram:1", "sub").await;
        h.send("telegram:2", "sub").await;
        h.telegram.clear();

        h.send("telegram:1", "tor poker night").await;

        // The host is acknowledged before the hook runs, then gets the
        // broadcast like everybody else.
        let to_host = h.replies_to("1");
        assert!(to_host[0].contains("opening the room"), "{to_host:?}");

        let room = h.engine.store().room().unwrap().expect("room recorded");
        assert_eq!(room.id(), ONION);
        assert_eq!(room.provider_kind(), ProviderKind::Tor);
        assert_eq!(room.note.as_deref(), Some("poker night"));

        for address in ["1", "2"] {
            let broadcast = h.last_reply_to(address);
            assert!(broadcast.contains(ONION), "{address} missed the onion");
            assert!(
                broadcast.contains(&secret_of(&room)),
                "{address} missed the secret"
            );
            assert!(broadcast.contains("poker night"));
        }

        assert!(
            !h.subscriber("telegram:1").open_log.is_empty(),
            "quota spent"
        );
    }

    #[tokio::test]
    async fn quiet_subscribers_are_skipped_silently() {
        let h = Harness::new("open", "", "2h");
        h.send("telegram:1", "sub").await;
        h.send("telegram:2", "sub").await;

        // Put subscriber 2 inside a window covering every hour of the day.
        {
            let mut subs = h.engine.subscribers().unwrap();
            let s = subs.get_mut(&"telegram:2".parse().unwrap()).unwrap();
            s.tz = Some("UTC".into());
            s.quiet = Some("00:00-23:59".parse().unwrap());
            h.engine.store().save_subscribers(&subs).unwrap();
        }
        h.telegram.clear();

        h.send("telegram:1", "tor").await;

        assert!(h.replies_to("1").iter().any(|b| b.contains(ONION)));
        assert!(
            h.replies_to("2").is_empty(),
            "a sleeping subscriber must receive nothing"
        );
    }

    #[tokio::test]
    async fn a_second_open_while_live_reuses_the_room() {
        let h = Harness::new("open", "", "2h");
        h.send("telegram:1", "sub").await;
        h.send("telegram:2", "sub").await;
        h.send("telegram:1", "tor").await;
        let first = h.engine.store().room().unwrap().unwrap();
        h.telegram.clear();

        h.send("telegram:2", "tor").await;

        let reply = h.last_reply_to("2");
        assert!(reply.contains("already open"), "{reply}");
        assert!(reply.contains(&secret_of(&first)), "the creds are repeated");
        assert_eq!(
            secret_of(&h.engine.store().room().unwrap().unwrap()),
            secret_of(&first),
            "no second room was started"
        );
        assert!(
            h.subscriber("telegram:2").open_log.is_empty(),
            "an already-live room costs no quota"
        );
    }

    #[tokio::test]
    async fn quota_exhaustion_reports_the_wait_and_sends_nothing() {
        let h = Harness::new("open", "", "1s");
        h.send("telegram:1", "sub").await;
        h.send("telegram:1", "tor").await;

        // Let the room expire so the live-room shortcut does not mask the quota.
        tokio::time::sleep(Duration::from_millis(1200)).await;
        h.telegram.clear();

        h.send("telegram:1", "tor").await;
        let reply = h.last_reply_to("1");
        assert!(reply.contains("used every room"), "{reply}");
        assert!(reply.contains("6d 23h"), "{reply}");
    }

    #[tokio::test]
    async fn receive_only_tiers_are_refused() {
        let h = Harness::new("open", "", "2h");
        h.send("telegram:1", "sub").await;
        {
            let mut subs = h.engine.subscribers().unwrap();
            subs.get_mut(&"telegram:1".parse().unwrap()).unwrap().tier = "lurker".into();
            h.engine.store().save_subscribers(&subs).unwrap();
        }
        h.send("telegram:1", "tor").await;
        assert!(h.last_reply_to("1").contains("not open a room"));
        assert!(h.engine.store().room().unwrap().is_none());
    }

    #[tokio::test]
    async fn banned_endpoints_are_refused_and_cannot_launder_the_ban() {
        let h = Harness::new("open", "", "2h");
        h.send("telegram:1", "sub").await;
        h.set_status("telegram:1", SubscriberStatus::Banned);

        h.send("telegram:1", "tor").await;
        assert_eq!(h.last_reply_to("1"), "You cannot use this service.");

        h.send("telegram:1", "unsub").await;
        h.send("telegram:1", "sub").await;
        assert_eq!(h.subscriber("telegram:1").status, SubscriberStatus::Banned);
    }

    #[tokio::test]
    async fn a_paused_instance_refuses_new_rooms() {
        let h = Harness::new("open", "", "2h");
        h.send("telegram:1", "sub").await;
        h.engine
            .store()
            .save_runtime(&party_line_pager_core::state::Runtime {
                paused: true,
                ..Default::default()
            })
            .unwrap();

        h.send("telegram:1", "tor").await;
        assert!(h.last_reply_to("1").contains("paused"));
        assert!(h.engine.store().room().unwrap().is_none());
    }

    #[tokio::test]
    async fn a_failed_hook_costs_no_quota_and_sends_no_broadcast() {
        let h = Harness::with_hooks(
            "open",
            "",
            "2h",
            "#!/bin/sh\necho 'tor is on fire' >&2\nexit 1\n",
            "#!/bin/sh\n",
        );
        h.send("telegram:1", "sub").await;
        h.send("telegram:2", "sub").await;
        h.telegram.clear();

        h.send("telegram:1", "tor").await;

        assert!(h.last_reply_to("1").contains("quota is untouched"));
        assert!(h.subscriber("telegram:1").open_log.is_empty());
        assert!(h.engine.store().room().unwrap().is_none());
        assert!(h.replies_to("2").is_empty(), "nobody should have been woken");
    }

    #[tokio::test]
    async fn a_hook_that_returns_a_bogus_address_broadcasts_nothing() {
        let h = Harness::with_hooks(
            "open",
            "",
            "2h",
            "#!/bin/sh\necho '{\"onion\":\"evil.example.com\"}'\n",
            "#!/bin/sh\n",
        );
        h.send("telegram:1", "sub").await;
        h.send("telegram:2", "sub").await;
        h.telegram.clear();

        h.send("telegram:1", "tor").await;
        assert!(h.replies_to("2").is_empty());
        assert!(h.engine.store().room().unwrap().is_none());
    }

    #[tokio::test]
    async fn the_ttl_tears_the_room_down() {
        let h = Harness::new("open", "", "1s");
        h.send("telegram:1", "sub").await;
        h.send("telegram:1", "tor").await;
        assert!(h.engine.store().room().unwrap().is_some());

        tokio::time::sleep(Duration::from_millis(1400)).await;

        assert!(h.engine.store().room().unwrap().is_none(), "room state cleared");
        assert!(h.torn_down().contains(ONION), "down hook ran with the onion");
    }

    #[tokio::test]
    async fn a_stalled_fanout_cannot_keep_the_room_alive() {
        // The teardown is armed the moment the hook returns, not after the
        // broadcast. Otherwise one slow transport leaves an onion up forever.
        let h = Harness::new("open", "", "1s");
        h.send("telegram:1", "sub").await;
        h.telegram.set_delay(Duration::from_millis(1500));

        let engine = Arc::clone(&h.engine);
        let raising = tokio::spawn(async move {
            engine
                .handle(Incoming {
                    endpoint: "telegram:1".parse().unwrap(),
                    text: "tor".into(),
                })
                .await;
        });

        // By now the room has expired and been torn down, while the broadcast
        // to the slow transport is still in flight.
        tokio::time::sleep(Duration::from_millis(3200)).await;
        assert!(
            h.engine.store().room().unwrap().is_none(),
            "the room outlived its TTL because the fanout was slow"
        );

        raising.await.unwrap();
        assert!(h.torn_down().contains(ONION));
    }

    #[tokio::test]
    async fn a_held_tier_cannot_bury_the_admin_in_duplicate_requests() {
        let h = Harness::new(
            "open",
            "[[tier]]\nname = \"held\"\nhold_for_approval = true\n",
            "2h",
        );
        h.send("telegram:1", "sub").await;
        {
            let mut subs = h.engine.subscribers().unwrap();
            subs.get_mut(&"telegram:1".parse().unwrap()).unwrap().tier = "held".into();
            h.engine.store().save_subscribers(&subs).unwrap();
        }

        for _ in 0..5 {
            h.send("telegram:1", "tor").await;
        }

        let pending = h.engine.store().pending().unwrap();
        assert_eq!(pending.0.len(), 1, "one queued request per endpoint");
        assert!(
            h.last_reply_to("1").contains("already have a request queued"),
            "{}",
            h.last_reply_to("1")
        );
    }

    #[tokio::test]
    async fn the_host_can_close_their_own_room_early() {
        let h = Harness::new("open", "", "2h");
        h.send("telegram:1", "sub").await;
        h.send("telegram:1", "tor").await;
        assert!(h.engine.store().room().unwrap().is_some());

        h.send("telegram:1", "close").await;

        assert!(h.engine.store().room().unwrap().is_none(), "room state cleared");
        assert!(h.torn_down().contains(ONION), "down hook ran with the onion");
        assert_eq!(h.last_reply_to("1"), "Closed. The room is coming down now.");
    }

    #[tokio::test]
    async fn nobody_but_the_host_may_close_the_room() {
        let h = Harness::new("open", "", "2h");
        h.send("telegram:1", "sub").await;
        h.send("telegram:2", "sub").await;
        h.send("telegram:1", "tor").await;

        h.send("telegram:2", "close").await;

        assert_eq!(
            h.last_reply_to("2"),
            "Only whoever opened this room can close it."
        );
        assert!(h.engine.store().room().unwrap().is_some(), "the room stays up");
    }

    #[tokio::test]
    async fn closing_with_nothing_live_says_so() {
        let h = Harness::new("open", "", "2h");
        h.send("telegram:1", "sub").await;

        h.send("telegram:1", "close").await;

        assert_eq!(h.last_reply_to("1"), "Nothing is live right now.");
    }

    #[tokio::test]
    async fn a_tier_without_close_rights_cannot_tear_down_its_own_room() {
        let h = Harness::new(
            "open",
            "[[tier]]\nname = \"open-only\"\nmay_close = false\n",
            "2h",
        );
        h.send("telegram:1", "sub").await;
        {
            let mut subs = h.engine.subscribers().unwrap();
            subs.get_mut(&"telegram:1".parse().unwrap()).unwrap().tier = "open-only".into();
            h.engine.store().save_subscribers(&subs).unwrap();
        }
        h.send("telegram:1", "tor").await;

        h.send("telegram:1", "close").await;

        assert_eq!(h.last_reply_to("1"), "Your tier cannot close a room.");
        assert!(h.engine.store().room().unwrap().is_some());
    }

    #[tokio::test]
    async fn a_stale_teardown_timer_cannot_kill_the_next_party() {
        // An admin closes room A early, somebody opens room B, and only then
        // does A's original TTL timer fire.
        let h = Harness::new("open", "", "2h");
        let live = partyline_room(
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.onion",
            "SECRET-B",
            "telegram:2",
            2,
        );
        h.engine.store().save_room(&live).unwrap();

        h.engine.teardown(ONION).await;

        assert_eq!(
            h.engine.store().room().unwrap().as_ref(),
            Some(&live),
            "the live room must survive a stale timer"
        );
        assert!(
            !h.torn_down().contains(ONION),
            "the down hook must not run for a room that is already gone"
        );
    }

    #[tokio::test]
    async fn a_teardown_with_nothing_live_does_not_touch_the_backend() {
        // Guards the other side of the same race: a stale timer firing while a
        // new room is still being provisioned must not run `compose down`.
        let h = Harness::new("open", "", "2h");
        h.engine.teardown(ONION).await;
        assert!(h.torn_down().is_empty());
    }

    #[tokio::test]
    async fn recovery_tears_down_a_room_that_outlived_the_daemon() {
        let h = Harness::new("open", "", "2h");
        let now = Utc::now();
        h.engine
            .store()
            .save_room(&Room {
                started_at: now - chrono::Duration::hours(3),
                expires_at: now - chrono::Duration::hours(1),
                ..partyline_room(ONION, "S", "telegram:1", 2)
            })
            .unwrap();

        h.engine.recover().await.unwrap();

        assert!(h.engine.store().room().unwrap().is_none());
        assert!(h.torn_down().contains(ONION));
    }

    #[tokio::test]
    async fn recovery_re_arms_a_room_that_is_still_live() {
        let h = Harness::new("open", "", "2h");
        let now = Utc::now();
        h.engine
            .store()
            .save_room(&Room {
                started_at: now,
                expires_at: now + chrono::Duration::milliseconds(600),
                ..partyline_room(ONION, "S", "telegram:1", 2)
            })
            .unwrap();

        h.engine.recover().await.unwrap();
        assert!(h.engine.store().room().unwrap().is_some(), "still live");

        tokio::time::sleep(Duration::from_millis(900)).await;
        assert!(h.engine.store().room().unwrap().is_none(), "torn down on schedule");
    }

    #[tokio::test]
    async fn held_tiers_wait_for_an_admin_then_provision() {
        let h = Harness::new(
            "open",
            "[[tier]]\nname = \"held\"\nhold_for_approval = true\n",
            "2h",
        );
        h.send("telegram:1", "sub").await;
        h.send("telegram:2", "sub").await;
        {
            let mut subs = h.engine.subscribers().unwrap();
            subs.get_mut(&"telegram:1".parse().unwrap()).unwrap().tier = "held".into();
            h.engine.store().save_subscribers(&subs).unwrap();
        }
        h.telegram.clear();

        h.send("telegram:1", "tor after work").await;

        assert!(h.last_reply_to("1").contains("queued"));
        assert!(h.replies_to("2").is_empty(), "nothing went out yet");
        let pending = h.engine.store().pending().unwrap();
        assert_eq!(pending.0.len(), 1);
        assert_eq!(pending.0[0].note.as_deref(), Some("after work"));

        // What `party_line_pagerctl approve-request` does: flip the flag on disk.
        let mut pending = h.engine.store().pending().unwrap();
        pending.0[0].approved = true;
        h.engine.store().save_pending(&pending).unwrap();

        let released = h.engine.take_approved_signals().unwrap();
        assert_eq!(released.len(), 1);
        h.engine
            .provision(
                &released[0].endpoint,
                released[0].kind,
                released[0].note.clone(),
            )
            .await
            .unwrap();

        assert!(h.last_reply_to("2").contains(ONION), "released signal reached the roster");
        assert!(h.engine.store().pending().unwrap().0.is_empty());
    }

    #[tokio::test]
    async fn status_reports_the_live_room() {
        let h = Harness::new("open", "", "2h");
        h.send("telegram:1", "sub").await;
        h.send("telegram:1", "tor").await;
        h.telegram.clear();

        h.send("telegram:1", "status").await;
        let status = h.last_reply_to("1");
        assert!(status.contains("Tier      weekly"), "{status}");
        assert!(status.contains("The room is open now"), "{status}");
        assert!(status.contains(ONION), "{status}");
    }

    // --- web rooms -------------------------------------------------------

    #[tokio::test]
    async fn web_opens_a_url_with_no_secret_and_no_warm_up_message() {
        let h = Harness::new("open", "", "2h");
        h.send("telegram:1", "sub").await;
        h.send("telegram:2", "sub").await;
        h.telegram.clear();

        h.send("telegram:1", "web poker night").await;

        let room = h.engine.store().room().unwrap().expect("room recorded");
        assert_eq!(room.provider_kind(), ProviderKind::Web);
        assert_eq!(room.id(), WEB_URL);

        for address in ["1", "2"] {
            let broadcast = h.last_reply_to(address);
            assert!(broadcast.contains(WEB_URL), "{address} missed the url");
            assert!(!broadcast.contains("Secret"), "{broadcast}");
            assert!(broadcast.contains("poker night"));
        }

        // A web room is instant, so the "warming the line, one to three
        // minutes" ack would be overtaken by the broadcast and read as a bug.
        assert!(
            !h.replies_to("1").iter().any(|b| b.contains("warming")),
            "{:?}",
            h.replies_to("1")
        );
        assert!(
            !h.subscriber("telegram:1").open_log.is_empty(),
            "quota spent"
        );
    }

    #[tokio::test]
    async fn either_kind_of_room_blocks_the_other() {
        let h = Harness::new("open", "", "2h");
        h.send("telegram:1", "sub").await;
        h.send("telegram:2", "sub").await;

        h.send("telegram:1", "web").await;
        h.telegram.clear();

        h.send("telegram:2", "tor").await;
        let reply = h.last_reply_to("2");
        assert!(reply.contains("already open"), "{reply}");
        assert!(reply.contains(WEB_URL), "the live room's creds are repeated");
        assert_eq!(
            h.engine.store().room().unwrap().unwrap().provider_kind(),
            ProviderKind::Web,
            "a tor room must not have started alongside the web room"
        );
        assert!(
            h.subscriber("telegram:2").open_log.is_empty(),
            "an already-live room costs no quota, whichever kind it is"
        );
    }

    #[tokio::test]
    async fn an_unconfigured_verb_gets_the_same_silence_as_any_other_word() {
        let h = Harness::build(Setup {
            web: None,
            ..Setup::default()
        });
        h.send("telegram:1", "sub").await;
        h.telegram.clear();

        h.send("telegram:1", "web").await;

        assert!(
            h.telegram.sent().is_empty(),
            "a verb this instance does not offer must not even be answered"
        );
        assert!(h.engine.store().room().unwrap().is_none());
    }

    #[tokio::test]
    async fn a_bare_signal_is_no_longer_recognized_even_on_a_single_provider_instance() {
        // signal/raise/batsignal used to defer to whichever provider was
        // configured. Removed on purpose: the four provider verbs are the only
        // ways to open a room now, so a bare "signal" is silence, same as any other
        // unknown word, even where it would have been unambiguous.
        let h = Harness::build(Setup {
            tor: None,
            ..Setup::default()
        });
        h.send("telegram:1", "sub").await;
        h.telegram.clear();

        h.send("telegram:1", "signal").await;

        assert!(
            h.telegram.sent().is_empty(),
            "a removed verb must not even be answered"
        );
        assert!(h.engine.store().room().unwrap().is_none());
    }

    #[tokio::test]
    async fn help_only_lists_what_this_instance_can_do() {
        let h = Harness::build(Setup::web_only());
        h.send("telegram:1", "help").await;
        let help = h.last_reply_to("1");
        assert!(help.contains("web [note]"), "{help}");
        for absent in ["tor", "i2p", "rns"] {
            assert!(!help.contains(absent), "{absent} is not configured: {help}");
        }
    }

    #[tokio::test]
    async fn help_lists_every_network_an_instance_does_offer() {
        let h = Harness::build(Setup::default());
        h.send("telegram:1", "help").await;
        let help = h.last_reply_to("1");
        for verb in ["tor [note]", "i2p [note]", "rns [note]", "web [note]"] {
            assert!(help.contains(verb), "{verb} missing from: {help}");
        }
    }

    #[tokio::test]
    async fn a_web_hook_that_returns_someone_elses_url_broadcasts_nothing() {
        let h = Harness::build(Setup {
            web: Some((
                "#!/bin/sh\ncat >/dev/null\necho '{\"url\":\"https://evil.example.com/join/x\"}'\n"
                    .into(),
                torn_down_line(),
            )),
            ..Setup::default()
        });
        h.send("telegram:1", "sub").await;
        h.send("telegram:2", "sub").await;
        h.telegram.clear();

        h.send("telegram:1", "web").await;

        assert!(h.engine.store().room().unwrap().is_none());
        assert!(h.replies_to("2").is_empty(), "nobody should have been woken");
        assert!(h.last_reply_to("1").contains("quota is untouched"));
        assert!(h.subscriber("telegram:1").open_log.is_empty());
    }

    #[tokio::test]
    async fn every_network_opens_a_room_of_its_own_kind() {
        // The verb decides the network, the network decides how the address is
        // checked, and the broadcast carries the client for that network and
        // no other. One test per network would be three copies of this.
        for (verb, kind, address, network) in [
            ("tor", ProviderKind::Tor, ONION, Network::Tor),
            ("i2p", ProviderKind::I2p, B32, Network::I2p),
            ("rns", ProviderKind::Rns, HASH, Network::Reticulum),
        ] {
            let h = Harness::new("open", "", "2h");
            h.send("telegram:1", "sub").await;
            h.send("telegram:2", "sub").await;
            h.telegram.clear();

            h.send("telegram:1", verb).await;

            let room = h
                .engine
                .store()
                .room()
                .unwrap()
                .unwrap_or_else(|| panic!("{verb} opened no room"));
            assert_eq!(room.provider_kind(), kind, "{verb}");
            assert_eq!(room.id(), address, "{verb}");
            match &room.kind {
                RoomKind::Partyline { transport, .. } => assert_eq!(*transport, network),
                other => panic!("{verb} produced {other:?}"),
            }

            let broadcast = h.replies_to("2").join("\n");
            assert!(broadcast.contains(address), "{verb} did not send the address");
            assert!(
                broadcast.contains(network.client_url()),
                "{verb} must link the client that dials it: {broadcast}"
            );
            assert!(
                broadcast.contains(network.label()),
                "{verb} must label the address the way its network does: {broadcast}"
            );
        }
    }

    #[tokio::test]
    async fn a_hook_that_returns_another_networks_address_broadcasts_nothing() {
        // The likeliest way to misconfigure this is to point one section's
        // hooks at another network's compose file. The address still parses,
        // so only the per-network check catches it, and it has to catch it
        // before the roster is told anything.
        let h = Harness::build(Setup {
            // The i2p section, wired to a hook that publishes an onion.
            i2p: hooks_publishing(ONION),
            ..Setup::default()
        });
        h.send("telegram:1", "sub").await;
        h.send("telegram:2", "sub").await;
        h.telegram.clear();

        h.send("telegram:1", "i2p").await;

        assert!(
            h.engine.store().room().unwrap().is_none(),
            "a mismatched address must not become a room"
        );
        assert!(
            h.replies_to("2").is_empty(),
            "nobody should have been woken: {:?}",
            h.replies_to("2")
        );
        assert!(
            h.subscriber("telegram:1").open_log.is_empty(),
            "a failed open costs no quota"
        );
    }

    #[tokio::test]
    async fn only_the_slow_networks_acknowledge_before_the_hook_returns() {
        // Tor, I2P, and Reticulum take long enough that the bot would look
        // dead without an "on it" reply. A web room is minted outright, so
        // the same reply would be overtaken by the broadcast it promises
        // and read as a bug.
        for (verb, expects_ack) in [
            ("tor", true),
            ("i2p", true),
            ("rns", true),
            ("web", false),
        ] {
            let h = Harness::new("open", "", "2h");
            h.send("telegram:1", "sub").await;
            h.telegram.clear();

            h.send("telegram:1", verb).await;

            let acked = h
                .replies_to("1")
                .iter()
                .any(|r| r.contains("opening the room"));
            assert_eq!(acked, expects_ack, "{verb}: {:?}", h.replies_to("1"));
        }
    }

    #[tokio::test]
    async fn a_held_web_request_is_released_as_a_web_room() {
        let h = Harness::new(
            "open",
            "[[tier]]\nname = \"held\"\nhold_for_approval = true\n",
            "2h",
        );
        h.send("telegram:1", "sub").await;
        {
            let mut subs = h.engine.subscribers().unwrap();
            subs.get_mut(&"telegram:1".parse().unwrap()).unwrap().tier = "held".into();
            h.engine.store().save_subscribers(&subs).unwrap();
        }

        h.send("telegram:1", "web drinks").await;
        let pending = h.engine.store().pending().unwrap();
        assert_eq!(pending.0[0].kind, ProviderKind::Web, "the verb is remembered");

        let mut pending = h.engine.store().pending().unwrap();
        pending.0[0].approved = true;
        h.engine.store().save_pending(&pending).unwrap();
        let released = h.engine.take_approved_signals().unwrap();
        h.engine
            .provision(
                &released[0].endpoint,
                released[0].kind,
                released[0].note.clone(),
            )
            .await
            .unwrap();

        let room = h.engine.store().room().unwrap().unwrap();
        assert_eq!(room.provider_kind(), ProviderKind::Web);
        assert_eq!(room.note.as_deref(), Some("drinks"));
    }

    #[tokio::test]
    async fn the_teardown_hook_that_runs_is_the_one_for_the_live_rooms_kind() {
        let h = Harness::build(Setup {
            ttl: "1s",
            // Each provider's down hook logs which one ran.
            tor: Some((
                hooks_publishing(ONION).unwrap().0,
                "#!/bin/sh\necho \"tor:$PARTY_LINE_PAGER_ROOM_ID\" >> \"$(dirname \"$0\")/torn-down\"\n"
                    .into(),
            )),
            web: Some((
                web_hooks().unwrap().0,
                "#!/bin/sh\necho \"web:$PARTY_LINE_PAGER_ROOM_ID\" >> \"$(dirname \"$0\")/torn-down\"\n"
                    .into(),
            )),
            ..Setup::default()
        });
        h.send("telegram:1", "sub").await;
        h.send("telegram:1", "web").await;

        tokio::time::sleep(Duration::from_millis(1400)).await;

        assert!(h.engine.store().room().unwrap().is_none(), "room state cleared");
        let torn = h.torn_down();
        assert!(torn.contains(&format!("web:{WEB_URL}")), "{torn}");
        assert!(!torn.contains("tor:"), "the wrong hook ran: {torn}");
    }

    #[tokio::test]
    async fn a_live_room_whose_provider_was_removed_can_still_be_retired() {
        // An admin deletes [provider.web] while a web room is up, then
        // restarts. Nothing can be torn down, but the instance must not stay
        // wedged behind a room nobody can retire.
        let h = Harness::build(Setup {
            web: None,
            ..Setup::default()
        });
        let now = Utc::now();
        h.engine
            .store()
            .save_room(&Room {
                kind: RoomKind::Web {
                    url: WEB_URL.into(),
                },
                opened_by: "telegram:1".parse().unwrap(),
                note: None,
                started_at: now,
                expires_at: now + chrono::Duration::hours(1),
            })
            .unwrap();

        h.engine.teardown(WEB_URL).await;

        assert!(h.engine.store().room().unwrap().is_none());
        assert!(h.torn_down().is_empty(), "no hook could have run");
    }

    #[tokio::test]
    async fn a_room_file_from_before_the_provider_split_does_not_block_startup() {
        let h = Harness::new("open", "", "2h");
        std::fs::write(
            h.engine.store().dir().join("room.json"),
            r#"{"onion":"abc.onion","secret":"S","opened_by":"telegram:1",
                "started_at":"2026-08-11T00:00:00Z","expires_at":"2126-08-11T02:00:00Z"}"#,
        )
        .unwrap();

        h.engine.recover().await.expect("recovery must not fail");

        assert!(h.engine.store().room().unwrap().is_none(), "cleared, not fatal");

        // And the instance still works afterwards.
        h.send("telegram:1", "sub").await;
        h.send("telegram:1", "tor").await;
        assert!(h.engine.store().room().unwrap().is_some());
    }
}
