//! Mastodon, over the standard Mastodon REST API.
//!
//! Only **direct** visibility mentions count as commands. A public "@bot
//! signal" toot is ignored, which matters because the reply would otherwise be
//! a public post naming an onion address.
//!
//! Endpoint addresses are `acct` handles (`user@instance` for remote accounts,
//! bare `user` for local ones), which is what the API wants back when replying.

use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use party_line_pager_core::{EndpointId, Style};
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::config;
use crate::transport::{Incoming, OutMessage, Transport};

pub struct Mastodon {
    base_url: String,
    token: String,
    poll_interval: Duration,
    client: reqwest::Client,
    /// Highest notification id already handled.
    since_id: Mutex<Option<String>>,
}

impl Mastodon {
    pub fn new(cfg: &config::Mastodon) -> Result<Self> {
        Ok(Self {
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            token: cfg.access_token.clone(),
            poll_interval: cfg.poll_interval,
            client: reqwest::Client::builder()
                // Not decoration: an instance that filters anonymous clients
                // rejects the request before it looks at the token. See
                // `crate::USER_AGENT`.
                .user_agent(crate::USER_AGENT)
                .timeout(Duration::from_secs(30))
                .build()?,
            since_id: Mutex::new(None),
        })
    }

    async fn poll(&self) -> Result<Vec<Incoming>> {
        let mut request = self
            .client
            .get(format!("{}/api/v1/notifications", self.base_url))
            .bearer_auth(&self.token)
            .query(&[("types[]", "mention"), ("limit", "40")]);

        if let Some(since) = self.since_id.lock().unwrap().clone() {
            request = request.query(&[("since_id", since)]);
        }

        let notifications: Vec<Notification> = request
            .send()
            .await
            .context("mastodon notifications request failed")?
            .error_for_status()?
            .json()
            .await
            .context("mastodon sent unparseable JSON")?;

        // The API returns newest first.
        if let Some(newest) = notifications.first() {
            *self.since_id.lock().unwrap() = Some(newest.id.clone());
        }

        Ok(extract(&notifications))
    }
}

#[async_trait]
impl Transport for Mastodon {
    fn name(&self) -> &str {
        "mastodon"
    }

    async fn run(&self, tx: mpsc::Sender<Incoming>) -> Result<()> {
        // Skip whatever arrived while the daemon was down: acting on a
        // day-old "signal" would wake people for a party nobody is at.
        if let Err(e) = self.poll().await {
            tracing::warn!(error = ?e, "mastodon priming poll failed");
        }

        loop {
            tokio::time::sleep(self.poll_interval).await;
            for incoming in self.poll().await? {
                if tx.send(incoming).await.is_err() {
                    return Ok(());
                }
            }
        }
    }

    async fn send(&self, address: &str, msg: &OutMessage) -> Result<()> {
        self.client
            .post(format!("{}/api/v1/statuses", self.base_url))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                // Vanilla Mastodon renders a status as plain text: markdown
                // arrives as literal asterisks, so there is nothing to mark up.
                "status": format!("@{address}\n{}", msg.render(Style::Plain)),
                "visibility": "direct",
            }))
            .send()
            .await
            .context("mastodon status post failed")?
            .error_for_status()?;
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct Notification {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    status: Option<Status>,
}

#[derive(Debug, Deserialize)]
struct Status {
    content: String,
    visibility: String,
    account: Account,
}

#[derive(Debug, Deserialize)]
struct Account {
    acct: String,
}

fn extract(notifications: &[Notification]) -> Vec<Incoming> {
    notifications
        .iter()
        .filter(|n| n.kind == "mention")
        .filter_map(|n| {
            let status = n.status.as_ref()?;
            if status.visibility != "direct" {
                return None;
            }
            let endpoint = EndpointId::new("mastodon", &status.account.acct).ok()?;
            Some(Incoming {
                endpoint,
                text: strip_html(&status.content),
            })
        })
        .collect()
}

/// Turns Mastodon's HTML status body back into plain text.
///
/// Statuses are HTML, and the parser must see `signal poker night` rather than
/// `<p>signal poker night</p>`. Block tags become newlines so a multi-line toot
/// still parses line by line.
fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut chars = html.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '<' {
            out.push(c);
            continue;
        }
        let mut tag = String::new();
        for c in chars.by_ref() {
            if c == '>' {
                break;
            }
            tag.push(c);
        }
        let name = tag.trim_start_matches('/').trim().to_ascii_lowercase();
        if name.starts_with("br") || name.starts_with('p') || name.starts_with("div") {
            out.push('\n');
        }
    }

    let decoded = out
        .replace("&apos;", "'")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        // Ampersand last, so "&amp;lt;" does not become "<".
        .replace("&amp;", "&");

    decoded
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notifications(json: &str) -> Vec<Notification> {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn direct_mentions_become_commands() {
        let got = extract(&notifications(
            r#"[{"id":"9","type":"mention","status":{
                "content":"<p><span class=\"h-card\"><a href=\"x\">@<span>bot</span></a></span> signal poker night</p>",
                "visibility":"direct",
                "account":{"acct":"marcus@example.org"}
            }}]"#,
        ));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].endpoint.to_string(), "mastodon:marcus@example.org");
        assert_eq!(got[0].text, "@bot signal poker night");
    }

    #[test]
    fn public_mentions_are_ignored() {
        for visibility in ["public", "unlisted", "private"] {
            let got = extract(&notifications(&format!(
                r#"[{{"id":"9","type":"mention","status":{{
                    "content":"<p>@bot signal</p>",
                    "visibility":"{visibility}",
                    "account":{{"acct":"someone"}}
                }}}}]"#
            )));
            assert!(got.is_empty(), "{visibility} must not be actionable");
        }
    }

    #[test]
    fn other_notification_types_are_ignored() {
        let got = extract(&notifications(
            r#"[{"id":"9","type":"favourite","status":{
                "content":"<p>signal</p>","visibility":"direct","account":{"acct":"a"}
            }}]"#,
        ));
        assert!(got.is_empty());
    }

    #[test]
    fn html_becomes_text_with_line_structure_preserved() {
        assert_eq!(strip_html("<p>signal</p><p>second line</p>"), "signal\nsecond line");
        assert_eq!(strip_html("a<br />b"), "a\nb");
        assert_eq!(strip_html("<p>tom &amp; jerry&#39;s</p>"), "tom & jerry's");
        assert_eq!(strip_html("<p>&amp;lt;script&amp;gt;</p>"), "&lt;script&gt;");
    }

    #[tokio::test]
    async fn replies_are_always_direct_visibility() {
        let (addr, requests) = crate::mock::http_stub(200).await;
        let mastodon = Mastodon::new(&config::Mastodon {
            enabled: true,
            base_url: format!("http://{addr}"),
            access_token: "t".into(),
            poll_interval: Duration::from_secs(30),
        })
        .unwrap();

        mastodon
            .send("marcus@example.org", &OutMessage::plain("onion + secret"))
            .await
            .unwrap();

        let request = requests.lock().unwrap().first().cloned().unwrap();
        assert!(request.contains("\"visibility\":\"direct\""), "{request}");
        assert!(request.contains("@marcus@example.org"), "{request}");
        assert!(
            request.contains(crate::USER_AGENT),
            "GoToSocial answers 418 to a request with no User-Agent, before it \
             looks at the token: {request}"
        );
    }
}
