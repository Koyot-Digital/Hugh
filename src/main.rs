mod bot;
mod config;
mod detection;
mod incident;
mod state;

use std::{env, path::PathBuf, sync::Arc};

use anyhow::{Context as _, Result, bail};
use clap::Parser;
use serenity::{Client, all::GatewayIntents};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::{bot::Handler, config::Config, incident::IncidentSink, state::SecurityState};

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

    let incidents = IncidentSink::open(&config.incident_log_path).await?;
    let handler = Handler::new(
        Arc::clone(&config),
        Arc::new(SecurityState::new(&config)),
        incidents,
    );

    let intents = GatewayIntents::GUILDS
        | GatewayIntents::GUILD_MEMBERS
        | GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;
    let mut client = Client::builder(&token, intents)
        .event_handler(handler)
        .await
        .context("failed to create Discord client; check the token")?;

    // The token is not retained by our application after the client is built.
    drop(token);
    info!(guild_id = config.guild_id, "starting Hugh");

    tokio::select! {
        result = client.start() => result.context("Discord gateway stopped unexpectedly")?,
        signal = tokio::signal::ctrl_c() => {
            signal.context("failed to listen for shutdown signal")?;
            warn!("shutdown signal received");
            client.shard_manager.shutdown_all().await;
        }
    }

    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("hugh=info"));
    let json = env::var("HUGH_LOG_FORMAT").is_ok_and(|value| value.eq_ignore_ascii_case("json"));
    if json {
        tracing_subscriber::fmt().with_env_filter(filter).json().init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
}
