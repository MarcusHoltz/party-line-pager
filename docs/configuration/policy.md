# policy.toml

Admin-owned, read-only to the daemon, no secrets, safe to
commit. Read at startup; restart the daemon after editing.

TOML rather than YAML because `serde_yaml` was archived by its
author in March 2024 and the surviving fork is not something to
put underneath a security policy file.

## `[instance]`

| Key | Default | Meaning |
|---|---|---|
| `name` | `PartyLinePager` | Shown in broadcasts and `help`. |
| `signups` | `approval` | `open`, `approval`, or `closed`. |
| `default_tier` | required | Tier for a new subscriber. Must name an existing `[[tier]]`. |
| `room_ttl` | `2h` | Wall clock, not an idle timer. |
| `hook_timeout` | `300s` | Ceiling for the slowest provider. Tor: 1-3 min, I2P: ~23s, Reticulum/web: immediate. |
| `paused` | `false` | Boot default for the kill switch. |
| `creds_delivery` | `inline` | `inline` or `link`. See [Link mode](#link-mode). |
| `yopass_url` | `https://share.yopass.se` | Only read in `link` mode. Web host the share link points at. |
| `yopass_api` | `https://api2.yopass.se` | Only read in `link` mode. API the `yopass` CLI mints against. |

### Signup modes

- **`open`**: anyone who messages the bot `sub` is on the
  roster immediately. Correct for a public hangout, wrong for
  anything else.
- **`approval`**: requests wait for
  `party-line-pagerctl approve`. The default.
- **`closed`**: nobody new gets in.

### Link mode

`creds_delivery = "link"` replaces the plaintext
address+secret (or room URL) in every broadcast with a one-time
[Yopass](https://github.com/jhaals/yopass) link, minted
separately for each recipient. Each subscriber opens (and
burns) their own copy without affecting anyone else's.

The image builds a `yopass` CLI binary from source and pins it
to a tag. Minting happens against `yopass_url`/`yopass_api`,
the public `share.yopass.se`/`api2.yopass.se` by default.
Point both at a self-hosted instance to keep minted links off
the public one.

!!! warning "yopass_url and yopass_api are a matched pair"

    A secret written to one API is invisible to any frontend
    reading from another. The hosted service moved from
    `yopass.se`/`api.yopass.se` to
    `share.yopass.se`/`api2.yopass.se`, and the old API still
    accepts writes, so a half-updated pair mints without error
    and every link it produces opens on
    "Secret does not exist".

A link is minted once per room per recipient and reused for
that room's whole life, including repeated `status` checks.
If minting fails, that recipient is skipped rather than
silently falling back to plaintext.

Run `hooks/check-yopass-version.sh` to see whether the pinned
tag has fallen behind upstream.

## `[fanout]`

| Key | Default | Meaning |
|---|---|---|
| `apprise_url` | `http://apprise:8000/notify` | The sidecar. Only used for `apprise:` endpoints. |
| `timeout` | `30s` | Bounds **every** delivery, native adapters included. A network that stalls costs one recipient, not the broadcast. |
| `concurrency` | `8` | Deliveries in flight at once. |

## Provider sections

Each `[provider.*]` section adds one command. A section that is
present must be complete; deleting a whole section removes that
command. At least one must remain, or startup fails.

### `[provider.tor]`, `[provider.i2p]`, `[provider.rns]`

All three party lines take the same two keys:

| Key | Required | Meaning |
|---|---|---|
| `up` | yes | Hook that brings the room up. See [Provider Hooks](../reference/provider-hooks.md). |
| `down` | yes | Hook that tears it down. |

```toml
[provider.tor]
up   = "/opt/party-line-pager/hooks/provider-tor.sh"
down = "/opt/party-line-pager/hooks/teardown-tor.sh"

[provider.i2p]
up   = "/opt/party-line-pager/hooks/provider-i2p.sh"
down = "/opt/party-line-pager/hooks/teardown-i2p.sh"

[provider.rns]
up   = "/opt/party-line-pager/hooks/provider-rns.sh"
down = "/opt/party-line-pager/hooks/teardown-rns.sh"
```

### `[provider.web]`

| Key | Default | Meaning |
|---|---|---|
| `up` | required | Mints a room URL. |
| `down` | required | Runs at TTL. Usually a no-op. |
| `base_url` | `https://p2p.mirotalk.com` | The instance rooms are minted on. |
| `path` | `join` | Segment(s) between `base_url` and the room slug. |
| `static_slug` | unset | Reuse the same room name every time. |

```toml
[provider.web]
up       = "/opt/party-line-pager/hooks/provider-web.sh"
down     = "/opt/party-line-pager/hooks/teardown-web.sh"
base_url = "https://p2p.mirotalk.com"
path     = "join"
# static_slug = "movie-night"
```

**`base_url`** decides whose server your calls run on. The
default is the public MiroTalk instance. Self-host MiroTalk,
Jitsi, or anything else that mints a room by URL, and point
`base_url` at it. Must start with `http://` or `https://` and
carry no query string or fragment.

**`path`** is the convention your backend expects. MiroTalk's
default is `/join/<room>`, so `path = "join"`. A self-hosted
Jitsi that serves rooms straight off the domain wants
`path = ""`. Can be several segments deep (`"conf/rooms"`).
Trimmed of leading/trailing slashes.

**`static_slug`** trades unguessability for a stable link.
Unset (the default) mints a fresh 160-bit random room name per
room. Set it to reuse the same name. A static slug is a
standing invite to anyone who has ever seen it.

## `[[tier]]`

| Key | Default | Meaning |
|---|---|---|
| `name` | required | Matched against each subscriber's tier. |
| `may_open` | `true` | `false` makes the tier receive-only. |
| `may_receive` | `true` | `false` makes it write-only. |
| `window` | `0s` | Rolling quota period. `0s` is unlimited. |
| `max_rooms` | `1` | Rooms allowed inside one `window`. Ignored when `window` is `0s`. |
| `hold_for_approval` | `false` | Park requests for `party-line-pagerctl approve-request`. |
| `may_close` | `true` | `false` stops the tier from using `close`. |

Quotas are **rolling**, not calendar-aligned: "max_rooms since
the oldest one still in the window". No midnight herd.

Example: allow 2 rooms per 24h instead of the default 1:

```toml
[[tier]]
name = "weekly"
window = "24h"
max_rooms = 2
```

If a subscriber's recorded tier has been deleted from policy,
they fall back to `default_tier` rather than to nothing. A typo
cannot silently grant rights.

### Default: admin approves everything

Out of the box, `config/policy.example.toml` puts two gates between a
stranger and an open room:

**Gate 1: subscription approval.** `signups = "approval"` means
`sub` lands in `pending`. Nothing reaches them until
`party-line-pagerctl approve <endpoint>`.

**Gate 2: room-open approval.** The default tier (`weekly`)
sets `hold_for_approval = true`, so even after approval, every
room is parked until `party-line-pagerctl approve-request <id>`.

```toml
[instance]
signups      = "approval"   # gate 1
default_tier = "weekly"     # new subscribers land here

[[tier]]
name               = "weekly"
window             = "168h"
hold_for_approval  = true   # gate 2
```

### EZ-Mode: no admin approval

For a public hangout, a trusted team, or an admin who wants to
set it and walk away:

```toml
[instance]
signups      = "open"       # gate 1 removed
default_tier = "daily"      # tier with no hold

[[tier]]
name   = "daily"
window = "24h"
# hold_for_approval defaults to false: no gate 2
```

Apply:

```sh
docker compose restart party-line-pagerd
# or from the wizard: Policy menu, items 1 and 2
```

Verify:

```sh
docker compose run --rm party-line-pagerd --check
```
