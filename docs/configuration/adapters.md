# Chat Networks

`adapters.toml` holds credentials. `chmod 600`, never commit.
Any secret may be written as `env:NAME` to read it from the
environment instead.

The daemon logs a warning at startup if the file is readable
beyond its owner, and refuses to start if an `env:` name is
unset. Unknown keys are a startup error rather than a silent
typo.

Every adapter follows three rules:

1. It only reacts to a **direct message**
2. It never makes a policy decision
3. Its endpoint address is a stable identifier, not a display
   name

## Supported networks

| Network | Endpoint format | Setup guide |
|---|---|---|
| Telegram | `telegram:123456789` | [Telegram](chat-networks/telegram.md) |
| Matrix | `matrix:@partylinepager:example.org` | [Matrix](chat-networks/matrix.md) |
| Signal | `signal:+15555550100` | [Signal](chat-networks/signal.md) |
| IRC | `irc:partylinepager` | [IRC](chat-networks/irc.md) |
| XMPP | `xmpp:partylinepager@example.org` | [XMPP](chat-networks/xmpp.md) |
| Mastodon | `mastodon:partylinepager@example.org` | [Mastodon](chat-networks/mastodon.md) |
| Email | `email:partylinepager@example.org` | [Email](chat-networks/email.md) |
| Mattermost | `mattermost:8y9z...` (26-char user id) | [Mattermost](chat-networks/mattermost.md) |
| Discord | `discord:891...` (numeric snowflake) | [Discord](chat-networks/discord.md) |
| Apprise | `apprise:ntfy://host/topic` | [Apprise](chat-networks/apprise.md) |

## Two caveats

- **IRC loses broadcasts.** A PRIVMSG to somebody who is not
  connected is gone. Tell IRC-only subscribers to add a second
  endpoint.
- **WhatsApp is not supported.** The only legitimate route is
  Meta's Cloud API (business account + pre-approved templates).
  Unofficial libraries impersonate a linked device and get
  numbers banned. If you must have it, run a Matrix bridge and
  subscribe the resulting Matrix endpoint.
