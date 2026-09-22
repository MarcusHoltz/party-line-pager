# Troubleshooting

## Startup

**"`$NAME`, which is unset"**
: An `env:NAME` secret has no value. Check `.env` and the
`environment:` block in the compose file.

**"no [provider.tor] or [provider.web] section"**
: Every `[provider.*]` section in `policy.toml` was deleted or
commented out. At least one provider must be configured.
See `config/policy.example.toml` for the available providers.

**"no chat adapters are enabled"**
: Every section in `adapters.toml` is missing or has
`enabled = false`. With none, there is no way to subscribe or
open a room.

**"room.json could not be read"**
: The state file is corrupt or was hand-edited into invalid JSON.
The daemon clears it and starts fresh. If a party line was live
at the time, its provider container may still be running; stop
it by hand with `docker compose down` in the transport directory.

## Rooms

**Host gets "warming the line" and nothing else**
: The provider hook failed or timed out.
`docker compose logs partylinepagerd` carries the hook's stderr.
Common causes: checkout dir wrong inside the container, Docker
socket not mounted, Tor needing longer than
`BOOTSTRAP_TIMEOUT`, or a transport compose file missing
`command: ["relay"]` (without it, partyline.sh blocks on a
confirmation prompt when run detached).

**Bot ignores `web` (or `tor`) completely**
: That command has no `[provider.*]` section, so it's not a
command on this instance. `--check` lists the commands that exist.
`help` never offers one that doesn't.

**`web` room answers "the line would not come up"**
: Room URL failed validation. The logs name the reason. Usually:
`base_url` in policy doesn't match the hook's output, `path` or
`static_slug` has a character outside `[A-Za-z0-9_/-]`, or a
custom hook emitted an invalid path.

  Test the hook directly:

  ```sh
  docker compose run --rm \
    --entrypoint /opt/partylinepager/hooks/provider-web.sh \
    -e PARTYLINEPAGER_WEB_BASE_URL=https://your.host \
    -e PARTYLINEPAGER_WEB_PATH=join \
    -e PARTYLINEPAGER_WEB_STATIC_SLUG= \
    partylinepagerd
  ```

**Web room link still works after it closed**
: Expected. The TTL frees the instance to open the next room;
it cannot end a call hosted on someone else's server.

## Subscribers

**Nobody received the broadcast**
: `partylinepagerctl who` and check for `active`. Pending
subscribers receive nothing. So do subscribers inside their
quiet window, silently and by design.

**Quiet hours are not silencing anything**
: They need a timezone. `status` shows `Timezone: unset` when
that is the problem. The bot refuses to guess an offset rather
than wake somebody at 3am.

## Lifecycle

**Room outlived a daemon restart**
: Expected. On startup the daemon re-arms the TTL if the room
is still live, and runs the teardown hook if it is not.

**Command sent while the daemon was down got no response**
: Expected for Telegram and Mastodon: the backlog is confirmed
but not acted on. Send it again.
