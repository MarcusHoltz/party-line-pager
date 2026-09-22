# Docker Compose Quickstart (with wizard)

**This version is NOT for users wanting a single file and image.**

The instructions below will get everything up and running, each with its own container and image. 

Nothing starts that you didn't set in the wizard.


## 1. Clone and configure

```sh
git clone <this repo> partylinepager && cd partylinepager
./partylinepager.sh
```

A menu comes up. Work down it, and it writes
`config/policy.toml`, `config/adapters.toml` and `.env` for
you, refusing to start until the requirements are actually met.

Two numbers to expect: **about five minutes** of configuration,
then a **20+ minute first build** you can walk away from.

## 2. The wizard

The same script is the admin console afterwards, so
`./partylinepager.sh` is the only command worth memorizing.
Everything it does is a `docker compose` or `partylinepagerctl`
invocation documented in the reference; the script only saves
the typing.

It reads the config files at startup and owns them while it
runs. To hand-edit them, quit the wizard first.

The setup screen and the admin screen share key assignments:
**3 is Adapters, 4 is Policy, 5 is Signal link**, before and
after the build. Nothing you learn while setting up has to be
relearned once it is running.

## 3. What it creates

| File | Purpose |
|------|---------|
| `config/policy.toml` | Admin-owned policy: tiers, providers, quotas |
| `config/adapters.toml` | Credentials (`chmod 600`, never commit) |
| `.env` | UIDs, compose file list, credential references |
| `config/state/` | Roster, room state, lock file (auto-created) |

Backups the script writes go to `/var/tmp/BACKUP/` under their
original path.

## 4. Verify

Check the configuration without starting anything:

```sh
docker compose run --rm partylinepagerd --check
```

```
policy:   /etc/partylinepager/policy.toml (4 tiers)
state:    /etc/partylinepager/state
adapters: telegram
command tor:  /opt/partylinepager/hooks/provider-tor.sh
          teardown /opt/partylinepager/hooks/teardown-tor.sh
command web:  /opt/partylinepager/hooks/provider-web.sh
          teardown /opt/partylinepager/hooks/teardown-web.sh
ok
```

## 5. Start

```sh
docker compose up -d
docker compose logs -f partylinepagerd
```

Then message the bot `sub` on whichever network you configured,
approve yourself, and open a room:

```sh
docker compose exec partylinepagerd partylinepagerctl who
docker compose exec partylinepagerd \
  partylinepagerctl approve telegram:123456789
```

## First build performance

The first build compiles the whole Rust dependency tree in
release mode (matrix-sdk, the TLS stack, etc.), which takes
over 20 minutes on ordinary hardware without a warmed build
cache.

The Dockerfile compiles dependencies against stub sources
first, in their own layer keyed only on `Cargo.toml` files, so
a later edit to `.rs` files does not force that whole tree to
recompile. Expect a few minutes instead of from scratch.

If the build looks stalled, `docker stats` will show a build
container pegging CPU, which means it is working, not hung.

## Next steps

- [Configure your chat networks](../configuration/adapters.md)
- [Set up policy and tiers](../configuration/policy.md)
- [Pick your compose overlays](../configuration/compose-files.md)
- [Subscriber commands](../usage/commands.md)
- [Administration reference](../usage/administration.md)
