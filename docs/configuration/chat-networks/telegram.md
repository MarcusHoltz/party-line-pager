# Telegram

Endpoint format: `telegram:123456789` (numeric chat id).

## Setup

1. Talk to [@BotFather](https://t.me/BotFather) on Telegram
2. Send `/newbot`, follow the prompts, get a token
3. Configure the adapter:

```toml
[telegram]
enabled = true
token = "env:TELEGRAM_TOKEN"
```

Set `TELEGRAM_TOKEN` in `.env` and add
`compose/docker-compose.telegram.yml` to your `COMPOSE_FILE` list.

## Behavior

- Only private (direct) messages are commands. Group,
  supergroup, and channel traffic is discarded before parsing.
- Messages sent while the daemon was down are skipped on
  startup, not replayed.
- The endpoint is the sender's numeric chat id, which is
  immutable across username and display name changes.
- Broadcasts use `parse_mode=HTML` (not MarkdownV2) because
  MarkdownV2 requires escaping 18 characters and rejects the
  whole message on a miss.

## Config reference

| Key | Required | Notes |
|---|---|---|
| `enabled` | no | Defaults to `true` if the section exists |
| `token` | yes | Bot token from @BotFather |

## Full image env var

| Variable | Enables |
|---|---|
| `TELEGRAM_TOKEN` | `[telegram]` adapter |
