# The Trifecta: Party Line Scripts

Three party-line scripts live in sibling repositories, each
providing the same encrypted push-to-talk voice relay over a
different transport:

| Script | Repository | Transport |
|---|---|---|
| `tor-party-line/partyline.sh` | [tor-party-line](https://gitlab.com/MarcusHoltz/tor-party-line) | Tor hidden services |
| `i2p-party-line/partyline.sh` | [i2p-party-line](https://gitlab.com/MarcusHoltz/i2p-party-line) | I2P garlic routing |
| `reticulum-party-line/partyline.sh` | [reticulum-party-line](https://gitlab.com/MarcusHoltz/reticulum-party-line) | Reticulum mesh |

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
  as a heredoc in `partyline.sh`)
- `tools/embed_bridge.py`
- `docker-entrypoint.sh`
- `measure/` directory

## How PartylinePager uses them

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

## Upstream issues

Things found while building PartylinePager that belong upstream
rather than here are tracked in the project's issue tracker.
