# Mastodon

Endpoint format: `mastodon:party-line-pager@example.org` (acct handle).
Bare (`party-line-pager`) for a local account,
`party-line-pager@elsewhere.org` for federated.

## Setup

Create an account for the bot on any instance, then create an
application under **Preferences > Development > New
application**. It needs two scopes:

- `read:notifications` (to see mentions)
- `write:statuses` (to reply)

Copy the access token into `.env` as `MASTODON_TOKEN`:

```toml
[mastodon]
enabled = true
base_url = "https://mastodon.example.org"
access_token = "env:MASTODON_TOKEN"
poll_interval = "30s"
```

## Behavior

- **Only direct-visibility mentions are commands.** A public
  "@party-line-pager tor" is ignored: the reply carries an onion
  address and a shared secret, and a reply to a public toot is
  a public toot. Every reply the bot sends is direct visibility.
- Polling, not webhooks: the adapter polls
  `/api/v1/notifications`, so `poll_interval` is the latency
  between sending a command and anything happening. 30 seconds
  is polite to somebody else's instance; on your own, go lower.
- The adapter identifies itself with a `User-Agent` naming
  PartyLinePager. An instance that filters anonymous clients
  rejects the request before looking at the token.
  GoToSocial answers empty `User-Agent` with
  `418 I'm a teapot`.

## Self-hosting

Mastodon itself is a real undertaking (Rails, Postgres, Redis,
Sidekiq). [GoToSocial](https://gotosocial.org) is one Go
binary on SQLite speaking the same client API. The live test
suite runs against GoToSocial.

## Config reference

| Key | Default | Notes |
|---|---|---|
| `enabled` | `true` | |
| `base_url` | required | Instance URL |
| `access_token` | required | Supports `env:NAME` |
| `poll_interval` | `30s` | How often to check for mentions |

## Full image env vars

| Variable | Default | Notes |
|---|---|---|
| `MASTODON_INSTANCE` | (none) | Enables `[mastodon]`. Base URL of instance |
| `MASTODON_TOKEN` | (none) | Application access token |
| `MASTODON_POLL_INTERVAL` | `30s` | Notification poll interval |
