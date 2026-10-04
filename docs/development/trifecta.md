# The Trifecta: Party Line Scripts

Three party-line scripts live in sibling repositories, each
providing the same encrypted push-to-talk voice relay over a
different transport:

| Script | Repository | Transport |
|---|---|---|
| `tor-party-line/tor-party-line.sh` | [tor-party-line](https://gitlab.com/MarcusHoltz/tor-party-line) | Tor hidden services |
| `i2p-party-line/i2p-party-line.sh` | [i2p-party-line](https://gitlab.com/MarcusHoltz/i2p-party-line) | I2P garlic routing |
| `reticulum-party-line/rns-party-line.sh` | [reticulum-party-line](https://gitlab.com/MarcusHoltz/reticulum-party-line) | Reticulum mesh |

## Shared vs transport-specific code

~80% of each script is shared code (audio relay, encryption,
PTT, recording, playback, settings menus, cleanup). ~20% is
transport-specific (Tor hidden service, i2pd tunnels, RNS
bridge).

**When any shared function or behavior is changed in one script,
apply the equivalent change to the other two.**
Transport-specific functions are the exception: changes to
`setup_tor()` do not propagate to `setup_i2pd()` or
`write_rns_bridge()`.

### How to tell them apart

- 109 functions are named identically across all three. Those
  are shared.
- Functions unique to one script (e.g. `setup_tor`,
  `_i2p_certsdir`, `_emit_rns_bridge`) are transport-specific.
- Comments referencing "Tor" in shared code may be cruft from
  forking. Fix the comment in all three when you touch it.

## Per-project files

Each script has its own Dockerfile, docker-compose.yml, and
entrypoint. These are **not shared** and differ fundamentally
(different base images, different system packages, different
entrypoint patterns). Do not assume a Dockerfile change in one
applies to the others.

Reticulum has extra files the others lack:

- `rns_bridge.py` (638-line Python transport bridge, embedded
  as a heredoc in `rns-party-line.sh`)
- `tools/embed_bridge.py`
- `docker-entrypoint.sh`
- `measure/` directory

## How PartyLinePager uses them

Standard deployment uses `transports/{tor,i2p,reticulum}-party-line/`,
which contain relay-only compose files that pull published images
from Docker Hub. The upstream compose files in `party-lines/`
are not used for deployment because they require a git checkout,
mount audio devices that don't exist on servers, and use
local-only image names that can't be pulled from a registry.

Checkouts under `party-lines/` are for upstream development
only. Set `TOR_PARTYLINE_DIR`, `I2P_PARTYLINE_DIR`, or
`RETICULUM_PARTYLINE_DIR` in `.env` to point at one.

The [provider hooks](../reference/provider-hooks.md) call
`plp-runtime.sh` which calls `docker compose` (Standard) or
runs the scripts directly (Full) in relay mode. The hooks read
one address
from the relay's output and pass it back to the daemon as JSON.

The daemon validates the address format for the network type,
never trusting the hook's output blindly.

## DOCKER_MODE

All three scripts check `DOCKER_MODE` (defaults to `0`).
When `1`:

- Dependency install prompts are skipped (packages are
  already in the image)
- Daemon management is deferred to the entrypoint (tor/i2pd
  already started externally)
- Paths point to container-standard locations:
  - tor: `/data/.partyline`, `/var/lib/tor`
  - i2p: `/data/.partyline`
  - rns: `/app/data` (persistent), `/dev/shm/partyline-$$`
    (ephemeral)

When `0` (the default), the scripts enter native/script
mode: they prompt interactively for missing packages, manage
daemons themselves, and use user-local paths.

Each upstream Dockerfile sets `ENV DOCKER_MODE=1`. The
published Docker Hub images inherit this, so compose-mode
deployments (pulling `marcusholtz/*-party-line:latest`) get
it automatically.

**`deploy/Dockerfile.full` must also set it.** The full
image builds from `debian:trixie-slim` (not from the
upstream images), clones the scripts, and runs them in
direct mode. Without `DOCKER_MODE=1` the scripts block on
"Install dependencies now? [Y/n]:" inside the container,
which hangs relay startup with no visible error in logs.

Any custom Dockerfile that bundles these scripts must set
`ENV DOCKER_MODE=1` or the relay will not start.

## Upstream versions

All three scripts are at version 2.1.0 (as of 2026-10-03).
Published Docker Hub images:

| Image | RNS lib | Base |
|---|---|---|
| `marcusholtz/tor-party-line:latest` | n/a | `debian:trixie-slim` |
| `marcusholtz/i2p-party-line:latest` | n/a | `debian:trixie-slim` |
| `marcusholtz/reticulum-party-line:latest` | 1.4.2 | `python:3.12-slim` |

`deploy/Dockerfile.full` must pin the same `rns==` version
as the upstream reticulum-party-line Dockerfile. A mismatch
means the full image runs upstream's `rns-party-line.sh`
against a different RNS library than the script was tested
with.

The Dockerfile also bumps the upstream tor entrypoint's
bootstrap timeout from 180s to 300s. On a cold start Tor
needs to download ~9000 microdescriptors, which can take 5-7
minutes on a slow link. Combined with `plp-runtime.sh`'s
retry logic (see [provider hooks](../reference/provider-hooks.md)),
a cold start still succeeds.

## Relay tuning

All three scripts expose relay-side env vars for anti-flood
and liveness control. The transport compose files in
`transports/` pass these through so operators can override
them in `.env` without editing the compose files:

| Variable | Default | Controls |
|---|---|---|
| `RELAY_IDLE_TIMEOUT` | 240 | Drop a caller after N seconds of silence |
| `RELAY_MAX_MSG_PER_SEC` | 15 | Per-caller message rate limit |
| `RELAY_MAX_INFLIGHT` | 64 | Cap concurrent FIFO-forward writes |
| `RELAY_WRITE_TIMEOUT` | 30 | Abandon a stalled write after N seconds |
| `MAX_LINE_BYTES` | 524288 | Drop inbound lines exceeding this size |

These defaults are the same across all three scripts.

## Upstream issues

Things found while building PartyLinePager that belong upstream
rather than here are tracked in the project's issue tracker.
