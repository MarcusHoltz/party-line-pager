#!/bin/sh
# PartyLinePager teardown hook: stop the tor-party-line relay.
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

TOR_PARTYLINE_DIR="${TOR_PARTYLINE_DIR:-/opt/tor-party-line}"
COMPOSE_SERVICE="${COMPOSE_SERVICE:-partyline}"
SECRET_FILE="${SECRET_FILE:-$TOR_PARTYLINE_DIR/secrets/shared_secret.txt}"
# Shred the key at teardown as well as at startup, so a stopped instance leaves
# no reusable identity on disk.
WIPE_ONION="${WIPE_ONION:-1}"

log() { echo "teardown-tor: $*" >&2; }

log "closing ${PARTY_LINE_PAGER_ROOM_ID:-unknown}"

if [ ! -d "$TOR_PARTYLINE_DIR" ]; then
    log "TOR_PARTYLINE_DIR $TOR_PARTYLINE_DIR does not exist, nothing to stop"
    exit 0
fi

# A room can expire and get torn down here before provider-tor.sh ever runs in
# this container's lifetime (e.g. a stale room found at daemon startup). Create
# the secrets dir ourselves first, so Docker never auto-vivifies it as root
# when the wipe container below binds it.
mkdir -p "$(dirname "$SECRET_FILE")"

"$PLR" down "$TOR_PARTYLINE_DIR" >&2 || log "transport stop failed, continuing"

rm -f "$SECRET_FILE"
if [ "$WIPE_ONION" = "1" ]; then
    "$PLR" run "$TOR_PARTYLINE_DIR" "$COMPOSE_SERVICE" \
        --name "${COMPOSE_SERVICE}-wipe" --entrypoint sh \
        -c 'rm -rf /var/lib/tor/hidden_service' >&2 \
        || log "could not wipe the onion key, continuing"
fi

log "closed"
