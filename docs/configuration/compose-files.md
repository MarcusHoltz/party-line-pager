# Compose Files

One base file plus one overlay per thing you actually run.
Nothing starts that you didn't ask for, and a credential you
don't use is not in the daemon's environment at all.

## Pick your files

Take `docker-compose.yml`, then add one per box you tick:

### Adapter overlays

| File | When |
|---|---|
| `compose/docker-compose.telegram.yml` | `[telegram]` is in `adapters.toml` |
| `compose/docker-compose.matrix.yml` | `[matrix]` is in `adapters.toml` |
| `compose/docker-compose.irc.yml` | `[irc]` is in `adapters.toml` |
| `compose/docker-compose.xmpp.yml` | `[xmpp]` is in `adapters.toml` |
| `compose/docker-compose.mastodon.yml` | `[mastodon]` is in `adapters.toml` |
| `compose/docker-compose.email.yml` | `[email]` is in `adapters.toml` |
| `compose/docker-compose.signal.yml` | `[signal]` is in `adapters.toml` |
| `compose/docker-compose.mattermost.yml` | `[mattermost]` is in `adapters.toml` |
| `compose/docker-compose.discord.yml` | `[discord]` is in `adapters.toml` |

### Provider overlays

| File | When |
|---|---|
| `compose/docker-compose.docker-socket.yml` | Any party line is in `policy.toml`. Grants the Docker socket, once. |
| `compose/docker-compose.tor.yml` | `[provider.tor]` is in `policy.toml` |
| `compose/docker-compose.i2p.yml` | `[provider.i2p]` is in `policy.toml` |
| `compose/docker-compose.rns.yml` | `[provider.rns]` is in `policy.toml` |
| `compose/docker-compose.apprise.yml` | You want `apprise:` endpoints to work |

No overlay for `[provider.web]`: it needs no container and no
privilege, so the base file already covers it.

`compose/docker-compose.docker-socket.yml` is loaded once no matter how
many party lines are on. Each party line overlay sets the transport
directory for its own provider hooks.

## Why `transports/`, not `party-lines/`

The provider hooks run relay-mode containers by doing
`cd <dir> && docker compose up`. The directory they `cd` into
needs a compose file that works on a headless server with no
git checkout.

The upstream compose files under `party-lines/` cannot do this:

- They use `build: .` (requires a full git checkout)
- They mount `/dev/snd` and PulseAudio (fails on headless servers)
- Their `image:` tags are local-only (can't pull from a registry)

`transports/{tor,i2p,reticulum}-party-line/` each contain a
single relay-only `docker-compose.yml` that pulls the published
image from Docker Hub. No checkout, no audio, no build context.
Each sets `command: ["relay"]` so the upstream entrypoint enters
relay mode directly. Without this, `docker compose up -d` hangs:
the transport script blocks on a confirmation prompt because `stdin_open`
keeps stdin as an open pipe with no writer.
Runtime state (`data/`, `secrets/`) lands in these directories
too (gitignored), which is why they live outside `compose/`.

## Using a git checkout instead

For upstream development or offline use, point at a checkout:

```sh
# In .env:
TOR_PARTYLINE_DIR=${PWD}/party-lines/tor-party-line
I2P_PARTYLINE_DIR=${PWD}/party-lines/i2p-party-line
RETICULUM_PARTYLINE_DIR=${PWD}/party-lines/reticulum-party-line
```

The hooks, override files, and plp-runtime all work identically
in both modes. The only difference is which compose file Docker
reads.

## Recipes

Put the list in `.env` once. Every `docker compose` command
then works with no `-f` flags:

**Telegram only, web rooms:**

```sh
echo 'COMPOSE_FILE=docker-compose.yml:compose/docker-compose.telegram.yml' >> .env
docker compose up -d
```

**Telegram and Signal, web rooms:**

```sh
echo 'COMPOSE_FILE=docker-compose.yml:compose/docker-compose.telegram.yml:compose/docker-compose.signal.yml' >> .env
docker compose up -d
```

**IRC only, web rooms:**

```sh
echo 'COMPOSE_FILE=docker-compose.yml:compose/docker-compose.irc.yml' >> .env
docker compose up -d
```

**Telegram and Matrix, Tor party line:**

```sh
echo 'COMPOSE_FILE=docker-compose.yml:compose/docker-compose.telegram.yml:compose/docker-compose.matrix.yml:compose/docker-compose.docker-socket.yml:compose/docker-compose.tor.yml' >> .env
docker compose up -d
```

**Telegram, I2P party line only:**

```sh
echo 'COMPOSE_FILE=docker-compose.yml:compose/docker-compose.telegram.yml:compose/docker-compose.docker-socket.yml:compose/docker-compose.i2p.yml' >> .env
docker compose up -d
```

**Telegram, all three party lines:**

```sh
echo 'COMPOSE_FILE=docker-compose.yml:compose/docker-compose.telegram.yml:compose/docker-compose.docker-socket.yml:compose/docker-compose.tor.yml:compose/docker-compose.i2p.yml:compose/docker-compose.rns.yml' >> .env
docker compose up -d
```

**Everything:**

```sh
echo 'COMPOSE_FILE=docker-compose.yml:compose/docker-compose.telegram.yml:compose/docker-compose.matrix.yml:compose/docker-compose.irc.yml:compose/docker-compose.xmpp.yml:compose/docker-compose.mastodon.yml:compose/docker-compose.email.yml:compose/docker-compose.signal.yml:compose/docker-compose.mattermost.yml:compose/docker-compose.discord.yml:compose/docker-compose.docker-socket.yml:compose/docker-compose.tor.yml:compose/docker-compose.i2p.yml:compose/docker-compose.rns.yml:compose/docker-compose.apprise.yml' >> .env
docker compose up -d
```

Naming files with `-f` overrides whatever `COMPOSE_FILE` says:

```sh
docker compose -f docker-compose.yml \
  -f compose/docker-compose.irc.yml up -d
```

`./party-line-pager.sh` needs none of this. It derives the list
from `policy.toml` and `adapters.toml`, passes it on every
command, and rewrites `COMPOSE_FILE` whenever you switch an
adapter on or off.

## After you change the list

**Restart to apply.** Adding an overlay changes the daemon's
environment, which only takes effect on a recreate:

```sh
docker compose up -d --remove-orphans
```

!!! warning "`--remove-orphans` is required when removing a file"

    Compose only manages what is in the files you gave it.
    Dropping `compose/docker-compose.signal.yml` does not stop
    `signal-cli-rest-api`. It keeps running until something
    tells it to stop.

## Web rooms only, no Docker socket

`[provider.web]` on its own needs no party line checkout and no
Docker socket. That is a meaningfully smaller attack surface.

If you're not using any party line, delete `[provider.tor]`,
`[provider.i2p]` and `[provider.rns]` from `policy.toml` and
leave `compose/docker-compose.docker-socket.yml` and every
`compose/docker-compose.<provider>.yml` out of your list.

Confirm:

```sh
docker compose config | grep -c docker.sock
# 0 is what you want
```

## Apprise is optional

Without `compose/docker-compose.apprise.yml`, the daemon still starts
and every native adapter works. Only `apprise:` endpoints
(admin-added) fail their delivery and say so.

`party-line-pager.sh` includes it unconditionally: an admin can
add one at any moment, and a sidecar that is not running turns
that into a failure nobody was expecting.
