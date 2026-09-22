mod bot;
mod commands;
mod config;
mod detection;
mod incident;
mod quarantine;
mod state;

use std::{env, path::PathBuf, sync::Arc};

use anyhow::{Context as _, Result, bail};
use clap::Parser;
use serenity::{
    Client,
    all::{GatewayIntents, Http, Webhook},
};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::{
    bot::Handler,
    config::Config,
    incident::{IncidentReporter, IncidentSink},
    quarantine::QuarantineStore,
    state::SecurityState,
};

#[derive(Debug, Parser)]
#[command(author, version, about)]
struct Args {
    /// Path to Hugh's TOML configuration.
    #[arg(long, default_value = "config.toml", env = "HUGH_CONFIG")]
    config: PathBuf,

    /// Validate configuration and exit without connecting to Discord.
    #[arg(long)]
    check_config: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let args = Args::parse();
    let config = Arc::new(Config::load(&args.config).await?);

    if args.check_config {
        info!(path = %args.config.display(), "configuration is valid");
        return Ok(());
    }

    let token = env::var("DISCORD_TOKEN").context("DISCORD_TOKEN is not set")?;
    if token.trim().is_empty() {
        bail!("DISCORD_TOKEN must not be empty");
    }

    let webhook_url =
        env::var("HUGH_INCIDENT_WEBHOOK_URL").context("HUGH_INCIDENT_WEBHOOK_URL is not set")?;
    if webhook_url.trim().is_empty() {
        bail!("HUGH_INCIDENT_WEBHOOK_URL must not be empty");
    }
    let bootstrap_http = Http::new(&token);
    let webhook = Webhook::from_url(&bootstrap_http, &webhook_url)
        .await
        .context("failed to load the incident webhook; check its URL")?;
    drop(webhook_url);

    let incidents = IncidentReporter::new(
        IncidentSink::open(&config.incident_log_path).await?,
        webhook,
    );
    let quarantine = if config.quarantine.enabled {
        Some(QuarantineStore::open(&config.quarantine.store_path).await?)
    } else {
        None
    };
    let handler = Handler::new(
        Arc::clone(&config),
        Arc::new(SecurityState::new(&config)),
        incidents,
        quarantine,
    );

    let intents = GatewayIntents::GUILDS
        | GatewayIntents::GUILD_MEMBERS
        | GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;
    let mut client = Client::builder(&token, intents)
        .event_handler(handler)
        .await
        .context("failed to create Discord client; check the token")?;

    // Drop our bootstrap copy; Serenity retains the authentication material it needs.
    drop(token);
    info!(guild_id = config.guild_id, "starting Hugh");

    tokio::select! {
        result = client.start() => result.context("Discord gateway stopped unexpectedly")?,
        signal = shutdown_signal() => {
            signal?;
            warn!("shutdown signal received");
            client.shard_manager.shutdown_all().await;
        }
    }

    Ok(())
}

async fn shutdown_signal() -> Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = signal(SignalKind::terminate())
            .context("failed to listen for the termination signal")?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result.context("failed to listen for Ctrl+C")?,
            _ = terminate.recv() => {},
        }
    }

    #[cfg(not(unix))]
    tokio::signal::ctrl_c()
        .await
        .context("failed to listen for Ctrl+C")?;

    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("hugh=info"));
    let json = env::var("HUGH_LOG_FORMAT").is_ok_and(|value| value.eq_ignore_ascii_case("json"));
    if json {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
}
