//! The adapter interface.
//!
//! Every chat network the partyline pager speaks implements this one trait.
//! Inbound, an adapter turns whatever its protocol calls a direct message into
//! an [`Incoming`] and pushes it onto a channel. Outbound, it takes an
//! [`OutMessage`] and delivers it to one address.
//!
//! Adding an eighth service is one file plus one line in [`crate::adapters`].

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use partylinepager_core::{Doc, EndpointId, Style};
use tokio::sync::mpsc;

/// A message from a human, addressed to the bot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Incoming {
    pub endpoint: EndpointId,
    pub text: String,
}

/// A message from the bot, addressed to a human.
///
/// The body travels as a [`Doc`] rather than a finished string, so each adapter
/// marks it up for its own network. That is the whole reason a subscriber on
/// Discord gets a tap-to-copy code block where one on IRC gets aligned spaces,
/// from one piece of copy written once in `render`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutMessage {
    /// Used by transports that have a notion of a subject or title (email,
    /// Apprise). Everywhere else it becomes the message's first line.
    pub title: Option<String>,
    pub body: Doc,
}

impl OutMessage {
    pub fn plain(body: impl Into<Doc>) -> Self {
        Self {
            title: None,
            body: body.into(),
        }
    }

    pub fn titled(title: impl Into<String>, body: impl Into<Doc>) -> Self {
        Self {
            title: Some(title.into()),
            body: body.into(),
        }
    }

    /// The whole message in one string, title included, marked up for `style`.
    ///
    /// Transports with a real subject line render [`Self::body`] directly and
    /// pass [`Self::title`] separately instead of calling this.
    pub fn render(&self, style: Style) -> String {
        match &self.title {
            Some(title) => Doc::new()
                .heading(title.clone())
                .append(self.body.clone())
                .render(style),
            None => self.body.render(style),
        }
    }
}

#[async_trait]
pub trait Transport: Send + Sync + 'static {
    /// Transport name, which is also the prefix of every endpoint id it owns.
    fn name(&self) -> &str;

    /// Runs the inbound loop until it fails. The supervisor restarts it.
    async fn run(&self, tx: mpsc::Sender<Incoming>) -> Result<()>;

    /// Delivers one message to one address on this transport.
    async fn send(&self, address: &str, msg: &OutMessage) -> Result<()>;
}

/// Restarts an adapter forever with capped exponential backoff.
///
/// A chat network going down must never take the daemon with it: the other six
/// keep running, and a room opened on Matrix still reaches everyone on IRC.
pub async fn supervise(transport: Arc<dyn Transport>, tx: mpsc::Sender<Incoming>) {
    let mut backoff = Duration::from_secs(1);
    let max = Duration::from_secs(300);

    loop {
        let started = std::time::Instant::now();
        match transport.run(tx.clone()).await {
            Ok(()) => tracing::warn!(transport = transport.name(), "adapter exited cleanly"),
            Err(e) => tracing::error!(transport = transport.name(), error = ?e, "adapter failed"),
        }

        // A connection that survived a while is not in a crash loop, so start
        // its backoff over rather than punishing it for a one-off blip.
        if started.elapsed() > Duration::from_secs(60) {
            backoff = Duration::from_secs(1);
        }

        tracing::info!(
            transport = transport.name(),
            seconds = backoff.as_secs(),
            "reconnecting"
        );
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(max);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_becomes_the_first_line_for_transports_with_no_subject() {
        let msg = OutMessage::titled("Signal up", Doc::new().para("onion + secret"));
        assert_eq!(msg.render(Style::Plain), "Signal up\n\nonion + secret");
        assert_eq!(
            OutMessage::plain(Doc::new().para("body")).render(Style::Plain),
            "body"
        );
    }

    #[test]
    fn the_title_is_marked_up_as_a_heading_where_the_transport_has_one() {
        let msg = OutMessage::titled("Signal up", Doc::new().para("body"));
        assert!(msg.render(Style::Markdown).starts_with("**Signal up**"));
        assert!(msg.render(Style::TelegramHtml).starts_with("<b>Signal up</b>"));
    }
}
