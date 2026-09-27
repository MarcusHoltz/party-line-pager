# Supply Chain Security

`Cargo.lock` pins ~530 transitive dependencies. Two tools stand
between a routine dependency bump and pulling in a compromised
crate. They apply at different moments.

## cargo-deny: known-bad packages

`deny.toml` (via
[cargo-deny](https://embarkstudios.github.io/cargo-deny/))
carries an explicit `[bans]` deny list of specific
package/version IOCs from public supply-chain reports. Also
checked: the RustSec advisory database and a source restriction
(crates.io only, no git or path dependencies).

Seeded from the `arrayref@0.3.10` compromise
([Wiz, 2026-08-20](https://www.wiz.io/blog/rust-supply-chain-attack-on-arrayref-significant-overlap-with-dprk-campaigns)):
nine package names (three hijacked releases, six typosquats)
are blocked.

This check reads `Cargo.lock` and reports. It never rewrites
the lockfile and never compiles anything.

When a new report names package/version IOCs, add them to
`deny.toml`'s `[bans].deny` list.

## cargo-cooldown: minimum publish age

Most supply-chain compromises get caught within hours to days.
`cooldown.toml` (via
[cargo-cooldown](https://github.com/dertin/cargo-cooldown))
holds a 7-day minimum publish age gate.

Only applies when the lockfile is refreshed. With
`lockfile-baseline = "floor"`, versions already in `Cargo.lock`
are the baseline; it says nothing about what's already locked.

## Scripts

Three scripts, available from the `party-line-pager.sh`
Maintenance menu (items 5, 6, 7) or standalone:

```
hooks/audit-cargo-deps.sh    # read-only: cargo deny check
hooks/update-cargo-deps.sh   # rewrites Cargo.lock, then audits,
                              # restores on failure
hooks/triage-advisory.sh     # diagnose failures, add ignores
```

### audit-cargo-deps.sh (menu item 5)

Read-only. Runs `cargo deny --locked check advisories bans
sources`. The `--locked` flag prevents cargo from rewriting
`Cargo.lock`.

### update-cargo-deps.sh (menu item 6)

Snapshots `Cargo.lock` before starting and restores it on every
failure path including Ctrl-C. Leaves either a lockfile that
passed the audit or the one you started with, never one that
failed.

### triage-advisory.sh (menu item 7)

Diagnoses advisory failures and helps resolve them. For each
RUSTSEC finding it:

1. Checks whether `cargo update -p <pkg>` can resolve it
2. If yes: directs you to update-cargo-deps.sh (menu item 6)
3. If blocked (transitive dep constraint): offers to add an
   ignore entry to `deny.toml` with a justification comment

Pass `-y` for non-interactive mode (auto-accepts ignores).

All three run inside `compose/docker-compose.build-tools.yml`,
the build-stage container. Each tool is pinned to an exact
version and installed under its own `--root` in
`.cache/cargo/pinned` on first run.

## Handling advisory failures

New RustSec advisories are published continuously. When the
audit fails in CI on an advisory, this is the procedure:

### Quick version

```
./hooks/triage-advisory.sh
# follow the prompts
git add deny.toml && git commit -m "Ignore RUSTSEC-YYYY-NNNN"
git push
```

### Step by step

1. **Run triage** (Maintenance menu item 7, or standalone).
   The script runs the audit, parses each finding, and checks
   whether a compatible update exists.

2. **If an update is available:** the script tells you to run
   update-cargo-deps.sh (menu item 6). That updates
   `Cargo.lock`, respects the cooldown gate, and re-audits.

3. **If blocked (common):** the advisory fix requires a version
   outside the semver range allowed by a parent dependency.
   Example: `imbl-sized-chunks 0.2.0` is the fix, but
   `imbl 6.1.0` pins `^0.1.3`. No amount of `cargo update`
   can cross that boundary. The triage script offers to add
   an ignore entry to `deny.toml`.

4. **Commit and push** `deny.toml` (and `Cargo.lock` if
   updated). The CI pipeline will pass.

5. **Revisit later.** Ignored advisories stay in `deny.toml`
   until the upstream blocker is resolved. When the parent
   crate releases a version that accepts the fix, remove the
   ignore and run update-cargo-deps.sh.

### Why blocking is common

This project depends on `matrix-sdk`, which pulls ~200
transitive crates. When an advisory hits a transitive dep
three levels deep, the fix often requires a new major version
of an intermediate crate, which requires matrix-sdk to update
its own dependency. That upstream release is outside your
control.

Ignoring the advisory is the correct, secure choice when:

- The advisory affects code paths you don't exercise
- The fix is published but blocked by a transitive dep
- You've verified the advisory and assessed the risk

The ignore entry in `deny.toml` documents the decision. The
comment explains why. When the blocker clears, the ignore is
removed and the dep is updated.

### Manual triage (without the script)

If you prefer to do it by hand:

1. Run `./hooks/audit-cargo-deps.sh` and read the output
2. Note the RUSTSEC ID and package name
3. Try: `docker compose -f compose/docker-compose.build-tools.yml
   run --rm -T build-tools cargo update -p <pkg> --dry-run`
4. If "Locking 0 packages": the update is blocked. Add to
   `deny.toml`:

```toml
ignore = [
    # <pkg>: transitive dep, no compatible update.
    "RUSTSEC-YYYY-NNNN",
]
```

5. If an update shows: run `./hooks/update-cargo-deps.sh`

## Not a supply-chain issue

### Compiler deprecation warnings (`future-incompat-report`)

Messages like "the following packages contain code that will
be rejected by a future version of Rust" are compiler
warnings, not security advisories. They do not fail the
audit and do not need ignore entries.

See [Building and Testing > Compiler warnings](../development/building-testing.md#compiler-warnings)
for how to read the report and what to do about them.

### Build failures after Rust version bump

If you update the `FROM rust:` line in the Dockerfile and
the build fails on a transitive dep, that is a toolchain
compatibility issue, not a supply-chain issue. Either pin
the Rust version back or wait for upstream to fix the dep.
The supply-chain scripts do not handle this.
