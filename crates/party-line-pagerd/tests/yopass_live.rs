//! Live test against the real, public Yopass service.
//!
//! Unlike the other `*_live.rs` suites, there is no throwaway container to
//! point this at: Link mode is deliberately wired to the hosted instance
//! (`https://share.yopass.se` / `https://api2.yopass.se`), not a self-hosted
//! sidecar. So this skips itself unless explicitly opted into, the same way
//! the others skip when their throwaway server's URL is unset — a plain
//! `cargo test` must never depend on outbound internet or a third party's
//! uptime.
//!
//! This exists because the unit tests in `yopass.rs` only prove our
//! subprocess-handling code is correct against a fake stand-in script; they
//! cannot catch a real protocol mismatch with the actual `yopass` binary or
//! the actual API. This test mints a real one-time link and decrypts it back
//! with the same CLI, so a break in that round trip shows up here instead of
//! in a subscriber's DM.

use party_line_pagerd::yopass::YopassClient;
use std::time::Duration;
use tokio::process::Command;

const OPT_IN_ENV: &str = "PARTY_LINE_PAGER_YOPASS_LIVE_TEST";
const YOPASS_URL: &str = "https://share.yopass.se";
const YOPASS_API: &str = "https://api2.yopass.se";

/// Whether these tests should run, or skip themselves.
fn opted_in() -> bool {
    std::env::var(OPT_IN_ENV).is_ok()
}

/// Skips the test, with a reason, unless explicitly opted into.
macro_rules! yopass_live {
    () => {
        if !opted_in() {
            eprintln!("skipped: {OPT_IN_ENV} is unset");
            return;
        }
    };
}

fn client() -> YopassClient {
    YopassClient::new("yopass", YOPASS_URL, YOPASS_API, Duration::from_secs(30))
}

/// A per-run marker, so a secret from a failed prior run is never mistaken
/// for the one this run minted.
fn canary() -> String {
    format!(
        "party-line-pager-live-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

/// Runs `yopass --decrypt <url>` the same way a subscriber's browser would
/// read the link, just from the CLI instead of the web viewer.
async fn decrypt(url: &str) -> std::process::Output {
    Command::new("yopass")
        .arg("--decrypt")
        .arg(url)
        .output()
        .await
        .expect("could not run the yopass CLI to decrypt")
}

#[tokio::test]
async fn a_minted_link_decrypts_back_to_the_original_plaintext() {
    yopass_live!();
    let plaintext = canary();

    let url = client()
        .mint(&plaintext, Duration::from_secs(300))
        .await
        .expect("minting a link against the live service failed");
    assert!(
        url.starts_with(&format!("{YOPASS_URL}/#/s/")),
        "unexpected share URL shape: {url}"
    );

    let output = decrypt(&url).await;
    assert!(
        output.status.success(),
        "decrypt failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let decrypted = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert_eq!(
        decrypted, plaintext,
        "what came back from the live service does not match what was minted"
    );
}

/// The whole reason each recipient gets their own link: one subscriber
/// opening it must not leave anything for the next to read.
#[tokio::test]
async fn a_one_time_link_cannot_be_read_twice() {
    yopass_live!();
    let url = client()
        .mint(&canary(), Duration::from_secs(300))
        .await
        .expect("minting a link against the live service failed");

    let first = decrypt(&url).await;
    assert!(
        first.status.success(),
        "the first read of a fresh link should succeed: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    let second = decrypt(&url).await;
    assert!(
        !second.status.success(),
        "a one-time link was read twice, which defeats the point of Link mode"
    );
}

/// `decrypt` above reads a link through the API, which is the one thing a
/// wrong `yopass_url` does not break: minting and decrypting both talk to
/// `YOPASS_API`, so the round trip passes no matter what host the share URL
/// names. That is exactly how a stale `yopass_url` shipped and reached a
/// subscriber, whose browser landed on a page that ignored the `#/s/...`
/// fragment entirely.
///
/// So check the web host the way a subscriber's browser does: fetch it and
/// require the Yopass single-page app, not something else that merely
/// answers 200. The app shell mounts into `<div id="root">`; the docs site
/// that now lives on the bare apex is a Docusaurus build, which says so in
/// its own `generator` meta tag.
#[tokio::test]
async fn the_web_host_serves_the_yopass_app_a_browser_can_decrypt_in() {
    yopass_live!();
    let response = reqwest::get(format!("{YOPASS_URL}/"))
        .await
        .unwrap_or_else(|e| panic!("could not reach {YOPASS_URL}: {e}"));
    assert!(
        response.status().is_success(),
        "{YOPASS_URL} answered {}",
        response.status()
    );

    let body = response.text().await.expect("no body from the web host");
    assert!(
        body.contains(r#"id="root""#),
        "{YOPASS_URL} did not serve the Yopass app shell, so a share link's \
         #/s/... fragment has nothing to decrypt it"
    );
    assert!(
        !body.contains("Docusaurus"),
        "{YOPASS_URL} is serving the Yopass docs site, not the app"
    );
}

/// The other half of the same blind spot, and the one that actually made a
/// link unreadable: the CLI mints against `YOPASS_API`, but the page a
/// subscriber opens fetches from whatever backend *that build* was compiled
/// against. When the hosted service moved to a new frontend and a new
/// backend, the old API kept accepting writes, so minting went on
/// "succeeding" while every link it produced pointed at a secret the new
/// page could not see ("Secret does not exist").
///
/// So assert the two agree, by reading the backend URL out of the app bundle
/// the browser actually runs. This is a minified build, so it is matched as a
/// string rather than parsed: if the shape ever changes, this fails loudly
/// asking to be re-checked, which is the right outcome for a fact that has
/// already silently moved once.
#[tokio::test]
async fn the_web_app_fetches_from_the_same_api_the_cli_mints_against() {
    yopass_live!();
    let shell = reqwest::get(format!("{YOPASS_URL}/"))
        .await
        .unwrap_or_else(|e| panic!("could not reach {YOPASS_URL}: {e}"))
        .text()
        .await
        .expect("no body from the web host");

    // <script type="module" crossorigin src="/assets/index-<hash>.js">
    let entry = shell
        .split("src=\"")
        .find_map(|rest| {
            let path = rest.split('"').next()?;
            (path.starts_with("/assets/index-") && path.ends_with(".js")).then_some(path)
        })
        .unwrap_or_else(|| panic!("no /assets/index-*.js entry point in {YOPASS_URL}'s HTML"));

    let bundle = reqwest::get(format!("{YOPASS_URL}{entry}"))
        .await
        .unwrap_or_else(|e| panic!("could not fetch {entry}: {e}"))
        .text()
        .await
        .expect("no body for the app bundle");

    let backend = bundle
        .split("YOPASS_BACKEND_URL:`")
        .nth(1)
        .and_then(|rest| rest.split('`').next())
        .unwrap_or_else(|| {
            panic!("no YOPASS_BACKEND_URL in {entry}; the app bundle's shape changed, re-check by hand which API {YOPASS_URL} fetches from")
        });

    assert_eq!(
        backend.trim_end_matches('/'),
        YOPASS_API.trim_end_matches('/'),
        "the CLI mints against {YOPASS_API}, but {YOPASS_URL} reads from {backend}, \
         so every minted link would open on \"Secret does not exist\""
    );
}
