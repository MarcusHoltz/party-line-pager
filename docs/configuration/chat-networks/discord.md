# Discord

Endpoint format: `discord:891...` (numeric snowflake).

Discord is not self-hosted, which forces a design the other
adapters don't need: a **lobby server**.

## Why a lobby server

A Discord bot can only DM someone it shares a guild (server)
with. So enrollment is: subscriber joins a PartylinePager-run
server once, then DMs the bot `sub`.

## Setup

### 1. Register the application and bot

In the [Discord Developer Portal](https://discord.com/developers/applications):

1. **New Application**, any name (subscribers see this as the
   bot's display name)
2. **Bot** in the sidebar > **Reset Token** > copy it.
   Discord only shows a token once.
   This value is `DISCORD_TOKEN`.
3. Leave every **Privileged Gateway Intent** off (Presence,
   Server Members, Message Content). DM message content is
   exempt from the `MESSAGE_CONTENT` intent.
4. Leave **Public Bot** on while testing. Turn it off before
   opening to subscribers.

### 2. Invite the bot into a server you own

1. **OAuth2** tab > scroll to **OAuth2 URL Generator**
2. Under **Scopes**, check only **`bot`**
3. Under **Bot Permissions**, leave everything unchecked.
   The generated URL should end in `permissions=0`.
4. Open that URL, pick your server, authorize.

If you don't have a server, create one: the `+` at the bottom
of your server list > **Create My Own** > name it
"PartylinePager Lobby".

### 3. Configure

```toml
[discord]
enabled = true
token = "env:DISCORD_TOKEN"
```

Set `DISCORD_TOKEN` in `.env`, add
`compose/docker-compose.discord.yml` to `COMPOSE_FILE`, and restart:

```sh
docker compose up -d --build --remove-orphans
```

`--build` matters: adding an adapter changes code, so a daemon
from an older image needs a rebuild, not just a recreate.

### 4. Verify

DM the bot `status`. Check the logs:

```sh
docker compose logs partylinepagerd | grep discord
```

A working exchange:

```
command endpoint=discord:513498094772420640 command=Status
```

### 5. Before you open to subscribers

Close the roster leak: by default every member can see every
other member.

1. **Server Settings > Roles > @everyone**: turn off
   **View Channel Members** (or **View Channels**)
2. **Server Settings > Invites > Create Invite**: never expire,
   never run out. This invite link (not the bot-install URL) is
   what you give subscribers.
3. Turn **Public Bot** off on the application's Bot page.

Subscribers now see the server with nobody visibly in it, DM
the bot, and all real interaction happens in DM.

## Behavior

- **Only DMs are commands.** Messages with a `guild_id`
  (anything in the lobby server) are discarded. Bot messages
  are dropped too.
- The endpoint is the sender's Discord snowflake, immutable
  across username changes.
- A send can return `403` (Discord error code `50007` or
  `50278`) when the subscriber has blocked the bot, turned off
  DMs from server members, or left the lobby. Usually permanent.
- Gateway websocket for inbound, REST for outbound.

## Known limitation

No automated live suite. Discord has no self-hostable
reimplementation of its gateway. Unit-tested plus manual
verification against a real bot and server.

## Config reference

| Key | Default | Notes |
|---|---|---|
| `enabled` | `true` | |
| `token` | required | Bot token from Developer Portal. Supports `env:NAME` |

## Full image env var

| Variable | Enables |
|---|---|
| `DISCORD_TOKEN` | `[discord]` adapter |
