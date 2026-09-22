//! Endpoint identity.
//!
//! A subscriber *is* an address on a service. There is no person model and no
//! account linking: `telegram:12345` and `mailto:someone@example.org` are two
//! unrelated subscribers even if the same human owns both. Abuse of that fact
//! is the deploying admin's problem to police, by design.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// A transport-qualified address, rendered as `transport:address`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EndpointId {
    transport: String,
    address: String,
}

impl EndpointId {
    /// Build an endpoint id, rejecting anything that could corrupt state files
    /// or inject extra lines into an outbound message.
    pub fn new(transport: impl Into<String>, address: impl Into<String>) -> Result<Self> {
        let transport = transport.into().trim().to_ascii_lowercase();
        let mut address = address.into().trim().to_string();

        // Mail addresses are lowercased here rather than in the adapter, so
        // that both ways in agree. The adapter already lowercased what it read
        // off the wire; `partylinepagerctl add email:Marcus@Example.ORG` did not,
        // and the result was an endpoint on the roster that no incoming mail
        // could ever match. The admin sees a subscriber who never gets a
        // broadcast, the person sees themselves land in `pending` a second
        // time, and nothing in either view says why.
        //
        // Only mail. A Matrix user id or an XMPP resource is not ours to
        // case-fold.
        if transport == "email" {
            address = address.to_ascii_lowercase();
        }

        if transport.is_empty() {
            return Err(Error::Endpoint("empty transport".into()));
        }
        if !transport
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(Error::Endpoint(format!(
                "transport {transport:?} must be alphanumeric, '_' or '-'"
            )));
        }
        if address.is_empty() {
            return Err(Error::Endpoint("empty address".into()));
        }
        if address.chars().any(|c| c.is_control()) {
            return Err(Error::Endpoint(
                "address contains control characters".into(),
            ));
        }
        if address.len() > 512 {
            return Err(Error::Endpoint("address longer than 512 bytes".into()));
        }

        Ok(Self { transport, address })
    }

    pub fn transport(&self) -> &str {
        &self.transport
    }

    pub fn address(&self) -> &str {
        &self.address
    }
}

impl fmt::Display for EndpointId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.transport, self.address)
    }
}

impl FromStr for EndpointId {
    type Err = Error;

    /// Splits on the *first* colon, so addresses may contain colons of their
    /// own (`xmpp:user@host/resource:1` is fine).
    fn from_str(s: &str) -> Result<Self> {
        let (transport, address) = s
            .split_once(':')
            .ok_or_else(|| Error::Endpoint(format!("{s:?} is not transport:address")))?;
        EndpointId::new(transport, address)
    }
}

impl Serialize for EndpointId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for EndpointId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_string() {
        let id: EndpointId = "telegram:12345".parse().unwrap();
        assert_eq!(id.transport(), "telegram");
        assert_eq!(id.address(), "12345");
        assert_eq!(id.to_string(), "telegram:12345");
    }

    #[test]
    fn splits_on_first_colon_only() {
        let id: EndpointId = "xmpp:user@host/resource:1".parse().unwrap();
        assert_eq!(id.transport(), "xmpp");
        assert_eq!(id.address(), "user@host/resource:1");
    }

    #[test]
    fn mail_addresses_are_case_folded_so_both_ways_in_agree() {
        // The adapter lowercases what it reads off the wire. An admin typing
        // the same address with capitals has to land on the same subscriber,
        // or `add` silently creates one that no mail will ever reach.
        let typed: EndpointId = "email:Marcus@Example.ORG".parse().unwrap();
        assert_eq!(typed.to_string(), "email:marcus@example.org");
        assert_eq!(typed, EndpointId::new("email", "marcus@example.org").unwrap());
    }

    #[test]
    fn other_transports_keep_their_case() {
        // Matrix user ids and XMPP resources are not ours to fold: the server
        // decides what they mean, and two that differ only in case may well be
        // two different people.
        assert_eq!(
            "matrix:@Marcus:Example.org"
                .parse::<EndpointId>()
                .unwrap()
                .address(),
            "@Marcus:Example.org"
        );
        assert_eq!(
            "xmpp:marcus@example.org/Phone"
                .parse::<EndpointId>()
                .unwrap()
                .address(),
            "marcus@example.org/Phone"
        );
    }

    #[test]
    fn transport_is_case_insensitive() {
        let a: EndpointId = "Telegram:1".parse().unwrap();
        let b: EndpointId = "telegram:1".parse().unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn rejects_newline_injection() {
        assert!(EndpointId::new("email", "a@b.c\nsecret: leak").is_err());
        assert!(EndpointId::new("email", "a@b.c\r\n.").is_err());
    }

    #[test]
    fn rejects_malformed() {
        assert!("no-colon".parse::<EndpointId>().is_err());
        assert!(":address".parse::<EndpointId>().is_err());
        assert!("transport:".parse::<EndpointId>().is_err());
        assert!(EndpointId::new("bad transport", "x").is_err());
        assert!(EndpointId::new("email", "x".repeat(513)).is_err());
    }

    #[test]
    fn serde_uses_the_flat_string_form() {
        let id: EndpointId = "matrix:@a:example.org".parse().unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"matrix:@a:example.org\"");
        assert_eq!(serde_json::from_str::<EndpointId>(&json).unwrap(), id);
    }
}
