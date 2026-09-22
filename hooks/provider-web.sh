#!/bin/sh
# PartylinePager provider hook: mint a MiroTalk-style room URL.
#
# There is nothing to provision. A room on a web meeting instance (MiroTalk,
# Jitsi, or any self-hosted alternative) exists because somebody opened its
# URL, so "raising a web room" is picking a slug and pasting it onto the
# configured base URL, under the configured path. That is also the entire
# access control: whoever has the link is in, there is no second secret.
#
# Contract with partylinepagerd:
#   stdin  nothing. This provider has no secret. Stdin is closed immediately,
#          so the drain below returns at once.
#   env    PARTYLINEPAGER_WEB_BASE_URL    the instance to mint on, from policy.toml
#          PARTYLINEPAGER_WEB_PATH        segment(s) between base_url and the slug,
#                                    from provider.web.path. May be empty.
#          PARTYLINEPAGER_WEB_STATIC_SLUG when non-empty, use this exact slug
#                                    instead of generating a random one, from
#                                    provider.web.static_slug. Lets an admin
#                                    hand out one reusable room URL instead of
#                                    a fresh crypto-random one every room.
#          PARTYLINEPAGER_TTL_SECS        how long the room is meant to live
#          PARTYLINEPAGER_NOTE            the host's one-line note, may be empty
#   stdout exactly one JSON object: {"url":"..."}
#          Anything else printed on stdout is ignored as long as the JSON object
#          is the last line starting with '{'. Log to stderr freely.
#   exit   non-zero means no room. The daemon then tells the host, sends
#          nothing to the roster, and does not spend their quota.
#
# Unlike provider-tor.sh this hook is unprivileged: no Docker, no root, no
# waiting. If your deployment needs an API call to pre-create rooms, this is
# the file to put it in.

set -eu

log() { echo "provider-web: $*" >&2; }

BASE_URL="${PARTYLINEPAGER_WEB_BASE_URL:?PARTYLINEPAGER_WEB_BASE_URL is required, set base_url under [provider.web]}"
WEB_PATH="${PARTYLINEPAGER_WEB_PATH:-}"
STATIC_SLUG="${PARTYLINEPAGER_WEB_STATIC_SLUG:-}"

# This provider takes no secret, but drain stdin anyway so the contract is the
# same shape as every other hook's.
cat >/dev/null || true

if [ -n "$STATIC_SLUG" ]; then
    SLUG="$STATIC_SLUG"
else
    # 160 bits from the OS CSPRNG, hex encoded. The daemon rejects anything
    # outside [A-Za-z0-9_/-], so keep the alphabet boring.
    SLUG="$(od -An -N20 -tx1 /dev/urandom | tr -d ' \t\n')"
    if [ "${#SLUG}" -ne 40 ]; then
        log "could not read 20 random bytes from /dev/urandom"
        exit 1
    fi
fi

# WEB_PATH may be empty (room straight under the domain), one segment
# ("join"), or several ("conf/rooms"). Strip any stray leading/trailing
# slashes an admin typed so the URL never doubles one up.
WEB_PATH="${WEB_PATH#/}"
WEB_PATH="${WEB_PATH%/}"

log "minting a room on ${BASE_URL%/}${WEB_PATH:+/$WEB_PATH} (static=${STATIC_SLUG:+yes} ttl ${PARTYLINEPAGER_TTL_SECS:-unset}s)"
printf '{"url":"%s/%s%s"}\n' "${BASE_URL%/}" "${WEB_PATH:+$WEB_PATH/}" "$SLUG"
