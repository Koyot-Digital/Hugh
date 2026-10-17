use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tokio::fs;
use url::Url;

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
    pub content: ContentGuardConfig,
    #[serde(default)]
    pub raids: RaidConfig,
    #[serde(default)]
    pub new_accounts: NewAccountConfig,
    #[serde(default)]
    pub role_guard: RoleGuardConfig,
    #[serde(default)]
    pub quarantine: QuarantineConfig,
    #[serde(default)]
    pub rotector: RotectorConfig,
    #[serde(default)]
    pub scrape: ScrapeConfig,
    /// Icon overrides for case and lookup cards, keyed by the names in
    /// [`ICONS`]. Values are plain emoji or Discord custom emoji such as
    /// `<:shield_alert:123456789012345678>`.
    #[serde(default)]
    pub icons: BTreeMap<String, String>,
    #[serde(default)]
    pub permissions: PermissionsConfig,
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
    pub max_coordinated_duplicates: usize,
    pub coordinated_window_seconds: u64,
    pub coordinated_min_length: usize,
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
            max_coordinated_duplicates: 4,
            coordinated_window_seconds: 20,
            coordinated_min_length: 12,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContentGuardConfig {
    pub enabled: bool,
    pub max_links: usize,
    pub max_attachments: usize,
    pub max_newlines: usize,
    pub blocked_domains: HashSet<String>,
    pub timeout_seconds: u64,
}

impl Default for ContentGuardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_links: 4,
            max_attachments: 6,
            max_newlines: 12,
            blocked_domains: HashSet::new(),
            timeout_seconds: 600,
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
pub struct NewAccountConfig {
    pub enabled: bool,
    pub minimum_account_age_seconds: i64,
    pub action: YoungAccountAction,
    pub timeout_seconds: u64,
}

impl Default for NewAccountConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            minimum_account_age_seconds: 3_600,
            action: YoungAccountAction::Log,
            timeout_seconds: 3_600,
        }
    }
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
    pub channel_name: String,
    pub role_name: String,
    pub store_path: String,
}

impl Default for QuarantineConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            channel_id: None,
            role_id: None,
            channel_name: "quarantine".to_owned(),
            role_name: "Quarantined".to_owned(),
            store_path: "data/quarantine.jsonl".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RotectorConfig {
    pub enabled: bool,
    pub base_url: String,
    /// Locale used for the provider-composed status and header text.
    pub language: String,
    pub request_timeout_seconds: u64,
    /// Write a case log whenever a staff lookup returns a flagged account.
    pub log_flagged_lookups: bool,
}

impl Default for RotectorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: "https://roscoe.rayward.app".to_owned(),
            language: "en".to_owned(),
            request_timeout_seconds: 10,
            log_flagged_lookups: true,
        }
    }
}

/// Forum exports written by the owner-only `/scrape` command.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScrapeConfig {
    pub enabled: bool,
    /// Directory the JSONL exports are written to.
    pub output_dir: String,
    /// Ceiling on posts per run. A run walks Discord one page at a time, so
    /// this is what keeps a huge forum inside the interaction's lifetime.
    pub max_posts: usize,
    /// Ceiling on replies captured per post, when replies are requested.
    pub max_replies_per_post: usize,
    /// Attach the export to the reply as well as writing it to disk.
    pub attach_results: bool,
}

impl Default for ScrapeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            output_dir: "data/scrapes".to_owned(),
            max_posts: 500,
            max_replies_per_post: 200,
            attach_results: true,
        }
    }
}

/// Staff tiers, ordered from least to most privileged. A member who satisfies a
/// higher tier also satisfies every tier below it.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AccessLevel {
    #[default]
    Everyone,
    Helper,
    Moderator,
    Admin,
    Owner,
}

impl AccessLevel {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Everyone => "everyone",
            Self::Helper => "helper",
            Self::Moderator => "moderator",
            Self::Admin => "admin",
            Self::Owner => "owner",
        }
    }
}

/// Discord-granted power held by the invoking member, used only when
/// `permissions.allow_discord_permissions` is enabled.
#[derive(Debug, Clone, Copy, Default)]
pub struct DiscordPowers {
    pub administrator: bool,
    pub moderator: bool,
}

/// Every icon slot Hugh renders, with the plain-emoji fallback used when the
/// operator has not uploaded a matching custom emoji. Keys are stable; the
/// Lucide icon each one is modelled on is noted in `docs/configuration.md`.
pub const ICONS: [(&str, &str); 28] = [
    ("case", "🗂️"),
    ("pin", "📌"),
    ("reason", "📝"),
    ("details", "🔎"),
    ("member", "👤"),
    ("moderator", "🛡️"),
    ("channel", "#️⃣"),
    ("deleted", "🗑️"),
    ("timeout", "⏳"),
    ("kicked", "🚪"),
    ("raid", "🚨"),
    ("join", "🧍"),
    ("role", "🎭"),
    ("quarantine", "🔒"),
    ("release", "🔓"),
    ("lock", "🔐"),
    ("denied", "⛔"),
    ("failed", "❗"),
    ("flagged", "⚠️"),
    ("safe", "✅"),
    ("review", "🕓"),
    ("unknown", "❔"),
    ("roblox", "🎮"),
    ("discord", "💬"),
    ("group", "👥"),
    ("source", "📡"),
    ("forum", "🧵"),
    ("image", "🖼️"),
];

/// Commands Hugh knows about, with the tier each one needs by default.
pub const COMMAND_LEVELS: [(&str, AccessLevel); 7] = [
    ("help", AccessLevel::Everyone),
    ("hugh", AccessLevel::Everyone),
    ("check", AccessLevel::Moderator),
    ("lock", AccessLevel::Moderator),
    ("banish", AccessLevel::Moderator),
    ("unbanish", AccessLevel::Moderator),
    ("scrape", AccessLevel::Owner),
];

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PermissionsConfig {
    /// Let Discord permissions stand in for tier membership: Administrator
    /// grants `admin`, and the moderation permissions grant `moderator`.
    pub allow_discord_permissions: bool,
    #[serde(deserialize_with = "snowflakes")]
    pub helper_role_ids: HashSet<u64>,
    #[serde(deserialize_with = "snowflakes")]
    pub helper_user_ids: HashSet<u64>,
    #[serde(deserialize_with = "snowflakes")]
    pub moderator_role_ids: HashSet<u64>,
    #[serde(deserialize_with = "snowflakes")]
    pub moderator_user_ids: HashSet<u64>,
    #[serde(deserialize_with = "snowflakes")]
    pub admin_role_ids: HashSet<u64>,
    #[serde(deserialize_with = "snowflakes")]
    pub admin_user_ids: HashSet<u64>,
    #[serde(deserialize_with = "snowflakes")]
    pub owner_role_ids: HashSet<u64>,
    #[serde(deserialize_with = "snowflakes")]
    pub owner_user_ids: HashSet<u64>,
    /// Command name to required tier. Unlisted commands keep their default.
    pub commands: BTreeMap<String, AccessLevel>,
}

impl Default for PermissionsConfig {
    fn default() -> Self {
        Self {
            allow_discord_permissions: true,
            helper_role_ids: HashSet::new(),
            helper_user_ids: HashSet::new(),
            moderator_role_ids: HashSet::new(),
            moderator_user_ids: HashSet::new(),
            admin_role_ids: HashSet::new(),
            admin_user_ids: HashSet::new(),
            owner_role_ids: HashSet::new(),
            owner_user_ids: HashSet::new(),
            commands: BTreeMap::new(),
        }
    }
}

impl PermissionsConfig {
    const fn tiers(&self) -> [(AccessLevel, &HashSet<u64>, &HashSet<u64>); 4] {
        [
            (
                AccessLevel::Helper,
                &self.helper_role_ids,
                &self.helper_user_ids,
            ),
            (
                AccessLevel::Moderator,
                &self.moderator_role_ids,
                &self.moderator_user_ids,
            ),
            (
                AccessLevel::Admin,
                &self.admin_role_ids,
                &self.admin_user_ids,
            ),
            (
                AccessLevel::Owner,
                &self.owner_role_ids,
                &self.owner_user_ids,
            ),
        ]
    }

    /// True when at least one tier that satisfies `level` lists explicit roles
    /// or users. Hugh then stops relying on Discord's own command gating so the
    /// configured staff can see the command whatever permissions they hold.
    #[must_use]
    pub fn grants_configured(&self, level: AccessLevel) -> bool {
        self.tiers()
            .into_iter()
            .filter(|(tier, _, _)| *tier >= level)
            .any(|(_, roles, users)| !roles.is_empty() || !users.is_empty())
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
        positive(
            "spam.max_coordinated_duplicates",
            &self.spam.max_coordinated_duplicates,
        )?;
        positive(
            "spam.coordinated_window_seconds",
            &self.spam.coordinated_window_seconds,
        )?;
        positive(
            "spam.coordinated_min_length",
            &self.spam.coordinated_min_length,
        )?;
        positive("spam.timeout_seconds", &self.spam.timeout_seconds)?;
        positive("content.timeout_seconds", &self.content.timeout_seconds)?;
        validate_domains(&self.content.blocked_domains)?;
        positive("raids.join_threshold", &self.raids.join_threshold)?;
        positive("raids.join_window_seconds", &self.raids.join_window_seconds)?;
        positive("raids.raid_mode_seconds", &self.raids.raid_mode_seconds)?;
        positive("raids.timeout_seconds", &self.raids.timeout_seconds)?;
        if self.raids.minimum_account_age_seconds < 0 {
            bail!("raids.minimum_account_age_seconds cannot be negative");
        }
        if self.new_accounts.minimum_account_age_seconds < 0 {
            bail!("new_accounts.minimum_account_age_seconds cannot be negative");
        }
        positive(
            "new_accounts.timeout_seconds",
            &self.new_accounts.timeout_seconds,
        )?;
        if self.role_guard.enabled && self.role_guard.protected_role_ids.is_empty() {
            bail!("role_guard is enabled but protected_role_ids is empty");
        }
        if self.mentions.enabled && self.mentions.max_total_mentions == 0 {
            bail!("mentions.max_total_mentions must be positive");
        }
        if self.quarantine.enabled {
            if self.quarantine.channel_id.is_some() != self.quarantine.role_id.is_some() {
                bail!(
                    "quarantine.channel_id and quarantine.role_id must either both be set or both be omitted"
                );
            }
            if self.quarantine.channel_name.trim().len() < 2
                || self.quarantine.channel_name.len() > 100
            {
                bail!("quarantine.channel_name must contain 2 to 100 characters");
            }
            if self.quarantine.role_name.trim().is_empty() || self.quarantine.role_name.len() > 100
            {
                bail!("quarantine.role_name must contain 1 to 100 characters");
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
        self.validate_rotector()?;
        self.validate_scrape()?;
        self.validate_permissions()?;
        self.validate_icons()?;
        Ok(())
    }

    fn validate_icons(&self) -> Result<()> {
        for (key, value) in &self.icons {
            if !ICONS.iter().any(|(known, _)| known == key) {
                bail!("icons has no slot named {key:?}; see docs/configuration.md for the list");
            }
            let value = value.trim();
            if value.is_empty() || value.chars().count() > 64 {
                bail!("icons.{key} must contain 1 to 64 characters");
            }
        }
        Ok(())
    }

    /// The emoji for an icon slot: the operator's override, else the built-in
    /// fallback. An unknown slot renders as nothing rather than breaking a card.
    #[must_use]
    pub fn icon(&self, slot: &str) -> &str {
        self.icons
            .get(slot)
            .map(|value| value.trim())
            .or_else(|| {
                ICONS
                    .iter()
                    .find(|(known, _)| *known == slot)
                    .map(|(_, fallback)| *fallback)
            })
            .unwrap_or_default()
    }

    fn validate_rotector(&self) -> Result<()> {
        if !self.rotector.enabled {
            return Ok(());
        }
        let base = Url::parse(self.rotector.base_url.trim())
            .context("rotector.base_url must be a valid URL")?;
        if !matches!(base.scheme(), "http" | "https") {
            bail!("rotector.base_url must use http or https");
        }
        if base.cannot_be_a_base() || base.host_str().is_none() {
            bail!("rotector.base_url must include a host, such as https://roscoe.rayward.app");
        }
        let language = self.rotector.language.trim();
        if language.is_empty() || language.len() > 16 {
            bail!("rotector.language must contain 1 to 16 characters, such as \"en\"");
        }
        if !(1..=60).contains(&self.rotector.request_timeout_seconds) {
            bail!("rotector.request_timeout_seconds must be between 1 and 60");
        }
        Ok(())
    }

    fn validate_scrape(&self) -> Result<()> {
        if !self.scrape.enabled {
            return Ok(());
        }
        if self.scrape.output_dir.trim().is_empty() {
            bail!("scrape.output_dir must not be empty");
        }
        if !(1..=10_000).contains(&self.scrape.max_posts) {
            bail!("scrape.max_posts must be between 1 and 10000");
        }
        if !(1..=10_000).contains(&self.scrape.max_replies_per_post) {
            bail!("scrape.max_replies_per_post must be between 1 and 10000");
        }
        Ok(())
    }

    fn validate_permissions(&self) -> Result<()> {
        for command in self.permissions.commands.keys() {
            if !COMMAND_LEVELS.iter().any(|(name, _)| name == command) {
                let known = COMMAND_LEVELS
                    .iter()
                    .map(|(name, _)| *name)
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!("permissions.commands has no command named {command:?}; known: {known}");
            }
        }
        Ok(())
    }

    /// The tier `command` requires, from configuration or its built-in default.
    #[must_use]
    pub fn required_level(&self, command: &str) -> AccessLevel {
        self.permissions
            .commands
            .get(command)
            .copied()
            .unwrap_or_else(|| {
                COMMAND_LEVELS
                    .iter()
                    .find(|(name, _)| *name == command)
                    .map_or(AccessLevel::Admin, |(_, level)| *level)
            })
    }

    /// The highest tier this member satisfies.
    #[must_use]
    pub fn access_level(&self, user_id: u64, roles: &[u64], powers: DiscordPowers) -> AccessLevel {
        if self.trusted_user_ids.contains(&user_id) {
            return AccessLevel::Owner;
        }
        let mut level = AccessLevel::Everyone;
        for (tier, role_ids, user_ids) in self.permissions.tiers() {
            if user_ids.contains(&user_id) || roles.iter().any(|role| role_ids.contains(role)) {
                level = level.max(tier);
            }
        }
        if self.permissions.allow_discord_permissions {
            if powers.administrator {
                level = level.max(AccessLevel::Admin);
            } else if powers.moderator {
                level = level.max(AccessLevel::Moderator);
            }
        }
        level
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

fn validate_domains(domains: &HashSet<String>) -> Result<()> {
    for domain in domains {
        let trimmed = domain.trim().trim_matches('.');
        if trimmed.is_empty() || trimmed.contains(['/', ':', ' ', '\\']) || !trimmed.contains('.') {
            bail!(
                "content.blocked_domains entries must be host names without a scheme or path: {domain:?}"
            );
        }
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

    #[test]
    fn quarantine_can_auto_provision_without_ids() {
        let mut config: Config = toml::from_str(
            r#"guild_id = "1"

[quarantine]
enabled = true"#,
        )
        .unwrap();
        config.role_guard.enabled = false;
        assert!(config.validate().is_ok());
        assert_eq!(config.quarantine.channel_name, "quarantine");
        assert_eq!(config.quarantine.role_name, "Quarantined");
    }

    #[test]
    fn quarantine_rejects_only_one_pinned_id() {
        let mut config: Config = toml::from_str(
            r#"guild_id = "1"

[quarantine]
enabled = true
role_id = "2""#,
        )
        .unwrap();
        config.role_guard.enabled = false;
        assert!(config.validate().is_err());
    }

    #[test]
    fn configured_tiers_outrank_discord_permissions() {
        let config: Config = toml::from_str(
            r#"guild_id = "1"

[role_guard]
enabled = false

[permissions]
moderator_role_ids = ["50"]
helper_user_ids = ["7"]

[permissions.commands]
lock = "admin"
check = "helper""#,
        )
        .unwrap();
        config.validate().unwrap();

        let none = DiscordPowers::default();
        assert_eq!(config.access_level(3, &[], none), AccessLevel::Everyone);
        assert_eq!(config.access_level(7, &[], none), AccessLevel::Helper);
        assert_eq!(config.access_level(3, &[50], none), AccessLevel::Moderator);
        assert_eq!(
            config.access_level(
                3,
                &[],
                DiscordPowers {
                    administrator: true,
                    moderator: false
                }
            ),
            AccessLevel::Admin
        );

        assert_eq!(config.required_level("lock"), AccessLevel::Admin);
        assert_eq!(config.required_level("check"), AccessLevel::Helper);
        assert_eq!(config.required_level("banish"), AccessLevel::Moderator);
        assert_eq!(config.required_level("help"), AccessLevel::Everyone);
        assert!(config.permissions.grants_configured(AccessLevel::Moderator));
        assert!(!config.permissions.grants_configured(AccessLevel::Admin));
    }

    #[test]
    fn trusted_users_reach_the_highest_tier() {
        let config: Config = toml::from_str(
            r#"guild_id = "1"
trusted_user_ids = ["9"]

[role_guard]
enabled = false"#,
        )
        .unwrap();
        assert_eq!(
            config.access_level(9, &[], DiscordPowers::default()),
            AccessLevel::Owner
        );
    }

    #[test]
    fn unknown_command_names_are_rejected() {
        let config: Config = toml::from_str(
            r#"guild_id = "1"

[role_guard]
enabled = false

[permissions.commands]
lockdown = "admin""#,
        )
        .unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn rotector_requires_a_usable_base_url() {
        let mut config: Config = toml::from_str(
            r#"guild_id = "1"

[role_guard]
enabled = false

[rotector]
enabled = true
base_url = "roscoe.rayward.app""#,
        )
        .unwrap();
        assert!(config.validate().is_err());

        config.rotector.base_url = "https://roscoe.rayward.app".to_owned();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn scrape_is_owner_only_and_bounded() {
        let config: Config = toml::from_str(
            r#"guild_id = "1"

[role_guard]
enabled = false

[scrape]
max_posts = 50"#,
        )
        .unwrap();
        config.validate().unwrap();
        assert_eq!(config.required_level("scrape"), AccessLevel::Owner);
        assert_eq!(config.scrape.max_posts, 50);
        assert_eq!(config.scrape.output_dir, "data/scrapes");

        // A nonsense ceiling stops startup rather than exporting nothing.
        let mut broken = config.clone();
        broken.scrape.max_posts = 0;
        assert!(broken.validate().is_err());
        let mut broken = config.clone();
        broken.scrape.output_dir = "  ".to_owned();
        assert!(broken.validate().is_err());

        // A disabled section is never validated, so it cannot block startup.
        let mut disabled = config;
        disabled.scrape.enabled = false;
        disabled.scrape.max_posts = 0;
        assert!(disabled.validate().is_ok());
    }

    #[test]
    fn blocked_domains_must_be_bare_hosts() {
        let mut config: Config = toml::from_str(
            r#"guild_id = "1"

[content]
blocked_domains = ["https://bad.example/path"]"#,
        )
        .unwrap();
        config.role_guard.enabled = false;
        assert!(config.validate().is_err());

        config.content.blocked_domains = HashSet::from(["bad.example".to_owned()]);
        assert!(config.validate().is_ok());
    }
}
