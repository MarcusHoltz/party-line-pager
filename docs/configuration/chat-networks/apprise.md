# Apprise

Endpoint format: `apprise:ntfy://host/topic` (any Apprise URL).

[Apprise](https://github.com/caronc/apprise) reaches 130+
notification services. It is the fallback for anything without
a native adapter.

## Setup

Apprise endpoints are **admin-added only**. Subscribers cannot
add them through the bot. An admin adds them with
`partylinepagerctl`:

```sh
partylinepagerctl add apprise:ntfy://ntfy.sh/partylinepager \
  --tier trusted
partylinepagerctl add \
  apprise:mailto://user:pw@smtp.example.org
```

The Apprise sidecar must be running. Add
`compose/docker-compose.apprise.yml` to your `COMPOSE_FILE` list:

```sh
echo 'COMPOSE_FILE=...existing...:compose/docker-compose.apprise.yml' >> .env
docker compose up -d
```

## Behavior

- Apprise endpoints are one-way: they receive broadcasts but
  cannot send commands. There is no inbound path.
- The `[fanout] apprise_url` setting in `policy.toml` points
  at the sidecar (default: `http://apprise:8000/notify`).
- Without `compose/docker-compose.apprise.yml`, native adapters still
  work. Only `apprise:` endpoint deliveries fail.
- `partylinepager.sh` includes the apprise overlay
  unconditionally because an admin can add an endpoint at any
  time.

## Examples

| Service | Endpoint URL |
|---|---|
| ntfy | `apprise:ntfy://ntfy.sh/your-topic` |
| Pushover | `apprise:pover://user@token` |
| Gotify | `apprise:gotify://hostname/token` |
| Slack webhook | `apprise:slack://TokenA/TokenB/TokenC/` |
| Generic webhook | `apprise:json://hostname/path` |

See [Apprise URL documentation](https://github.com/caronc/apprise/wiki)
for the full list of supported services and their URL formats.

## Full image

The full image includes Apprise (installed via pip). The
`[fanout] apprise_url` in `policy.toml` should point at the
local sidecar or be adjusted if running Apprise differently.
