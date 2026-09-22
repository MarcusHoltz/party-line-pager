# Provider Hooks

The partyline pager never speaks Docker, Tor, MiroTalk, or any
backend's protocol. It runs two scripts per provider and reads
one JSON object back. This keeps the privileged half in shell
an admin can audit, and makes swapping backends a hook change
rather than a rewrite.

## Common plumbing

Every provider hook shares the same rules:

- stdin is always closed for the hook
- stderr is always logged
- `hook_timeout` (from `policy.toml`) always applies
- The JSON object must be the last stdout line starting with `{`

## Two contracts, not four

All three party lines use the same contract. Which network a
room is on is decided by the `[provider.*]` section that ran
the hook, never by anything the hook says. A hook wired to
the wrong network's compose file is caught by validation.

### Up hook contract

| | Party lines (`tor`, `i2p`, `rns`) | `web` |
|---|---|---|
| stdin | The shared secret, one line. | Closed immediately. |
| env | `PARTYLINEPAGER_TTL_SECS`, `PARTYLINEPAGER_NOTE` | `PARTYLINEPAGER_WEB_BASE_URL`, `PARTYLINEPAGER_WEB_PATH`, `PARTYLINEPAGER_WEB_STATIC_SLUG`, `PARTYLINEPAGER_TTL_SECS`, `PARTYLINEPAGER_NOTE` |
| stdout | `{"address":"...","ttl_secs":7200}` | `{"url":"...","ttl_secs":7200}` |
| exit | Non-zero = room didn't come up. The daemon tells the host, sends nothing to the roster, and **does not spend quota**. | Same. |

`ttl_secs` in the response is optional; the daemon uses
`room_ttl` from policy if absent.

**No port travels with an address.** `partyline.sh` accepts
only a bare address, and a pasted `address:port` breaks its
normalization.

### Down hook contract

Receives `PARTYLINEPAGER_ROOM_ID` (the address for a party
line, the room URL for web). Non-zero exit is logged and
otherwise ignored: a container that won't die must not block
the next room.

### Secret delivery

The secret arrives on stdin rather than in argv or the
environment, because `/proc/<pid>/cmdline` and
`/proc/<pid>/environ` are readable by any process running as
the same user. A web room has no secret, so its hook gets
nothing.

## Address validation

The daemon validates hook output before anyone sees it:

| Network | Shape | Example |
|---|---|---|
| Tor | 56 lowercase base32 chars + `.onion` | `abc...xyz.onion` |
| I2P | 52 lowercase base32 chars + `.b32.i2p` | `jhqkryid...abcd.b32.i2p` |
| Reticulum | exactly 32 lowercase hex chars | `3a1c9d4e07b21f88c2a04e7d612b0f4e` |

Whitespace and control characters are refused. A web room URL
must start with the configured `base_url` and have a path of
at most 200 characters drawn from `[A-Za-z0-9_/-]`.

A hook that prints a log line where its JSON should be, an
address for the wrong network, or a URL pointing at someone
else's server fails loudly.

## Shipped party line hooks

Each pair drives one transport directory's `docker compose` in
relay mode. By default the transport directory is under
`transports/` (published image, no checkout). Set
`TOR_PARTYLINE_DIR`, `I2P_PARTYLINE_DIR`, or
`RETICULUM_PARTYLINE_DIR` in `.env` to point at a git checkout
under `party-lines/` instead.

Near-identical by design; the differences are how each network
signals reachability and what is shredded for a fresh address.

Every party line hook mints nothing and stores nothing itself.
The relay is a fan-out for ciphertext it has no key for.

### Tor hooks

`hooks/provider-tor.sh` and `hooks/teardown-tor.sh`

| Variable | Default | Meaning |
|---|---|---|
| `TOR_PARTYLINE_DIR` | `/opt/tor-party-line` | Transport directory (image or checkout) |
| `COMPOSE_SERVICE` | `partyline` | Service to start |
| `WIPE_ONION` | `1` | Discard onion key first, forcing a fresh address. `0` keeps a stable address. |
| `BOOTSTRAP_TIMEOUT` | `240` | Seconds to wait for Tor to publish |

Readiness: appearance of `hidden_service/hostname`, read
through `docker compose exec` (the directory is `0700
debian-tor` inside the container).

### I2P hooks

`hooks/provider-i2p.sh` and `hooks/teardown-i2p.sh`

| Variable | Default | Meaning |
|---|---|---|
| `I2P_PARTYLINE_DIR` | `/opt/i2p-party-line` | Transport directory (image or checkout) |
| `I2P_COMPOSE_SERVICE` | `partyline` | Service to start |
| `I2P_DATA_DIR` | `/data/.partyline` | State dir inside container |
| `I2P_WIPE_KEY` | `1` | Discard destination key first |
| `I2P_BOOTSTRAP_TIMEOUT` | `240` | Seconds to wait for reachability |
| `I2P_RELAY_OVERRIDE` | `/opt/partylinepager/hooks/i2p-relay.override.yml` | Compose overlay dropping `/dev/snd` and audio socket |

Readiness is **not** the address file. i2pd writes an address
when the key exists, but the destination is not callable until
its LeaseSet reaches the floodfills. The hook waits for
`partyline.sh` to log `I2P destination active`, which it prints
only after self-dialling through its own SOCKS proxy. This is a
string match on human-facing output and the most fragile thing
in these hooks.

The transport compose file must set `command: ["relay"]`.
Without it, partyline.sh blocks on a confirmation prompt when
run detached (`docker compose up -d`), i2pd starts in the
background but the readiness check never executes, and the
hook times out. Measured bootstrap: 18-53s on a warm NetDb.

### Reticulum hooks

`hooks/provider-rns.sh` and `hooks/teardown-rns.sh`

| Variable | Default | Meaning |
|---|---|---|
| `RETICULUM_PARTYLINE_DIR` | `/opt/reticulum-party-line` | Transport directory (image or checkout) |
| `RETICULUM_COMPOSE_PROFILE` | `reflector` | Required compose profile |
| `RETICULUM_COMPOSE_SERVICE` | `reflector` | Service to start |
| `RETICULUM_DATA_DIR` | `/app/data` | State dir inside container |
| `RETICULUM_CONTAINER_UID` | `1000` | UID the image runs as |
| `RETICULUM_WIPE_KEY` | `1` | Discard RNS identity first |
| `RETICULUM_BOOTSTRAP_TIMEOUT` | `120` | Seconds to wait for announcement |

The file *is* the readiness signal: `rns_bridge.py` writes the
destination hash only once the destination exists and is
announced. The hook wipes any earlier copy before starting.

### Key wiping

Each backend's keys normally survive restarts. Without wiping,
every party in your instance's history shares one address. The
cost differs:

- **Tor**: 1-3 minutes of bootstrap
- **I2P**: ~23 seconds
- **Reticulum**: effectively nothing (derived from stored
  identity, not negotiated)

This is also why the host gets an immediate acknowledgement
before the hook returns for Tor and I2P, and none for
Reticulum, where it would be overtaken by the broadcast.

## Shipped web hooks

`hooks/provider-web.sh` and `hooks/teardown-web.sh` are
deliberately trivial.

Unless `PARTYLINEPAGER_WEB_STATIC_SLUG` is set, the up hook
reads 20 bytes from `/dev/urandom`, hex-encodes them, and
prints `{"url":"<base_url>/<path>/<40 hex chars>"}`. 160 bits
from the OS CSPRNG.

When a static slug is set, that exact string is used as the
room name every time. No Docker, no root, no waiting, no state.

The down hook does nothing but log. **The partyline pager
cannot close a web room.** The URL keeps working after the TTL.
The broadcast says so.

## Runtime shim: Standard vs Full

`hooks/plp-runtime.sh` abstracts transport lifecycle across two
modes:

- **`PARTYLINEPAGER_RUNTIME=compose`** (default, Standard):
  passthrough to `docker compose`
- **`PARTYLINEPAGER_RUNTIME=direct`** (Full image): local
  process management via PID files under
  `$PLP_PID_DIR` (default `/var/run/plp`)

All provider hooks call `plp-runtime.sh` instead of `docker
compose` directly. In compose mode, `PLP_COMPOSE_ARGS` passes
extra flags (override files, profiles). In direct mode, the
shim starts transport scripts directly and manages them with
PID files and signal-based process tree collection.
