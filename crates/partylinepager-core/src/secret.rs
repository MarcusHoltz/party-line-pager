//! Secret generation.
//!
//! The partyline pager generates the shared secret rather than letting each
//! provider hook invent its own, so secret quality is decided in one audited
//! place regardless of which voice backend a deployment uses.
//!
//! 256 bits from the OS CSPRNG, rendered as unpadded base32 (A-Z and 2-7). The
//! alphabet matters: people read these aloud over another channel and type them
//! into a terminal, and base32 has no case ambiguity and no `0`/`O` confusion.

use data_encoding::BASE32_NOPAD;

use crate::error::Result;

/// Bytes of entropy in a room secret.
pub const SECRET_BYTES: usize = 32;

/// Bytes of entropy in a pending-signal id. Not a secret, just a handle an
/// admin types after `partylinepagerctl approve-request`.
pub const ID_BYTES: usize = 8;

/// A fresh 256-bit shared secret.
pub fn generate_secret() -> Result<String> {
    let mut buf = [0u8; SECRET_BYTES];
    getrandom::fill(&mut buf)?;
    Ok(BASE32_NOPAD.encode(&buf))
}

/// A short opaque identifier, lowercased for typing comfort.
pub fn generate_id() -> Result<String> {
    let mut buf = [0u8; ID_BYTES];
    getrandom::fill(&mut buf)?;
    Ok(BASE32_NOPAD.encode(&buf).to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn secret_has_the_expected_shape() {
        let s = generate_secret().unwrap();
        // 32 bytes = 256 bits, 5 bits per base32 character, rounded up.
        assert_eq!(s.len(), 52);
        assert!(
            s.chars().all(|c| c.is_ascii_uppercase() || ('2'..='7').contains(&c)),
            "unexpected characters in {s}"
        );
    }

    #[test]
    fn secrets_do_not_repeat() {
        let n = 1000;
        let unique: HashSet<String> = (0..n).map(|_| generate_secret().unwrap()).collect();
        assert_eq!(unique.len(), n, "CSPRNG produced a collision");
    }

    #[test]
    fn ids_are_short_and_lowercase() {
        let id = generate_id().unwrap();
        assert_eq!(id.len(), 13);
        assert!(id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
    }
}
