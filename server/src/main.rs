//! One process. What it needs comes from the environment. The two master keys, and the old
//! invite service's admin token, are read here and go nowhere but to their services.

use axum::http::StatusCode;
use std::{sync::Arc, time::Duration};
use zu::Doc;
use zutalk_server::{BoxFuture, Config, Directory, Invites, OpenRouter, Refusal, Server};
use zutalk_server::{Soniox, serve};

/// How long a DID's host has to give its document; the same as for any upstream
/// (`ledger.rs`). Origin to be traced.
const DIRECTORY_WAIT_MS: u64 = 10_000;

/// DID documents fetched where `zu::did::url` says, each time: only requests that spend or
/// make something are checked, and those are few.
struct Resolver(reqwest::Client);

impl Directory for Resolver {
    fn doc<'a>(&'a self, did: &'a str) -> BoxFuture<'a, Result<Doc, Refusal>> {
        let (signer, away) = (
            Refusal(StatusCode::FORBIDDEN, "signer"),
            Refusal(StatusCode::BAD_GATEWAY, "directory"),
        );
        Box::pin(async move {
            let url = zu::did::url(did, false).map_err(|_| signer)?;
            let fetched =
                self.0.get(url).timeout(Duration::from_millis(DIRECTORY_WAIT_MS)).send().await;
            let fetched = fetched.and_then(|answer| answer.error_for_status()).map_err(|_| away)?;
            zu::did::resolve(did, &fetched.bytes().await.map_err(|_| away)?).map_err(|_| signer)
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // One TLS provider in the whole workspace, as in the core: ring.
    _ = rustls::crypto::ring::default_provider().install_default();
    let env = |name: &str, or: &str| std::env::var(name).unwrap_or_else(|_| or.to_owned());
    // The log first, beside the ledger and the rooms, and on standard error for whoever deploys.
    let (data, level) = (env("ZUTALK_DATA", "data"), std::env::var("ZUTALK_LOG").ok());
    let level = zutalk_log::Level::from_setting(level.as_deref());
    let logs = std::path::Path::new(&data).join("logs");
    let _log = match zutalk_log::install(&logs, "zutalk-server", level, true) {
        Ok(guard) => Some(guard),
        Err(error) => {
            eprintln!("zutalk-server has no log: {error}"); // before the log: none
            None
        }
    };
    zutalk_log::info!("server.start", version = env!("CARGO_PKG_VERSION"));
    let coordinators = env("ZUTALK_COORDINATORS", "");
    let coordinators = coordinators.split(',').filter(|did| !did.is_empty()).map(str::to_owned);
    let origin = env("ZUTALK_ORIGIN", "http://127.0.0.1:8000");
    let mut config = Config::new(&origin, data, coordinators.collect());
    let http = reqwest::Client::new();
    // The 0.8 line's invite service: its `ZULANGUE_ADMIN_TOKEN`, so that claims retire invites.
    let (url, key) =
        (env("INVITES_URL", "https://zulangue-invite.exe.xyz"), env("INVITES_TOKEN", ""));
    config.invites = Some(Invites { http: http.clone(), url, key });
    let soniox = Soniox {
        http: http.clone(),
        url: env("SONIOX_URL", "https://api.soniox.com"),
        key: env("SONIOX_API_KEY", ""),
        // A second of stream came to about 46 µ$ over the 0.8 line, translation included.
        micros_per_s: env("SONIOX_MICROS_PER_SECOND", "50").parse()?,
        // Charged whole, so short: a lane hands over to a fresh key at a pause.
        cap_s: env("SONIOX_SESSION_CAP_S", "300").parse()?,
    };
    let openrouter = OpenRouter {
        http: http.clone(),
        url: env("OPENROUTER_URL", "https://openrouter.ai/api/v1"),
        key: env("OPENROUTER_MANAGEMENT_KEY", ""),
        // Charged whole, so small: a long write leases again.
        slice: env("OPENROUTER_SLICE_MICROS", "100000").parse()?,
    };
    let server =
        Server::open(config, Arc::new(Resolver(http)), [Arc::new(soniox), Arc::new(openrouter)])?;
    let listener = tokio::net::TcpListener::bind(env("ZUTALK_LISTEN", "127.0.0.1:8000")).await?;
    Ok(serve(listener, server).await?)
}
