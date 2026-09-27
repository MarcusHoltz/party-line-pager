#!/bin/sh
# PartyLinePager teardown hook: stop the reticulum-party-line reflector.
#
# Contract with party-line-pagerd:
#   env    PARTY_LINE_PAGER_ROOM_ID  the address that is being retired
#   exit   non-zero is logged and otherwise ignored. The daemon forgets the room
#          either way, because a container that will not die must not block the
#          next signal.
#
# Runs when the room's TTL expires, when an admin runs `party-line-pagerctl close`, and
# at startup if a room outlived the daemon.

set -eu

HOOKS_DIR="$(cd "$(dirname "$0")" && pwd)"
PLR="$HOOKS_DIR/plp-runtime.sh"

PARTYLINE_DIR="${RETICULUM_PARTYLINE_DIR:-/opt/reticulum-party-line}"
COMPOSE_PROFILE="${RETICULUM_COMPOSE_PROFILE:-reflector}"
COMPOSE_SERVICE="${RETICULUM_COMPOSE_SERVICE:-reflector}"
DATA_DIR="${RETICULUM_DATA_DIR:-/app/data}"
# Shred the identity at teardown as well as at startup, so a stopped instance
# leaves no reusable address on disk.
WIPE_KEY="${RETICULUM_WIPE_KEY:-1}"

export PLP_COMPOSE_ARGS="--profile $COMPOSE_PROFILE"

log() { echo "teardown-rns: $*" >&2; }

log "closing ${PARTY_LINE_PAGER_ROOM_ID:-unknown}"

if [ ! -d "$PARTYLINE_DIR" ]; then
    log "RETICULUM_PARTYLINE_DIR $PARTYLINE_DIR does not exist, nothing to stop"
    exit 0
fi

# A room can expire and get torn down here before provider-rns.sh ever runs in
# this container's lifetime (e.g. a stale room found at daemon startup). Create
# the secrets dir ourselves first, matching provider-rns.sh, so Docker never
# auto-vivifies it as root when the wipe container below binds it.
mkdir -p "$PARTYLINE_DIR/secrets"
[ -e "$PARTYLINE_DIR/secrets/shared_secret.txt" ] || \
    ( umask 077; : > "$PARTYLINE_DIR/secrets/shared_secret.txt" )

"$PLR" down "$PARTYLINE_DIR" >&2 || log "transport stop failed, continuing"

if [ "$WIPE_KEY" = "1" ]; then
    "$PLR" run "$PARTYLINE_DIR" "$COMPOSE_SERVICE" \
        --name "${COMPOSE_SERVICE}-wipe" --entrypoint sh \
        -c "rm -f '$DATA_DIR/identity' '$DATA_DIR/destination'" >&2 \
        || log "could not wipe the identity, continuing"
fi

log "closed"
