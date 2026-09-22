# Mattermost

Endpoint format: `mattermost:8y9z...` (26-character user id).

Self-hosted: no roster leak, no platform policy risk.

## Setup

In **System Console > Integrations > Integration Management**,
turn on personal access tokens. Then, as the bot's account (or
an admin minting one on its behalf), **Account Settings >
Security > Personal Access Tokens** and create one:

```toml
[mattermost]
enabled = true
base_url = "https://mattermost.example.org"
access_token = "env:MATTERMOST_TOKEN"
```

## Behavior

- Inbound rides a websocket (`/api/v4/websocket`), authenticated
  by an `authentication_challenge` frame carrying the same token
  as the REST calls. No poll interval to set.
- Outbound is REST: `POST /api/v4/channels/direct` to open the
  direct channel (idempotent), then `POST /api/v4/posts`.
- **Only direct posts are commands.** Channel posts (public or
  private) are discarded before parsing. The bot's own posts
  are dropped too, so a broadcast cannot loop.
- The endpoint is the poster's 26-character user id, immutable
  across username changes.
- The websocket carries a double-JSON-encoded `post` field:
  this is Mattermost's wire format, not a quirk of this client.

## Mostlymatter (alternative)

[Mostlymatter](https://framagit.org/framasoft/framateam/mostlymatter)
is Framasoft's fork of Mattermost with the enterprise user-count
limit patched out. Same source, same API, Apache licensed.

`adapters.toml` still says `[mattermost]`, pointed at wherever
the swapped binary runs. The adapter does not know or care.

Verified: the same four live tests pass unmodified against a
container with the binary swapped.

### Quick recipe

```sh
curl -fsSLO https://packages.framasoft.org/projects/mostlymatter/mostlymatter-amd64-v11.9.0
curl -fsSLO https://packages.framasoft.org/projects/mostlymatter/mostlymatter-amd64-v11.9.0.sha512
sha512sum -c mostlymatter-amd64-v11.9.0.sha512

cat > Dockerfile.mostlymatter <<'EOF'
FROM mattermost/mattermost-preview:latest
COPY mostlymatter-amd64-v11.9.0 /mm/mattermost/bin/mattermost
RUN chmod 755 /mm/mattermost/bin/mattermost
EOF
docker build -f Dockerfile.mostlymatter \
  -t mostlymatter-preview:local .

# Verify it's the swapped binary:
docker run --rm --entrypoint sh mostlymatter-preview:local \
  -c '/mm/mattermost/bin/mattermost version'
```

Point your compose overlay at `image: mostlymatter-preview:local`.

## Config reference

| Key | Default | Notes |
|---|---|---|
| `enabled` | `true` | |
| `base_url` | required | Server URL |
| `access_token` | required | Personal or bot access token. Supports `env:NAME` |

## Full image env vars

| Variable | Default | Notes |
|---|---|---|
| `MATTERMOST_URL` | (none) | Enables `[mattermost]`. Base URL |
| `MATTERMOST_TOKEN` | (none) | Access token |
