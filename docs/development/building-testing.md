# Building and Testing

Everything runs through Docker; no host toolchain needed.

## Building from source

```sh
docker compose build
```

This builds the `party-line-pagerd` image from the `Dockerfile`:
a multi-stage build that compiles the Rust workspace and
installs the yopass CLI, Docker CLI, and compose plugin into a
minimal Debian runtime image.

The full image (all transports in one container):

```sh
docker build -f deploy/Dockerfile.full -t party-line-pager-full .
```

## Build memory requirements

The Rust dependency tree includes several C/C++ libraries
(`aws-lc-sys`, `ring`, `libsqlite3-sys`) that compile native
code. Without limits, Cargo spawns parallel jobs per CPU core,
and each C/C++ sub-build spawns its own parallel `cc`/`cmake`
jobs. On machines with 16 GB of RAM or less, this can exceed
available memory and the build gets OOM-killed (exit 137).

Two build args in both Dockerfiles control this:

- `CARGO_BUILD_JOBS` (default `2`) limits how many crates
  cargo compiles in parallel. The `cc` crate (`ring`,
  `libsqlite3-sys`) respects this via `NUM_JOBS`.
- `CMAKE_BUILD_PARALLEL_LEVEL` (default `2`) limits cmake's
  internal parallelism. `aws-lc-sys` uses cmake and ignores
  `CARGO_BUILD_JOBS`; without this, cmake uses all CPU cores.

Together at `=2` these keep peak memory under ~8.5 GB. The
`cmake` package is also installed in the build stage because
`aws-lc-sys` needs it to build AWS-LC from source.

Both `ring` and `aws-lc-rs` are in the dependency tree (ring
for `tokio-xmpp` and `irc`; aws-lc-rs as rustls's default
provider, pulled in by `matrix-sdk`). Neither can be removed
without dropping a chat adapter or patching upstream.

### Overriding parallelism

Both values are Dockerfile `ARG`s with a default of `2`. You
can override them at build time without editing the Dockerfile:

```sh
# Local machine with 16+ GB RAM: use 4 jobs for faster builds
docker compose build \
  --build-arg CARGO_BUILD_JOBS=4 \
  --build-arg CMAKE_BUILD_PARALLEL_LEVEL=4

# Or via shell env (compose files read these from the environment)
CARGO_BUILD_JOBS=4 CMAKE_BUILD_PARALLEL_LEVEL=4 docker compose build
```

GitLab CI overrides both to `1` (the `small` runner has only
8 GB and no swap). GitHub Actions uses the default `2` with a
4 GB swapfile for linker spikes. GitLab CI runs inside
`docker:dind` where `swapon` is not available; lowering to
`1` is the only option there.

If your build exits with code 137, check `docker stats` during
the next attempt. If memory peaks near 100%, lower both args
to `1` or increase Docker's memory limit.

## Running tests

`compose/docker-compose.build-tools.yml` carries a `build-tools`
service. It builds from the same Dockerfile's build stage and
keeps the cargo registry in `./.cache` and build output in
`./target`, so repeat runs are fast and nothing root-owned
lands in the checkout.

```sh
T="-f compose/docker-compose.build-tools.yml"

# Whole workspace
docker compose $T run --rm build-tools

# One crate
docker compose $T run --rm build-tools cargo test -p party-line-pager-core
```

## Ad-hoc cargo commands

The `build-tools` service is a general-purpose Rust
environment. Any cargo command works:

```sh
T="-f compose/docker-compose.build-tools.yml"

# Check if a specific package can be updated
docker compose $T run --rm -T build-tools \
  cargo update -p imbl-sized-chunks --dry-run

# Show future-incompatibility report (compiler deprecation warnings)
docker compose $T run --rm -T build-tools \
  cargo report future-incompatibilities --id 1

# Inspect dependency tree for a crate
docker compose $T run --rm -T build-tools \
  cargo tree -p imbl-sized-chunks

# Check what depends on a specific crate
docker compose $T run --rm -T build-tools \
  cargo tree -i imbl-sized-chunks
```

!!! note
    Always use `-T` (no TTY) and prefer `run --rm` so
    the container is cleaned up. When calling from a script,
    also redirect stdin: `</dev/null`.

## Supply-chain auditing

Three maintenance scripts use `compose/docker-compose.build-tools.yml`
as a build environment. Available from the `party-line-pager.sh`
Maintenance menu (items 5, 6, 7) or standalone:

```sh
hooks/audit-cargo-deps.sh    # read-only cargo deny check
hooks/update-cargo-deps.sh   # rewrites Cargo.lock, audits, restores on failure
hooks/triage-advisory.sh     # diagnose failures, add ignores to deny.toml
```

See [Supply chain](../reference/supply-chain.md) for the full
workflow, including how to handle advisory failures.

## Compiler warnings

### `future-incompat-report`

During builds you may see:

```
warning: the following packages contain code that will be
rejected by a future version of Rust: proc-macro-error2 v2.0.1
note: to see what the problems were, use the option
`--future-incompat-report`
```

This is a Rust compiler warning, not a security advisory.
The code compiles today but uses deprecated language features
that a future Rust version will remove.

**To read the report:**

```sh
docker compose -f compose/docker-compose.build-tools.yml \
  run --rm -T build-tools \
  cargo report future-incompatibilities --id 1
```

**Can you fix it?** Usually no. These are transitive
dependencies (like `proc-macro-error2`, used by `ruma-macros`
in `matrix-sdk`). The fix must come from upstream.

**Does it block anything?** No. It is a warning about a
future Rust version, not the one pinned in the Dockerfile
(`rust:1.98.1`). Your builds, tests, and CI pass.

**When does it matter?** When you bump the Rust version in
the `FROM rust:` line of the Dockerfile. If upstream hasn't
fixed it by then, the build will fail. At that point, either
wait for upstream or stay on the current Rust version.

## CI/CD

Two GitHub Actions workflows live in `.github/workflows/`. Both
use `workflow_dispatch` (manual "Run workflow" button in the
Actions tab). No automatic triggers on push, PR, or tag.

### CI (`ci.yml`)

Validates the codebase without pushing anything.

1. **test** and **audit** run in parallel:
   - `test`: builds the `build-tools` image, runs
     `cargo test --workspace`
   - `audit`: builds the same image, runs
     `hooks/audit-cargo-deps.sh` (pinned cargo-deny)
2. **build-compose-image** and **build-full-image** run only
   after both pass. Each builds its respective Dockerfile to
   confirm the images compile.

### Release (`release.yml`)

Builds both images and pushes them to registries. Inputs
(filled in when you click the button):

| Input | Type | Default | Purpose |
|-------|------|---------|---------|
| `version` | string (required) | -- | Version tag, e.g. `1.0.0` |
| `dry_run` | boolean | false | Build only, no push (no secrets needed) |
| `push_dockerhub` | boolean | false | Push to Docker Hub |
| `push_ghcr` | boolean | true | Push to GitHub Container Registry (`ghcr.io`) |

Each image gets two tags: `latest` and `<version>`.

A self-hosted registry placeholder is commented out in the
workflow, ready to uncomment when needed. Look for
`Self-hosted` comments in `release.yml`.

Tests and audit gate the push: if either fails, no image ships.

### What gets built

Two images, from two Dockerfiles, built in parallel:

| Image | Dockerfile | Description |
|-------|-----------|-------------|
| `party-line-pager` | `./Dockerfile` | V2 modular daemon (docker-compose deployment) |
| `party-line-pager-full` | `deploy/Dockerfile.full` | V1 all-in-one (everything in one container) |

Both get tagged `:latest` and `:<version>` on each registry.

### Registry image names

Each CI platform pushes to its own native registry plus
Docker Hub. GitHub does not push to GitLab CR, and vice
versa.

**GitHub Actions (`release.yml`) pushes to:**

| Image | Docker Hub | GHCR |
|-------|-----------|------|
| Standard (daemon) | `<owner>/party-line-pager` | `ghcr.io/<owner>/party-line-pager` |
| Full (all-in-one) | `<owner>/party-line-pager-full` | `ghcr.io/<owner>/party-line-pager-full` |

**GitLab CI (`.gitlab-ci.yml`) pushes to:**

| Image | Docker Hub | GitLab CR |
|-------|-----------|-----------|
| Standard (daemon) | `<owner>/party-line-pager` | `registry.gitlab.com/<ns>/<project>` |
| Full (all-in-one) | `<owner>/party-line-pager-full` | `registry.gitlab.com/<ns>/<project>/party-line-pager-full` |

`<owner>` adapts to forks automatically (GitHub username or
Docker Hub username). `<ns>/<project>` is the GitLab project
path.

!!! warning "Deleting a repo does not delete GHCR images"

    **GitLab CR** images are scoped to the project. Deleting the
    project deletes the images. Nothing to clean up.

    **GHCR** images are scoped to your GitHub **account**, not
    the repo. Deleting the repo leaves the images behind. To
    remove them: go to your profile > Packages, find each
    image, then Package settings > Danger zone > Delete.

### Prerequisites

**GitHub**: before the first release, go to Settings > Actions >
General > Workflow permissions and select **Read and write
permissions**. The default is read-only, which blocks the GHCR
push. This is a one-time setting.

### Required secrets

**GitHub Actions** (Settings > Secrets and variables > Actions):

| Secret | Used by | Required? |
|--------|---------|-----------|
| `DOCKERHUB_USERNAME` | Docker Hub login | Only if pushing to Docker Hub |
| `DOCKERHUB_TOKEN` | Docker Hub login | Only if pushing to Docker Hub |
| `GITHUB_TOKEN` | GHCR login | Automatic, no setup needed |

**GitLab CI** (Settings > CI/CD > Variables):

| Variable | Used by | Required? |
|----------|---------|-----------|
| `DOCKERHUB_USERNAME` | Docker Hub login | Only if pushing to Docker Hub |
| `DOCKERHUB_TOKEN` | Docker Hub login | Only if pushing to Docker Hub |
| `CI_REGISTRY_*` | GitLab CR login | Automatic, no setup needed |

### GitLab CI (`.gitlab-ci.yml`)

Mirrors the GitHub workflows. Uses a `WORKFLOW` dropdown on
the "New pipeline" page to select which workflow to run:

| Workflow | Stages | Description |
|----------|--------|-------------|
| `test` (default) | test, build | Unit tests, supply-chain audit, image builds |
| `release` | test, build, release | Tests + builds, then manual push-registries button |
| `docs` | pages | Build and deploy the docs site |

Set `RELEASE_VERSION` (e.g. `1.0.0`) and optionally
`PUSH_DOCKERHUB` (default `"false"`) / `PUSH_GITLAB`
(default `"true"`)
when running the release workflow. The push-registries job
is manual (click play). Uses `docker:29.8.0` with
`docker:29.8.0-dind`.

### Project version

`version` under `[workspace.package]` in the repo-root
`Cargo.toml` is the only place the project version is written
down. Both release workflows fall back to it when no version
is supplied, so a plain `cargo build --release` and a release
tag agree by construction.

Bumping it means two edits, not one: change `Cargo.toml`, then
run `cargo update --workspace` (`hooks/update-cargo-deps.sh`
menu item 6 does this among other things). Cargo records the
version of all three workspace members in `Cargo.lock`, and
`hooks/audit-cargo-deps.sh` runs `cargo deny --locked`, which
fails if the lockfile is behind the manifest.

The image label is the one thing that cannot read the manifest,
because `Dockerfile` and `deploy/Dockerfile.full` both declare
it in a final stage that never copies `Cargo.toml`. Both take
`ARG PLP_VERSION` with no default and label the image
`${PLP_VERSION:-unknown}`, so an unlabelled build says
`unknown` instead of a number that quietly went stale. Only the
release jobs pass the value.

### Pinned base image versions

Both Dockerfiles pin base images to exact versions for
reproducible builds. See the `FROM` lines and `ARG`
declarations in `Dockerfile` and `deploy/Dockerfile.full`
for current pins. `debian:trixie-slim`, `alpine/git`, and
`bbernhard/signal-cli-rest-api:latest` use codename or
rolling tags (no meaningful numeric version to pin).

### Docs site

The documentation site uses [MkDocs Material](https://squidfunk.github.io/mkdocs-material/).
Source files live in `docs/`, build config in
`docs/website/mkdocs.yml`.

#### Local preview

From the project root:

```sh
# Live-reload dev server at http://localhost:8000
docker compose -f docs/website/docker-compose.yml up serve

# Static HTML build (output in ./site/, gitignored)
docker compose -f docs/website/docker-compose.yml run --rm build
```

#### Deploying to GitHub Pages

1. Go to **Settings > Pages > Source** and set it to
   **GitHub Actions**.
2. Go to **Actions > "Deploy Docs" > Run workflow**.
3. Leave the "Custom domain" field blank.

The workflow auto-detects your Pages URL from the repository
name (`https://<owner>.github.io/<repo>/`), writes it into
`mkdocs.yml` before the build, and deploys. No secrets or
manual URL editing needed.

The site will be live at `https://<owner>.github.io/<repo>/`
once the workflow finishes.

#### Custom domain (GitHub)

Three ways to tell the workflow about your domain. All three
produce the same result: a CNAME file in the build output
and the correct `site_url` in `mkdocs.yml`. Pick whichever
fits your workflow.

**Method 1: Workflow input (one-off, nothing stored)**

When you click "Run workflow", type your domain in the
"Custom domain" field (e.g. `docs.example.com`). You have
to type it each time. Good for testing before committing.

**Method 2: Repository variable (persistent, not in git)**

Go to **Settings > Secrets and variables > Actions >
Variables** and create a variable:

| Name | Value |
|------|-------|
| `DOCS_CNAME` | `docs.example.com` |

The workflow reads it automatically on every run. Forks
don't inherit repository variables, so they won't get a
CNAME that doesn't belong to them.

**Method 3: Edit the workflow file (persistent, in git)**

Open `.github/workflows/docs.yml` and find the `env:`
block near the top:

```yaml
env:
  # DOCS_CNAME: "docs.example.com"
  DOCS_CNAME: ""
```

Replace the empty string with your domain:

```yaml
env:
  DOCS_CNAME: "docs.example.com"
```

Forks will inherit this value.

**Precedence:** workflow input > repository variable >
value in the file. If none are set, the default
`.github.io` URL is used.

**DNS setup (required for all three methods):**

1. Add a DNS CNAME record pointing your subdomain to
   `<owner>.github.io` (e.g. `docs.example.com CNAME
   youruser.github.io`).
2. In repo **Settings > Pages > Custom domain**, enter the
   same subdomain.
3. Wait for DNS to propagate, then check **Enforce HTTPS**.

!!! note
    You cannot serve from both the `.github.io` URL and a
    custom domain simultaneously. With a CNAME configured,
    the `.github.io` URL always redirects.

#### Deploying to GitLab Pages

1. Go to **CI/CD > Pipelines** and run a pipeline.
2. Click the play button on the `pages` job.

The job auto-detects the Pages URL from `$CI_PAGES_URL`,
writes it into `mkdocs.yml`, and deploys. No secrets or
manual URL editing needed.

The site will be live at
`https://<namespace>.gitlab.io/<project>/`.

#### Custom domain (GitLab)

Two ways to configure a custom domain:

**Method 1: CI/CD variable (persistent, not in git)**

Go to **Settings > CI/CD > Variables** and create
`DOCS_CNAME` with your domain as the value. You can also
type `DOCS_CNAME` as a pipeline variable when clicking
"Run pipeline" for a one-off override.

**Method 2: Edit the CI file (persistent, in git)**

Open `.gitlab-ci.yml` and find the `pages` job's
`variables:` block:

```yaml
  variables:
    # DOCS_CNAME: "docs.example.com"
    DOCS_CNAME: ""
```

Replace the empty string with your domain.

**DNS setup:**

1. Go to **Settings > Pages > New Domain**.
2. Enter your domain and add the TXT record for
   verification.
3. Add a DNS CNAME record pointing your subdomain to
   `<namespace>.gitlab.io`.

#### For forks

Both workflows work immediately on forks with zero secrets
and zero configuration. The site URL is computed from the
fork's repository name, so no edits to `mkdocs.yml` or
CI files are needed. The default Pages URL is available as
soon as the first deployment completes.
