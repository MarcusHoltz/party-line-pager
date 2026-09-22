# Signal

Endpoint format: `signal:+15555550100`

Via signal-cli-rest-api. Standard runs the upstream container
in `MODE=json-rpc`; the full image runs the GraalVM native
binary in `MODE=native` (no JVM). Receive holds a websocket
open rather
than polling. Group messages are ignored.

## Setup

### Standard (Docker Compose)

Signal requires a separate container running
[signal-cli-rest-api](https://github.com/bbernhard/signal-cli-rest-api).
The compose overlay (`compose/docker-compose.signal.yml`) starts it for
you.

1. Add `compose/docker-compose.signal.yml` to your `COMPOSE_FILE` list
2. Set `SIGNAL_NUMBER` in `.env` to the phone number you'll
   register (E.164 format, e.g. `+15555550100`)
3. Configure the adapter:

```toml
[signal]
enabled = true
rest_url = "http://signal-cli-rest-api:8080"
number = "+15555550100"
```

4. Start the stack and link/register the number:

```sh
docker compose up -d
# Follow signal-cli-rest-api's docs to register or link
# the number via its REST API or QR code flow
```

The `partylinepager.sh` wizard (menu item 5) handles Signal
linking interactively.

### Full image

The full image bundles `signal-cli-native` (GraalVM) and
`signal-cli-rest-api`. When `SIGNAL_NUMBER` is set, the
entrypoint starts signal-cli-rest-api in the background in
`MODE=native`. No separate container needed.

```sh
docker run -d --name partylinepager \
  -v ./config:/config \
  -e SIGNAL_NUMBER="+15555550100" \
  -e PLP_AUTO_START=1 \
  -e TTYD_CREDENTIAL=admin:changeme \
  partylinepager-full
```

Signal data is stored in `$CONFIG_DIR/signal-cli/`. This
directory holds the linked-device identity and is not
replaceable without re-registering the number.

## Behavior

- Only direct messages are commands. Group messages are
  discarded before parsing.
- Receiving uses a websocket (`MODE=json-rpc` in Standard,
  `MODE=native` in Full), not polling. No `poll_interval` to
  set.
- The endpoint is the phone number in E.164 format.

## Config reference

| Key | Default | Notes |
|---|---|---|
| `enabled` | `true` | |
| `rest_url` | required | Base URL of signal-cli-rest-api |
| `number` | required | E.164 phone number |

## Full image env vars

| Variable | Default | Notes |
|---|---|---|
| `SIGNAL_NUMBER` | (none) | Enables `[signal]`, starts signal-cli-rest-api |
| `SIGNAL_REST_URL` | `http://127.0.0.1:8080` | Override signal-cli-rest-api URL |
