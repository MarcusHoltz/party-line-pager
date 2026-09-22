# Upgrading

How to move from one release to the next. Read all steps before
starting: pulling a new image while a room is live can orphan
the container running that room.

## 1. Close any live room

If a room is open, close it before upgrading. The teardown hook
needs the running daemon to clean up the provider container.

```sh
partylinepagerctl close
```

Or wait for the room to expire on its own (check `room_ttl` in
your `policy.toml`).

## 2. Pull the new image

If you run from a pre-built image (`docker-compose.user.yml`):

```sh
docker compose -f docker-compose.user.yml pull
```

If you build from source (`docker-compose.yml` with `build: .`):

```sh
git pull
docker compose build
```

## 3. Check the config

Run the config validator before bringing the daemon up. It
catches missing keys, bad types, and provider sections that
reference hooks the image does not contain.

```sh
docker compose run --rm partylinepagerd --check
```

Fix anything it reports before continuing.

## 4. Restart

```sh
docker compose down
docker compose up -d
```

Or, if you use overlays:

```sh
docker compose -f docker-compose.yml \
  -f compose/docker-compose.telegram.yml \
  down

docker compose -f docker-compose.yml \
  -f compose/docker-compose.telegram.yml \
  up -d
```

## 5. Verify

```sh
docker compose exec partylinepagerd partylinepagerctl status
```

Confirm the daemon is running, no rooms are stuck, and your
provider list matches what you expect.

## Release notes

Each release tags what changed in `policy.toml` or
`adapters.toml`, if anything. Most releases need no config
changes at all. When a release does require a config change,
the `--check` validator catches it before the daemon starts.
