//! One rustls setup for the whole daemon.
//!
//! Several dependencies (`tokio-xmpp`, `lettre`, the IMAP client) each want a
//! process-wide rustls crypto provider. Installing it twice is an error, so it
//! happens exactly once here, and every TLS client in the daemon trusts the
//! same root store.

use std::sync::{Arc, OnceLock};

use tokio_rustls::rustls::{ClientConfig, RootCertStore};

/// Installs the ring provider as the process default. Safe to call repeatedly.
pub fn install_default_provider() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        // An error means another call already installed one, which is fine.
        let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
    });
}

/// A client config trusting the Mozilla root store baked into the binary.
///
/// Deliberately not the system trust store: the daemon ships in a scratch-ish
/// container where `/etc/ssl` may be empty, and a silently empty root store
/// would turn every TLS connection into a runtime surprise.
pub fn client_config() -> ClientConfig {
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    let config = CONFIG.get_or_init(|| {
        install_default_provider();
        let roots = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        Arc::new(
            ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        )
    });
    (**config).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_provider_installs_once_and_the_root_store_is_not_empty() {
        install_default_provider();
        install_default_provider();
        assert!(
            !webpki_roots::TLS_SERVER_ROOTS.is_empty(),
            "the bundled root store must not be empty"
        );
        // Building twice must not panic on a duplicate provider install.
        let _ = client_config();
        let _ = client_config();
    }
}
