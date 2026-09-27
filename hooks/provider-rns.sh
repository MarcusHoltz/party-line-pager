#!/bin/sh
# PartylinePager provider hook: bring up a reticulum-party-line reflector and print
# its destination hash.
#
# Contract with partylinepagerd:
#   stdin  the shared secret, one line. Never argv, never the environment,
#          because /proc/<pid>/cmdline and /proc/<pid>/environ are readable by
#          any process running as the same user.
#   env    PARTYLINEPAGER_TTL_SECS  how long the room is meant to live
#          PARTYLINEPAGER_NOTE      the host's one-line note, may be empty
#   stdout exactly one JSON object: {"address":"<32 hex characters>"}
#          Anything else printed on stdout is ignored as long as the JSON object
#          is the last line starting with '{'. Log to stderr freely.
#   exit   non-zero means the room did not come up. The daemon then tells the
#          host, sends nothing to the roster, and does not spend their quota.
#
# The secret is read and discarded on purpose, same as the other party lines:
# "The relay operator does NOT need a shared secret"
# (reticulum-party-line/rns-party-line.sh, relay_mode). The reflector fans out
# ciphertext it has no key for.
#
# Reticulum is NOT onion routing. The reflector sees every caller's IP address
# and the destination hash is announced across the network. Offer this command
# when reachability without port forwarding is the point; offer tor or i2p
# when anonymity is.

set -eu

HOOKS_DIR="$(cd "$(dirname "$0")" && pwd)"
PLR="$HOOKS_DIR/plp-runtime.sh"

# Its own variable rather than TOR_PARTYLINE_DIR: all four hooks run inside the
# same partylinepagerd container, so one shared name would have the last compose
# overlay loaded silently win and point every hook at one checkout.
PARTYLINE_DIR="${RETICULUM_PARTYLINE_DIR:-/opt/reticulum-party-line}"
# The compose file puts every service behind a profile, so a bare `up` is a
# no-op and the profile has to be named on every call.
COMPOSE_PROFILE="${RETICULUM_COMPOSE_PROFILE:-reflector}"
COMPOSE_SERVICE="${RETICULUM_COMPOSE_SERVICE:-reflector}"
# Where rns-party-line.sh keeps its persistent state inside the container. Only the
# identity and the address it derives live here; RNS's own storage is on tmpfs.
DATA_DIR="${RETICULUM_DATA_DIR:-/app/data}"
# The uid the image runs as (Dockerfile: USER partyline). Bind-mount sources
# Docker creates for us would otherwise be root-owned and unwritable.
CONTAINER_UID="${RETICULUM_CONTAINER_UID:-1000}"
# Wiping the identity forces a brand new destination hash for every signal, so
# no two parties share an identifier. Near-free here: the hash is derived from
# the identity rather than negotiated with a network.
WIPE_KEY="${RETICULUM_WIPE_KEY:-1}"
# Generous next to the 30s the bridge itself allows, so a slow reach for the
# public transport nodes is a retry rather than a failure.
BOOTSTRAP_TIMEOUT="${RETICULUM_BOOTSTRAP_TIMEOUT:-120}"

export PLP_COMPOSE_ARGS="--profile $COMPOSE_PROFILE"

log() { echo "provider-rns: $*" >&2; }

read -r SECRET || true
if [ -z "${SECRET:-}" ]; then
    log "no secret arrived on stdin"
    exit 1
fi
# Deliberately not stored anywhere. See the header.
SECRET=""

if [ ! -d "$PARTYLINE_DIR" ]; then
    log "RETICULUM_PARTYLINE_DIR $PARTYLINE_DIR does not exist"
    exit 1
fi

# The compose file declares a file-backed secret. Compose refuses to start at
# all if the host-side file is missing, so make sure one exists. Empty is the
# correct content: rns-party-line.sh reads an absent or empty file as "no secret
# set", which is exactly what a relay wants.
mkdir -p "$PARTYLINE_DIR/secrets"
[ -e "$PARTYLINE_DIR/secrets/shared_secret.txt" ] || \
    ( umask 077; : > "$PARTYLINE_DIR/secrets/shared_secret.txt" )

"$PLR" down "$PARTYLINE_DIR" >&2 || true

# Create the persistent data dir as the uid the image runs as. Docker would
# otherwise create a missing bind-mount source as root, and the container could
# not write the identity it is about to generate.
mkdir -p "$PARTYLINE_DIR/data"
"$PLR" run "$PARTYLINE_DIR" "$COMPOSE_SERVICE" \
    --user root --entrypoint sh \
    -c "mkdir -p '$DATA_DIR' && chown $CONTAINER_UID:$CONTAINER_UID '$DATA_DIR'" >&2 \
    || log "could not normalize ownership of $DATA_DIR, continuing"

if [ "$WIPE_KEY" = "1" ]; then
    log "discarding the previous identity"
    "$PLR" run "$PARTYLINE_DIR" "$COMPOSE_SERVICE" \
        --name "${COMPOSE_SERVICE}-wipe" --entrypoint sh \
        -c "rm -f '$DATA_DIR/identity' '$DATA_DIR/destination'" >&2
fi

log "starting the reflector"
"$PLR" up "$PARTYLINE_DIR" "$COMPOSE_SERVICE" >&2

# Unlike I2P, the file IS the readiness signal. rns_bridge.py writes it only
# once the destination exists and is announced (--dest-hash-out, written from
# the listen role), and the wipe above removed any copy an earlier run left
# behind, so a non-empty file here can only have come from this relay.
waited=0
while ! "$PLR" exec "$PARTYLINE_DIR" "$COMPOSE_SERVICE" \
        sh -c "test -s '$DATA_DIR/destination'" 2>/dev/null; do
    if [ "$waited" -ge "$BOOTSTRAP_TIMEOUT" ]; then
        log "the reflector did not publish a destination within ${BOOTSTRAP_TIMEOUT}s"
        "$PLR" logs "$PARTYLINE_DIR" "$COMPOSE_SERVICE" 2>/dev/null | tail -20 >&2 || true
        "$PLR" down "$PARTYLINE_DIR" >&2 || true
        exit 1
    fi
    sleep 2
    waited=$((waited + 2))
done

ADDRESS="$("$PLR" exec "$PARTYLINE_DIR" "$COMPOSE_SERVICE" \
    sh -c "cat '$DATA_DIR/destination'" | tr -d ' \t\r\n')"
if [ -z "$ADDRESS" ]; then
    log "the reflector reported a destination file but it was empty"
    "$PLR" down "$PARTYLINE_DIR" >&2 || true
    exit 1
fi

log "up at $ADDRESS after ${waited}s (ttl ${PARTYLINEPAGER_TTL_SECS:-unset}s)"
printf '{"address":"%s"}\n' "$ADDRESS"
