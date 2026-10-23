//! The moderator's own tools: `/warn`, `/mute`, `/unmute`, `/kick`, `/ban`,
//! `/unban`, `/unwarn`, `/modlogs` and `/standing`.
//!
//! Every one of them writes a case card to the mod log and a row to the
//! moderation record, so one case number covers what happened, why, who did
//! it, and what it cost the member's standing.

use anyhow::{Context as _, Result, bail};
use chrono::Utc;
use serenity::{
    all::{
        CommandDataOption, CommandInteraction, CommandOptionType, Context, GuildId, Member,
        Permissions, Timestamp, User, UserId,
    },
    builder::{
        CreateCommand, CreateCommandOption, CreateEmbed, CreateEmbedAuthor, CreateEmbedFooter,
        CreateMessage, EditInteractionResponse, EditMember,
    },
};
use tracing::warn;

use crate::{
    commands::{invoker_level, member_level, option_integer, option_string, option_user},
    config::{AccessLevel, Config, StandingBand},
    incident::{Incident, IncidentReporter, UserSnapshot},
    moderation::{ModerationStore, NewPunishment, Punishment, Record, Standing, parse_duration},
};

/// Discord's audit-log reason field, with room left for Hugh's own prefix.
const AUDIT_LIMIT: usize = 440;
/// Width of the standing bar on a `/standing` card.
const BAR_SEGMENTS: usize = 20;
/// Discord refuses a timeout that ends more than 28 days from now.
const MAX_TIMEOUT_SECONDS: u64 = 28 * 86_400;

fn reason(description: &str, required: bool) -> CreateCommandOption {
    CreateCommandOption::new(CommandOptionType::String, "reason", description)
        .min_length(1)
        .max_length(512)
        .required(required)
}

fn target(description: &str) -> CreateCommandOption {
    CreateCommandOption::new(CommandOptionType::User, "user", description).required(true)
}

pub fn commands() -> Vec<(&'static str, CreateCommand)> {
    let mut commands = vec![
        (
            "warn",
            CreateCommand::new("warn")
                .description("Warn a member and record it against their standing")
                .add_option(target("Member to warn"))
                .add_option(reason("Why they are being warned", true)),
        ),
        (
            "kick",
            CreateCommand::new("kick")
                .description("Remove a member from the server")
                .add_option(target("Member to kick"))
                .add_option(reason("Why they are being kicked", false)),
        ),
        (
            "ban",
            CreateCommand::new("ban")
                .description("Ban a member, permanently or for a set length of time")
                .add_option(target("Member to ban"))
                .add_option(reason("Why they are being banned", false))
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::String,
                        "length",
                        "How long, such as 30m, 1h30m, 2d or 1w. Omit to ban permanently",
                    )
                    .min_length(1)
                    .max_length(32),
                )
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::Integer,
                        "delete_days",
                        "Days of their recent messages to delete, 0 to 7",
                    )
                    .min_int_value(0)
                    .max_int_value(7),
                ),
        ),
        (
            "unban",
            CreateCommand::new("unban")
                .description("Lift a ban, by user ID")
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::String,
                        "user",
                        "ID of the banned account",
                    )
                    .required(true)
                    .min_length(15)
                    .max_length(20),
                )
                .add_option(reason("Why the ban is being lifted", false)),
        ),
        (
            "unwarn",
            CreateCommand::new("unwarn")
                .description("Take a case back, so it stops counting against standing")
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::Integer,
                        "case",
                        "Case number from /modlogs",
                    )
                    .required(true)
                    .min_int_value(1),
                )
                .add_option(reason("Why the case is being taken back", false)),
        ),
        (
            "modlogs",
            CreateCommand::new("modlogs")
                .description("Show a member's moderation history")
                .add_option(target("Member to look up")),
        ),
        (
            "standing",
            CreateCommand::new("standing")
                .description("Show a member's standing and the automod strictness it earns")
                .add_option(CreateCommandOption::new(
                    CommandOptionType::User,
                    "user",
                    "Member to look up; defaults to you",
                )),
        ),
    ];
    commands.extend(timeout_commands());
    commands
}

/// `/mute` and `/unmute`, which work through Discord's own timeout rather than
/// a muted role, so nothing has to be kept in step with channel permissions.
fn timeout_commands() -> [(&'static str, CreateCommand); 2] {
    [
        (
            "mute",
            CreateCommand::new("mute")
                .description("Time a member out so they cannot talk, for up to 28 days")
                .add_option(target("Member to mute"))
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::String,
                        "length",
                        "How long, such as 10m, 2h or 3d. Up to 28 days",
                    )
                    .required(true)
                    .min_length(1)
                    .max_length(32),
                )
                .add_option(reason("Why they are being muted", false)),
        ),
        (
            "unmute",
            CreateCommand::new("unmute")
                .description("Lift a member's timeout before it runs out")
                .add_option(target("Member to unmute"))
                .add_option(reason("Why the timeout is being lifted", false)),
        ),
    ]
}

pub async fn handle(
    ctx: &Context,
    command: &CommandInteraction,
    config: &Config,
    store: &ModerationStore,
    incidents: &IncidentReporter,
) -> Result<()> {
    if !config.moderation.enabled {
        bail!("the moderation record is disabled in Hugh's configuration");
    }
    match command.data.name.as_str() {
        "warn" => warn_member(ctx, command, config, store, incidents).await,
        "mute" => mute(ctx, command, config, store, incidents).await,
        "unmute" => unmute(ctx, command, config, incidents).await,
        "kick" => kick(ctx, command, config, store, incidents).await,
        "ban" => ban(ctx, command, config, store, incidents).await,
        "unban" => unban(ctx, command, config, store, incidents).await,
        "unwarn" => unwarn(ctx, command, config, store, incidents).await,
        "modlogs" => modlogs(ctx, command, config, store).await,
        "standing" => standing(ctx, command, config, store).await,
        other => bail!("unknown moderation command {other:?}"),
    }
}

async fn warn_member(
    ctx: &Context,
    command: &CommandInteraction,
    config: &Config,
    store: &ModerationStore,
    incidents: &IncidentReporter,
) -> Result<()> {
    command.defer_ephemeral(&ctx.http).await?;
    let target = target_user(&command.data.options)?;
    let member = actionable(ctx, command, config, target).await?;
    let member = member.context("that member is not in this server")?;
    let reason = option_string(&command.data.options, "reason").context("missing reason option")?;

    notify(ctx, config, target, "warned in", reason, None).await;
    let outcome = record_case(
        ctx,
        command,
        config,
        store,
        incidents,
        Case {
            kind: Punishment::Warn,
            subject: (&member.user).into(),
            action: "member_warned",
            reason,
            duration_seconds: None,
        },
    )
    .await?;
    reply(
        ctx,
        command,
        config,
        &outcome,
        format!("Warned <@{target}>."),
    )
    .await
}

async fn mute(
    ctx: &Context,
    command: &CommandInteraction,
    config: &Config,
    store: &ModerationStore,
    incidents: &IncidentReporter,
) -> Result<()> {
    command.defer_ephemeral(&ctx.http).await?;
    let guild_id = command.guild_id.context("this command is server-only")?;
    let target = target_user(&command.data.options)?;
    let member = actionable(ctx, command, config, target).await?;
    let member = member.context("that member is not in this server")?;
    let reason = option_string(&command.data.options, "reason").unwrap_or("No reason given");
    let length = option_string(&command.data.options, "length").context("missing length option")?;
    let seconds = mute_length(length)?;
    let until_unix = Utc::now()
        .timestamp()
        .saturating_add(i64::try_from(seconds)?);
    let until = Timestamp::from_unix_timestamp(until_unix)
        .context("that length does not land on a date Discord accepts")?;

    let audit = audit_reason(command, reason);
    guild_id
        .edit_member(
            &ctx.http,
            target,
            EditMember::new()
                .disable_communication_until_datetime(until)
                .audit_log_reason(&audit),
        )
        .await
        .context(
            "Discord refused the timeout; check Hugh's Moderate Members permission and role \
             order, and that the member is not an administrator",
        )?;
    // Unlike a kick, the member is still here afterwards, so the DM can wait
    // until the timeout has actually landed.
    notify(ctx, config, target, "timed out in", reason, Some(seconds)).await;

    let outcome = record_case(
        ctx,
        command,
        config,
        store,
        incidents,
        Case {
            kind: Punishment::Timeout,
            subject: (&member.user).into(),
            action: "member_timed_out",
            reason,
            duration_seconds: Some(seconds),
        },
    )
    .await?;
    reply(
        ctx,
        command,
        config,
        &outcome,
        format!(
            "Muted <@{target}> for {}. Discord lifts it <t:{until_unix}:R>.",
            crate::incident::human_duration(seconds)
        ),
    )
    .await
}

/// Lifting a timeout costs the member nothing and takes nothing back: the case
/// that muted them stays on record, the same way `/unban` leaves the ban case.
async fn unmute(
    ctx: &Context,
    command: &CommandInteraction,
    config: &Config,
    incidents: &IncidentReporter,
) -> Result<()> {
    command.defer_ephemeral(&ctx.http).await?;
    let guild_id = command.guild_id.context("this command is server-only")?;
    let target = target_user(&command.data.options)?;
    let member = actionable(ctx, command, config, target).await?;
    let member = member.context("that member is not in this server")?;
    let reason = option_string(&command.data.options, "reason").unwrap_or("No reason given");
    let now = Utc::now().timestamp();
    if member
        .communication_disabled_until
        .is_none_or(|until| until.unix_timestamp() <= now)
    {
        bail!("<@{target}> is not timed out");
    }

    let audit = audit_reason(command, reason);
    guild_id
        .edit_member(
            &ctx.http,
            target,
            EditMember::new()
                .enable_communication()
                .audit_log_reason(&audit),
        )
        .await
        .context(
            "Discord refused to lift the timeout; check Hugh's Moderate Members permission and \
             role order",
        )?;

    let subject: UserSnapshot = (&member.user).into();
    let mut incident = Incident::new(
        "member_unmute",
        config.guild_id,
        "member_unmuted",
        reason.to_owned(),
    );
    incident.subject = Some(subject.clone());
    incident.moderator = Some((&command.user).into());
    incident.channel_id = Some(command.channel_id.get());
    incident
        .details
        .insert("target_user_id".into(), target.get().into());
    let case_id = incidents.report(&ctx.http, incident).await;

    let embed = CreateEmbed::new()
        .title(format!(
            "{}Case #{case_id} · Timeout lifted",
            config.mark("release")
        ))
        .description(format!(
            "<@{target}> can talk again.\n{}",
            reason_line(config, reason)
        ))
        .colour(0x0023_A55A);
    command
        .edit_response(
            &ctx.http,
            EditInteractionResponse::new().embed(with_portrait(embed, &subject)),
        )
        .await?;
    Ok(())
}

/// A moderator's `/mute` length, held to what a Discord timeout can be.
fn mute_length(input: &str) -> Result<u64> {
    let Some(seconds) = parse_duration(input)? else {
        bail!(
            "a mute has to end; give a length such as 10m, 2h or 3d, or use /ban to remove \
             somebody for good"
        );
    };
    if seconds > MAX_TIMEOUT_SECONDS {
        bail!("Discord caps a timeout at 28 days; use /ban for anything longer");
    }
    Ok(seconds)
}

async fn kick(
    ctx: &Context,
    command: &CommandInteraction,
    config: &Config,
    store: &ModerationStore,
    incidents: &IncidentReporter,
) -> Result<()> {
    command.defer_ephemeral(&ctx.http).await?;
    let guild_id = command.guild_id.context("this command is server-only")?;
    let target = target_user(&command.data.options)?;
    let member = actionable(ctx, command, config, target).await?;
    let member = member.context("that member is not in this server")?;
    let reason = option_string(&command.data.options, "reason").unwrap_or("No reason given");

    // The DM goes first: once they are out of the server Hugh may no longer
    // share a mutual guild with them, and the message would never arrive.
    notify(ctx, config, target, "kicked from", reason, None).await;
    guild_id
        .kick_with_reason(&ctx.http, target, &audit_reason(command, reason))
        .await
        .context("Discord refused the kick; check Hugh's Kick Members permission and role order")?;

    let outcome = record_case(
        ctx,
        command,
        config,
        store,
        incidents,
        Case {
            kind: Punishment::Kick,
            subject: (&member.user).into(),
            action: "member_kicked",
            reason,
            duration_seconds: None,
        },
    )
    .await?;
    reply(
        ctx,
        command,
        config,
        &outcome,
        format!("Kicked <@{target}>."),
    )
    .await
}

async fn ban(
    ctx: &Context,
    command: &CommandInteraction,
    config: &Config,
    store: &ModerationStore,
    incidents: &IncidentReporter,
) -> Result<()> {
    command.defer_ephemeral(&ctx.http).await?;
    let guild_id = command.guild_id.context("this command is server-only")?;
    let target = target_user(&command.data.options)?;
    // A ban is the one action that can land on somebody who already left, so a
    // missing member is not a refusal here.
    let member = actionable(ctx, command, config, target).await?;
    let reason = option_string(&command.data.options, "reason").unwrap_or("No reason given");
    let length = option_string(&command.data.options, "length").unwrap_or_default();
    let duration = parse_duration(length)?;
    if let Some(seconds) = duration
        && config.moderation.max_ban_duration_seconds > 0
        && seconds > config.moderation.max_ban_duration_seconds
    {
        bail!(
            "that length is above the configured ceiling of {}",
            crate::incident::human_duration(config.moderation.max_ban_duration_seconds)
        );
    }
    let delete_days = option_integer(&command.data.options, "delete_days")
        .and_then(|days| u8::try_from(days).ok())
        .unwrap_or(config.moderation.ban_delete_message_days)
        .min(7);

    notify(ctx, config, target, "banned from", reason, duration).await;
    guild_id
        .ban_with_reason(
            &ctx.http,
            target,
            delete_days,
            &audit_reason(command, reason),
        )
        .await
        .context("Discord refused the ban; check Hugh's Ban Members permission and role order")?;

    let kind = if duration.is_some() {
        Punishment::TemporaryBan
    } else {
        Punishment::Ban
    };
    let subject = member.as_ref().map_or_else(
        || UserSnapshot::placeholder(target.get()),
        |member| (&member.user).into(),
    );
    let outcome = record_case(
        ctx,
        command,
        config,
        store,
        incidents,
        Case {
            kind,
            subject,
            action: if duration.is_some() {
                "member_temporarily_banned"
            } else {
                "member_banned"
            },
            reason,
            duration_seconds: duration,
        },
    )
    .await?;
    let summary = duration.map_or_else(
        || format!("Banned <@{target}> permanently."),
        |seconds| {
            format!(
                "Banned <@{target}> for {}. Hugh lifts it when the time is up.",
                crate::incident::human_duration(seconds)
            )
        },
    );
    reply(ctx, command, config, &outcome, summary).await
}

async fn unban(
    ctx: &Context,
    command: &CommandInteraction,
    config: &Config,
    store: &ModerationStore,
    incidents: &IncidentReporter,
) -> Result<()> {
    command.defer_ephemeral(&ctx.http).await?;
    let guild_id = command.guild_id.context("this command is server-only")?;
    let raw = option_string(&command.data.options, "user").context("missing user option")?;
    let target: UserId = raw
        .trim()
        .parse::<u64>()
        .map(UserId::new)
        .map_err(|_| anyhow::anyhow!("{raw:?} is not a Discord user ID"))?;
    let reason = option_string(&command.data.options, "reason").unwrap_or("No reason given");

    guild_id.unban(&ctx.http, target).await.context(
        "Discord refused the unban; the account may not be banned, or Hugh may lack Ban Members",
    )?;
    // Close the open ban case, so the expiry sweep does not try to lift a ban
    // a moderator has already lifted by hand.
    let closed = store
        .history(target.get())
        .await
        .into_iter()
        .find(|record| {
            matches!(record.kind, Punishment::Ban | Punishment::TemporaryBan) && !record.lifted
        })
        .map(|record| record.case_id);
    if let Some(case_id) = closed {
        store.lift(case_id).await?;
    }

    let mut incident = Incident::new(
        "member_unban",
        config.guild_id,
        "member_unbanned",
        reason.to_owned(),
    );
    incident.moderator = Some((&command.user).into());
    incident.channel_id = Some(command.channel_id.get());
    incident
        .details
        .insert("target_user_id".into(), target.get().into());
    if let Some(case_id) = closed {
        incident
            .details
            .insert("original_case".into(), case_id.into());
    }
    let case_id = incidents.report(&ctx.http, incident).await;

    let note = closed.map_or_else(
        || " No open ban case was on record for them.".to_owned(),
        |case| format!(" Case #{case} is now closed."),
    );
    let mut embed = CreateEmbed::new()
        .title(format!(
            "{}Case #{case_id} · Ban lifted",
            config.mark("unban")
        ))
        .description(format!(
            "Unbanned <@{target}>.{note}\n{}",
            reason_line(config, reason)
        ))
        .colour(0x0023_A55A);
    // Hugh holds only an ID here, so the face comes from Discord or not at all.
    if let Some(subject) = portrait(ctx, target.get()).await {
        embed = with_portrait(embed, &subject);
    }
    command
        .edit_response(&ctx.http, EditInteractionResponse::new().embed(embed))
        .await?;
    Ok(())
}

async fn unwarn(
    ctx: &Context,
    command: &CommandInteraction,
    config: &Config,
    store: &ModerationStore,
    incidents: &IncidentReporter,
) -> Result<()> {
    command.defer_ephemeral(&ctx.http).await?;
    let case = option_integer(&command.data.options, "case")
        .and_then(|case| u64::try_from(case).ok())
        .context("missing case option")?;
    let reason = option_string(&command.data.options, "reason").unwrap_or("Revoked by a moderator");

    let record = store
        .revoke(case, Some(command.user.id.get()))
        .await?
        .with_context(|| format!("no case numbered #{case} is on the moderation record"))?;
    if record.revoked {
        bail!("Case #{case} was already taken back");
    }

    let standing = store.standing(record.user_id, config).await;
    let mut incident = Incident::new(
        "case_revoked",
        config.guild_id,
        "warning_revoked",
        reason.to_owned(),
    );
    incident.moderator = Some((&command.user).into());
    incident.channel_id = Some(command.channel_id.get());
    incident.actor_id = Some(record.user_id);
    incident
        .details
        .insert("target_user_id".into(), record.user_id.into());
    incident.details.insert("revoked_case".into(), case.into());
    incident
        .details
        .insert("punishment".into(), record.kind.label().into());
    incident
        .details
        .insert("standing".into(), standing_text(&standing).into());
    let case_id = incidents.report(&ctx.http, incident).await;

    let mut embed = CreateEmbed::new()
        .title(format!(
            "{}Case #{case_id} · Case #{case} taken back",
            config.mark("release")
        ))
        .description(format!(
            "The {} on <@{}> no longer counts.\n{}\nTheir standing is now {}.",
            record.kind.label().to_lowercase(),
            record.user_id,
            reason_line(config, reason),
            standing_text(&standing)
        ))
        .colour(0x0023_A55A);
    if let Some(subject) = portrait(ctx, record.user_id).await {
        embed = with_portrait(embed, &subject);
    }
    command
        .edit_response(&ctx.http, EditInteractionResponse::new().embed(embed))
        .await?;
    Ok(())
}

/// What taking back a false positive undid, as phrases for the case card.
#[derive(Debug)]
pub struct Reversal {
    pub case_id: u64,
    pub undone: Vec<String>,
}

/// Takes back an automod case staff marked a false positive: the case stops
/// costing standing, and a timeout or ban it carried is lifted. Returns
/// `None` when the case had already been taken back. A deleted message and a
/// kick cannot be undone; the card says so rather than pretending.
pub async fn reverse_false_positive(
    ctx: &Context,
    config: &Config,
    store: &ModerationStore,
    incidents: &IncidentReporter,
    moderator: &User,
    case_id: u64,
) -> Result<Option<Reversal>> {
    let record = store
        .revoke(case_id, Some(moderator.id.get()))
        .await?
        .with_context(|| format!("Case #{case_id} is not on the moderation record"))?;
    if record.revoked {
        return Ok(None);
    }
    let guild_id = GuildId::new(config.guild_id);
    let target = UserId::new(record.user_id);
    let audit = truncate(
        &format!(
            "Hugh · {} · Case #{case_id} was a false positive",
            moderator.name
        ),
        AUDIT_LIMIT,
    );
    let mut undone = vec!["it no longer counts against their standing".to_owned()];
    match record.kind {
        Punishment::Timeout => {
            let lifted = guild_id
                .edit_member(
                    &ctx.http,
                    target,
                    EditMember::new()
                        .enable_communication()
                        .audit_log_reason(&audit),
                )
                .await;
            undone.push(match lifted {
                Ok(_) => "their timeout is lifted".to_owned(),
                Err(error) => {
                    warn!(?error, case_id, "could not lift a false-positive timeout");
                    "their timeout could not be lifted, so lift it by hand".to_owned()
                }
            });
        }
        Punishment::Ban | Punishment::TemporaryBan => {
            match guild_id.unban(&ctx.http, target).await {
                Ok(()) => {
                    store.lift(case_id).await?;
                    undone.push("their ban is lifted".to_owned());
                }
                Err(error) => {
                    warn!(?error, case_id, "could not lift a false-positive ban");
                    undone.push("their ban could not be lifted, so use /unban".to_owned());
                }
            }
        }
        Punishment::Kick => {
            undone.push("the kick itself cannot be undone, but they are free to rejoin".to_owned());
        }
        Punishment::Warn | Punishment::Quarantine | Punishment::Automod => {}
    }

    let standing = store.standing(record.user_id, config).await;
    let mut incident = Incident::new(
        "case_revoked",
        config.guild_id,
        "false_positive_reversed",
        format!("Case #{case_id} was marked a false positive"),
    );
    incident.moderator = Some(moderator.into());
    incident.actor_id = Some(record.user_id);
    incident
        .details
        .insert("target_user_id".into(), record.user_id.into());
    incident
        .details
        .insert("revoked_case".into(), case_id.into());
    incident
        .details
        .insert("punishment".into(), record.kind.label().into());
    incident
        .details
        .insert("standing".into(), standing_text(&standing).into());
    incidents.report(&ctx.http, incident).await;

    if config.moderation.dm_on_action {
        let content = format!(
            "A moderator reviewed an automatic action Hugh took against you in the server and \
             found it was a mistake. It has been taken back: {}.",
            undone.join(", ")
        );
        if let Err(error) = target
            .direct_message(ctx, CreateMessage::new().content(content))
            .await
        {
            warn!(
                ?error,
                user_id = record.user_id,
                "could not tell the member their case was taken back"
            );
        }
    }
    Ok(Some(Reversal { case_id, undone }))
}

async fn modlogs(
    ctx: &Context,
    command: &CommandInteraction,
    config: &Config,
    store: &ModerationStore,
) -> Result<()> {
    command.defer_ephemeral(&ctx.http).await?;
    let target = target_user(&command.data.options)?;
    let history = store.history(target.get()).await;
    let standing = store.standing(target.get(), config).await;

    let mut embed = CreateEmbed::new()
        .title(format!("{}Moderation history", config.mark("case")))
        .colour(if history.is_empty() {
            0x0023_A55A
        } else {
            0x00F0_B232
        });
    if history.is_empty() {
        embed = embed.description(format!(
            "<@{target}> has nothing on record. Standing is {}.",
            standing_text(&standing)
        ));
    } else {
        let shown = config.moderation.modlog_page_size.min(history.len());
        let lines: Vec<String> = history
            .iter()
            .take(shown)
            .map(|record| case_line(config, record))
            .collect();
        let more = history.len().saturating_sub(shown);
        let footer = if more > 0 {
            format!(" {more} older case(s) are on record.")
        } else {
            String::new()
        };
        embed = embed
            .description(format!(
                "<@{target}> · {} case(s) · standing {}{footer}",
                history.len(),
                standing_text(&standing)
            ))
            .field(
                format!("{}Cases", config.mark("details")),
                truncate(&lines.join("\n"), 1_024),
                false,
            )
            .footer(CreateEmbedFooter::new(
                "Take a case back with /unwarn case:<number>",
            ));
    }
    command
        .edit_response(&ctx.http, EditInteractionResponse::new().embed(embed))
        .await?;
    Ok(())
}

async fn standing(
    ctx: &Context,
    command: &CommandInteraction,
    config: &Config,
    store: &ModerationStore,
) -> Result<()> {
    command.defer_ephemeral(&ctx.http).await?;
    let target = option_user(&command.data.options, "user").unwrap_or(command.user.id);
    let standing = store.standing(target.get(), config).await;
    let strictness = standing.strictness;

    let mut embed = CreateEmbed::new()
        .title(format!(
            "{}Standing · {}",
            config.mark(standing.band.icon()),
            standing.band.label()
        ))
        .colour(band_colour(standing.band))
        .description(format!(
            "<@{target}>\n`[{}]` **{}**",
            standing_bar(standing.fraction()),
            standing_text(&standing)
        ))
        .field(
            format!("{}What it changes", config.mark("details")),
            format!(
                "Automod limits at **{:.0}%** of the configured values\nTimeouts at **{:.0}%** of \
                 the configured length",
                strictness.thresholds * 100.0,
                strictness.timeouts * 100.0
            ),
            false,
        );

    if standing.total_cases == 0 {
        embed = embed.field(
            format!("{}History", config.mark("case")),
            "Nothing on record.",
            false,
        );
    } else {
        let counts = standing
            .counts
            .iter()
            .map(|(kind, count)| format!("{} × {count}", kind.label()))
            .collect::<Vec<_>>()
            .join(" · ");
        let revoked = if standing.revoked_cases > 0 {
            format!("\n{} case(s) taken back", standing.revoked_cases)
        } else {
            String::new()
        };
        let latest = standing.latest.as_ref().map_or_else(String::new, |record| {
            format!("\nLatest: {}", case_line(config, record))
        });
        embed = embed.field(
            format!("{}History", config.mark("case")),
            truncate(
                &format!(
                    "{} case(s) · {:.1} weight still counting{revoked}\n{counts}{latest}",
                    standing.total_cases, standing.active_weight
                ),
                1_024,
            ),
            false,
        );
    }

    embed = embed.footer(CreateEmbedFooter::new(format!(
        "Weight fades to nothing over {:.0} days, so standing recovers on its own",
        config.standing.decay_days
    )));
    command
        .edit_response(&ctx.http, EditInteractionResponse::new().embed(embed))
        .await?;
    Ok(())
}

/// A punishment on its way to both the mod log and the moderation record.
struct Case<'a> {
    kind: Punishment,
    subject: UserSnapshot,
    action: &'a str,
    reason: &'a str,
    duration_seconds: Option<u64>,
}

/// What a recorded case leaves behind for the moderator's reply.
struct Outcome {
    case_id: u64,
    kind: Punishment,
    standing: Standing,
    /// Who it landed on, so the reply card shows their name and face.
    subject: UserSnapshot,
    /// The reason the moderator typed, repeated back to them.
    reason: String,
}

/// Posts the case card, writes the record row, and reads the member's standing
/// back so the moderator sees what the case cost them.
async fn record_case(
    ctx: &Context,
    command: &CommandInteraction,
    config: &Config,
    store: &ModerationStore,
    incidents: &IncidentReporter,
    case: Case<'_>,
) -> Result<Outcome> {
    let weight = case.kind.weight(&config.standing.punishments);
    let user_id = case.subject.id;
    let mut incident = Incident::new(
        format!("member_{}", case.kind.as_str()),
        config.guild_id,
        case.action,
        case.reason.to_owned(),
    );
    let subject = case.subject.clone();
    incident.subject = Some(case.subject);
    incident.moderator = Some((&command.user).into());
    incident.channel_id = Some(command.channel_id.get());
    incident
        .details
        .insert("target_user_id".into(), user_id.into());
    incident
        .details
        .insert("punishment".into(), case.kind.label().into());
    if let Some(seconds) = case.duration_seconds {
        incident
            .details
            .insert("duration_seconds".into(), seconds.into());
    }
    if weight > 0.0 {
        incident.details.insert("weight".into(), weight.into());
    }
    let case_id = incidents.report(&ctx.http, incident).await;

    store
        .record(NewPunishment {
            case_id,
            user_id,
            kind: case.kind,
            reason: case.reason.to_owned(),
            moderator_id: Some(command.user.id.get()),
            duration_seconds: case.duration_seconds,
            weight,
        })
        .await?;
    let standing = store.standing(user_id, config).await;
    Ok(Outcome {
        case_id,
        kind: case.kind,
        standing,
        subject,
        reason: case.reason.to_owned(),
    })
}

async fn reply(
    ctx: &Context,
    command: &CommandInteraction,
    config: &Config,
    outcome: &Outcome,
    summary: String,
) -> Result<()> {
    let embed = CreateEmbed::new()
        .title(format!(
            "{}Case #{} · {}",
            config.mark(outcome.kind.icon()),
            outcome.case_id,
            outcome.kind.label()
        ))
        .description(format!(
            "{summary}\n{}\nStanding is now {}.",
            reason_line(config, &outcome.reason),
            standing_text(&outcome.standing)
        ))
        .colour(band_colour(outcome.standing.band))
        .footer(CreateEmbedFooter::new(format!(
            "Case #{} is in the mod log and on their /modlogs",
            outcome.case_id
        )));
    command
        .edit_response(
            &ctx.http,
            EditInteractionResponse::new().embed(with_portrait(embed, &outcome.subject)),
        )
        .await?;
    Ok(())
}

/// The member's name and face on a reply card, the way a case card in the mod
/// log carries them. An account with no avatar simply keeps its name.
fn with_portrait(embed: CreateEmbed, subject: &UserSnapshot) -> CreateEmbed {
    let name = subject.display_name.as_ref().map_or_else(
        || format!("@{}", subject.username),
        |display| format!("@{} · {display}", subject.username),
    );
    let author = CreateEmbedAuthor::new(truncate(&name, 256));
    if subject.avatar_url.is_empty() {
        return embed.author(author);
    }
    embed
        .author(author.icon_url(&subject.avatar_url))
        .thumbnail(&subject.avatar_url)
}

/// The reason a moderator typed, on its own line under the summary.
fn reason_line(config: &Config, reason: &str) -> String {
    format!("{}**{}**", config.mark("reason"), truncate(reason, 1_500))
}

/// The face of somebody Hugh holds only an ID for. A fetch Discord refuses
/// leaves the card without a portrait rather than failing the command.
async fn portrait(ctx: &Context, user_id: u64) -> Option<UserSnapshot> {
    UserId::new(user_id)
        .to_user(&ctx.http)
        .await
        .ok()
        .map(|user| (&user).into())
}

fn target_user(options: &[CommandDataOption]) -> Result<UserId> {
    option_user(options, "user").context("missing user option")
}

/// Refuses the targets Hugh must not act on, and the mistake moderators make
/// most: acting on somebody at or above their own staff level. Returns the
/// member when they are in the server, and `None` when they are not.
async fn actionable(
    ctx: &Context,
    command: &CommandInteraction,
    config: &Config,
    target: UserId,
) -> Result<Option<Member>> {
    if target == command.user.id {
        bail!("that command is for other members, not yourself");
    }
    if target == ctx.cache.current_user().id {
        bail!("Hugh will not act on itself");
    }
    let guild_id = command.guild_id.context("this command is server-only")?;
    let guild = guild_id.to_partial_guild(&ctx.http).await?;
    if target == guild.owner_id {
        bail!("Discord does not allow a bot to act on the server owner");
    }

    let Ok(member) = guild_id.member(&ctx.http, target).await else {
        return Ok(None);
    };
    let their_level = target_level(ctx, config, guild_id, &member).await?;
    let own_level = invoker_level(command, config);
    if their_level >= own_level && their_level > AccessLevel::Everyone {
        bail!(
            "<@{target}> holds the **{}** staff level and you hold **{}**, so this is not yours to do",
            their_level.as_str(),
            own_level.as_str()
        );
    }
    Ok(Some(member))
}

/// The target's staff level, including any level their Discord permissions
/// grant them, so an administrator cannot be kicked by a helper.
async fn target_level(
    ctx: &Context,
    config: &Config,
    guild_id: GuildId,
    member: &Member,
) -> Result<AccessLevel> {
    let role_ids: Vec<u64> = member.roles.iter().map(|role| role.get()).collect();
    let mut permissions = Permissions::empty();
    if config.permissions.allow_discord_permissions {
        let roles = guild_id.roles(&ctx.http).await?;
        for role in &member.roles {
            if let Some(role) = roles.get(role) {
                permissions |= role.permissions;
            }
        }
    }
    Ok(member_level(
        config,
        member.user.id.get(),
        &role_ids,
        permissions,
    ))
}

/// Tells the member what happened and why. A closed DM is their choice, not an
/// error, so a failure here never stops the punishment.
async fn notify(
    ctx: &Context,
    config: &Config,
    target: UserId,
    phrase: &str,
    reason: &str,
    duration: Option<u64>,
) {
    if !config.moderation.dm_on_action {
        return;
    }
    let length = duration.map_or_else(String::new, |seconds| {
        format!(" for {}", crate::incident::human_duration(seconds))
    });
    let content = format!(
        "You have been {phrase} the server{length}.\n\nReason: {}",
        truncate(reason, 1_500)
    );
    if let Err(error) = target
        .direct_message(ctx, CreateMessage::new().content(content))
        .await
    {
        warn!(
            ?error,
            user_id = target.get(),
            "could not tell the member why; their DMs are closed"
        );
    }
}

/// The audit-log reason Discord shows beside the action.
fn audit_reason(command: &CommandInteraction, reason: &str) -> String {
    format!(
        "Hugh · {} · {}",
        command.user.name,
        truncate(reason, AUDIT_LIMIT)
    )
}

fn case_line(config: &Config, record: &Record) -> String {
    let when = record.at.timestamp();
    let length = record.duration_seconds.map_or_else(String::new, |seconds| {
        format!(" · {}", crate::incident::human_duration(seconds))
    });
    let moderator = record
        .moderator_id
        .map_or_else(|| "Hugh · automatic".to_owned(), |id| format!("<@{id}>"));
    let state = if record.revoked {
        " · **taken back**"
    } else if record.lifted {
        " · served"
    } else {
        ""
    };
    format!(
        "`#{}` {}{} — {}{length} · <t:{when}:R> · {moderator}{state}",
        record.case_id,
        config.mark(record.kind.icon()),
        record.kind.label(),
        truncate(&record.reason, 90),
    )
}

fn standing_text(standing: &Standing) -> String {
    format!(
        "**{:.1}** / {:.0} ({})",
        standing.points,
        standing.starting_points,
        standing.band.label()
    )
}

/// A plain-text bar, so standing reads at a glance without an emoji in sight.
fn standing_bar(fraction: f64) -> String {
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let filled = (fraction * BAR_SEGMENTS as f64)
        .round()
        .clamp(0.0, BAR_SEGMENTS as f64) as usize;
    format!(
        "{}{}",
        "#".repeat(filled),
        ".".repeat(BAR_SEGMENTS - filled)
    )
}

const fn band_colour(band: StandingBand) -> u32 {
    match band {
        StandingBand::Exemplary | StandingBand::Good => 0x0023_A55A,
        StandingBand::Neutral => 0x0058_65F2,
        StandingBand::Poor => 0x00F0_B232,
        StandingBand::Bad => 0x00ED_4245,
    }
}

pub fn truncate(value: &str, maximum: usize) -> String {
    if value.chars().count() <= maximum {
        return value.to_owned();
    }
    value
        .chars()
        .take(maximum.saturating_sub(1))
        .chain(std::iter::once('…'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        let config: Config = toml::from_str(
            "guild_id = \"1\"\nincident_channel_id = \"2\"\n\n[role_guard]\nenabled = false",
        )
        .unwrap();
        config.validate().unwrap();
        config
    }

    #[test]
    fn the_bar_tracks_the_fraction() {
        assert_eq!(standing_bar(1.0), "#".repeat(BAR_SEGMENTS));
        assert_eq!(standing_bar(0.0), ".".repeat(BAR_SEGMENTS));
        assert_eq!(standing_bar(0.5).matches('#').count(), BAR_SEGMENTS / 2);
        // Nonsense input cannot produce a bar of the wrong width.
        assert_eq!(standing_bar(-1.0).len(), BAR_SEGMENTS);
        assert_eq!(standing_bar(9.0).len(), BAR_SEGMENTS);
    }

    /// A timeout always ends, and Discord ends it no later than 28 days out.
    #[test]
    fn a_mute_must_end_within_discords_ceiling() {
        assert_eq!(mute_length("10m").unwrap(), 600);
        assert_eq!(mute_length("4w").unwrap(), MAX_TIMEOUT_SECONDS);
        assert!(mute_length("4w1s").is_err());
        assert!(mute_length("perm").is_err());
        assert!(mute_length("").is_err());
        assert!(mute_length("ten minutes").is_err());
    }

    #[test]
    fn standing_reads_as_points_out_of_the_starting_total() {
        let config = config();
        let standing = Standing::clean(&config);
        assert_eq!(standing_text(&standing), "**100.0** / 100 (Exemplary)");
        assert_eq!(standing.band, StandingBand::Exemplary);
    }

    /// A moderator should not have to open the mod log to see who they just
    /// acted on, or what they typed as the reason.
    #[test]
    fn a_reply_card_carries_the_member_and_the_reason() {
        let config = config();
        let subject = UserSnapshot {
            id: 42,
            username: "pluh".to_owned(),
            display_name: Some("Pluh".to_owned()),
            avatar_url: "https://cdn.discordapp.com/avatars/42/abc.png".to_owned(),
            bot: false,
        };
        let embed = serde_json::to_value(with_portrait(
            CreateEmbed::new().description(reason_line(&config, "Raid advertising")),
            &subject,
        ))
        .unwrap();

        assert_eq!(embed["author"]["name"].as_str().unwrap(), "@pluh · Pluh");
        assert_eq!(
            embed["author"]["icon_url"].as_str().unwrap(),
            subject.avatar_url
        );
        assert_eq!(
            embed["thumbnail"]["url"].as_str().unwrap(),
            subject.avatar_url
        );
        assert_eq!(
            embed["description"].as_str().unwrap(),
            "**Raid advertising**"
        );
    }

    /// An account with no avatar keeps its name and loses only the picture.
    #[test]
    fn a_faceless_account_still_names_itself() {
        let embed = serde_json::to_value(with_portrait(
            CreateEmbed::new(),
            &UserSnapshot::placeholder(42),
        ))
        .unwrap();
        assert_eq!(embed["author"]["name"].as_str().unwrap(), "@unknown-42");
        assert!(embed["author"]["icon_url"].is_null());
        assert!(embed["thumbnail"].is_null());
    }

    /// With no icon uploaded a case line still reads as a sentence, with no
    /// stranded space where a glyph would have been.
    #[test]
    fn case_lines_survive_an_empty_icon_slot() {
        let config = config();
        let record = Record {
            case_id: 7,
            user_id: 42,
            kind: Punishment::TemporaryBan,
            reason: "Raid advertising".to_owned(),
            moderator_id: Some(9),
            at: chrono::Utc::now(),
            duration_seconds: Some(3_600),
            weight: 40.0,
            revoked: false,
            lifted: false,
        };
        let line = case_line(&config, &record);
        assert!(
            line.starts_with("`#7` Temporary ban — Raid advertising"),
            "{line}"
        );
        assert!(line.contains("1 hour"), "{line}");
        assert!(line.contains("<@9>"), "{line}");
    }
}
