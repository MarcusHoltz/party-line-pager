#![recursion_limit = "512"]
//! The PartyLinePager daemon.
//!
//! Layering, outermost first:
//!
//! 1. [`adapters`] speak one chat protocol each and do nothing else. They turn
//!    direct messages into [`transport::Incoming`] and deliver
//!    [`transport::OutMessage`] back out.
//! 2. [`engine`] owns every decision and every state mutation.
//! 3. [`provider`] runs the admin's hook scripts to bring a room up and down.
//! 4. [`fanout`] delivers one message to many subscribers.
//!
//! Nothing above layer 2 is allowed to decide anything about policy, which is
//! why an adapter can be reviewed in isolation.

pub mod adapters;
pub mod config;
pub mod crypto;
pub mod engine;
pub mod fanout;
pub mod mock;
pub mod provider;
pub mod transport;
pub mod yopass;

pub use engine::Engine;
pub use transport::{Incoming, OutMessage, Transport};

/// Sent on every HTTP request the daemon makes.
///
/// `reqwest` sends no `User-Agent` unless it is told to, and an anonymous
/// client is a scraper as far as some servers are concerned. GoToSocial
/// answers a request with an empty one with **418 I'm a teapot**, before it
/// looks at the token or the path, which is how this was found: the Mastodon
/// adapter could not talk to a GoToSocial instance at all. Telegram, Signal and
/// Apprise do not filter on it today, but the assumption that nobody will is
/// the same one that was already wrong once.
///
/// Naming the software and linking its source is the convention, and it is also
/// what lets the operator on the other end tell who is calling them.
pub const USER_AGENT: &str = concat!(
    "PartyLinePager/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/MarcusHoltz/party-line-pager)"
);
