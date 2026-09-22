//! Delivery of one message to many subscribers.
//!
//! Two paths exist. Endpoints that arrived through an adapter are delivered by
//! that same adapter, which already holds an authenticated session. Endpoints an
//! admin added by hand as `apprise:<url>` go to the Apprise sidecar, which
//! covers 130+ services we will never write an adapter for.
//!
//! One failing recipient never stops the fanout. A broadcast that reaches
//! nineteen of twenty people is a success with a logged failure, not an error.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use partylinepager_core::state::Delivery;
use partylinepager_core::{EndpointId, Style, Subscriber};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::transport::{OutMessage, Transport};

/// POSTs to the stateless Apprise endpoint.
#[derive(Clone)]
pub struct AppriseClient {
    url: String,
    client: reqwest::Client,
}

impl AppriseClient {
    pub fn new(url: impl Into<String>, timeout: Duration) -> Result<Self> {
        Ok(Self {
            url: url.into(),
            client: reqwest::Client::builder()
                .user_agent(crate::USER_AGENT)
                .timeout(timeout)
                .build()?,
        })
    }

    pub async fn notify(&self, target: &str, msg: &OutMessage) -> Result<()> {
        let payload = serde_json::json!({
            "urls": target,
            "title": msg.title.clone().unwrap_or_default(),
            // Apprise fans out to 130+ services with wildly different markup
            // support and no way to ask which one is on the other end of a
            // given URL, so plain text is the only safe answer here.
            "body": msg.body.render(Style::Plain),
            "format": "text",
        });

        let response = self
            .client
            .post(&self.url)
            .json(&payload)
            .send()
            .await
            .with_context(|| format!("apprise POST to {} failed", self.url))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!("apprise returned {status}: {}", body.trim());
        }
        Ok(())
    }
}

/// Outcome of one broadcast.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct FanoutReport {
    pub delivered: usize,
    pub failures: Vec<(EndpointId, String)>,
}

impl FanoutReport {
    pub fn attempted(&self) -> usize {
        self.delivered + self.failures.len()
    }
}

pub struct Fanout {
    transports: HashMap<String, Arc<dyn Transport>>,
    apprise: Option<AppriseClient>,
    concurrency: usize,
    send_timeout: Duration,
}

impl Fanout {
    pub fn new(
        transports: HashMap<String, Arc<dyn Transport>>,
        apprise: Option<AppriseClient>,
        concurrency: usize,
        send_timeout: Duration,
    ) -> Self {
        Self {
            transports,
            apprise,
            concurrency: concurrency.max(1),
            send_timeout,
        }
    }

    /// Delivers to one subscriber, choosing the path from their endpoint.
    pub async fn send(&self, subscriber: &Subscriber, msg: &OutMessage) -> Result<()> {
        self.send_to(&subscriber.endpoint, msg).await
    }

    /// Delivers to any endpoint, subscribed or not. Replies to strangers use
    /// this: `help` has to work before you are on the roster.
    ///
    /// Every delivery is bounded by `send_timeout`. Adapters talk to networks
    /// that stall, and an unbounded send would hold its slot forever and take
    /// the whole broadcast, or the admin poll loop, down with it.
    pub async fn send_to(&self, endpoint: &EndpointId, msg: &OutMessage) -> Result<()> {
        tokio::time::timeout(self.send_timeout, self.deliver(endpoint, msg))
            .await
            .map_err(|_| {
                anyhow::anyhow!("delivery timed out after {}s", self.send_timeout.as_secs())
            })?
    }

    async fn deliver(&self, endpoint: &EndpointId, msg: &OutMessage) -> Result<()> {
        match Delivery::for_endpoint(endpoint) {
            Delivery::Native { transport, address } => {
                let adapter = self
                    .transports
                    .get(&transport)
                    .with_context(|| format!("no adapter enabled for transport {transport:?}"))?;
                adapter.send(&address, msg).await
            }
            Delivery::Apprise { url } => {
                let apprise = self
                    .apprise
                    .as_ref()
                    .context("an apprise: endpoint exists but no Apprise endpoint is configured")?;
                apprise.notify(&url, msg).await
            }
        }
    }

    /// Delivers the same message to everyone, `concurrency` at a time.
    pub async fn broadcast(
        self: &Arc<Self>,
        subscribers: Vec<Subscriber>,
        msg: OutMessage,
    ) -> FanoutReport {
        let addressed = subscribers
            .into_iter()
            .map(|subscriber| (subscriber, msg.clone()))
            .collect();
        self.broadcast_each(addressed).await
    }

    /// Delivers a message built for each recipient in turn.
    ///
    /// The invite takes this path: a closing time is only useful in the
    /// reader's own timezone, so the body is rendered once per subscriber
    /// rather than once per room. That costs one `Doc` per recipient, which is
    /// nothing next to the network call it is about to ride on.
    pub async fn broadcast_each(
        self: &Arc<Self>,
        messages: Vec<(Subscriber, OutMessage)>,
    ) -> FanoutReport {
        let permits = Arc::new(Semaphore::new(self.concurrency));
        let mut tasks = JoinSet::new();

        for (subscriber, msg) in messages {
            let me = Arc::clone(self);
            let permits = Arc::clone(&permits);
            tasks.spawn(async move {
                let _permit = permits.acquire().await;
                let result = me.send_to(&subscriber.endpoint, &msg).await;
                (subscriber.endpoint, result)
            });
        }

        let mut report = FanoutReport::default();
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok((_endpoint, Ok(()))) => report.delivered += 1,
                Ok((endpoint, Err(e))) => {
                    tracing::warn!(%endpoint, error = ?e, "delivery failed");
                    report.failures.push((endpoint, e.to_string()));
                }
                Err(e) => tracing::error!(error = ?e, "delivery task panicked"),
            }
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockTransport;
    use partylinepager_core::Subscriber;

    fn sub(id: &str) -> Subscriber {
        Subscriber::new(id.parse::<EndpointId>().unwrap(), "weekly")
    }

    fn fanout(mock: &Arc<MockTransport>) -> Arc<Fanout> {
        let mut transports: HashMap<String, Arc<dyn Transport>> = HashMap::new();
        transports.insert("telegram".into(), mock.clone());
        Arc::new(Fanout::new(transports, None, 4, Duration::from_secs(5)))
    }

    #[tokio::test]
    async fn delivers_natively_through_the_originating_adapter() {
        let mock = MockTransport::new("telegram");
        let fanout = fanout(&mock);

        let report = fanout
            .broadcast(
                vec![sub("telegram:1"), sub("telegram:2")],
                OutMessage::plain("line is up"),
            )
            .await;

        assert_eq!(report.delivered, 2);
        assert!(report.failures.is_empty());
        let sent = mock.sent();
        assert_eq!(sent.len(), 2);
        assert!(sent.iter().all(|(_, m)| m.body.plain() == "line is up"));
    }

    #[tokio::test]
    async fn one_failure_does_not_stop_the_rest() {
        let mock = MockTransport::new("telegram");
        mock.fail_for("2");
        let fanout = fanout(&mock);

        let report = fanout
            .broadcast(
                vec![sub("telegram:1"), sub("telegram:2"), sub("telegram:3")],
                OutMessage::plain("x"),
            )
            .await;

        assert_eq!(report.delivered, 2);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].0.to_string(), "telegram:2");
        assert_eq!(report.attempted(), 3);
    }

    #[tokio::test]
    async fn an_endpoint_with_no_adapter_fails_only_itself() {
        let mock = MockTransport::new("telegram");
        let fanout = fanout(&mock);

        let report = fanout
            .broadcast(
                vec![sub("telegram:1"), sub("matrix:@nobody:example.org")],
                OutMessage::plain("x"),
            )
            .await;

        assert_eq!(report.delivered, 1);
        assert_eq!(report.failures.len(), 1);
        assert!(report.failures[0].1.contains("no adapter enabled"));
    }

    #[tokio::test]
    async fn apprise_endpoints_without_a_sidecar_fail_clearly() {
        let mock = MockTransport::new("telegram");
        let fanout = fanout(&mock);
        let report = fanout
            .broadcast(vec![sub("apprise:ntfy://host/topic")], OutMessage::plain("x"))
            .await;
        assert_eq!(report.delivered, 0);
        assert!(report.failures[0].1.contains("no Apprise endpoint is configured"));
    }

    #[tokio::test]
    async fn apprise_endpoints_post_to_the_sidecar() {
        let (addr, requests) = crate::mock::http_stub(200).await;
        let mut transports: HashMap<String, Arc<dyn Transport>> = HashMap::new();
        let mock = MockTransport::new("telegram");
        transports.insert("telegram".into(), mock.clone());

        let apprise =
            AppriseClient::new(format!("http://{addr}/notify"), Duration::from_secs(5)).unwrap();
        let fanout = Arc::new(Fanout::new(transports, Some(apprise), 4, Duration::from_secs(5)));

        let report = fanout
            .broadcast(
                vec![sub("apprise:ntfy://ntfy.sh/partylinepager")],
                OutMessage::titled("PartylinePager", "onion + secret"),
            )
            .await;

        assert_eq!(report.delivered, 1, "{:?}", report.failures);
        let request = requests.lock().unwrap().first().cloned().unwrap();
        assert!(request.contains("ntfy://ntfy.sh/partylinepager"), "{request}");
        assert!(request.contains("onion + secret"), "{request}");
        assert!(
            request.contains(crate::USER_AGENT),
            "every request has to name the software; some servers reject anonymous \
             clients before reading anything else: {request}"
        );
    }

    #[tokio::test]
    async fn an_apprise_error_status_is_reported_as_a_failure() {
        let (addr, _requests) = crate::mock::http_stub(500).await;
        let apprise =
            AppriseClient::new(format!("http://{addr}/notify"), Duration::from_secs(5)).unwrap();
        let fanout = Arc::new(Fanout::new(HashMap::new(), Some(apprise), 4, Duration::from_secs(5)));

        let report = fanout
            .broadcast(vec![sub("apprise:ntfy://host/t")], OutMessage::plain("x"))
            .await;
        assert_eq!(report.delivered, 0);
        assert!(report.failures[0].1.contains("500"), "{:?}", report.failures);
    }

    #[tokio::test]
    async fn a_stalled_transport_is_cut_loose_instead_of_hanging_the_broadcast() {
        let mock = MockTransport::new("telegram");
        mock.set_delay(Duration::from_secs(30));
        let mut transports: HashMap<String, Arc<dyn Transport>> = HashMap::new();
        transports.insert("telegram".into(), mock.clone());
        let fanout = Arc::new(Fanout::new(
            transports,
            None,
            4,
            Duration::from_millis(100),
        ));

        let report = tokio::time::timeout(
            Duration::from_secs(5),
            fanout.broadcast(vec![sub("telegram:1")], OutMessage::plain("x")),
        )
        .await
        .expect("broadcast must return even when a transport stalls");

        assert_eq!(report.delivered, 0);
        assert!(report.failures[0].1.contains("timed out"), "{:?}", report.failures);
    }

    #[tokio::test]
    async fn concurrency_limit_is_respected() {
        let mock = MockTransport::new("telegram");
        mock.set_delay(Duration::from_millis(50));
        let mut transports: HashMap<String, Arc<dyn Transport>> = HashMap::new();
        transports.insert("telegram".into(), mock.clone());
        let fanout = Arc::new(Fanout::new(transports, None, 2, Duration::from_secs(5)));

        let subs: Vec<_> = (0..6).map(|i| sub(&format!("telegram:{i}"))).collect();
        let report = fanout.broadcast(subs, OutMessage::plain("x")).await;

        assert_eq!(report.delivered, 6);
        assert!(
            mock.max_concurrent() <= 2,
            "saw {} concurrent deliveries",
            mock.max_concurrent()
        );
    }
}
