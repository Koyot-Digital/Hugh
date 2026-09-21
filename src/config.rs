use std::{collections::HashSet, path::Path};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tokio::fs;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(deserialize_with = "snowflake")]
    pub guild_id: u64,
    #[serde(default = "default_incident_path")]
    pub incident_log_path: String,
    #[serde(default, deserialize_with = "snowflakes")]
    pub trusted_user_ids: HashSet<u64>,
    #[serde(default, deserialize_with = "snowflakes")]
    pub trusted_role_ids: HashSet<u64>,
    #[serde(default)]
    pub mentions: MentionConfig,
    #[serde(default)]
    pub spam: SpamConfig,
    #[serde(default)]
    pub invites: InviteConfig,
    #[serde(default)]
    pub raids: RaidConfig,
    #[serde(default)]
    pub role_guard: RoleGuardConfig,
    #[serde(default)]
    pub quarantine: QuarantineConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MentionConfig {
    pub enabled: bool,
    pub max_total_mentions: usize,
    pub max_everyone_mentions: usize,
    #[serde(deserialize_with = "snowflakes")]
    pub protected_role_ids: HashSet<u64>,
    pub protected_role_max_pings: usize,
    pub protected_role_window_seconds: u64,
    pub timeout_seconds: u64,
}

impl Default for MentionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_total_mentions: 5,
            max_everyone_mentions: 0,
            protected_role_ids: HashSet::new(),
            protected_role_max_pings: 2,
            protected_role_window_seconds: 20,
            timeout_seconds: 600,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SpamConfig {
    pub enabled: bool,
    pub max_messages: usize,
    pub window_seconds: u64,
    pub max_duplicates: usize,
    pub duplicate_window_seconds: u64,
    pub timeout_seconds: u64,
}

impl Default for SpamConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_messages: 7,
            window_seconds: 8,
            max_duplicates: 3,
            duplicate_window_seconds: 30,
            timeout_seconds: 300,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InviteConfig {
    pub enabled: bool,
    pub allowed_codes: HashSet<String>,
    pub timeout_seconds: u64,
}

impl Default for InviteConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allowed_codes: HashSet::new(),
            timeout_seconds: 300,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum YoungAccountAction {
    Log,
    Timeout,
    Kick,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RaidConfig {
    pub enabled: bool,
    pub join_threshold: usize,
    pub join_window_seconds: u64,
    pub raid_mode_seconds: u64,
    pub minimum_account_age_seconds: i64,
    pub young_account_action: YoungAccountAction,
    pub timeout_seconds: u64,
}

impl Default for RaidConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            join_threshold: 10,
            join_window_seconds: 15,
            raid_mode_seconds: 600,
            minimum_account_age_seconds: 86_400,
            young_account_action: YoungAccountAction::Log,
            timeout_seconds: 3600,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RoleGuardConfig {
    pub enabled: bool,
    #[serde(deserialize_with = "snowflakes")]
    pub protected_role_ids: HashSet<u64>,
    #[serde(deserialize_with = "snowflakes")]
    pub authorized_member_ids: HashSet<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QuarantineConfig {
    pub enabled: bool,
    #[serde(deserialize_with = "optional_snowflake")]
    pub channel_id: Option<u64>,
    #[serde(deserialize_with = "optional_snowflake")]
    pub role_id: Option<u64>,
    pub store_path: String,
}

impl Default for QuarantineConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            channel_id: None,
            role_id: None,
            store_path: "data/quarantine.jsonl".to_owned(),
        }
    }
}

impl Config {
    pub async fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .await
            .with_context(|| format!("failed to read {}", path.display()))?;
        let config: Self = toml::from_str(&raw)
            .with_context(|| format!("invalid configuration in {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.guild_id == 0 {
            bail!("guild_id must be a non-zero Discord ID");
        }
        if self.incident_log_path.trim().is_empty() {
            bail!("incident_log_path must not be empty");
        }
        positive(
            "mentions.protected_role_max_pings",
            &self.mentions.protected_role_max_pings,
        )?;
        positive(
            "mentions.protected_role_window_seconds",
            &self.mentions.protected_role_window_seconds,
        )?;
        positive("mentions.timeout_seconds", &self.mentions.timeout_seconds)?;
        positive("spam.max_messages", &self.spam.max_messages)?;
        positive("spam.window_seconds", &self.spam.window_seconds)?;
        positive("spam.max_duplicates", &self.spam.max_duplicates)?;
        positive(
            "spam.duplicate_window_seconds",
            &self.spam.duplicate_window_seconds,
        )?;
        positive("spam.timeout_seconds", &self.spam.timeout_seconds)?;
        positive("raids.join_threshold", &self.raids.join_threshold)?;
        positive("raids.join_window_seconds", &self.raids.join_window_seconds)?;
        positive("raids.raid_mode_seconds", &self.raids.raid_mode_seconds)?;
        positive("raids.timeout_seconds", &self.raids.timeout_seconds)?;
        if self.raids.minimum_account_age_seconds < 0 {
            bail!("raids.minimum_account_age_seconds cannot be negative");
        }
        if self.role_guard.enabled && self.role_guard.protected_role_ids.is_empty() {
            bail!("role_guard is enabled but protected_role_ids is empty");
        }
        if self.mentions.enabled && self.mentions.max_total_mentions == 0 {
            bail!("mentions.max_total_mentions must be positive");
        }
        if self.quarantine.enabled {
            if self.quarantine.channel_id.is_none() || self.quarantine.role_id.is_none() {
                bail!("quarantine.channel_id and quarantine.role_id are required when enabled");
            }
            if self.quarantine.store_path.trim().is_empty() {
                bail!("quarantine.store_path must not be empty");
            }
            if self.quarantine.role_id.is_some_and(|role| {
                self.trusted_role_ids.contains(&role)
                    || self.role_guard.protected_role_ids.contains(&role)
            }) {
                bail!("the quarantine role cannot be trusted or protected");
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn is_trusted(&self, user_id: u64, roles: impl IntoIterator<Item = u64>) -> bool {
        self.trusted_user_ids.contains(&user_id)
            || roles
                .into_iter()
                .any(|role| self.trusted_role_ids.contains(&role))
    }
}

fn positive<T>(name: &str, value: &T) -> Result<()>
where
    T: PartialEq + From<u8>,
{
    if *value == T::from(0) {
        bail!("{name} must be positive");
    }
    Ok(())
}

fn default_incident_path() -> String {
    "data/incidents.jsonl".to_owned()
}

fn snowflake<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    value.parse().map_err(serde::de::Error::custom)
}

fn snowflakes<'de, D>(deserializer: D) -> Result<HashSet<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let values = Vec::<String>::deserialize(deserializer)?;
    values
        .into_iter()
        .map(|value| value.parse().map_err(serde::de::Error::custom))
        .collect()
}

fn optional_snowflake<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)?
        .map(|value| value.parse().map_err(serde::de::Error::custom))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_keys() {
        let result = toml::from_str::<Config>(
            r#"guild_id = "1"
surprise = true"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn trusted_role_grants_bypass() {
        let mut config: Config = toml::from_str(
            r#"guild_id = "1"
trusted_role_ids = ["99"]"#,
        )
        .unwrap();
        config.role_guard.enabled = false;
        assert!(config.is_trusted(3, [12, 99]));
        assert!(!config.is_trusted(3, [12]));
    }
}
