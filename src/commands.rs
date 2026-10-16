use std::collections::BTreeMap;

use anyhow::{Context as _, Result, bail};
use serenity::{
    all::{
        Channel, ChannelId, CommandDataOption, CommandDataOptionValue, CommandInteraction,
        CommandOptionType, Context, GuildChannel, GuildId, Interaction, PermissionOverwrite,
        PermissionOverwriteType, Permissions, RoleId, UserId,
    },
    builder::{
        CreateCommand, CreateCommandOption, CreateEmbed, CreateInteractionResponse,
        CreateInteractionResponseMessage, EditInteractionResponse,
    },
};
use tracing::{error, info};

use crate::{
    config::{AccessLevel, Config, DiscordPowers},
    incident::{Incident, IncidentReporter},
    lookup,
    quarantine::{QuarantineManager, QuarantineStore},
    rotector::RotectorClient,
};

pub const REPOSITORY_URL: &str = "https://github.com/Koyot-Digital/Hugh";

/// Discord permissions that stand in for the `moderator` tier when
/// `permissions.allow_discord_permissions` is enabled.
fn moderation_permissions() -> Permissions {
    Permissions::MODERATE_MEMBERS
        | Permissions::MANAGE_ROLES
        | Permissions::MANAGE_CHANNELS
        | Permissions::MANAGE_MESSAGES
        | Permissions::KICK_MEMBERS
        | Permissions::BAN_MEMBERS
        | Permissions::MANAGE_GUILD
}

/// The Discord-side default gate for a command, used only while no staff tier
/// lists explicit roles or users. Once tiers are configured, Hugh keeps the
/// command visible and enforces the tier itself, so a staff role without
/// Discord moderation permissions can still use it.
fn discord_gate(command: &str) -> Option<Permissions> {
    match command {
        "lock" => Some(Permissions::MANAGE_CHANNELS),
        "banish" | "unbanish" => Some(Permissions::MANAGE_ROLES | Permissions::MODERATE_MEMBERS),
        "rotector" => Some(Permissions::MODERATE_MEMBERS),
        _ => None,
    }
}

fn gated(command: CreateCommand, name: &str, config: &Config) -> CreateCommand {
    let level = config.required_level(name);
    if level == AccessLevel::Everyone || config.permissions.grants_configured(level) {
        return command;
    }
    match discord_gate(name) {
        Some(permissions) => command.default_member_permissions(permissions),
        None => command,
    }
}

pub async fn register(ctx: &Context, config: &Config) -> Result<()> {
    let guild_id = GuildId::new(config.guild_id);
    let reason = || {
        CreateCommandOption::new(
            CommandOptionType::String,
            "reason",
            "Reason for the case log",
        )
        .min_length(1)
        .max_length(512)
    };
    let mut commands = vec![
        (
            "lock",
            CreateCommand::new("lock")
                .description("Lock a channel so regular members cannot send messages")
                .add_option(CreateCommandOption::new(
                    CommandOptionType::Channel,
                    "channel",
                    "Channel to lock; defaults to the current channel",
                ))
                .add_option(reason()),
        ),
        (
            "banish",
            CreateCommand::new("banish")
                .description("Move a member into quarantine and safely store their roles")
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::User,
                        "user",
                        "Member to quarantine",
                    )
                    .required(true),
                )
                .add_option(reason()),
        ),
        (
            "unbanish",
            CreateCommand::new("unbanish")
                .description("Release a member and restore their stored roles")
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::User,
                        "user",
                        "Member to release from quarantine",
                    )
                    .required(true),
                )
                .add_option(reason()),
        ),
        (
            "help",
            CreateCommand::new("help").description("Show Hugh's command and protection guide"),
        ),
        (
            "hugh",
            CreateCommand::new("hugh").description("Learn about Hugh and its development"),
        ),
    ];
    if config.rotector.enabled {
        commands.push(("rotector", lookup::command()));
    }
    let names = commands
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(", ");
    let registered: Vec<CreateCommand> = commands
        .into_iter()
        .map(|(name, command)| gated(command, name, config))
        .collect();
    guild_id
        .set_commands(&ctx.http, registered)
        .await
        .context("failed to register guild commands")?;
    info!(
        guild_id = config.guild_id,
        commands = names,
        "registered guild slash commands"
    );
    Ok(())
}

pub async fn handle(
    ctx: &Context,
    interaction: Interaction,
    config: &Config,
    store: Option<&QuarantineStore>,
    resources: &QuarantineManager,
    incidents: &IncidentReporter,
    rotector: Option<&RotectorClient>,
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

    let name = command.data.name.clone();
    if let Err(denied) = access_check(&command, config) {
        deny(ctx, &command, config, incidents, &name, denied).await;
        return;
    }
    let result = match name.as_str() {
        "lock" => lock(ctx, &command, config, incidents).await,
        "banish" => banish(ctx, &command, config, store, resources, incidents).await,
        "unbanish" => unbanish(ctx, &command, config, store, resources, incidents).await,
        "rotector" => lookup::handle(ctx, &command, config, rotector, incidents).await,
        "help" => help(ctx, &command, config).await,
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
        let mut incident = Incident::new(
            "command_failure",
            config.guild_id,
            "command_failed",
            format!("/{} failed: {error_message}", command.data.name),
        );
        incident.moderator = Some((&command.user).into());
        incident.channel_id = Some(command.channel_id.get());
        incident.details.insert(
            "command".into(),
            serde_json::Value::String(command.data.name.clone()),
        );
        if let Some(user_id) = option_user(&command.data.options, "user") {
            incident
                .details
                .insert("target_user_id".into(), user_id.get().into());
        }
        incidents.report(&ctx.http, incident).await;
    }
}

async fn lock(
    ctx: &Context,
    command: &CommandInteraction,
    config: &Config,
    incidents: &IncidentReporter,
) -> Result<()> {
    command.defer_ephemeral(&ctx.http).await?;
    let guild_id = command.guild_id.context("this command is server-only")?;
    let channel_id = option_channel(&command.data.options, "channel").unwrap_or(command.channel_id);
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

    let reason =
        option_string(&command.data.options, "reason").unwrap_or("Channel locked by a moderator");
    let mut incident = Incident::new("channel_lock", config.guild_id, "channel_locked", reason);
    incident.moderator = Some((&command.user).into());
    incident.channel_id = Some(channel_id.get());
    incidents.report(&ctx.http, incident).await;
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
    resources: &QuarantineManager,
    incidents: &IncidentReporter,
) -> Result<()> {
    command.defer_ephemeral(&ctx.http).await?;
    let store = quarantine_store(config, store)?;
    let guild_id = command.guild_id.context("this command is server-only")?;
    let user_id = option_user(&command.data.options, "user").context("missing user option")?;
    if user_id == ctx.cache.current_user().id {
        bail!("Hugh cannot quarantine itself");
    }
    let guild = guild_id.to_partial_guild(&ctx.http).await?;
    if user_id == guild.owner_id {
        bail!("Discord does not allow a bot to quarantine the server owner");
    }
    let resources = resources
        .provision(ctx, config)
        .await?
        .context("quarantine is disabled")?;

    let member = guild_id.member(&ctx.http, user_id).await?;
    let original_roles: Vec<u64> = member.roles.iter().map(|role| role.get()).collect();
    if !store
        .snapshot(user_id.get(), original_roles.clone())
        .await?
    {
        bail!("that member already has a stored quarantine snapshot");
    }

    let quarantine_role = resources.role_id;
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
    let reason = option_string(&command.data.options, "reason")
        .unwrap_or("Member quarantined by a moderator");
    let mut incident = Incident::new("member_banish", config.guild_id, action, reason);
    incident.subject = Some((&member.user).into());
    incident.moderator = Some((&command.user).into());
    incident.channel_id = Some(command.channel_id.get());
    incident.details = BTreeMap::from([
        ("target_user_id".into(), user_id.get().into()),
        ("stored_role_ids".into(), serde_json::json!(original_roles)),
        ("failed_role_ids".into(), serde_json::json!(failed)),
    ]);
    incidents.report(&ctx.http, incident).await;
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
    resources: &QuarantineManager,
    incidents: &IncidentReporter,
) -> Result<()> {
    command.defer_ephemeral(&ctx.http).await?;
    let store = quarantine_store(config, store)?;
    let guild_id = command.guild_id.context("this command is server-only")?;
    let user_id = option_user(&command.data.options, "user").context("missing user option")?;
    let stored = store
        .get(user_id.get())
        .await
        .context("no stored quarantine snapshot exists for that member")?;
    let member = guild_id.member(&ctx.http, user_id).await?;
    let roles = guild_id.roles(&ctx.http).await?;
    let quarantine_role = resources
        .provision(ctx, config)
        .await?
        .context("quarantine is disabled")?
        .role_id;

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
    let reason =
        option_string(&command.data.options, "reason").unwrap_or("Member released from quarantine");
    let mut incident = Incident::new(
        "member_unbanish",
        config.guild_id,
        "member_released",
        reason,
    );
    incident.subject = Some((&member.user).into());
    incident.moderator = Some((&command.user).into());
    incident.channel_id = Some(command.channel_id.get());
    incident.details = BTreeMap::from([
        ("target_user_id".into(), user_id.get().into()),
        ("restored_role_ids".into(), serde_json::json!(restored)),
        ("missing_role_ids".into(), serde_json::json!(missing)),
    ]);
    incidents.report(&ctx.http, incident).await;
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

async fn help(ctx: &Context, command: &CommandInteraction, config: &Config) -> Result<()> {
    let level = |name: &str| config.required_level(name).as_str();
    let mut embed = CreateEmbed::new()
        .title("Hugh help")
        .description("Hugh protects this community from raids, risky new accounts, mention abuse, coordinated spam, unsafe links, malicious invites, and unauthorized role elevation.")
        .field("Public", "`/hugh` — about the project\n`/help` — this guide", false)
        .field(
            "Moderation",
            format!(
                "`/lock [channel] [reason]` — stop regular members sending ({})\n\
                 `/banish user [reason]` — isolate a member and save their roles ({})\n\
                 `/unbanish user [reason]` — restore their roles ({})",
                level("lock"),
                level("banish"),
                level("unbanish")
            ),
            false,
        );
    if config.rotector.enabled {
        embed = embed.field(
            "Account checks",
            format!(
                "`/rotector discord [user] [id]` — safety findings for a Discord account\n\
                 `/rotector roblox id` — findings for a Roblox account\n\
                 `/rotector links id` — Discord accounts and alts linked to a Roblox account\n\
                 `/rotector group id` — findings for a Roblox group\n\
                 `/rotector sources` — which detection sources are enabled\n\
                 Required level: {}",
                level("rotector")
            ),
            false,
        );
    }
    embed = embed
        .field("Staff levels", "Access is set per command in Hugh's configuration using the `everyone`, `helper`, `moderator`, `admin`, and `owner` levels. A higher level always satisfies a lower one.", false)
        .field("Case logs", "Every enforcement action receives a durable case number with the user, moderator, reason, outcome, channel, and relevant evidence.", false)
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
        .description("I'm this community's open-source security bot. I watch for raids, risky new accounts, abusive mentions, coordinated spam, unsafe links, and unauthorized role elevation.")
        .field("Help build Hugh", format!("Contributions are welcome: {REPOSITORY_URL}"), false)
        .colour(0x0058_65F2)
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

/// The tier a command needs, and the tier the invoking member actually holds.
#[derive(Debug, Clone, Copy)]
pub struct Denied {
    required: AccessLevel,
    held: AccessLevel,
}

fn access_check(command: &CommandInteraction, config: &Config) -> Result<(), Denied> {
    let required = config.required_level(&command.data.name);
    if required == AccessLevel::Everyone {
        return Ok(());
    }
    let member = command.member.as_ref();
    let roles: Vec<u64> = member.map_or_else(Vec::new, |member| {
        member.roles.iter().map(|role| role.get()).collect()
    });
    let permissions = member
        .and_then(|member| member.permissions)
        .unwrap_or_default();
    let powers = DiscordPowers {
        administrator: permissions.administrator(),
        moderator: permissions.intersects(moderation_permissions()),
    };
    let held = config.access_level(command.user.id.get(), &roles, powers);
    if held >= required {
        Ok(())
    } else {
        Err(Denied { required, held })
    }
}

/// Refuse the command, tell the member which tier it needs, and keep a case log
/// so repeated attempts at staff tooling are visible.
async fn deny(
    ctx: &Context,
    command: &CommandInteraction,
    config: &Config,
    incidents: &IncidentReporter,
    name: &str,
    denied: Denied,
) {
    let message = format!(
        "`/{name}` needs the **{}** staff level and you have **{}**. An administrator can change \
         that under `[permissions.commands]` in Hugh's configuration.",
        denied.required.as_str(),
        denied.held.as_str()
    );
    let _ = respond_text(ctx, command, message, true).await;
    let mut incident = Incident::new(
        "permission_denied",
        config.guild_id,
        "command_blocked",
        format!("/{name} needs the {} staff level", denied.required.as_str()),
    );
    incident.moderator = Some((&command.user).into());
    incident.channel_id = Some(command.channel_id.get());
    incident
        .details
        .insert("command".into(), name.to_owned().into());
    incident
        .details
        .insert("required_level".into(), denied.required.as_str().into());
    incident
        .details
        .insert("held_level".into(), denied.held.as_str().into());
    incidents.report(&ctx.http, incident).await;
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

/// The invoked subcommand and the options belonging to it.
pub fn subcommand(command: &CommandInteraction) -> Option<(&str, &[CommandDataOption])> {
    command
        .data
        .options
        .iter()
        .find_map(|option| match &option.value {
            CommandDataOptionValue::SubCommand(options) => {
                Some((option.name.as_str(), options.as_slice()))
            }
            _ => None,
        })
}

pub fn option_user(options: &[CommandDataOption], name: &str) -> Option<UserId> {
    options.iter().find_map(|option| match option.value {
        CommandDataOptionValue::User(id) if option.name == name => Some(id),
        _ => None,
    })
}

fn option_channel(options: &[CommandDataOption], name: &str) -> Option<ChannelId> {
    options.iter().find_map(|option| match option.value {
        CommandDataOptionValue::Channel(id) if option.name == name => Some(id),
        _ => None,
    })
}

pub fn option_integer(options: &[CommandDataOption], name: &str) -> Option<i64> {
    options.iter().find_map(|option| match option.value {
        CommandDataOptionValue::Integer(value) if option.name == name => Some(value),
        _ => None,
    })
}

pub fn option_string<'a>(options: &'a [CommandDataOption], name: &str) -> Option<&'a str> {
    options.iter().find_map(|option| match &option.value {
        CommandDataOptionValue::String(value) if option.name == name => {
            let value = value.trim();
            (!value.is_empty()).then_some(value)
        }
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

    fn config(extra: &str) -> Config {
        let base = format!("guild_id = \"1\"\n\n[role_guard]\nenabled = false\n\n{extra}");
        let config: Config = toml::from_str(&base).unwrap();
        config.validate().unwrap();
        config
    }

    /// Without configured tiers, Discord keeps hiding the staff commands.
    #[test]
    fn discord_gating_stays_until_tiers_are_configured() {
        let plain = config("");
        let command = |config: &Config, name: &str| {
            serde_json::to_value(gated(CreateCommand::new(name), name, config)).unwrap()
                ["default_member_permissions"]
                .clone()
        };

        assert!(!command(&plain, "banish").is_null());
        assert!(command(&plain, "help").is_null());

        // A configured staff role may hold no Discord permissions at all, so
        // Hugh takes the gate over for the commands that role can reach.
        let staffed = config("[permissions]\nmoderator_role_ids = [\"50\"]");
        assert!(command(&staffed, "banish").is_null());
        assert!(command(&staffed, "lock").is_null());

        // A command raised above every configured tier keeps Discord's gate.
        let raised = config(
            "[permissions]\nmoderator_role_ids = [\"50\"]\n\n[permissions.commands]\nbanish = \"owner\"",
        );
        assert!(!command(&raised, "banish").is_null());
        assert!(command(&raised, "lock").is_null());
    }
}
