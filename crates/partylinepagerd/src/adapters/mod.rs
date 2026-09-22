//! One module per chat network.
//!
//! Every adapter obeys the same three rules:
//!
//! 1. It only ever reacts to a **direct message**. Nothing said in a channel,
//!    room, or public timeline is treated as a command, so the bot cannot be
//!    driven by somebody shouting in a room it happens to sit in.
//! 2. It never decides anything. It reports what it heard and delivers what it
//!    is given.
//! 3. Its endpoint address is stable for the person, so the roster survives
//!    display-name changes.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;

use crate::config::Adapters;
use crate::transport::Transport;

pub mod discord;
pub mod email;
pub mod irc;
pub mod mastodon;
pub mod matrix;
pub mod mattermost;
pub mod signal;
pub mod telegram;
pub mod xmpp;

/// Builds every adapter that is configured and enabled.
pub async fn build(config: &Adapters) -> Result<HashMap<String, Arc<dyn Transport>>> {
    let mut built: HashMap<String, Arc<dyn Transport>> = HashMap::new();

    if let Some(cfg) = config.telegram.as_ref().filter(|c| c.enabled) {
        built.insert("telegram".into(), Arc::new(telegram::Telegram::new(cfg)?));
    }
    if let Some(cfg) = config.matrix.as_ref().filter(|c| c.enabled) {
        built.insert("matrix".into(), Arc::new(matrix::Matrix::new(cfg).await?));
    }
    if let Some(cfg) = config.irc.as_ref().filter(|c| c.enabled) {
        built.insert("irc".into(), Arc::new(irc::Irc::new(cfg)?));
    }
    if let Some(cfg) = config.xmpp.as_ref().filter(|c| c.enabled) {
        built.insert("xmpp".into(), Arc::new(xmpp::Xmpp::new(cfg)?));
    }
    if let Some(cfg) = config.mastodon.as_ref().filter(|c| c.enabled) {
        built.insert("mastodon".into(), Arc::new(mastodon::Mastodon::new(cfg)?));
    }
    if let Some(cfg) = config.email.as_ref().filter(|c| c.enabled) {
        built.insert("email".into(), Arc::new(email::Email::new(cfg)?));
    }
    if let Some(cfg) = config.signal.as_ref().filter(|c| c.enabled) {
        built.insert("signal".into(), Arc::new(signal::Signal::new(cfg)?));
    }
    if let Some(cfg) = config.mattermost.as_ref().filter(|c| c.enabled) {
        built.insert(
            "mattermost".into(),
            Arc::new(mattermost::Mattermost::new(cfg).await?),
        );
    }
    if let Some(cfg) = config.discord.as_ref().filter(|c| c.enabled) {
        built.insert("discord".into(), Arc::new(discord::Discord::new(cfg)?));
    }

    Ok(built)
}
