# Manual Install

Skip the wizard. Hand-edit configs, pick compose overlays,
bring it up yourself.

## 1. Clone and create config

```sh
git clone <this repo> party-line-pager && cd party-line-pager
mkdir -p config
cp policy.example.toml config/policy.toml
cp adapters.example.toml config/adapters.toml
cp .env.example .env
chmod 600 config/adapters.toml .env
```

Set the container UIDs so files they write belong to you:

```sh
printf 'HOST_UID=%s\nHOST_GID=%s\nDOCKER_GID=%s\n' \
    "$(id -u)" "$(id -g)" \
    "$(getent group docker | cut -d: -f3)" >> .env
```

Create the matrix store directory if using the Matrix adapter:

```sh
mkdir -p config/matrix-store    # only if using [matrix]
```

The `config/state/` subdirectory is created automatically by the
daemon on first run.

## 2. Edit adapters.toml

Delete every section for a network you don't use, and fill in
the rest. Secrets can be `env:NAME` and passed through `.env`.

See [Chat Networks](../configuration/adapters.md) for what each
adapter needs.

## 3. Edit policy.toml

At minimum, decide:

- `signups` (`open`, `approval`, or `closed`)
- Your tiers and their quotas
- Which `[provider.*]` sections you want

All four providers ship enabled. Delete a section to remove
that command; at least one must remain.

See [Policy](../configuration/policy.md) for the full
reference.

## 4. Party line transports

No action needed. Each overlay defaults to
`transports/<name>/docker-compose.yml`, which pulls a
published image. Nothing to clone.

To use a local git checkout instead (offline use, upstream
development), set the matching env var in `.env`:

| Provider | Env var | Clone command |
|---|---|---|
| `[provider.tor]` | `TOR_PARTYLINE_DIR` | `git clone https://gitlab.com/MarcusHoltz/tor-party-line.git party-lines/tor-party-line` |
| `[provider.i2p]` | `I2P_PARTYLINE_DIR` | `git clone https://gitlab.com/MarcusHoltz/i2p-party-line.git party-lines/i2p-party-line` |
| `[provider.rns]` | `RETICULUM_PARTYLINE_DIR` | `git clone https://gitlab.com/MarcusHoltz/reticulum-party-line.git party-lines/reticulum-party-line` |

Three variables rather than one shared name because all the
hooks run in the same daemon container: a shared name would
have whichever overlay is loaded last silently point every
hook at one directory.

## 5. Set your compose files

List which compose files this instance uses. The base file,
one per adapter, plus `compose/docker-compose.docker-socket.yml` and
one `compose/docker-compose.<provider>.yml` for each party line.

See [Compose Files](../configuration/compose-files.md) for the
full reference and ready-made recipes.

Example for Telegram with web rooms only:

```sh
echo 'COMPOSE_FILE=docker-compose.yml:compose/docker-compose.telegram.yml' >> .env
```

## 6. Start

```sh
docker compose up -d
docker compose logs -f party-line-pagerd
```

## 7. Verify

```sh
docker compose run --rm party-line-pagerd --check
```

This lists every command the instance will answer, the resolved
web base URL/path/slug mode, and refuses to print `ok` if a
hook script is missing or not executable.

Then message the bot `sub` on your configured network, approve
yourself, and open a room:

```sh
docker compose exec party-line-pagerd party-line-pagerctl who
docker compose exec party-line-pagerd \
  party-line-pagerctl approve telegram:123456789
```
