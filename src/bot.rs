use std::{collections::BTreeMap, sync::Arc, time::Instant};

use chrono::Utc;
use serenity::{
    all::{
        Context, EventHandler, GuildChannel, GuildId, Interaction, Member, Message,
        MessageUpdateEvent, Ready, RoleId, Timestamp,
    },
    async_trait,
    builder::CreateMessage,
};
use tracing::{error, info};

use crate::{
    commands,
    config::{Config, YoungAccountAction},
    detection::{mention_counts, unauthorized_protected_roles},
    incident::{Incident, IncidentReporter},
    quarantine::{QuarantineManager, QuarantineStore},
    rotector::RotectorClient,
    state::{MessageDecision, MessageInput, SecurityState},
};

pub struct Handler {
    config: Arc<Config>,
    state: Arc<SecurityState>,
    incidents: IncidentReporter,
    quarantine: Option<QuarantineStore>,
    quarantine_resources: QuarantineManager,
    rotector: Option<Arc<RotectorClient>>,
}

impl Handler {
    #[must_use]
    pub const fn new(
        config: Arc<Config>,
        state: Arc<SecurityState>,
        incidents: IncidentReporter,
        quarantine: Option<QuarantineStore>,
        quarantine_resources: QuarantineManager,
        rotector: Option<Arc<RotectorClient>>,
    ) -> Self {
        Self {
            config,
            state,
            incidents,
            quarantine,
            quarantine_resources,
            rotector,
        }
    }

    async fn handle_message(&self, ctx: &Context, message: Message, is_edit: bool) {
        let Some(guild_id) = message.guild_id else {
            return;
        };
        if guild_id.get() != self.config.guild_id
            || message.author.id == ctx.cache.current_user().id
        {
            return;
        }

        let roles: Vec<u64> = if let Some(member) = &message.member {
            member.roles.iter().map(|id| id.get()).collect()
        } else if message.webhook_id.is_none() {
            guild_id
                .member(&ctx.http, message.author.id)
                .await
                .map_or_else(
                    |_| Vec::new(),
                    |member| member.roles.iter().map(|id| id.get()).collect(),
                )
        } else {
            Vec::new()
        };
        let mentions_hugh = message
            .mentions
            .iter()
            .any(|user| user.id == ctx.cache.current_user().id);
        if self.config.is_trusted(message.author.id.get(), roles) {
            if !is_edit && mentions_hugh {
                self.send_about(ctx, message.channel_id).await;
            }
            return;
        }

        let mut parsed_mentions = mention_counts(&message.content, message.mention_everyone);
        let resolved_mentions = message.mentions.len() + message.mention_roles.len();
        let everyone_mentions = parsed_mentions.everyone;
        parsed_mentions.direct = parsed_mentions.direct.max(resolved_mentions);
        let total_mentions = parsed_mentions.total();
        let mentioned_roles: Vec<u64> = message.mention_roles.iter().map(|id| id.get()).collect();
        let input = MessageInput {
            user_id: message.author.id.get(),
            content: &message.content,
            total_mentions,
            everyone_mentions,
            mentioned_roles: &mentioned_roles,
            attachment_count: message.attachments.len(),
            count_towards_rate_limits: !is_edit,
        };
        let decision = self
            .state
            .inspect_message(&input, &self.config, Instant::now());
        if !decision.should_delete() {
            if !is_edit && mentions_hugh {
                self.send_about(ctx, message.channel_id).await;
            }
            return;
        }

        self.enforce_message(ctx, guild_id, message, is_edit, total_mentions, decision)
            .await;
    }

    async fn enforce_message(
        &self,
        ctx: &Context,
        guild_id: GuildId,
        message: Message,
        is_edit: bool,
        total_mentions: usize,
        decision: MessageDecision,
    ) {
        let reason_codes = decision
            .violations
            .iter()
            .map(|violation| violation.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let reasons = decision
            .violations
            .iter()
            .map(|violation| violation.label())
            .collect::<Vec<_>>()
            .join(", ");
        let delete_result = message.delete(&ctx.http).await;
        if let Err(error) = &delete_result {
            error!(
                ?error,
                message_id = message.id.get(),
                "failed to delete unsafe message"
            );
        }

        let automated_sender = message.author.bot || message.webhook_id.is_some();
        let timeout_result = if decision.timeout_seconds > 0 && !automated_sender {
            Some(
                match timeout_member(ctx, guild_id, message.author.id, decision.timeout_seconds)
                    .await
                {
                    Ok(()) => true,
                    Err(error) => {
                        error!(
                            ?error,
                            user_id = message.author.id.get(),
                            "failed to time out member"
                        );
                        false
                    }
                },
            )
        } else {
            None
        };
        let action = match (delete_result.is_ok(), timeout_result) {
            (true, Some(true)) => "message_deleted_and_member_timed_out",
            (true, Some(false)) => "message_deleted_timeout_failed",
            (false, Some(true)) => "member_timed_out_delete_failed",
            (false, Some(false)) => "enforcement_failed",
            (true, None) => "message_deleted",
            (false, None) => "delete_failed",
        };

        let mut incident = Incident::new("message_guard", self.config.guild_id, action, reasons);
        incident.subject = Some((&message.author).into());
        incident.channel_id = Some(message.channel_id.get());
        incident.details = message_evidence(
            &message,
            is_edit,
            total_mentions,
            &decision,
            &reason_codes,
            timeout_result,
            automated_sender,
        );
        self.incidents.report(&ctx.http, incident).await;
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
            let mut incident = Incident::new(
                "raid_started",
                self.config.guild_id,
                "raid_mode_enabled",
                "The server join-rate threshold was reached",
            );
            incident
                .details
                .insert("joins_in_window".into(), decision.join_count.into());
            incident.details.insert(
                "window_seconds".into(),
                self.config.raids.join_window_seconds.into(),
            );
            incident.details.insert(
                "raid_mode_seconds".into(),
                self.config.raids.raid_mode_seconds.into(),
            );
            self.incidents.report(&ctx.http, incident).await;
        }

        if self.config.trusted_user_ids.contains(&member.user.id.get()) {
            return;
        }

        let policy = if decision.raid_active && decision.young_account {
            Some((
                "young_account_during_raid",
                "A young account joined while raid mode was active",
                self.config.raids.young_account_action,
                self.config.raids.timeout_seconds,
            ))
        } else if self.config.new_accounts.enabled
            && decision.account_age_seconds < self.config.new_accounts.minimum_account_age_seconds
        {
            Some((
                "new_account_join",
                "An account younger than the configured minimum joined",
                self.config.new_accounts.action,
                self.config.new_accounts.timeout_seconds,
            ))
        } else {
            None
        };
        let Some((kind, reason, configured_action, timeout_seconds)) = policy else {
            return;
        };

        let (action, result) = match configured_action {
            YoungAccountAction::Log => ("logged", Ok(())),
            YoungAccountAction::Timeout => {
                let result = timeout_existing_member(ctx, &mut member, timeout_seconds).await;
                ("timed_out", result)
            }
            YoungAccountAction::Kick => {
                let result = member
                    .kick_with_reason(&ctx.http, "Hugh: account age protection")
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
        let mut incident = Incident::new(kind, self.config.guild_id, final_action, reason);
        incident.subject = Some((&member.user).into());
        incident.details.insert(
            "account_age_seconds".into(),
            decision.account_age_seconds.into(),
        );
        incident
            .details
            .insert("joins_in_window".into(), decision.join_count.into());
        if matches!(configured_action, YoungAccountAction::Timeout) {
            incident
                .details
                .insert("timeout_seconds".into(), timeout_seconds.into());
        }
        self.incidents.report(&ctx.http, incident).await;
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
        let mut incident = Incident::new(
            "unauthorized_protected_role",
            self.config.guild_id,
            action,
            "Member held one or more protected roles without authorisation",
        );
        incident.subject = Some((&member.user).into());
        incident
            .details
            .insert("removed_role_ids".into(), serde_json::json!(removed));
        incident
            .details
            .insert("failed_role_ids".into(), serde_json::json!(failed));
        self.incidents.report(&ctx.http, incident).await;
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
                "Hugh could not provision quarantine isolation; check permissions, role hierarchy, and pinned resource IDs",
            );
            self.incidents.report(&ctx.http, incident).await;
        }
    }

    async fn message(&self, ctx: Context, message: Message) {
        self.handle_message(&ctx, message, false).await;
    }

    async fn message_update(
        &self,
        ctx: Context,
        _old_if_available: Option<Message>,
        new: Option<Message>,
        event: MessageUpdateEvent,
    ) {
        let message = if let Some(message) = new {
            message
        } else {
            match event.channel_id.message(&ctx.http, event.id).await {
                Ok(message) => message,
                Err(error) => {
                    error!(
                        ?error,
                        message_id = event.id.get(),
                        "failed to fetch edited message for protection scan"
                    );
                    return;
                }
            }
        };
        self.handle_message(&ctx, message, true).await;
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
            self.rotector.as_deref(),
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

fn message_evidence(
    message: &Message,
    is_edit: bool,
    total_mentions: usize,
    decision: &MessageDecision,
    reason_codes: &str,
    timeout_result: Option<bool>,
    automated_sender: bool,
) -> BTreeMap<String, serde_json::Value> {
    let mut details = BTreeMap::from([
        ("message_id".into(), message.id.get().into()),
        (
            "source".into(),
            if is_edit {
                "message_edit"
            } else {
                "message_create"
            }
            .into(),
        ),
        ("reasons".into(), reason_codes.into()),
    ]);
    for (key, count) in [
        ("total_mentions", total_mentions),
        ("attachment_count", message.attachments.len()),
        ("link_count", decision.link_count),
        ("coordinated_users", decision.coordinated_users),
    ] {
        if count > 0 {
            details.insert(key.into(), count.into());
        }
    }
    if timeout_result.is_some() {
        details.insert("timeout_seconds".into(), decision.timeout_seconds.into());
    } else if automated_sender && decision.timeout_seconds > 0 {
        details.insert("timeout_skipped".into(), "automated sender".into());
    }
    if let Some(domain) = &decision.blocked_domain {
        details.insert("blocked_domain".into(), domain.clone().into());
    }
    details
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
