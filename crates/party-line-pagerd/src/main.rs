#![recursion_limit = "512"]
//! `party_line_pagerd`: the daemon.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use party_line_pagerd::config::Adapters;
use party_line_pagerd::engine::{Engine, Providers};
use party_line_pagerd::fanout::{AppriseClient, Fanout};
use party_line_pagerd::provider::ProviderRunner;
use party_line_pagerd::{adapters, transport};
use party_line_pager_core::{config::Policy, ProviderKind, Store};
use clap::Parser;
use tokio::sync::mpsc;

/// Reasons a hook would fail the moment somebody opened a room. Reported by
/// `--check` so an admin finds out at deploy time rather than at 1am.
fn hook_problems(path: &std::path::Path) -> Vec<String> {
    use std::os::unix::fs::PermissionsExt;

    let Ok(meta) = std::fs::metadata(path) else {
        return vec![format!("{} does not exist", path.display())];
    };
    if !meta.is_file() {
        return vec![format!("{} is not a file", path.display())];
    }
    if meta.permissions().mode() & 0o111 == 0 {
        return vec![format!(
            "{} is not executable (chmod +x it)",
            path.display()
        )];
    }
    Vec::new()
}

#[derive(Debug, Parser)]
#[command(name = "party_line_pagerd", version, about = "Open a Tor voice room across every chat network you use")]
struct Args {
    /// Admin-owned policy. The daemon only ever reads this.
    #[arg(long, default_value = "/etc/party-line-pager/policy.toml")]
    policy: PathBuf,

    /// Chat network credentials. Should be mode 0600.
    #[arg(long, default_value = "/etc/party-line-pager/adapters.toml")]
    adapters: PathBuf,

    /// Mutable state: roster, quotas, live room.
    #[arg(long, default_value = "/etc/party-line-pager/state")]
    state: PathBuf,

    /// Validate configuration, report what would start, and exit.
    #[arg(long)]
    check: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,party_line_pagerd=info".into()),
        )
        .init();

    let args = Args::parse();

    let policy = Policy::load(&args.policy)
        .with_context(|| format!("could not load {}", args.policy.display()))?;
    let adapter_config = Adapters::load(&args.adapters)
        .with_context(|| format!("could not load {}", args.adapters.display()))?;

    if args.check {
        println!("policy:   {} ({} tiers)", args.policy.display(), policy.tiers.len());
        println!("state:    {}", args.state.display());
        println!("adapters: {}", adapter_config.enabled_names().join(", "));

        // One line per configured provider, plus the resolved base_url, which
        // is the single most likely thing to be wrong and is otherwise
        // invisible until somebody opens a real room.
        let mut problems = Vec::new();
        for kind in policy.provider.configured() {
            let (up, down, extra) = match kind.transport() {
                // A party line is two script paths and nothing else worth
                // printing; which network it is on is already in the section
                // name this line reports.
                Some(_) => {
                    let (up, down) = policy.provider.get(kind).unwrap();
                    (up, down, String::new())
                }
                None => {
                    let w = policy.provider.web.as_ref().unwrap();
                    let path = w.trimmed_path();
                    let slug = w
                        .static_slug
                        .as_deref()
                        .map(|s| format!(", static slug {s:?}"))
                        .unwrap_or_else(|| ", random slug per room".to_string());
                    (
                        &w.up,
                        &w.down,
                        format!(
                            "  base_url {}{}{path}{slug}",
                            w.trimmed_base_url(),
                            if path.is_empty() { "" } else { "/" },
                        ),
                    )
                }
            };
            println!("command {kind}:  {}{}", up.display(), extra);
            println!("          teardown {}", down.display());
            problems.extend(hook_problems(up));
            problems.extend(hook_problems(down));
        }

        for problem in &problems {
            println!("PROBLEM:  {problem}");
        }
        if problems.is_empty() {
            println!("ok");
        } else {
            anyhow::bail!("{} hook problem(s); see above", problems.len());
        }
        return Ok(());
    }

    let store = Store::open(&args.state)
        .with_context(|| format!("could not open the state directory {}", args.state.display()))?;

    let transports = adapters::build(&adapter_config).await?;
    if transports.is_empty() {
        // Without an adapter there is no way to subscribe and no way to open
        // a room, so this is a configuration mistake rather than a mode.
        anyhow::bail!(
            "no chat adapters are enabled in {}, so nobody could talk to this daemon",
            args.adapters.display()
        );
    }

    let apprise = AppriseClient::new(&policy.fanout.apprise_url, policy.fanout.timeout)?;
    let fanout = Arc::new(Fanout::new(
        transports.clone(),
        Some(apprise),
        policy.fanout.concurrency,
        policy.fanout.timeout,
    ));
    // One runner per configured provider. A missing section is not an error:
    // it is how an admin restricts this instance to one kind of room.
    let mut providers = Providers::default();
    for kind in policy.provider.configured() {
        let (up, down) = policy
            .provider
            .get(kind)
            .expect("configured() only returns kinds that are present");
        let mut runner = ProviderRunner::new(up, down, policy.instance.hook_timeout);
        if let Some(w) = policy.provider.web.as_ref().filter(|_| kind == ProviderKind::Web) {
            // The web hook needs to know which instance to mint rooms on, what
            // path to mint them under, and whether to reuse a fixed slug
            // instead of a random one. A party-line hook needs none of that:
            // its compose file already says which network it brings up.
            runner = runner
                .with_env("PARTY_LINE_PAGER_WEB_BASE_URL", w.trimmed_base_url())
                .with_env("PARTY_LINE_PAGER_WEB_PATH", w.trimmed_path())
                .with_env("PARTY_LINE_PAGER_WEB_STATIC_SLUG", w.static_slug.as_deref().unwrap_or(""));
        }
        providers.insert(kind, runner);
    }
    for kind in policy.provider.configured() {
        tracing::info!(provider = %kind, "provider configured");
    }

    let engine = Engine::new(policy, store, fanout, providers);
    engine.recover().await?;

    // One inbound queue for every network.
    let (tx, mut rx) = mpsc::channel(256);
    for (name, transport) in transports {
        tracing::info!(adapter = %name, "starting");
        tokio::spawn(transport::supervise(transport, tx.clone()));
    }
    drop(tx);

    {
        let engine = Arc::clone(&engine);
        tokio::spawn(async move { engine.poll_admin_decisions().await });
    }

    tracing::info!("party_line_pagerd is up");
    loop {
        tokio::select! {
            Some(incoming) = rx.recv() => {
                let engine = Arc::clone(&engine);
                tokio::spawn(async move { engine.handle(incoming).await });
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("shutting down; any live room keeps its own TTL");
                return Ok(());
            }
            else => return Ok(()),
        }
    }
}
