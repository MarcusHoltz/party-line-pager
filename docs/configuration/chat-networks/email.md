# Email

Endpoint format: `email:partylinepager@example.org` (case-folded).

The universal fallback: anybody on any service can drive the
bot from a mail client.

## Setup

Any mailbox the bot can read over IMAP and send from over SMTP:

```toml
[email]
enabled = true
imap_host = "imap.example.org"
imap_port = 993
imap_user = "partylinepager@example.org"
imap_password = "env:IMAP_PASSWORD"
mailbox = "INBOX"

smtp_host = "smtp.example.org"
smtp_port = 587
smtp_user = "partylinepager@example.org"
smtp_password = "env:SMTP_PASSWORD"
from = "partylinepager@example.org"

tls = true
poll_interval = "60s"
```

### Separate credentials

IMAP and SMTP passwords are separate settings because some
providers issue separate credentials. Set both to the same
value when yours doesn't. The login and the address don't have
to match either: some servers want a bare username where the
address is `user@domain`.

### Encryption

SMTP encryption is chosen by port, not guesswork:

- **465**: implicit TLS
- **587** and **25**: STARTTLS negotiation

`tls = false` turns encryption off on both legs and exists for
test servers only. On a real mailbox it sends the password in
the clear.

### Polling

`poll_interval` is latency between sending mail and anything
happening. Only **unseen** mail is read, and every message is
marked seen whether or not it parsed, so one malformed mail
cannot be re-read forever.

## Behavior

- Addresses are case-folded: `PartylinePager@Example.ORG` and
  `partylinepager@example.org` are one subscriber.
- The broadcast subject line comes from the room's title.
- A command may appear on any of the first five non-empty
  lines, so a `Re:` subject and quoted reply still parse.

## Privacy note

Email is the slowest and least private adapter. The broadcast
carrying an onion address and shared secret sits in a mailbox
on somebody's server until deleted. Prefer Matrix for sensitive
use.

## Self-hosting

Full internet-facing email is a real undertaking (DNS, SPF,
DKIM, DMARC, reputation). For **internal** use (bot and
subscribers on the same server),
[docker-mailserver](https://docker-mailserver.github.io/docker-mailserver/latest/)
or Postfix+Dovecot works with no reputation problem.

## Config reference

| Key | Default | Notes |
|---|---|---|
| `enabled` | `true` | |
| `imap_host` | required | IMAP server |
| `imap_port` | `993` | IMAP port |
| `imap_user` | required | IMAP login |
| `imap_password` | required | Supports `env:NAME` |
| `mailbox` | `INBOX` | Mailbox to watch |
| `smtp_host` | required | SMTP server |
| `smtp_port` | `587` | SMTP port (465/587/25) |
| `smtp_user` | required | SMTP login |
| `smtp_password` | required | Supports `env:NAME` |
| `from` | required | Envelope and header From address |
| `tls` | `true` | Encrypt both IMAP and SMTP |
| `poll_interval` | `60s` | How often to check for mail |

## Full image env vars

| Variable | Default | Notes |
|---|---|---|
| `EMAIL_SMTP_HOST` | (none) | Enables `[email]` adapter |
| `EMAIL_SMTP_PORT` | `587` | SMTP port |
| `EMAIL_SMTP_USER` | (none) | SMTP login |
| `EMAIL_SMTP_PASSWORD` | (none) | SMTP password |
| `EMAIL_IMAP_HOST` | same as SMTP host | IMAP server |
| `EMAIL_IMAP_PORT` | `993` | IMAP port |
| `EMAIL_IMAP_USER` | same as SMTP user | IMAP login |
| `EMAIL_IMAP_PASSWORD` | same as SMTP password | IMAP password |
| `EMAIL_MAILBOX` | `INBOX` | Mailbox to watch |
| `EMAIL_FROM` | same as SMTP user | From address |
| `EMAIL_TLS` | `true` | Encrypt both legs |
| `EMAIL_POLL_INTERVAL` | `60s` | Mail check interval |
