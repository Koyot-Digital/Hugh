use std::collections::BTreeMap;

use anyhow::{Context as _, Result, bail};
use serenity::{
    all::{
        Channel, ChannelId, CommandDataOptionValue, CommandInteraction, CommandOptionType, Context,
        GuildChannel, GuildId, Interaction, PermissionOverwrite, PermissionOverwriteType,
        Permissions, RoleId, UserId,
    },
    builder::{
        CreateCommand, CreateCommandOption, CreateEmbed, CreateInteractionResponse,
        CreateInteractionResponseMessage, EditInteractionResponse,
    },
};
use tracing::{error, info};

use crate::{
    config::Config,
    incident::{Incident, IncidentReporter},
    quarantine::QuarantineStore,
};

pub const REPOSITORY_URL: &str = "https://github.com/Koyot-Digital/Hugh";

pub async fn register(ctx: &Context, config: &Config) -> Result<()> {
    let guild_id = GuildId::new(config.guild_id);
    guild_id
        .set_commands(
            &ctx.http,
            vec![
                CreateCommand::new("lock")
                    .description("Lock a channel so regular members cannot send messages")
                    .default_member_permissions(Permissions::MANAGE_CHANNELS)
                    .add_option(CreateCommandOption::new(
                        CommandOptionType::Channel,
                        "channel",
                        "Channel to lock; defaults to the current channel",
                    )),
                CreateCommand::new("banish")
                    .description("Move a member into quarantine and safely store their roles")
                    .default_member_permissions(
                        Permissions::MANAGE_ROLES | Permissions::MODERATE_MEMBERS,
                    )
                    .add_option(
                        CreateCommandOption::new(
                            CommandOptionType::User,
                            "user",
                            "Member to quarantine",
                        )
                        .required(true),
                    ),
                CreateCommand::new("unbanish")
                    .description("Release a member and restore their stored roles")
                    .default_member_permissions(
                        Permissions::MANAGE_ROLES | Permissions::MODERATE_MEMBERS,
                    )
                    .add_option(
                        CreateCommandOption::new(
                            CommandOptionType::User,
                            "user",
                            "Member to release from quarantine",
                        )
                        .required(true),
                    ),
                CreateCommand::new("help").description("Show Hugh's command and protection guide"),
                CreateCommand::new("hugh").description("Learn about Hugh and its development"),
            ],
        )
        .await
        .context("failed to register guild commands")?;
    info!(
        guild_id = config.guild_id,
        "registered guild slash commands"
    );
    Ok(())
}

pub async fn handle(
    ctx: &Context,
    interaction: Interaction,
    config: &Config,
    store: Option<&QuarantineStore>,
    incidents: &IncidentReporter,
) {
    let Interaction::Command(command) = interaction else {
        return;
    };
    if command
        .guild_id
        .is_none_or(|guild| guild.get() != config.guild_id)
    {
        return;
    }

    let result = match command.data.name.as_str() {
        "lock" => lock(ctx, &command, config, incidents).await,
        "banish" => banish(ctx, &command, config, store, incidents).await,
        "unbanish" => unbanish(ctx, &command, config, store, incidents).await,
        "help" => help(ctx, &command).await,
        "hugh" => about(ctx, &command).await,
        _ => Ok(()),
    };
    if let Err(error) = result {
        error!(?error, command = command.data.name, "slash command failed");
        let error_message = error.to_string();
        let message = format!("Hugh could not complete that command: {error_message}");
        if command.get_response(&ctx.http).await.is_ok() {
            let _ = command
                .edit_response(&ctx.http, EditInteractionResponse::new().content(message))
                .await;
        } else {
            let _ = respond_text(ctx, &command, message, true).await;
        }
        let mut incident = Incident::new("command_failure", config.guild_id, "command_failed");
        incident.actor_id = Some(command.user.id.get());
        incident.channel_id = Some(command.channel_id.get());
        incident.details.insert(
            "command".into(),
            serde_json::Value::String(command.data.name.clone()),
        );
        incidents
            .report(
                &ctx.http,
                incident,
                format!(
                    "Command /{} by user {} failed: {error_message}",
                    command.data.name,
                    command.user.id.get()
                ),
            )
            .await;
    }
}

async fn lock(
    ctx: &Context,
    command: &CommandInteraction,
    config: &Config,
    incidents: &IncidentReporter,
) -> Result<()> {
    require_permissions(command, config, Permissions::MANAGE_CHANNELS)?;
    command.defer_ephemeral(&ctx.http).await?;
    let guild_id = command.guild_id.context("this command is server-only")?;
    let channel_id = option_channel(command).unwrap_or(command.channel_id);
    let channel = match channel_id.to_channel(&ctx.http).await? {
        Channel::Guild(channel) if channel.guild_id == guild_id => channel,
        _ => bail!("the selected channel does not belong to this server"),
    };

    let blocked = Permissions::SEND_MESSAGES
        | Permissions::SEND_MESSAGES_IN_THREADS
        | Permissions::CREATE_PUBLIC_THREADS
        | Permissions::CREATE_PRIVATE_THREADS
        | Permissions::ADD_REACTIONS;
    apply_overwrite(
        &ctx.http,
        &channel,
        PermissionOverwriteType::Role(RoleId::new(guild_id.get())),
        Permissions::empty(),
        blocked,
    )
    .await?;

    let mut incident = Incident::new("channel_lock", config.guild_id, "channel_locked");
    incident.actor_id = Some(command.user.id.get());
    incident.channel_id = Some(channel_id.get());
    incidents
        .report(
            &ctx.http,
            incident,
            format!(
                "Channel {} was locked by moderator {}.",
                channel_id.get(),
                command.user.id.get()
            ),
        )
        .await;
    command
        .edit_response(
            &ctx.http,
            EditInteractionResponse::new().content(format!(
                "Locked <#{}>. Existing permission settings were preserved.",
                channel_id.get()
            )),
        )
        .await?;
    Ok(())
}

async fn banish(
    ctx: &Context,
    command: &CommandInteraction,
    config: &Config,
    store: Option<&QuarantineStore>,
    incidents: &IncidentReporter,
) -> Result<()> {
    require_permissions(
        command,
        config,
        Permissions::MANAGE_ROLES | Permissions::MODERATE_MEMBERS,
    )?;
    command.defer_ephemeral(&ctx.http).await?;
    let store = quarantine_store(config, store)?;
    let guild_id = command.guild_id.context("this command is server-only")?;
    let user_id = option_user(command).context("missing user option")?;
    if user_id == ctx.cache.current_user().id {
        bail!("Hugh cannot quarantine itself");
    }
    let guild = guild_id.to_partial_guild(&ctx.http).await?;
    if user_id == guild.owner_id {
        bail!("Discord does not allow a bot to quarantine the server owner");
    }
    configure_quarantine(ctx, config).await?;

    let member = guild_id.member(&ctx.http, user_id).await?;
    let original_roles: Vec<u64> = member.roles.iter().map(|role| role.get()).collect();
    if !store
        .snapshot(user_id.get(), original_roles.clone())
        .await?
    {
        bail!("that member already has a stored quarantine snapshot");
    }

    let quarantine_role = RoleId::new(
        config
            .quarantine
            .role_id
            .context("quarantine role is not configured")?,
    );
    if let Err(error) = member.add_role(&ctx.http, quarantine_role).await {
        // No roles were removed, so it is safe to discard the unused snapshot.
        store.clear(user_id.get()).await?;
        return Err(error).context("failed to add the quarantine role");
    }

    let roles = guild_id.roles(&ctx.http).await?;
    let removable: Vec<RoleId> = member
        .roles
        .iter()
        .copied()
        .filter(|role| *role != quarantine_role)
        .filter(|role| roles.get(role).is_some_and(|details| !details.managed))
        .collect();
    let mut failed = Vec::new();
    for role in removable {
        if let Err(error) = member.remove_role(&ctx.http, role).await {
            error!(
                ?error,
                role_id = role.get(),
                user_id = user_id.get(),
                "failed to remove role while banishing"
            );
            failed.push(role.get());
        }
    }

    let action = if failed.is_empty() {
        "member_banished"
    } else {
        "member_partially_banished"
    };
    let response = if failed.is_empty() {
        format!(
            "Banished <@{}>. Their roles are stored for `/unbanish`.",
            user_id.get()
        )
    } else {
        format!(
            "Quarantine was applied to <@{}>, but these role IDs could not be removed: `{}`. The recovery snapshot was kept.",
            user_id.get(),
            join_ids(&failed)
        )
    };
    let mut incident = Incident::new("member_banish", config.guild_id, action);
    incident.actor_id = Some(command.user.id.get());
    incident.details = BTreeMap::from([
        ("target_user_id".into(), user_id.get().into()),
        ("stored_role_ids".into(), serde_json::json!(original_roles)),
        ("failed_role_ids".into(), serde_json::json!(failed)),
    ]);
    incidents
        .report(
            &ctx.http,
            incident,
            format!(
                "Moderator {} banished user {} ({action}).",
                command.user.id.get(),
                user_id.get()
            ),
        )
        .await;
    command
        .edit_response(&ctx.http, EditInteractionResponse::new().content(response))
        .await?;
    Ok(())
}

async fn unbanish(
    ctx: &Context,
    command: &CommandInteraction,
    config: &Config,
    store: Option<&QuarantineStore>,
    incidents: &IncidentReporter,
) -> Result<()> {
    require_permissions(
        command,
        config,
        Permissions::MANAGE_ROLES | Permissions::MODERATE_MEMBERS,
    )?;
    command.defer_ephemeral(&ctx.http).await?;
    let store = quarantine_store(config, store)?;
    let guild_id = command.guild_id.context("this command is server-only")?;
    let user_id = option_user(command).context("missing user option")?;
    let stored = store
        .get(user_id.get())
        .await
        .context("no stored quarantine snapshot exists for that member")?;
    let member = guild_id.member(&ctx.http, user_id).await?;
    let roles = guild_id.roles(&ctx.http).await?;
    let quarantine_role = RoleId::new(
        config
            .quarantine
            .role_id
            .context("quarantine role is not configured")?,
    );

    let mut restored = Vec::new();
    let mut missing = Vec::new();
    let mut failed = Vec::new();
    for role_id in &stored {
        let role = RoleId::new(*role_id);
        let Some(details) = roles.get(&role) else {
            missing.push(*role_id);
            continue;
        };
        if details.managed || role == quarantine_role {
            continue;
        }
        match member.add_role(&ctx.http, role).await {
            Ok(()) => restored.push(*role_id),
            Err(error) => {
                error!(
                    ?error,
                    role_id,
                    user_id = user_id.get(),
                    "failed to restore role"
                );
                failed.push(*role_id);
            }
        }
    }

    if !failed.is_empty() {
        bail!(
            "role restoration is incomplete for IDs `{}`; the member remains quarantined and the snapshot was kept",
            join_ids(&failed)
        );
    }
    member
        .remove_role(&ctx.http, quarantine_role)
        .await
        .context("roles were restored, but the quarantine role could not be removed")?;
    store.clear(user_id.get()).await?;

    let missing_note = if missing.is_empty() {
        String::new()
    } else {
        format!(" Deleted roles skipped: `{}`.", join_ids(&missing))
    };
    let mut incident = Incident::new("member_unbanish", config.guild_id, "member_released");
    incident.actor_id = Some(command.user.id.get());
    incident.details = BTreeMap::from([
        ("target_user_id".into(), user_id.get().into()),
        ("restored_role_ids".into(), serde_json::json!(restored)),
        ("missing_role_ids".into(), serde_json::json!(missing)),
    ]);
    incidents
        .report(
            &ctx.http,
            incident,
            format!(
                "Moderator {} released user {} from quarantine.",
                command.user.id.get(),
                user_id.get()
            ),
        )
        .await;
    command
        .edit_response(
            &ctx.http,
            EditInteractionResponse::new().content(format!(
                "Released <@{}> and restored {} role(s).{missing_note}",
                user_id.get(),
                restored.len()
            )),
        )
        .await?;
    Ok(())
}

async fn help(ctx: &Context, command: &CommandInteraction) -> Result<()> {
    let embed = CreateEmbed::new()
        .title("Hugh help")
        .description("Hugh protects this community from raids, mention abuse, spam, malicious invites, and unauthorized role elevation.")
        .field("Public", "`/hugh` - about the project\n`/help` - this guide", false)
        .field("Moderation", "`/lock [channel]` - stop regular members sending\n`/banish user` - isolate a member and save their roles\n`/unbanish user` - restore their roles", false)
        .field("Open source", format!("Contribute or report issues at {REPOSITORY_URL}"), false)
        .colour(0x0058_65F2);
    respond_embed(ctx, command, embed, true).await
}

async fn about(ctx: &Context, command: &CommandInteraction) -> Result<()> {
    respond_embed(ctx, command, about_embed(), false).await
}

pub fn about_embed() -> CreateEmbed {
    CreateEmbed::new()
        .title("Hi, I'm Hugh")
        .description("I'm this community's open-source security bot. I watch for raids, abusive mentions, spam, unsafe invites, and unauthorized role elevation.")
        .field("Help build Hugh", format!("Contributions are welcome: {REPOSITORY_URL}"), false)
        .colour(0x0058_65F2)
}

pub async fn configure_quarantine(ctx: &Context, config: &Config) -> Result<()> {
    if !config.quarantine.enabled {
        return Ok(());
    }
    let guild_id = GuildId::new(config.guild_id);
    let channels = guild_id.channels(&ctx.http).await?;
    let quarantine_channel = ChannelId::new(
        config
            .quarantine
            .channel_id
            .context("quarantine channel is not configured")?,
    );
    if !channels.contains_key(&quarantine_channel) {
        bail!("the configured quarantine channel does not exist in this server");
    }
    let quarantine_role = RoleId::new(
        config
            .quarantine
            .role_id
            .context("quarantine role is not configured")?,
    );
    if !guild_id
        .roles(&ctx.http)
        .await?
        .contains_key(&quarantine_role)
    {
        bail!("the configured quarantine role does not exist in this server");
    }
    for channel in channels.values() {
        configure_quarantine_channel(ctx, config, channel).await?;
    }
    Ok(())
}

pub async fn configure_quarantine_channel(
    ctx: &Context,
    config: &Config,
    channel: &GuildChannel,
) -> Result<()> {
    if !config.quarantine.enabled || channel.guild_id.get() != config.guild_id {
        return Ok(());
    }
    let quarantine_channel = ChannelId::new(
        config
            .quarantine
            .channel_id
            .context("quarantine channel is not configured")?,
    );
    let quarantine_role = RoleId::new(
        config
            .quarantine
            .role_id
            .context("quarantine role is not configured")?,
    );

    if channel.id == quarantine_channel {
        let bot_user_id = ctx.cache.current_user().id;
        apply_overwrite(
            &ctx.http,
            channel,
            PermissionOverwriteType::Role(RoleId::new(config.guild_id)),
            Permissions::empty(),
            Permissions::VIEW_CHANNEL,
        )
        .await?;
        let access = Permissions::VIEW_CHANNEL
            | Permissions::SEND_MESSAGES
            | Permissions::READ_MESSAGE_HISTORY;
        apply_overwrite(
            &ctx.http,
            channel,
            PermissionOverwriteType::Role(quarantine_role),
            access,
            Permissions::empty(),
        )
        .await?;
        apply_overwrite(
            &ctx.http,
            channel,
            PermissionOverwriteType::Member(bot_user_id),
            access,
            Permissions::empty(),
        )
        .await?;
    } else {
        apply_overwrite(
            &ctx.http,
            channel,
            PermissionOverwriteType::Role(quarantine_role),
            Permissions::empty(),
            Permissions::VIEW_CHANNEL,
        )
        .await?;
    }
    Ok(())
}

async fn apply_overwrite(
    http: &serenity::all::Http,
    channel: &GuildChannel,
    kind: PermissionOverwriteType,
    add_allow: Permissions,
    add_deny: Permissions,
) -> Result<()> {
    let existing = channel
        .permission_overwrites
        .iter()
        .find(|entry| entry.kind == kind);
    let overwrite = merged_overwrite(existing, kind, add_allow, add_deny);
    channel.create_permission(http, overwrite).await?;
    Ok(())
}

fn merged_overwrite(
    existing: Option<&PermissionOverwrite>,
    kind: PermissionOverwriteType,
    add_allow: Permissions,
    add_deny: Permissions,
) -> PermissionOverwrite {
    let mut allow = existing.map_or(Permissions::empty(), |entry| entry.allow);
    let mut deny = existing.map_or(Permissions::empty(), |entry| entry.deny);
    allow.remove(add_deny);
    deny.remove(add_allow);
    allow.insert(add_allow);
    deny.insert(add_deny);
    PermissionOverwrite { allow, deny, kind }
}

fn require_permissions(
    command: &CommandInteraction,
    config: &Config,
    required: Permissions,
) -> Result<()> {
    if config.trusted_user_ids.contains(&command.user.id.get()) {
        return Ok(());
    }
    let permissions = command
        .member
        .as_ref()
        .and_then(|member| member.permissions)
        .unwrap_or_default();
    if permissions.administrator() || permissions.contains(required) {
        Ok(())
    } else {
        bail!("you do not have the required server permissions")
    }
}

fn quarantine_store<'a>(
    config: &Config,
    store: Option<&'a QuarantineStore>,
) -> Result<&'a QuarantineStore> {
    if !config.quarantine.enabled {
        bail!("quarantine is disabled in Hugh's configuration");
    }
    store.context("the quarantine store is unavailable")
}

fn option_user(command: &CommandInteraction) -> Option<UserId> {
    command
        .data
        .options
        .iter()
        .find_map(|option| match option.value {
            CommandDataOptionValue::User(id) if option.name == "user" => Some(id),
            _ => None,
        })
}

fn option_channel(command: &CommandInteraction) -> Option<ChannelId> {
    command
        .data
        .options
        .iter()
        .find_map(|option| match option.value {
            CommandDataOptionValue::Channel(id) if option.name == "channel" => Some(id),
            _ => None,
        })
}

async fn respond_text(
    ctx: &Context,
    command: &CommandInteraction,
    content: impl Into<String>,
    ephemeral: bool,
) -> Result<()> {
    command
        .create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(content)
                    .ephemeral(ephemeral),
            ),
        )
        .await?;
    Ok(())
}

async fn respond_embed(
    ctx: &Context,
    command: &CommandInteraction,
    embed: CreateEmbed,
    ephemeral: bool,
) -> Result<()> {
    command
        .create_response(
            &ctx.http,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .embed(embed)
                    .ephemeral(ephemeral),
            ),
        )
        .await?;
    Ok(())
}

fn join_ids(ids: &[u64]) -> String {
    ids.iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merging_overwrite_preserves_unrelated_permissions() {
        let kind = PermissionOverwriteType::Role(RoleId::new(7));
        let existing = PermissionOverwrite {
            allow: Permissions::ATTACH_FILES | Permissions::SEND_MESSAGES,
            deny: Permissions::MANAGE_MESSAGES,
            kind,
        };
        let merged = merged_overwrite(
            Some(&existing),
            kind,
            Permissions::VIEW_CHANNEL,
            Permissions::SEND_MESSAGES,
        );

        assert!(merged.allow.attach_files());
        assert!(merged.allow.view_channel());
        assert!(!merged.allow.send_messages());
        assert!(merged.deny.manage_messages());
        assert!(merged.deny.send_messages());
        assert!(!merged.deny.view_channel());
    }
}
