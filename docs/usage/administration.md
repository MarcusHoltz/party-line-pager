# Administration

`party-line-pagerctl` runs on the host, over SSH, as a user who
can read the state directory. It takes the same lock the daemon
takes, so an admin decision cannot interleave with a fanout in
progress. The daemon notices decisions within five seconds
without a restart.

Every command below is also a menu item in `./party-line-pager.sh`,
which lists the roster first and lets you pick by number
instead of typing an endpoint id.

## Roster commands

```sh
party-line-pagerctl who [--status pending|active|banned]
party-line-pagerctl status
```

`who` and `pending` print a table with a header row, columns
sized to their contents:

```
STATUS  TIER     ENDPOINT                          TZ  QUIET  ROOMS   LAST ROOM
active  trusted  apprise:ntfy://ntfy.sh/plp        -   -      0       never
active  weekly   telegram:120646099                -   -      0       never
```

If you script against this, skip the header line and split on
runs of two or more spaces.

## Subscriber management

```sh
party-line-pagerctl approve telegram:123
party-line-pagerctl deny telegram:123
party-line-pagerctl ban telegram:123
party-line-pagerctl unban telegram:123
party-line-pagerctl tier telegram:123 trusted
party-line-pagerctl reset-quota telegram:123
```

Approving sends a note to the subscriber on their network the
moment it takes effect.

### Adding endpoints by hand

```sh
party-line-pagerctl add apprise:ntfy://ntfy.sh/plp --tier trusted
party-line-pagerctl add apprise:mailto://user:pw@smtp.example.org
```

This is the only way to add `apprise:` endpoints.

## Request management

```sh
party-line-pagerctl pending
party-line-pagerctl approve-request <id>
party-line-pagerctl deny-request <id>
```

## Instance control

```sh
party-line-pagerctl pause      # refuse new rooms instance-wide
party-line-pagerctl resume
party-line-pagerctl close      # tear the live room down early
```

## Common workflows

### Onboarding a new subscriber

They message the bot `sub`, landing as `pending` on
`default_tier`:

```sh
party-line-pagerctl who --status pending
party-line-pagerctl tier telegram:123 lurker  # optional
party-line-pagerctl approve telegram:123
```

`tier` and `approve` are independent: `tier` edits
`subscribers.json` with no check on status, so it works before
or after `approve`. There is no single "approve onto tier X"
command.

### Wrong kind of room opened

Closing the room is only half the fix:

```sh
party-line-pagerctl status     # host: line names who to reset
party-line-pagerctl close      # torn down within 5 seconds
party-line-pagerctl reset-quota telegram:123
```

`close` does not refund quota. Quota is spent at the moment a
room comes up, so on a `weekly` tier they would wait 168 hours
for a replacement without `reset-quota`.

Anyone *else* who tried while it was live got "The line is
already up" and needs no reset.

If the request is still held (never provisioned), use
`deny-request` instead. No room came up, no quota spent.

`./party-line-pager.sh` does this sequence as **Rooms > Clear the
board**, reading the host off `status` automatically.
