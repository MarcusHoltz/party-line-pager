#!/bin/sh
# PartylinePager teardown hook: stop the i2p-party-line relay.
#
# Contract with partylinepagerd:
#   env    PARTYLINEPAGER_ROOM_ID  the address that is being retired
#   exit   non-zero is logged and otherwise ignored. The daemon forgets the room
#          either way, because a container that will not die must not block the
#          next signal.
#
# Runs when the room's TTL expires, when an admin runs `partylinepagerctl close`, and
# at startup if a room outlived the daemon.

set -eu

HOOKS_DIR="$(cd "$(dirname "$0")" && pwd)"
PLR="$HOOKS_DIR/plp-runtime.sh"

PARTYLINE_DIR="${I2P_PARTYLINE_DIR:-/opt/i2p-party-line}"
COMPOSE_SERVICE="${I2P_COMPOSE_SERVICE:-partyline}"
DATA_DIR="${I2P_DATA_DIR:-/data/.partyline}"
# Shred the key at teardown as well as at startup, so a stopped instance leaves
# no reusable identity on disk.
WIPE_KEY="${I2P_WIPE_KEY:-1}"

# Relay-only overrides layered on top of the checkout's own compose file.
RELAY_OVERRIDE="${I2P_RELAY_OVERRIDE-/opt/partylinepager/hooks/i2p-relay.override.yml}"

override_arg=""
if [ -n "$RELAY_OVERRIDE" ] && [ -f "$RELAY_OVERRIDE" ]; then
    override_arg="-f $RELAY_OVERRIDE"
fi
# shellcheck disable=SC2086
export PLP_COMPOSE_ARGS="-f docker-compose.yml $override_arg"

log() { echo "teardown-i2p: $*" >&2; }

log "closing ${PARTYLINEPAGER_ROOM_ID:-unknown}"

if [ ! -d "$PARTYLINE_DIR" ]; then
    log "I2P_PARTYLINE_DIR $PARTYLINE_DIR does not exist, nothing to stop"
    exit 0
fi

"$PLR" down "$PARTYLINE_DIR" >&2 || log "transport stop failed, continuing"

if [ "$WIPE_KEY" = "1" ]; then
    "$PLR" run "$PARTYLINE_DIR" "$COMPOSE_SERVICE" \
        --name "${COMPOSE_SERVICE}-wipe" --entrypoint sh \
        -c "rm -f '$DATA_DIR/partyline-keys.dat' '$DATA_DIR/address'" >&2 \
        || log "could not wipe the destination key, continuing"
fi

log "closed"
