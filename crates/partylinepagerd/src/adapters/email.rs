//! Email: IMAP in, SMTP out.
//!
//! The universal fallback. Anybody on any service can drive the bot from a mail
//! client, which is what keeps "works everywhere" true for the transports that
//! will never get an adapter.
//!
//! Only unseen messages are read, and each one is marked seen whether or not it
//! parsed, so a malformed mail cannot become an infinite loop.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use partylinepager_core::{EndpointId, Style};
use futures_util::TryStreamExt;
use mailparse::MailHeaderMap;
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message as Mail, Tokio1Executor};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::config;
use crate::transport::{Incoming, OutMessage, Transport};

pub struct Email {
    imap_host: String,
    imap_port: u16,
    imap_user: String,
    imap_password: String,
    mailbox: String,
    tls: bool,
    poll_interval: Duration,
    from: String,
    smtp: AsyncSmtpTransport<Tokio1Executor>,
}

impl Email {
    pub fn new(cfg: &config::Email) -> Result<Self> {
        crate::crypto::install_default_provider();

        // Port 465 is implicit TLS ("submissions"); 587 and 25 negotiate
        // STARTTLS. Choosing by port keeps a working config from failing with
        // an unreadable handshake error.
        let builder = if !cfg.tls {
            tracing::warn!(
                host = %cfg.smtp_host,
                "sending mail without TLS: the mailbox password is going over the wire in the clear"
            );
            Ok(AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(
                &cfg.smtp_host,
            ))
        } else if cfg.smtp_port == 465 {
            AsyncSmtpTransport::<Tokio1Executor>::relay(&cfg.smtp_host)
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&cfg.smtp_host)
        };
        let smtp = builder
            .with_context(|| format!("could not set up SMTP to {}", cfg.smtp_host))?
            .port(cfg.smtp_port)
            .credentials(Credentials::new(
                cfg.smtp_user.clone(),
                cfg.smtp_password.clone(),
            ))
            .build();

        Ok(Self {
            imap_host: cfg.imap_host.clone(),
            imap_port: cfg.imap_port,
            imap_user: cfg.imap_user.clone(),
            imap_password: cfg.imap_password.clone(),
            mailbox: cfg.mailbox.clone(),
            tls: cfg.tls,
            poll_interval: cfg.poll_interval,
            from: cfg.from.clone(),
            smtp,
        })
    }

    /// Fetches and marks every unseen message, returning the ones that parsed.
    ///
    /// The two arms differ only in the type of the socket, so everything after
    /// connecting lives in [`drain`], which is generic over it. Without that
    /// the whole session would have to be written twice.
    async fn poll(&self) -> Result<Vec<Incoming>> {
        let tcp = TcpStream::connect((self.imap_host.as_str(), self.imap_port))
            .await
            .with_context(|| format!("could not reach {}:{}", self.imap_host, self.imap_port))?;

        if !self.tls {
            tracing::warn!(
                host = %self.imap_host,
                "reading mail without TLS: the mailbox password is going over the wire in the clear"
            );
            return self.drain(async_imap::Client::new(tcp)).await;
        }

        let server_name = tokio_rustls::rustls::pki_types::ServerName::try_from(
            self.imap_host.clone(),
        )
        .map_err(|_| anyhow!("{:?} is not a valid TLS server name", self.imap_host))?;

        let tls = tokio_rustls::TlsConnector::from(Arc::new(crate::crypto::client_config()))
            .connect(server_name, tcp)
            .await
            .context("IMAP TLS handshake failed")?;

        // async-imap with the runtime-tokio feature takes tokio streams
        // directly, so no futures-io compatibility shim is needed.
        self.drain(async_imap::Client::new(tls)).await
    }

    /// Logs in, reads every unseen message, and marks them all seen.
    async fn drain<S>(&self, mut client: async_imap::Client<S>) -> Result<Vec<Incoming>>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + std::fmt::Debug,
    {
        client
            .read_response()
            .await
            .context("IMAP server sent no greeting")?;

        let mut session = client
            .login(&self.imap_user, &self.imap_password)
            .await
            .map_err(|(e, _)| e)
            .context("IMAP login failed")?;

        session.select(&self.mailbox).await?;
        let unseen = session.search("UNSEEN").await?;

        let mut found = Vec::new();
        if !unseen.is_empty() {
            let set = unseen
                .iter()
                .map(|seq| seq.to_string())
                .collect::<Vec<_>>()
                .join(",");

            let fetches: Vec<_> = session.fetch(&set, "RFC822").await?.try_collect().await?;
            for fetch in fetches {
                let Some(raw) = fetch.body() else { continue };
                match interpret(raw) {
                    Some(incoming) => found.push(incoming),
                    None => tracing::debug!("skipped an unparseable message"),
                }
            }

            // Mark everything seen, parsed or not, so one bad mail cannot be
            // re-read forever.
            let mut store = session.store(&set, "+FLAGS (\\Seen)").await?;
            while store.try_next().await?.is_some() {}
        }

        session.logout().await.ok();
        Ok(found)
    }
}

#[async_trait]
impl Transport for Email {
    fn name(&self) -> &str {
        "email"
    }

    async fn run(&self, tx: mpsc::Sender<Incoming>) -> Result<()> {
        loop {
            match self.poll().await {
                Ok(messages) => {
                    for incoming in messages {
                        if tx.send(incoming).await.is_err() {
                            return Ok(());
                        }
                    }
                }
                // A mail server hiccup should cost one poll, not the adapter.
                Err(e) => tracing::warn!(error = ?e, "IMAP poll failed"),
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    async fn send(&self, address: &str, msg: &OutMessage) -> Result<()> {
        let mail = Mail::builder()
            .from(self.from.parse().context("invalid `from` address")?)
            .to(address.parse().with_context(|| format!("invalid recipient {address}"))?)
            .subject(msg.title.clone().unwrap_or_else(|| "PartylinePager".to_string()))
            // Plain text, and the one transport where that is the right
            // answer rather than a limitation: mail clients render a
            // fixed-width part faithfully, and the title is already a real
            // subject line rather than a first line pretending to be one.
            .header(ContentType::TEXT_PLAIN)
            .body(msg.body.render(Style::Plain))
            .context("could not build the message")?;

        self.smtp
            .send(mail)
            .await
            .with_context(|| format!("SMTP delivery to {address} failed"))?;
        Ok(())
    }
}

/// Pulls the sender address and the text body out of a raw RFC822 message.
///
/// Multipart mail is walked for the first `text/plain` part, because a
/// phone mail client sends `multipart/alternative` and the HTML half would
/// otherwise reach the command parser as tag soup.
fn interpret(raw: &[u8]) -> Option<Incoming> {
    let parsed = mailparse::parse_mail(raw).ok()?;

    let from = parsed.headers.get_first_value("From")?;
    // Not lowercased here: `EndpointId::new` case-folds mail addresses, so that
    // this path and `partylinepagerctl add` cannot disagree about who a person is.
    let address = mailparse::addrparse(&from).ok()?.extract_single_info()?.addr;

    let text = plain_text(&parsed)?;
    let endpoint = EndpointId::new("email", address).ok()?;
    Some(Incoming {
        endpoint,
        text,
    })
}

/// Depth-first search for a text/plain body, falling back to the top level.
fn plain_text(part: &mailparse::ParsedMail) -> Option<String> {
    if part.subparts.is_empty() {
        return body_text(part);
    }
    for sub in &part.subparts {
        if sub.ctype.mimetype == "text/plain" {
            return body_text(sub);
        }
    }
    part.subparts.iter().find_map(plain_text)
}

/// Decodes one part's body.
///
/// When the sender declared a charset we honour it. When they did not,
/// `mailparse` assumes US-ASCII and mangles UTF-8, so valid UTF-8 wins instead:
/// undeclared UTF-8 is extremely common and a mangled command is a support
/// ticket.
fn body_text(part: &mailparse::ParsedMail) -> Option<String> {
    if !part.ctype.params.contains_key("charset") {
        if let Ok(raw) = part.get_body_raw() {
            if let Ok(utf8) = String::from_utf8(raw) {
                return Some(utf8);
            }
        }
    }
    part.get_body().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_message_yields_sender_and_body() {
        let raw = b"From: Marcus <Marcus@Example.ORG>\r\n\
                    To: bot@example.org\r\n\
                    Subject: hello\r\n\
                    \r\n\
                    signal poker night\r\n";
        let got = interpret(raw).unwrap();
        assert_eq!(
            got.endpoint.to_string(),
            "email:marcus@example.org",
            "addresses are lowercased so one human is one subscriber"
        );
        assert!(got.text.contains("signal poker night"));
    }

    #[test]
    fn a_bare_address_without_a_display_name_works() {
        let raw = b"From: marcus@example.org\r\n\r\nhelp\r\n";
        assert_eq!(
            interpret(raw).unwrap().endpoint.to_string(),
            "email:marcus@example.org"
        );
    }

    #[test]
    fn multipart_mail_uses_the_plain_text_half() {
        let raw = b"From: marcus@example.org\r\n\
                    Content-Type: multipart/alternative; boundary=\"b\"\r\n\
                    \r\n\
                    --b\r\n\
                    Content-Type: text/plain\r\n\
                    \r\n\
                    signal\r\n\
                    --b\r\n\
                    Content-Type: text/html\r\n\
                    \r\n\
                    <p>signal</p>\r\n\
                    --b--\r\n";
        let got = interpret(raw).unwrap();
        assert_eq!(got.text.trim(), "signal");
    }

    #[test]
    fn quoted_printable_bodies_are_decoded() {
        let raw = b"From: marcus@example.org\r\n\
                    Content-Type: text/plain\r\n\
                    Content-Transfer-Encoding: quoted-printable\r\n\
                    \r\n\
                    signal caf=C3=A9 night\r\n";
        let got = interpret(raw).unwrap();
        assert!(got.text.contains("café"), "{}", got.text);
    }

    #[test]
    fn a_declared_charset_is_honoured() {
        let raw = b"From: marcus@example.org\r\n\
                    Content-Type: text/plain; charset=utf-8\r\n\
                    Content-Transfer-Encoding: quoted-printable\r\n\
                    \r\n\
                    signal caf=C3=A9 night\r\n";
        assert!(interpret(raw).unwrap().text.contains("café"));
    }

    #[test]
    fn a_message_with_no_from_header_is_skipped() {
        assert!(interpret(b"Subject: nope\r\n\r\nsignal\r\n").is_none());
    }

    #[test]
    fn garbage_is_skipped_rather_than_panicking() {
        assert!(interpret(&[0xff, 0xfe, 0x00, 0x01]).is_none());
    }
}
