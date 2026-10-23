mod archive;
mod bot;
mod commands;
mod config;
mod detection;
mod discipline;
mod features;
mod incident;
mod lookup;
mod message_log;
mod mind;
mod moderation;
mod quarantine;
mod rotector;
mod scrape;
mod state;
mod welcome;

use std::{env, path::PathBuf, sync::Arc};

use anyhow::{Context as _, Result, bail};
use clap::Parser;
use serenity::{
    Client,
    all::{ChannelId, GatewayIntents},
};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::{
    archive::Archive,
    bot::Handler,
    config::Config,
    incident::{IncidentReporter, IncidentSink},
    mind::MindClient,
    moderation::ModerationStore,
    quarantine::{QuarantineManager, QuarantineStore},
    rotector::RotectorClient,
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

    // Validation guarantees the channel; Hugh posts its own case cards so the
    // icon emoji resolve and the channel's permissions apply.
    let incident_channel = ChannelId::new(
        config
            .incident_channel_id
            .context("incident_channel_id is not set")?,
    );
    let incidents = IncidentReporter::new(
        IncidentSink::open(&config.incident_log_path).await?,
        incident_channel,
        Arc::clone(&config),
    );
    // The record is opened whether or not the commands are registered:
    // standing and automod escalation are built on it either way.
    let moderation = ModerationStore::open(&config.moderation.store_path).await?;
    let quarantine = if config.quarantine.enabled {
        Some(QuarantineStore::open(&config.quarantine.store_path).await?)
    } else {
        None
    };
    let rotector = if config.rotector.enabled {
        let api_key = env::var("HUGH_ROTECTOR_API_KEY").context(
            "HUGH_ROTECTOR_API_KEY is not set; it is required while [rotector] is enabled",
        )?;
        if api_key.trim().is_empty() {
            bail!("HUGH_ROTECTOR_API_KEY must not be empty while [rotector] is enabled");
        }
        info!(
            base_url = config.rotector.base_url,
            "Rotector lookups are enabled"
        );
        Some(Arc::new(RotectorClient::new(api_key, &config.rotector)?))
    } else {
        None
    };
    let (archive, mind) = learning(&config)?;
    let handler = Handler::new(
        Arc::clone(&config),
        Arc::new(SecurityState::new(&config)),
        incidents.clone(),
        quarantine,
        QuarantineManager::new(),
        rotector,
        moderation.clone(),
    )
    .with_archive(archive)
    .with_mind(mind);

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

    // Temporary bans outlive the command that set them, so a background sweep
    // lifts the ones that have run out.
    tokio::spawn(moderation::sweep_expired_bans(
        Arc::clone(&client.http),
        moderation,
        Arc::clone(&config),
        incidents,
    ));

    info!(
        guild_id = config.guild_id,
        incident_channel_id = incident_channel.get(),
        "starting Hugh"
    );

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

/// The message archive and the Hugh Mind client, each when configured.
/// Validation guarantees Hugh Mind never runs without the archive.
fn learning(config: &Config) -> Result<(Option<Archive>, Option<Arc<MindClient>>)> {
    let archive = if config.archive.enabled {
        let archive = Archive::open(&config.archive)?;
        info!(
            path = config.archive.path,
            retention_days = config.archive.retention_days,
            "the message archive is open"
        );
        Some(archive)
    } else {
        None
    };
    let mind = if config.mind.enabled {
        info!(
            endpoint = config.mind.endpoint,
            mode = ?config.mind.mode,
            "Hugh Mind scores every new message"
        );
        Some(Arc::new(MindClient::new(&config.mind)?))
    } else {
        None
    };
    Ok((archive, mind))
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
        // Human-readable by default: the module path and span noise add nothing
        // for an operator watching enforcement decisions scroll past.
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .with_ansi(true)
            .compact()
            .init();
    }
}
