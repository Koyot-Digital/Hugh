use std::{collections::BTreeMap, path::Path, sync::Arc};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serenity::{
    all::{
        ChannelId, ChannelType, Context as DiscordContext, GuildChannel, GuildId,
        PermissionOverwrite, PermissionOverwriteType, Permissions, RoleId,
    },
    builder::{CreateChannel, EditRole},
};
use tokio::{
    fs::{self, File, OpenOptions},
    io::AsyncWriteExt,
    sync::Mutex,
};
use tracing::info;

use crate::config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuarantineResources {
    pub channel_id: ChannelId,
    pub role_id: RoleId,
}

#[derive(Debug, Clone, Default)]
pub struct QuarantineManager {
    resources: Arc<Mutex<Option<QuarantineResources>>>,
}

impl QuarantineManager {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn current(&self) -> Option<QuarantineResources> {
        *self.resources.lock().await
    }

    pub async fn provision(
        &self,
        ctx: &DiscordContext,
        config: &Config,
    ) -> Result<Option<QuarantineResources>> {
        if !config.quarantine.enabled {
            return Ok(None);
        }

        let resources = self.resolve(ctx, config).await?;
        let channels = GuildId::new(config.guild_id).channels(&ctx.http).await?;
        for channel in channels.values() {
            self.configure_channel_with(ctx, config, channel, resources)
                .await?;
        }
        Ok(Some(resources))
    }

    pub async fn configure_channel(
        &self,
        ctx: &DiscordContext,
        config: &Config,
        channel: &GuildChannel,
    ) -> Result<()> {
        if !config.quarantine.enabled || channel.guild_id.get() != config.guild_id {
            return Ok(());
        }
        let resources = self.resolve(ctx, config).await?;
        self.configure_channel_with(ctx, config, channel, resources)
            .await
    }

    async fn resolve(&self, ctx: &DiscordContext, config: &Config) -> Result<QuarantineResources> {
        let mut current = self.resources.lock().await;
        if let Some(resources) = *current {
            return Ok(resources);
        }

        let guild_id = GuildId::new(config.guild_id);
        let roles = guild_id.roles(&ctx.http).await?;
        let role_id = if let Some(role_id) = config.quarantine.role_id {
            let role_id = RoleId::new(role_id);
            roles
                .contains_key(&role_id)
                .then_some(role_id)
                .context("the configured quarantine role does not exist in this server")?
        } else if let Some(role) = roles
            .values()
            .filter(|role| role.name == config.quarantine.role_name)
            .min_by_key(|role| role.id.get())
        {
            role.id
        } else {
            let role = guild_id
                .create_role(
                    &ctx.http,
                    EditRole::new()
                        .name(&config.quarantine.role_name)
                        .permissions(Permissions::empty())
                        .hoist(false)
                        .mentionable(false)
                        .audit_log_reason("Hugh automatic quarantine setup"),
                )
                .await
                .context("failed to create the quarantine role")?;
            info!(role_id = role.id.get(), "created quarantine role");
            role.id
        };

        if config.trusted_role_ids.contains(&role_id.get())
            || config
                .role_guard
                .protected_role_ids
                .contains(&role_id.get())
        {
            anyhow::bail!("the resolved quarantine role cannot be trusted or protected");
        }

        let channels = guild_id.channels(&ctx.http).await?;
        let channel_id = if let Some(channel_id) = config.quarantine.channel_id {
            let channel_id = ChannelId::new(channel_id);
            let channel = channels
                .get(&channel_id)
                .context("the configured quarantine channel does not exist in this server")?;
            if channel.kind != ChannelType::Text {
                anyhow::bail!("the configured quarantine channel must be a text channel");
            }
            channel_id
        } else if let Some(channel) = channels
            .values()
            .filter(|channel| {
                channel.name == config.quarantine.channel_name && channel.kind == ChannelType::Text
            })
            .min_by_key(|channel| channel.id.get())
        {
            channel.id
        } else {
            let channel = guild_id
                .create_channel(
                    &ctx.http,
                    CreateChannel::new(&config.quarantine.channel_name)
                        .kind(ChannelType::Text)
                        .topic("Private holding channel managed by Hugh")
                        .audit_log_reason("Hugh automatic quarantine setup"),
                )
                .await
                .context("failed to create the quarantine channel")?;
            info!(channel_id = channel.id.get(), "created quarantine channel");
            channel.id
        };

        let resources = QuarantineResources {
            channel_id,
            role_id,
        };
        *current = Some(resources);
        drop(current);
        Ok(resources)
    }

    async fn configure_channel_with(
        &self,
        ctx: &DiscordContext,
        config: &Config,
        channel: &GuildChannel,
        resources: QuarantineResources,
    ) -> Result<()> {
        if channel.id == resources.channel_id {
            let bot_user_id = ctx.cache.current_user().id;
            let access = Permissions::VIEW_CHANNEL
                | Permissions::SEND_MESSAGES
                | Permissions::READ_MESSAGE_HISTORY;
            let maintenance_access =
                access | Permissions::MANAGE_CHANNELS | Permissions::MANAGE_ROLES;
            // Grant Hugh explicit maintenance access before hiding the channel
            // from @everyone, otherwise a private parent can lock Hugh out in
            // the middle of provisioning.
            apply_overwrite(
                &ctx.http,
                channel,
                PermissionOverwriteType::Member(bot_user_id),
                maintenance_access,
                Permissions::empty(),
            )
            .await?;
            apply_overwrite(
                &ctx.http,
                channel,
                PermissionOverwriteType::Role(RoleId::new(config.guild_id)),
                Permissions::empty(),
                Permissions::VIEW_CHANNEL,
            )
            .await?;
            apply_overwrite(
                &ctx.http,
                channel,
                PermissionOverwriteType::Role(resources.role_id),
                access,
                Permissions::empty(),
            )
            .await?;
        } else {
            if quarantine_is_already_hidden(
                &channel.permission_overwrites,
                config.guild_id,
                resources.role_id.get(),
            ) {
                return Ok(());
            }
            apply_overwrite(
                &ctx.http,
                channel,
                PermissionOverwriteType::Role(resources.role_id),
                Permissions::empty(),
                Permissions::VIEW_CHANNEL,
            )
            .await?;
        }
        Ok(())
    }
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
    let mut allow = existing.map_or(Permissions::empty(), |entry| entry.allow);
    let mut deny = existing.map_or(Permissions::empty(), |entry| entry.deny);
    allow.remove(add_deny);
    deny.remove(add_allow);
    allow.insert(add_allow);
    deny.insert(add_deny);
    channel
        .create_permission(http, PermissionOverwrite { allow, deny, kind })
        .await?;
    Ok(())
}

fn quarantine_is_already_hidden(
    overwrites: &[PermissionOverwrite],
    guild_id: u64,
    quarantine_role_id: u64,
) -> bool {
    let quarantine = overwrites
        .iter()
        .find(|entry| entry.kind == PermissionOverwriteType::Role(RoleId::new(quarantine_role_id)));
    if quarantine.is_some_and(|entry| entry.allow.view_channel()) {
        return false;
    }
    if quarantine.is_some_and(|entry| entry.deny.view_channel()) {
        return true;
    }

    overwrites
        .iter()
        .find(|entry| entry.kind == PermissionOverwriteType::Role(RoleId::new(guild_id)))
        .is_some_and(|entry| entry.deny.view_channel() && !entry.allow.view_channel())
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum StoreEvent {
    Snapshot {
        user_id: String,
        role_ids: Vec<String>,
    },
    Cleared {
        user_id: String,
    },
}

#[derive(Debug)]
struct StoreInner {
    records: BTreeMap<u64, Vec<u64>>,
    file: File,
}

#[derive(Debug, Clone)]
pub struct QuarantineStore {
    inner: Arc<Mutex<StoreInner>>,
}

impl QuarantineStore {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).await.with_context(|| {
                format!("failed to create quarantine directory {}", parent.display())
            })?;
        }

        let mut records = BTreeMap::new();
        match fs::read_to_string(path).await {
            Ok(contents) => {
                for (index, line) in contents.lines().enumerate() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let event: StoreEvent = serde_json::from_str(line).with_context(|| {
                        format!(
                            "invalid quarantine record at {}:{}",
                            path.display(),
                            index + 1
                        )
                    })?;
                    apply_event(&mut records, event).with_context(|| {
                        format!("invalid quarantine IDs at {}:{}", path.display(), index + 1)
                    })?;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", path.display()));
            }
        }

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
            .with_context(|| format!("failed to open quarantine store {}", path.display()))?;
        Ok(Self {
            inner: Arc::new(Mutex::new(StoreInner { records, file })),
        })
    }

    /// Saves the original roles exactly once. Returns `false` when a snapshot
    /// already exists, preventing a repeated banish from overwriting recovery data.
    pub async fn snapshot(&self, user_id: u64, role_ids: Vec<u64>) -> Result<bool> {
        let mut inner = self.inner.lock().await;
        if inner.records.contains_key(&user_id) {
            return Ok(false);
        }
        let event = StoreEvent::Snapshot {
            user_id: user_id.to_string(),
            role_ids: role_ids.iter().map(u64::to_string).collect(),
        };
        append(&mut inner.file, &event).await?;
        inner.records.insert(user_id, role_ids);
        drop(inner);
        Ok(true)
    }

    pub async fn get(&self, user_id: u64) -> Option<Vec<u64>> {
        self.inner.lock().await.records.get(&user_id).cloned()
    }

    /// Clears a snapshot only after the durable `cleared` event is flushed.
    pub async fn clear(&self, user_id: u64) -> Result<bool> {
        let mut inner = self.inner.lock().await;
        if !inner.records.contains_key(&user_id) {
            return Ok(false);
        }
        let event = StoreEvent::Cleared {
            user_id: user_id.to_string(),
        };
        append(&mut inner.file, &event).await?;
        inner.records.remove(&user_id);
        drop(inner);
        Ok(true)
    }
}

async fn append(file: &mut File, event: &StoreEvent) -> Result<()> {
    let mut line = serde_json::to_vec(event).context("failed to serialize quarantine record")?;
    line.push(b'\n');
    file.write_all(&line)
        .await
        .context("failed to write quarantine record")?;
    file.flush()
        .await
        .context("failed to flush quarantine record")
}

fn apply_event(records: &mut BTreeMap<u64, Vec<u64>>, event: StoreEvent) -> Result<()> {
    match event {
        StoreEvent::Snapshot { user_id, role_ids } => {
            records.insert(
                user_id.parse().context("invalid user ID")?,
                role_ids
                    .into_iter()
                    .map(|role| role.parse().context("invalid role ID"))
                    .collect::<Result<Vec<_>>>()?,
            );
        }
        StoreEvent::Cleared { user_id } => {
            records.remove(&user_id.parse().context("invalid user ID")?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_everyone_overwrite_already_hides_quarantine() {
        let overwrites = [PermissionOverwrite {
            allow: Permissions::empty(),
            deny: Permissions::VIEW_CHANNEL,
            kind: PermissionOverwriteType::Role(RoleId::new(1)),
        }];
        assert!(quarantine_is_already_hidden(&overwrites, 1, 2));
    }

    #[test]
    fn quarantine_allow_must_be_repaired_even_when_everyone_is_denied() {
        let overwrites = [
            PermissionOverwrite {
                allow: Permissions::empty(),
                deny: Permissions::VIEW_CHANNEL,
                kind: PermissionOverwriteType::Role(RoleId::new(1)),
            },
            PermissionOverwrite {
                allow: Permissions::VIEW_CHANNEL,
                deny: Permissions::empty(),
                kind: PermissionOverwriteType::Role(RoleId::new(2)),
            },
        ];
        assert!(!quarantine_is_already_hidden(&overwrites, 1, 2));
    }

    #[tokio::test]
    async fn snapshots_survive_restart_and_are_not_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("quarantine.jsonl");
        let store = QuarantineStore::open(&path).await.unwrap();
        assert!(store.snapshot(7, vec![10, 11]).await.unwrap());
        assert!(!store.snapshot(7, vec![99]).await.unwrap());
        drop(store);

        let reopened = QuarantineStore::open(&path).await.unwrap();
        assert_eq!(reopened.get(7).await, Some(vec![10, 11]));
        assert!(reopened.clear(7).await.unwrap());
        assert_eq!(reopened.get(7).await, None);
    }
}
