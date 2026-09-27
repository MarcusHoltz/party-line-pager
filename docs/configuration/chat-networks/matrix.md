# Matrix

Endpoint format: `matrix:@party-line-pager:example.org`

E2EE (end-to-end encrypted). Best of the nine for carrying a
secret: the homeserver operator cannot read what the bot sends.

## Setup

Any homeserver works, yours or somebody else's. Register an
ordinary account for the bot from Element or any client, then
configure:

```toml
[matrix]
enabled = true
homeserver = "https://matrix.example.org"
user = "party-line-pager"
password = "env:MATRIX_PASSWORD"
store_path = "/var/lib/party-line-pager/matrix"
```

`user` is the localpart, not `@party-line-pager:example.org`.

### Encryption store

`store_path` holds the device identity and room keys. It must
outlive the container the same way `config/state` does.

`compose/docker-compose.matrix.yml` bind-mounts `./config/matrix-store`
there. Create it before the first `up`:

```sh
mkdir -p config/matrix-store
docker compose up -d
```

Delete that directory and you get a fresh login, a new device,
and old encrypted history the bot can no longer read. Move it
with the project.

## Behavior

- A Matrix conversation starts with an **invitation**. The bot
  accepts and joins any room it is invited to, records direct
  rooms as direct. Only messages in a direct room are commands.
- Rooms the bot opens are end-to-end encrypted.
- The endpoint is the full Matrix ID
  (`@party-line-pager:example.org`), stable across display name changes.

## Self-hosting

Synapse's own
[installation guide](https://element-hq.github.io/synapse/latest/setup/installation.html)
is the reference.

## Config reference

| Key | Default | Notes |
|---|---|---|
| `enabled` | `true` | |
| `homeserver` | required | Full URL, e.g. `https://matrix.example.org` |
| `user` | required | Localpart only |
| `password` | required | Supports `env:NAME` |
| `store_path` | `/var/lib/party-line-pager/matrix` | E2EE key store, must be persistent |

## Full image env vars

| Variable | Default | Notes |
|---|---|---|
| `MATRIX_HOMESERVER` | (none) | Enables `[matrix]` adapter |
| `MATRIX_USER` | (none) | Bot's localpart |
| `MATRIX_PASSWORD` | (none) | Account password |
| `MATRIX_STORE_PATH` | `/var/lib/party-line-pager/matrix` | E2EE key store |
