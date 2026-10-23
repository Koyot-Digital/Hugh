use std::{collections::BTreeMap, sync::Arc, time::Instant};

use chrono::{DateTime, Timelike, Utc};
use serenity::{
    all::{
        ChannelId, Context, EventHandler, GuildChannel, GuildId, Interaction, Member, Message,
        MessageId, MessageUpdateEvent, Ready, RoleId, Timestamp,
    },
    async_trait,
    builder::{CreateActionRow, CreateMessage},
};
use tracing::{error, info};

use crate::{
    archive::{Archive, ArchivedMessage, snowflake_millis},
    commands,
    config::{Config, YoungAccountAction},
    detection::{mention_counts, unauthorized_protected_roles},
    features::{self, Features, MessageShape},
    incident::{Incident, IncidentReporter},
    message_log,
    mind::{self, MindClient, Subject},
    moderation::{ModerationStore, NewPunishment, Punishment},
    quarantine::{QuarantineManager, QuarantineStore},
    rotector::RotectorClient,
    state::{Escalation, MessageDecision, MessageInput, SecurityState},
    welcome,
};

pub struct Handler {
    config: Arc<Config>,
    state: Arc<SecurityState>,
    incidents: IncidentReporter,
    quarantine: Option<QuarantineStore>,
    quarantine_resources: QuarantineManager,
    rotector: Option<Arc<RotectorClient>>,
    moderation: ModerationStore,
    archive: Option<Archive>,
    mind: Option<Arc<MindClient>>,
}

/// What filing a message in the archive produced.
struct Filed {
    features: [u8; features::COUNT],
    /// For an edit that changed the text, the text it replaced.
    previous: Option<String>,
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
        moderation: ModerationStore,
    ) -> Self {
        Self {
            config,
            state,
            incidents,
            quarantine,
            quarantine_resources,
            rotector,
            moderation,
            archive: None,
            mind: None,
        }
    }

    /// Files every message in the archive from here on.
    #[must_use]
    pub fn with_archive(mut self, archive: Option<Archive>) -> Self {
        self.archive = archive;
        self
    }

    /// Has Hugh Mind score every new message once the rules are done with it.
    #[must_use]
    pub fn with_mind(mut self, mind: Option<Arc<MindClient>>) -> Self {
        self.mind = mind;
        self
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

        // Everybody is archived, trusted staff included: the archive is the
        // evidence, and evidence that skips people is not worth much.
        let filed = self.archive_message(&message, is_edit).await;
        if let Some(previous) = filed.as_ref().and_then(|filed| filed.previous.as_deref()) {
            message_log::edited(&ctx.http, &self.config, &message, previous).await;
        }
        // A message is scored once, when it is sent; edits are the rules' job.
        let shape = filed.filter(|_| !is_edit).map(|filed| filed.features);

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
        // A member's own record decides how tightly the limits are drawn
        // around them, so the standing lookup comes before the inspection.
        let strictness = self
            .moderation
            .strictness(message.author.id.get(), &self.config)
            .await;
        let account_age_seconds = Utc::now()
            .timestamp()
            .saturating_sub(message.author.created_at().unix_timestamp())
            .max(0);
        let input = MessageInput {
            user_id: message.author.id.get(),
            channel_id: message.channel_id.get(),
            content: &message.content,
            total_mentions,
            everyone_mentions,
            mentioned_roles: &mentioned_roles,
            attachment_count: message.attachments.len(),
            count_towards_rate_limits: !is_edit,
            account_age_seconds,
            strictness,
        };
        let decision = self
            .state
            .inspect_message(&input, &self.config, Instant::now());
        self.consider(ctx, &message, shape, decision.should_delete());
        if !decision.should_delete() {
            if !is_edit && mentions_hugh {
                self.send_about(ctx, message.channel_id).await;
            }
            return;
        }

        self.enforce_message(ctx, guild_id, message, is_edit, total_mentions, decision)
            .await;
    }

    /// Hands a message to Hugh Mind on a task of its own, so the scorer's
    /// latency never reaches the gateway. Trusted members never get here.
    fn consider(
        &self,
        ctx: &Context,
        message: &Message,
        shape: Option<[u8; features::COUNT]>,
        flagged: bool,
    ) {
        let (Some(client), Some(archive), Some(features)) = (&self.mind, &self.archive, shape)
        else {
            return;
        };
        if message.author.bot || message.webhook_id.is_some() {
            return;
        }
        let subject = Subject {
            id: message.id.get(),
            channel_id: message.channel_id.get(),
            reply_to: message
                .message_reference
                .as_ref()
                .and_then(|reference| reference.message_id)
                .map(MessageId::get),
            created_at: snowflake_millis(message.id.get()),
            content: message.content.clone(),
            author: (&message.author).into(),
            features,
            flagged,
        };
        let http = Arc::clone(&ctx.http);
        let config = Arc::clone(&self.config);
        let client = Arc::clone(client);
        let archive = archive.clone();
        tokio::spawn(async move {
            mind::consider(&http, &config, &client, &archive, subject).await;
        });
    }

    /// Files a message, or an edit to one, in the archive.
    async fn archive_message(&self, message: &Message, is_edit: bool) -> Option<Filed> {
        let archive = self.archive.as_ref()?;
        let automated = message.author.bot || message.webhook_id.is_some();
        if automated && !self.config.archive.include_bots {
            return None;
        }
        let created_at = snowflake_millis(message.id.get());
        // An edit says nothing about how quickly somebody is talking.
        let gap_seconds = if is_edit {
            None
        } else {
            archive.gap(message.author.id.get(), created_at)
        };
        let features = Features::extract(&MessageShape {
            content: &message.content,
            mentions: message.mentions.len()
                + message.mention_roles.len()
                + usize::from(message.mention_everyone),
            attachments: message.attachments.len(),
            is_reply: message.message_reference.is_some(),
            is_edit,
            hour_utc: DateTime::<Utc>::from_timestamp_millis(created_at).map_or(0, |at| at.hour()),
            gap_seconds,
        });
        let entry = ArchivedMessage {
            id: message.id.get(),
            channel_id: message.channel_id.get(),
            author_id: message.author.id.get(),
            reply_to: message
                .message_reference
                .as_ref()
                .and_then(|reference| reference.message_id)
                .map(MessageId::get),
            created_at,
            content: message.content.clone(),
            attachments: message
                .attachments
                .iter()
                .map(|attachment| attachment.filename.clone())
                .collect(),
            features: features.quantize(),
        };
        let features = entry.features;
        let previous = if is_edit {
            archive.edit(entry, Utc::now().timestamp_millis()).await
        } else {
            archive.record(entry);
            None
        };
        Some(Filed { features, previous })
    }

    /// Marks deleted messages in the archive and posts what they said.
    async fn handle_deletion(&self, ctx: &Context, guild_id: Option<GuildId>, ids: Vec<u64>) {
        if guild_id.is_none_or(|guild| guild.get() != self.config.guild_id) {
            return;
        }
        let Some(archive) = &self.archive else {
            return;
        };
        let removed = archive.delete(ids, Utc::now().timestamp_millis()).await;
        message_log::deleted(&ctx.http, &self.config, &removed).await;
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
        // Flagged before the delete goes out, so by the time Discord reports
        // the deletion the archive already knows it was Hugh's.
        self.flag(message.id.get(), None);
        let delete_result = message.delete(&ctx.http).await;
        if let Err(error) = &delete_result {
            error!(
                ?error,
                message_id = message.id.get(),
                "failed to delete unsafe message"
            );
        }

        let automated_sender = message.author.bot || message.webhook_id.is_some();
        // An escalation to kick or ban supersedes the timeout: there is no
        // point timing out somebody who is about to be removed.
        let escalation = decision
            .escalation
            .filter(|_| !automated_sender)
            .filter(|step| *step != Escalation::Timeout);
        let timeout_result =
            if decision.timeout_seconds > 0 && !automated_sender && escalation.is_none() {
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
        let escalation_result = match escalation {
            Some(step) => Some((
                step,
                self.escalate(ctx, guild_id, &message, step, &reasons).await,
            )),
            None => None,
        };

        let action = match escalation_result {
            Some((Escalation::Kick, true)) => "message_deleted_and_member_kicked",
            Some((Escalation::Ban, true)) => "message_deleted_and_member_banned",
            Some((_, false)) => "escalation_failed",
            Some((Escalation::Timeout, true)) | None => {
                match (delete_result.is_ok(), timeout_result) {
                    (true, Some(true)) => "message_deleted_and_member_timed_out",
                    (true, Some(false)) => "message_deleted_timeout_failed",
                    (false, Some(true)) => "member_timed_out_delete_failed",
                    (false, Some(false)) => "enforcement_failed",
                    (true, None) => "message_deleted",
                    (false, None) => "delete_failed",
                }
            }
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
        let buttons = self.verdict_buttons(message.id.get(), automated_sender);
        let case_id = self
            .incidents
            .report_card(&ctx.http, incident, Vec::new(), buttons)
            .await;
        self.flag(message.id.get(), Some(case_id));

        if !automated_sender {
            self.record_automod_case(
                case_id,
                message.author.id.get(),
                &decision,
                escalation_result,
                timeout_result,
            )
            .await;
        }
    }

    /// Marks a message Hugh's automod removed as evidence in the archive.
    fn flag(&self, message_id: u64, case_id: Option<u64>) {
        if let Some(archive) = &self.archive {
            archive.flag(message_id, case_id);
        }
    }

    /// ✅ and ❌ for an automod case card. Staff verdicts on the rules' own
    /// calls are labels too: a false positive pressed here is exactly what
    /// teaches Hugh Mind to forgive somebody's normal way of writing.
    fn verdict_buttons(&self, message_id: u64, automated_sender: bool) -> Vec<CreateActionRow> {
        if self.mind.is_none() || !self.config.mind.feedback_buttons || automated_sender {
            return Vec::new();
        }
        vec![mind::feedback_buttons(
            message_id,
            "Right call",
            "False positive",
        )]
    }

    /// Writes an automod hit onto the member's record, so repeated enforcement
    /// shows up in `/modlogs` and keeps costing them standing.
    async fn record_automod_case(
        &self,
        case_id: u64,
        user_id: u64,
        decision: &MessageDecision,
        escalation: Option<(Escalation, bool)>,
        timeout_applied: Option<bool>,
    ) {
        if decision.weight <= 0.0 {
            return;
        }
        let escalated_ban_seconds = self.config.escalation.ban_duration_seconds;
        let kind = match escalation {
            Some((Escalation::Kick, true)) => Punishment::Kick,
            Some((Escalation::Ban, true)) if escalated_ban_seconds > 0 => Punishment::TemporaryBan,
            Some((Escalation::Ban, true)) => Punishment::Ban,
            _ if timeout_applied == Some(true) => Punishment::Timeout,
            _ => Punishment::Automod,
        };
        let duration_seconds = match kind {
            Punishment::Timeout => Some(decision.timeout_seconds),
            Punishment::TemporaryBan => Some(escalated_ban_seconds),
            _ => None,
        };
        let weight = decision
            .weight
            .max(kind.weight(&self.config.standing.punishments));
        if let Err(error) = self
            .moderation
            .record(NewPunishment {
                case_id,
                user_id,
                kind,
                reason: decision
                    .violations
                    .iter()
                    .map(|violation| violation.label())
                    .collect::<Vec<_>>()
                    .join(", "),
                moderator_id: None,
                duration_seconds,
                weight,
            })
            .await
        {
            error!(?error, case_id, "failed to record the automod case");
        }
    }

    /// Carries out an escalated kick or ban. Returns whether Discord accepted
    /// it, which decides the outcome written on the case card.
    async fn escalate(
        &self,
        ctx: &Context,
        guild_id: GuildId,
        message: &Message,
        step: Escalation,
        reasons: &str,
    ) -> bool {
        let reason = format!("Hugh automod escalation: {reasons}");
        let result = match step {
            Escalation::Timeout => return true,
            Escalation::Kick => {
                guild_id
                    .kick_with_reason(&ctx.http, message.author.id, &reason)
                    .await
            }
            Escalation::Ban => {
                // Escalated bans delete nothing by default: Hugh has already
                // removed the message that earned the ban.
                guild_id
                    .ban_with_reason(
                        &ctx.http,
                        message.author.id,
                        self.config.moderation.ban_delete_message_days.min(7),
                        &reason,
                    )
                    .await
            }
        };
        match result {
            Ok(()) => {
                info!(
                    user_id = message.author.id.get(),
                    step = step.as_str(),
                    "automod escalated"
                );
                true
            }
            Err(error) => {
                error!(
                    ?error,
                    user_id = message.author.id.get(),
                    step = step.as_str(),
                    "automod escalation failed"
                );
                false
            }
        }
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

        let removed = self.screen_join(ctx, &mut member, decision).await;
        // A member Hugh just removed is not welcomed, and neither is anybody
        // arriving in the middle of a raid.
        if !removed {
            welcome::greet(ctx, &self.config, &member, decision.raid_active).await;
        }
    }

    /// Applies the account-age policy to a new member. Returns whether they
    /// were removed from the server.
    async fn screen_join(
        &self,
        ctx: &Context,
        member: &mut Member,
        decision: crate::state::JoinDecision,
    ) -> bool {
        if self.config.trusted_user_ids.contains(&member.user.id.get()) {
            return false;
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
            return false;
        };

        let (action, result) = match configured_action {
            YoungAccountAction::Log => ("logged", Ok(())),
            YoungAccountAction::Timeout => {
                let result = timeout_existing_member(ctx, member, timeout_seconds).await;
                ("timed_out", result)
            }
            YoungAccountAction::Kick => {
                let result = member
                    .kick_with_reason(&ctx.http, "Hugh: account age protection")
                    .await;
                ("kicked", result)
            }
        };

        let enforced = result.is_ok();
        let final_action = if enforced {
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
        enforced && matches!(configured_action, YoungAccountAction::Kick)
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

    async fn message_delete(
        &self,
        ctx: Context,
        _channel_id: ChannelId,
        deleted_message_id: MessageId,
        guild_id: Option<GuildId>,
    ) {
        self.handle_deletion(&ctx, guild_id, vec![deleted_message_id.get()])
            .await;
    }

    async fn message_delete_bulk(
        &self,
        ctx: Context,
        _channel_id: ChannelId,
        multiple_deleted_messages_ids: Vec<MessageId>,
        guild_id: Option<GuildId>,
    ) {
        let ids = multiple_deleted_messages_ids
            .into_iter()
            .map(MessageId::get)
            .collect();
        self.handle_deletion(&ctx, guild_id, ids).await;
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
            &commands::Services {
                config: &self.config,
                quarantine: self.quarantine.as_ref(),
                quarantine_resources: &self.quarantine_resources,
                incidents: &self.incidents,
                rotector: self.rotector.as_deref(),
                moderation: &self.moderation,
                archive: self.archive.as_ref(),
            },
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
        ("hop_channels", decision.hop_channels),
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
    if let Some(phrase) = &decision.blocked_phrase {
        details.insert("blocked_phrase".into(), phrase.clone().into());
    }
    if decision.weight > 0.0 {
        details.insert("weight".into(), decision.weight.into());
        details.insert("recent_weight".into(), decision.recent_weight.into());
    }
    if let Some(step) = decision.escalation {
        details.insert("escalated".into(), step.as_str().into());
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
