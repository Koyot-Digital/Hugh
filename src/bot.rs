use std::{collections::BTreeMap, sync::Arc, time::Instant};

use chrono::Utc;
use serenity::{
    all::{
        Context, EventHandler, GuildChannel, GuildId, Interaction, Member, Message, Ready, RoleId,
        Timestamp,
    },
    async_trait,
    builder::CreateMessage,
};
use tracing::{error, info};

use crate::{
    commands,
    config::{Config, YoungAccountAction},
    detection::unauthorized_protected_roles,
    incident::{Incident, IncidentReporter},
    quarantine::{QuarantineManager, QuarantineStore},
    state::{MessageInput, SecurityState},
};

pub struct Handler {
    config: Arc<Config>,
    state: Arc<SecurityState>,
    incidents: IncidentReporter,
    quarantine: Option<QuarantineStore>,
    quarantine_resources: QuarantineManager,
}

impl Handler {
    #[must_use]
    pub const fn new(
        config: Arc<Config>,
        state: Arc<SecurityState>,
        incidents: IncidentReporter,
        quarantine: Option<QuarantineStore>,
        quarantine_resources: QuarantineManager,
    ) -> Self {
        Self {
            config,
            state,
            incidents,
            quarantine,
            quarantine_resources,
        }
    }

    async fn handle_message(&self, ctx: &Context, message: Message) {
        let Some(guild_id) = message.guild_id else {
            return;
        };
        if guild_id.get() != self.config.guild_id
            || message.author.id == ctx.cache.current_user().id
        {
            return;
        }

        let roles: Vec<u64> = message.member.as_ref().map_or_else(Vec::new, |member| {
            member.roles.iter().map(|id| id.get()).collect()
        });
        let mentions_hugh = message
            .mentions
            .iter()
            .any(|user| user.id == ctx.cache.current_user().id);
        if self.config.is_trusted(message.author.id.get(), roles) {
            if mentions_hugh {
                self.send_about(ctx, message.channel_id).await;
            }
            return;
        }

        let everyone_mentions = usize::from(message.mention_everyone);
        let explicit_mentions = message.mentions.len() + message.mention_roles.len();
        let mentioned_roles: Vec<u64> = message.mention_roles.iter().map(|id| id.get()).collect();
        let input = MessageInput {
            user_id: message.author.id.get(),
            content: &message.content,
            total_mentions: explicit_mentions + everyone_mentions,
            everyone_mentions,
            mentioned_roles: &mentioned_roles,
        };
        let decision = self
            .state
            .inspect_message(&input, &self.config, Instant::now());
        if !decision.should_delete() {
            if mentions_hugh {
                self.send_about(ctx, message.channel_id).await;
            }
            return;
        }

        let reasons = decision
            .violations
            .iter()
            .map(|violation| violation.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let delete_result = message.delete(&ctx.http).await;
        if let Err(error) = &delete_result {
            error!(
                ?error,
                message_id = message.id.get(),
                "failed to delete unsafe message"
            );
        }

        let mut action = if delete_result.is_ok() {
            "message_deleted"
        } else {
            "delete_failed"
        };
        let automated_sender = message.author.bot || message.webhook_id.is_some();
        if decision.timeout_seconds > 0 && !automated_sender {
            match timeout_member(ctx, guild_id, message.author.id, decision.timeout_seconds).await {
                Ok(()) => action = "message_deleted_and_member_timed_out",
                Err(error) => {
                    error!(
                        ?error,
                        user_id = message.author.id.get(),
                        "failed to time out member"
                    );
                    action = "message_handled_timeout_failed";
                }
            }
        }

        let mut incident = Incident::new("message_guard", self.config.guild_id, action);
        incident.actor_id = Some(message.author.id.get());
        incident.channel_id = Some(message.channel_id.get());
        incident.details = BTreeMap::from([
            ("message_id".into(), message.id.get().into()),
            ("reasons".into(), reasons.clone().into()),
        ]);
        self.incidents
            .report(
                &ctx.http,
                incident,
                format!(
                    "Message guard: user {} in channel {} - {reasons} ({action})",
                    message.author.id.get(),
                    message.channel_id.get()
                ),
            )
            .await;
    }

    async fn handle_join(&self, ctx: &Context, mut member: Member) {
        if member.guild_id.get() != self.config.guild_id
            || member.user.id == ctx.cache.current_user().id
        {
            return;
        }
        let now_unix = Utc::now().timestamp();
        let decision = self.state.inspect_join(
            &self.config,
            Instant::now(),
            now_unix,
            member.user.created_at().unix_timestamp(),
        );

        if decision.raid_activated {
            let mut incident =
                Incident::new("raid_started", self.config.guild_id, "raid_mode_enabled");
            incident
                .details
                .insert("joins_in_window".into(), decision.join_count.into());
            self.incidents
                .report(
                    &ctx.http,
                    incident,
                    format!(
                        "Raid mode activated: {} joins inside the configured window.",
                        decision.join_count
                    ),
                )
                .await;
        }

        if !decision.raid_active
            || !decision.young_account
            || self.config.trusted_user_ids.contains(&member.user.id.get())
        {
            return;
        }

        let (action, result) = match self.config.raids.young_account_action {
            YoungAccountAction::Log => ("logged", Ok(())),
            YoungAccountAction::Timeout => {
                let result =
                    timeout_existing_member(ctx, &mut member, self.config.raids.timeout_seconds)
                        .await;
                ("timed_out", result)
            }
            YoungAccountAction::Kick => {
                let result = member
                    .kick_with_reason(&ctx.http, "Hugh: young account joined during raid mode")
                    .await;
                ("kicked", result)
            }
        };

        let final_action = if result.is_ok() {
            action
        } else {
            "enforcement_failed"
        };
        if let Err(error) = result {
            error!(
                ?error,
                user_id = member.user.id.get(),
                "raid enforcement failed"
            );
        }
        let mut incident = Incident::new(
            "young_account_during_raid",
            self.config.guild_id,
            final_action,
        );
        incident.actor_id = Some(member.user.id.get());
        incident.details.insert(
            "account_age_seconds".into(),
            decision.account_age_seconds.into(),
        );
        self.incidents
            .report(
                &ctx.http,
                incident,
                format!(
                    "Raid guard: user {} is {} seconds old ({final_action}).",
                    member.user.id.get(),
                    decision.account_age_seconds
                ),
            )
            .await;
    }

    async fn handle_member_update(&self, ctx: &Context, member: Member) {
        if member.guild_id.get() != self.config.guild_id
            || member.user.id == ctx.cache.current_user().id
        {
            return;
        }
        let roles = member.roles.iter().map(|role| role.get());
        let quarantine_role = self
            .quarantine_resources
            .current()
            .await
            .map(|resources| resources.role_id.get())
            .or(self.config.quarantine.role_id);
        let unauthorized =
            unauthorized_protected_roles(member.user.id.get(), roles, &self.config.role_guard);
        let unauthorized: Vec<u64> = unauthorized
            .into_iter()
            .filter(|role| Some(*role) != quarantine_role)
            .collect();
        if unauthorized.is_empty() {
            return;
        }

        let mut removed = Vec::new();
        let mut failed = Vec::new();
        for role_id in unauthorized {
            match member.remove_role(&ctx.http, RoleId::new(role_id)).await {
                Ok(()) => removed.push(role_id),
                Err(error) => {
                    error!(
                        ?error,
                        user_id = member.user.id.get(),
                        role_id,
                        "failed to remove protected role"
                    );
                    failed.push(role_id);
                }
            }
        }
        let action = if failed.is_empty() {
            "roles_removed"
        } else {
            "role_removal_failed"
        };
        let mut incident =
            Incident::new("unauthorized_protected_role", self.config.guild_id, action);
        incident.actor_id = Some(member.user.id.get());
        incident
            .details
            .insert("removed_role_ids".into(), serde_json::json!(removed));
        incident
            .details
            .insert("failed_role_ids".into(), serde_json::json!(failed));
        self.incidents
            .report(
                &ctx.http,
                incident,
                format!(
                    "Role guard: unauthorized protected role(s) found on user {} ({action}).",
                    member.user.id.get()
                ),
            )
            .await;
    }

    async fn send_about(&self, ctx: &Context, channel_id: serenity::all::ChannelId) {
        let message = CreateMessage::new().embed(commands::about_embed());
        if let Err(error) = channel_id.send_message(&ctx.http, message).await {
            error!(?error, "failed to send Hugh introduction");
        }
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        let configured_guild = GuildId::new(self.config.guild_id);
        if !ready
            .guilds
            .iter()
            .any(|guild| guild.id == configured_guild)
        {
            error!(
                guild_id = self.config.guild_id,
                "configured guild is not visible to Hugh"
            );
        }
        info!(user = %ready.user.name, user_id = ready.user.id.get(), "Hugh is connected");
        if let Err(error) = commands::register(&ctx, &self.config).await {
            error!(?error, "failed to register slash commands");
        }
        if let Err(error) = self
            .quarantine_resources
            .provision(&ctx, &self.config)
            .await
        {
            error!(?error, "failed to configure quarantine channel isolation");
            let incident = Incident::new(
                "quarantine_configuration",
                self.config.guild_id,
                "configuration_failed",
            );
            self.incidents
                .report(
                    &ctx.http,
                    incident,
                    "Hugh could not provision quarantine isolation. Check Manage Roles, Manage Channels, role hierarchy, and any pinned resource IDs.",
                )
                .await;
        }
    }

    async fn message(&self, ctx: Context, message: Message) {
        self.handle_message(&ctx, message).await;
    }

    async fn guild_member_addition(&self, ctx: Context, new_member: Member) {
        self.handle_join(&ctx, new_member).await;
    }

    async fn guild_member_update(
        &self,
        ctx: Context,
        _old_if_available: Option<Member>,
        new: Option<Member>,
        event: serenity::all::GuildMemberUpdateEvent,
    ) {
        let member = if let Some(member) = new {
            member
        } else {
            match event.guild_id.member(&ctx.http, event.user.id).await {
                Ok(member) => member,
                Err(error) => {
                    error!(
                        ?error,
                        user_id = event.user.id.get(),
                        "failed to fetch member after uncached update"
                    );
                    return;
                }
            }
        };
        self.handle_member_update(&ctx, member).await;
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        commands::handle(
            &ctx,
            interaction,
            &self.config,
            self.quarantine.as_ref(),
            &self.quarantine_resources,
            &self.incidents,
        )
        .await;
    }

    async fn channel_create(&self, ctx: Context, channel: GuildChannel) {
        if let Err(error) = self
            .quarantine_resources
            .configure_channel(&ctx, &self.config, &channel)
            .await
        {
            error!(
                ?error,
                channel_id = channel.id.get(),
                "failed to isolate new channel from quarantine"
            );
        }
    }
}

async fn timeout_member(
    ctx: &Context,
    guild_id: GuildId,
    user_id: serenity::all::UserId,
    seconds: u64,
) -> serenity::Result<()> {
    let mut member = guild_id.member(&ctx.http, user_id).await?;
    timeout_existing_member(ctx, &mut member, seconds).await
}

async fn timeout_existing_member(
    ctx: &Context,
    member: &mut Member,
    seconds: u64,
) -> serenity::Result<()> {
    // Discord's API rejects timeouts over 28 days. Configuration validation keeps
    // values useful, and this clamp protects operators after a future config reload.
    let seconds = seconds.min(28 * 24 * 60 * 60);
    let until_unix = Utc::now()
        .timestamp()
        .saturating_add(i64::try_from(seconds).unwrap_or(i64::MAX));
    let until = Timestamp::from_unix_timestamp(until_unix)
        .map_err(|_| serenity::Error::Other("invalid timeout timestamp"))?;
    member
        .disable_communication_until_datetime(ctx, until)
        .await
}
