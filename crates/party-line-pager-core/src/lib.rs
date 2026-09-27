//! Core logic for PartyLinePager: configuration, persisted state, policy decisions,
//! and the user command parser.
//!
//! This crate deliberately performs no network I/O. Everything here is either
//! pure computation or reads and writes inside the state directory, which keeps
//! the security-relevant decisions (who may open a room, who receives it,
//! when a quota resets) testable without a running chat network.

pub mod command;
pub mod config;
pub mod doc;
pub mod endpoint;
pub mod error;
pub mod policy;
pub mod quiet;
pub mod render;
pub mod secret;
pub mod state;

pub use command::Command;
pub use config::{CredsMode, Policy, ProviderKind, Transport};
pub use doc::{Doc, Style};
pub use endpoint::EndpointId;
pub use error::{Error, Result};
pub use quiet::QuietWindow;
pub use state::{Delivery, Room, RoomKind, Store, Subscriber, SubscriberStatus, Subscribers};
