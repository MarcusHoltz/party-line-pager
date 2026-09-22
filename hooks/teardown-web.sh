#!/bin/sh
# PartylinePager teardown hook: nothing to tear down.
#
# A MiroTalk-style room is a URL on a server that is always running. The
# partyline pager cannot revoke it, and anybody already in the call stays in
# it. All that happens at the TTL is that this instance stops offering the room
# and lets somebody open the next one.
#
# This script exists so the web provider has the same two-hook shape as every
# other one, and so there is somewhere obvious to put a real teardown (an API
# call that closes the room, a webhook, a log line for an audit trail) if your
# deployment grows one.
#
# Contract with partylinepagerd:
#   env    PARTYLINEPAGER_ROOM_ID  the room URL that is being retired
#   exit   non-zero is logged and otherwise ignored.

set -eu

echo "teardown-web: releasing ${PARTYLINEPAGER_ROOM_ID:-unknown} (the link itself stays valid)" >&2
exit 0
