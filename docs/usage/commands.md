# Subscriber Commands

Twelve commands, identical on every network. Case-insensitive,
`/` `!` and `.` prefixes accepted, leading `@mentions` ignored.
Anything unrecognized gets **no reply at all**, so the bot
never argues in a channel or a thread.

## Command table

| Command | Effect |
|---|---|
| `tor [note]` | Open a Tor party line. Everyone gets the onion address and shared secret. |
| `i2p [note]` | Open an I2P party line. Everyone gets the `.b32.i2p` address and shared secret. |
| `rns [note]` | Open a Reticulum party line. Everyone gets the destination hash and shared secret. |
| `web [note]` | Open a video-call link. Everyone gets the room URL (no secret). |
| `close` | Tear down the room you opened, before it expires. Only the host can do this, and only if their tier allows it. |
| `sub` | Subscribe. |
| `unsub` | Unsubscribe. |
| `tz <zone>` | Set your IANA timezone, e.g. `tz America/Denver`. Required before quiet hours work. |
| `quiet 23:00-07:00` | Set quiet hours. `quiet off` clears, bare `quiet` shows. |
| `status` | Your tier, quota, quiet window, and whether a room is live. |
| `help [command]` | The list above, or one command in detail: `help close`. Works before you subscribe. |
| `wiki [topic]` | Deep-dive reference pages. Bare `wiki` lists topics, `wiki audio` reads one. Also spelled `guide`. |

## Details

### Command parsing

A command may appear on any of the first five non-empty lines,
so an email with a `Re:` subject and a quoted reply still
parses.

### Help

`help` on its own is the table above. `help <command>` is one
page for that command: what to type, what it does, and an
example. Aliases land on the same page (`help stop` and
`help unsubscribe` both give the `unsub` page).

A word that names nothing this instance answers gets the list
back rather than a "no such command", including a provider command
that is not configured. Refusing to confirm what doesn't exist
follows the same rule as silent-on-unknown.

### Wiki

`wiki` is deep-dive reference, separate from command help. Bare
`wiki` lists the topics, `wiki <topic>` reads one. Also spelled
`guide`.

| Topic | Covers |
|---|---|
| `audio` | Full-duplex vs half-duplex audio modes |
| `connect` | Which party-line client each transport needs |
| `mobile` | Running the party-line client on a phone |
| `security` | What is encrypted, what is not |
| `rooms` | How rooms work: opening, closing, expiry |
| `platforms` | Supported messaging services |
| `selfhost` | Running your own Party Line Pager |

A topic that names nothing falls back to the index, the same
way an unknown `help` word gets the list back.

### Provider commands

A command for a provider this instance doesn't have simply doesn't
exist. Sending `web` to a tor-only instance gets the same
silence as sending `hello`. `help` never lists a command the
instance can't answer.

The presence of the `[provider.*]` section in `policy.toml` is
the switch. No separate on/off toggle to drift out of sync.

### One room at a time

Only one room is live at a time, whichever command opened it.
Opening while a room is up repeats that room's details, costs
no quota, and starts nothing new.

Quota is shared across all four providers: an `i2p` room spends
one of a tier's `max_rooms` slots, exactly as a `tor` or `web`
room does.

### Party line clients

Each party line needs its own build of the client.
The transport script from `tor-party-line` dials onions, the
`i2p-party-line` build dials `.b32.i2p` addresses, and the
`reticulum-party-line` build dials destination hashes. Every
broadcast carries the link for the one that room needs.

### No admin commands in the parser

`approve`, `ban`, `pause` and the rest are not hidden or
permission-checked: they are simply not reachable from any
network. That is the point. See
[Administration](administration.md) for the admin CLI.
