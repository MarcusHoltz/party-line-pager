#!/bin/sh
# PartylinePager provider hook: bring up a tor-party-line relay and print its onion.
#
# Contract with partylinepagerd:
#   stdin  the shared secret, one line. Never argv, never the environment,
#          because /proc/<pid>/cmdline and /proc/<pid>/environ are readable by
#          any process running as the same user.
#   env    PARTYLINEPAGER_TTL_SECS  how long the room is meant to live
#          PARTYLINEPAGER_NOTE      the host's one-line note, may be empty
#   stdout exactly one JSON object: {"address":"...onion"}
#          Anything else printed on stdout is ignored as long as the JSON object
#          is the last line starting with '{'. Log to stderr freely.
#   exit   non-zero means the room did not come up. The daemon then tells the
#          host, sends nothing to the roster, and does not spend their quota.

set -eu

HOOKS_DIR="$(cd "$(dirname "$0")" && pwd)"
PLR="$HOOKS_DIR/plp-runtime.sh"

TOR_PARTYLINE_DIR="${TOR_PARTYLINE_DIR:-/opt/tor-party-line}"
COMPOSE_SERVICE="${COMPOSE_SERVICE:-partyline}"
SECRET_FILE="${SECRET_FILE:-$TOR_PARTYLINE_DIR/secrets/shared_secret.txt}"
# Wiping the key forces Tor to publish a brand new address for every signal, so
# no two parties share an identifier. Costs one to three minutes of bootstrap.
# Set to 0 to keep a stable address and start instantly.
WIPE_ONION="${WIPE_ONION:-1}"
BOOTSTRAP_TIMEOUT="${BOOTSTRAP_TIMEOUT:-240}"

log() { echo "provider-tor: $*" >&2; }

read -r SECRET || true
if [ -z "${SECRET:-}" ]; then
    log "no secret arrived on stdin"
    exit 1
fi

if [ ! -d "$TOR_PARTYLINE_DIR" ]; then
    log "TOR_PARTYLINE_DIR $TOR_PARTYLINE_DIR does not exist"
    exit 1
fi

# The party line reads the shared secret from this file. Write it before the
# relay starts, and keep it unreadable by anyone else on the box.
mkdir -p "$(dirname "$SECRET_FILE")"
( umask 077; printf '%s\n' "$SECRET" > "$SECRET_FILE" )

# Bring down any relay left over from a previous run before touching its onion
# key. In compose mode the key dir is owned by the container's `debian-tor`
# user, unreadable from the host. In direct mode the full container runs as
# root, so local access works.
"$PLR" down "$TOR_PARTYLINE_DIR" >&2 || true

if [ "$WIPE_ONION" = "1" ]; then
    log "discarding the previous onion key"
    "$PLR" run "$TOR_PARTYLINE_DIR" "$COMPOSE_SERVICE" \
        --name "${COMPOSE_SERVICE}-wipe" --entrypoint sh \
        -c 'rm -rf /var/lib/tor/hidden_service' >&2
fi

log "starting the relay"
"$PLR" up "$TOR_PARTYLINE_DIR" "$COMPOSE_SERVICE" >&2

# Tor writes the hostname file once the descriptor is ready.
waited=0
while ! "$PLR" exec "$TOR_PARTYLINE_DIR" "$COMPOSE_SERVICE" \
        sh -c 'test -s /var/lib/tor/hidden_service/hostname' 2>/dev/null; do
    if [ "$waited" -ge "$BOOTSTRAP_TIMEOUT" ]; then
        log "tor did not publish an onion within ${BOOTSTRAP_TIMEOUT}s"
        "$PLR" down "$TOR_PARTYLINE_DIR" >&2 || true
        exit 1
    fi
    sleep 2
    waited=$((waited + 2))
done

ONION="$("$PLR" exec "$TOR_PARTYLINE_DIR" "$COMPOSE_SERVICE" \
    sh -c 'cat /var/lib/tor/hidden_service/hostname' | tr -d ' \t\r\n')"
log "up at $ONION after ${waited}s (ttl ${PARTYLINEPAGER_TTL_SECS:-unset}s)"

# No port travels with the address: partyline.sh accepts only a bare .onion,
# and a pasted "onion:port" breaks its normalization instead of being ignored.
printf '{"address":"%s"}\n' "$ONION"
