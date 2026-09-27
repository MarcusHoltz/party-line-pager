# IRC

Endpoint format: `irc:party-line-pager` (nickname).

## Setup

Register the bot's nick before enabling the adapter. On a
network that reserves registered nicknames, an unregistered bot
is refused at connect or renamed to a guest nick.

On Libera.Chat, from any client:

```
/msg NickServ REGISTER <password> <email>
/msg NickServ VERIFY REGISTER <nick> <code from the mail>
```

Put that password in `IRC_PASSWORD` and configure:

```toml
[irc]
enabled = true
server = "irc.libera.chat"
port = 6697
tls = true
nick = "party-line-pager"
# account = "party-line-pager"
# password = "env:IRC_PASSWORD"
channels = ["#your-channel"]
```

### SASL authentication

The password is sent over **SASL PLAIN** during connection
registration, before the server assigns a nickname. This is the
only point early enough to claim a reserved nick. A
`NICKSERV IDENTIFY` after the MOTD is too late.

Leave `password` out to connect without logging in, which is
fine on a network that reserves nothing.

`password` is the services account password, not a server
`PASS`. There is no setting for a server password.

### Channels

`channels` is optional. The bot idles in listed channels so
people can find it. Channel traffic is never treated as a
command. Only PRIVMSG to the bot's nick is parsed.

### Account vs nick

`account` is only needed when the services account name
differs from `nick`. Blank means they're the same (the normal
case).

## Behavior

- Only PRIVMSG to the bot's current nick is a command.
- **No offline delivery.** A broadcast to somebody not
  connected is lost. Tell IRC-only subscribers to add a second
  endpoint.
- The endpoint is a nickname. A subscriber who changes nick
  stops receiving until they `sub` again under the new one.
  Registering their nick is what makes it stable.

!!! warning "Keep TLS on"

    `tls = false` sends the SASL password in the clear.
    SASL PLAIN is base64, not encryption.

## Config reference

| Key | Default | Notes |
|---|---|---|
| `enabled` | `true` | |
| `server` | required | IRC server hostname |
| `port` | `6697` | Server port |
| `tls` | `true` | TLS for the connection |
| `nick` | required | Bot's nickname |
| `account` | (same as nick) | Services account, if different |
| `password` | (none) | NickServ/SASL password. Supports `env:NAME` |
| `channels` | `[]` | Channels to idle in for discoverability |

## Full image env vars

| Variable | Default | Notes |
|---|---|---|
| `IRC_SERVER` | (none) | Enables `[irc]` adapter |
| `IRC_PORT` | `6697` | Server port |
| `IRC_NICK` | `party-line-pager` | Bot's nickname |
| `IRC_TLS` | `true` | TLS for the connection |
| `IRC_CHANNEL` | (none) | Channel to idle in |
| `IRC_ACCOUNT` | (none) | Services account, if different from nick |
| `IRC_PASSWORD` | (none) | NickServ/SASL password |
