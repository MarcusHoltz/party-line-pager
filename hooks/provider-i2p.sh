#!/bin/sh
# PartylinePager provider hook: bring up an i2p-party-line relay and print its
# .b32.i2p address.
#
# Contract with partylinepagerd:
#   stdin  the shared secret, one line. Never argv, never the environment,
#          because /proc/<pid>/cmdline and /proc/<pid>/environ are readable by
#          any process running as the same user.
#   env    PARTYLINEPAGER_TTL_SECS  how long the room is meant to live
#          PARTYLINEPAGER_NOTE      the host's one-line note, may be empty
#   stdout exactly one JSON object: {"address":"...b32.i2p"}
#          Anything else printed on stdout is ignored as long as the JSON object
#          is the last line starting with '{'. Log to stderr freely.
#   exit   non-zero means the room did not come up. The daemon then tells the
#          host, sends nothing to the roster, and does not spend their quota.
#
# The secret is read and discarded on purpose. A party-line relay is a dumb
# fan-out that forwards ciphertext it has no key for: "The relay operator does
# NOT need a shared secret" (i2p-party-line/partyline.sh, relay_mode). Writing
# it here would leave a plaintext secret on the relay host for the life of the
# room and buy nothing. The daemon still mints it and still broadcasts it; only
# the callers ever use it.

set -eu

HOOKS_DIR="$(cd "$(dirname "$0")" && pwd)"
PLR="$HOOKS_DIR/plp-runtime.sh"

# Its own variable rather than PARTYLINE_DIR: all four hooks run inside the
# same partylinepagerd container, so one shared name would have the last compose
# overlay loaded silently win and point every hook at one checkout.
PARTYLINE_DIR="${I2P_PARTYLINE_DIR:-/opt/i2p-party-line}"
COMPOSE_SERVICE="${I2P_COMPOSE_SERVICE:-partyline}"
# Where partyline.sh keeps its state inside the container (DOCKER_MODE sets
# DATA_DIR=/data/.partyline). The identity key is the only persistent file.
DATA_DIR="${I2P_DATA_DIR:-/data/.partyline}"
# Wiping the key forces i2pd to publish a brand new destination for every
# signal, so no two parties share an identifier. Measured cost on a cold
# container: 23s to callable.
# Set to 0 to keep a stable address and start faster.
WIPE_KEY="${I2P_WIPE_KEY:-1}"
BOOTSTRAP_TIMEOUT="${I2P_BOOTSTRAP_TIMEOUT:-240}"

# Relay-only overrides layered on top of the checkout's own compose file: no
# /dev/snd (a relay makes no sound, and a headless host has no such device, so
# `up` would fail on it) and no PulseAudio socket. Kept as a separate file so
# the checkout stays exactly as upstream ships it. Set to an empty value to run
# the shipped compose file unmodified.
RELAY_OVERRIDE="${I2P_RELAY_OVERRIDE-/opt/partylinepager/hooks/i2p-relay.override.yml}"

override_arg=""
if [ -n "$RELAY_OVERRIDE" ] && [ -f "$RELAY_OVERRIDE" ]; then
    override_arg="-f $RELAY_OVERRIDE"
fi
# shellcheck disable=SC2086
export PLP_COMPOSE_ARGS="-f docker-compose.yml $override_arg"

log() { echo "provider-i2p: $*" >&2; }

read -r SECRET || true
if [ -z "${SECRET:-}" ]; then
    log "no secret arrived on stdin"
    exit 1
fi
# Deliberately not stored anywhere. See the header.
SECRET=""

if [ ! -d "$PARTYLINE_DIR" ]; then
    log "I2P_PARTYLINE_DIR $PARTYLINE_DIR does not exist"
    exit 1
fi

# Stop anything left over from a previous run before touching its key: the
# state dir is owned by the container's root, not by whatever uid partylinepagerd
# runs as, so it can only be read or wiped from *inside* the container. Doing
# it under a still-running container would race i2pd's open handle on the key.
"$PLR" down "$PARTYLINE_DIR" >&2 || true

if [ "$WIPE_KEY" = "1" ]; then
    log "discarding the previous destination key"
    "$PLR" run "$PARTYLINE_DIR" "$COMPOSE_SERVICE" \
        --name "${COMPOSE_SERVICE}-wipe" --entrypoint sh \
        -c "rm -f '$DATA_DIR/partyline-keys.dat' '$DATA_DIR/address'" >&2
fi

# partyline.sh picks relay mode when it is in Docker with no TTY, which is
# exactly what `up -d` gives it in compose mode. In direct mode the shim
# pipes stdin from `tail -f /dev/null` to keep I2P's `read -r -t 2` loop
# from busy-spinning on a closed stdin.
log "starting the relay"
"$PLR" up "$PARTYLINE_DIR" "$COMPOSE_SERVICE" >&2

# The address file appears as soon as the key exists, which is well BEFORE the
# destination is callable: i2pd has to build tunnels and publish a LeaseSet to
# the floodfills first, and a caller who dials early gets a flat "not found"
# for no visible reason. partyline.sh gates on a self-dial through its own
# SOCKS proxy and only then prints this line, so the line is the readiness
# signal and the file alone is not.
READY_MARKER='I2P destination active'
waited=0
while ! "$PLR" logs "$PARTYLINE_DIR" "$COMPOSE_SERVICE" 2>/dev/null \
        | grep -q "$READY_MARKER"; do
    if [ "$waited" -ge "$BOOTSTRAP_TIMEOUT" ]; then
        log "i2pd did not publish a reachable destination within ${BOOTSTRAP_TIMEOUT}s"
        "$PLR" logs "$PARTYLINE_DIR" "$COMPOSE_SERVICE" 2>/dev/null | tail -20 >&2 || true
        "$PLR" down "$PARTYLINE_DIR" >&2 || true
        exit 1
    fi
    sleep 2
    waited=$((waited + 2))
done

ADDRESS="$("$PLR" exec "$PARTYLINE_DIR" "$COMPOSE_SERVICE" \
    sh -c "cat '$DATA_DIR/address'" | tr -d ' \t\r\n')"
if [ -z "$ADDRESS" ]; then
    log "the relay reported ready but wrote no address"
    "$PLR" down "$PARTYLINE_DIR" >&2 || true
    exit 1
fi

log "up at $ADDRESS after ${waited}s (ttl ${PARTYLINEPAGER_TTL_SECS:-unset}s)"
printf '{"address":"%s"}\n' "$ADDRESS"
