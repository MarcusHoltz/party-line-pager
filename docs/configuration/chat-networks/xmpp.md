# XMPP

Endpoint format: `xmpp:party-line-pager@example.org`

## Setup

Register an account for the bot on any server, then configure:

```toml
[xmpp]
enabled = true
jid = "party-line-pager@example.org"
password = "env:XMPP_PASSWORD"
tls = true
```

The JID is bare (no resource). The resource is chosen at
connect time.

There is no server or port setting. XMPP resolves the server
from the domain: the adapter looks up `_xmpp-client._tcp` and
falls back to the domain's own address on port 5222.

## Behavior

- The bot **publishes presence on connect**. Without it, the
  bot is logged in but silently unreachable (no available
  resource for the server to route to).
- Message bodies are read from whatever `xml:lang` key the
  server files them under. ejabberd stamps `xml:lang='en'` by
  default, filing the body under `en`.
- The resource is stripped from incoming messages, so
  `party-line-pager@example.org` is one subscriber whether they write
  from phone or laptop. Broadcasts go to the bare JID, so
  every online client gets a copy.
- `groupchat` and `error` stanzas are never treated as
  commands.

!!! warning "Keep TLS on"

    `tls = false` sends the password in the clear. SASL PLAIN
    is base64, not encryption. It exists for the throwaway
    server the live tests use.

## Self-hosting

[Prosody](https://prosody.im) and
[ejabberd](https://www.ejabberd.im) both run in one container.

## Config reference

| Key | Default | Notes |
|---|---|---|
| `enabled` | `true` | |
| `jid` | required | Bare JID (no resource) |
| `password` | required | Supports `env:NAME` |
| `tls` | `true` | STARTTLS negotiation |

## Full image env vars

| Variable | Default | Notes |
|---|---|---|
| `XMPP_JID` | (none) | Enables `[xmpp]` adapter |
| `XMPP_PASSWORD` | (none) | Account password |
| `XMPP_TLS` | `true` | STARTTLS negotiation |
