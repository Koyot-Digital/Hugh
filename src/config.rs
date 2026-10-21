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
    /// Private staff channel Hugh posts case cards to. Hugh sends them under
    /// its own identity rather than through a webhook, so custom emoji and the
    /// server's own permissions apply.
    #[serde(default, deserialize_with = "optional_snowflake")]
    pub incident_channel_id: Option<u64>,
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
    pub automod: AutomodConfig,
    #[serde(default)]
    pub escalation: EscalationConfig,
    #[serde(default)]
    pub standing: StandingConfig,
    #[serde(default)]
    pub moderation: ModerationConfig,
    #[serde(default)]
    pub welcome: WelcomeConfig,
    #[serde(default)]
    pub lock: LockConfig,
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

/// Content shape checks that go beyond counting links and attachments. Every
/// limit is disabled by setting it to zero, so an operator can adopt them one
/// at a time.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AutomodConfig {
    pub enabled: bool,
    /// Share of letters that may be uppercase, from 0.0 to 1.0. A value of
    /// 1.0 or more accepts any amount of shouting.
    pub max_caps_ratio: f64,
    /// Messages shorter than this are never judged for shouting, because a
    /// three-letter message is uppercase by accident as often as by intent.
    pub caps_min_letters: usize,
    /// Longest run of one repeated character, as in `aaaaaaaaaaaa`.
    pub max_repeated_characters: usize,
    /// Emoji per message, counting both Unicode and custom `<:name:id>` emoji.
    pub max_emoji: usize,
    /// Longest run of combining marks, which is what stacks "zalgo" text.
    pub max_combining_marks: usize,
    /// Case-insensitive substrings Hugh refuses. Matching is on the message
    /// with its zero-width characters stripped, so padding does not hide one.
    pub blocked_phrases: Vec<String>,
    /// Distinct channels one member may repeat a message across inside
    /// `channel_hop_window_seconds` before it counts as channel hopping.
    pub channel_hop_threshold: usize,
    pub channel_hop_window_seconds: u64,
    /// Accounts younger than this may not post links. Zero disables the check.
    pub new_account_link_age_seconds: i64,
    pub timeout_seconds: u64,
}

impl Default for AutomodConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_caps_ratio: 0.8,
            caps_min_letters: 12,
            max_repeated_characters: 10,
            max_emoji: 12,
            max_combining_marks: 6,
            blocked_phrases: Vec::new(),
            channel_hop_threshold: 3,
            channel_hop_window_seconds: 20,
            new_account_link_age_seconds: 0,
            timeout_seconds: 600,
        }
    }
}

/// Automatic escalation. Hugh adds the weight of every automod hit to a rolling
/// per-member total and steps up the response as that total climbs, so a member
/// who trips one guard after another stops being handled one message at a time.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EscalationConfig {
    pub enabled: bool,
    /// How long an automod hit keeps counting towards the running total.
    pub window_seconds: u64,
    /// Accumulated weight at which each step takes over. Zero disables a step,
    /// and a member's standing scales every threshold.
    pub timeout_at: f64,
    pub kick_at: f64,
    pub ban_at: f64,
    /// Timeout applied by the escalated step, before standing scaling.
    pub timeout_seconds: u64,
    /// Length of an escalated ban. Zero bans permanently.
    pub ban_duration_seconds: u64,
}

impl Default for EscalationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            window_seconds: 3_600,
            timeout_at: 14.0,
            kick_at: 32.0,
            ban_at: 60.0,
            timeout_seconds: 3_600,
            ban_duration_seconds: 0,
        }
    }
}

/// Weight each kind of punishment takes off a member's standing.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PunishmentWeights {
    pub warn: f64,
    pub timeout: f64,
    pub kick: f64,
    pub ban: f64,
    pub temporary_ban: f64,
    pub quarantine: f64,
    /// Used for an automod hit whose violations carry no weight of their own.
    pub automod: f64,
}

impl Default for PunishmentWeights {
    fn default() -> Self {
        Self {
            warn: 8.0,
            timeout: 12.0,
            kick: 25.0,
            ban: 60.0,
            temporary_ban: 40.0,
            quarantine: 20.0,
            automod: 3.0,
        }
    }
}

/// Weight each automod violation contributes, both to the escalation total and
/// to the standing cost of the resulting case.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ViolationWeights {
    pub mass_mention: f64,
    pub everyone_mention: f64,
    pub protected_role_ping: f64,
    pub message_flood: f64,
    pub duplicate_spam: f64,
    pub coordinated_spam: f64,
    pub unapproved_invite: f64,
    pub link_flood: f64,
    pub attachment_flood: f64,
    pub excessive_newlines: f64,
    pub blocked_domain: f64,
    pub excessive_caps: f64,
    pub repeated_characters: f64,
    pub emoji_flood: f64,
    pub zalgo_text: f64,
    pub blocked_phrase: f64,
    pub channel_hop_spam: f64,
    pub new_account_link: f64,
}

impl Default for ViolationWeights {
    fn default() -> Self {
        Self {
            mass_mention: 6.0,
            everyone_mention: 10.0,
            protected_role_ping: 6.0,
            message_flood: 5.0,
            duplicate_spam: 5.0,
            coordinated_spam: 12.0,
            unapproved_invite: 10.0,
            link_flood: 4.0,
            attachment_flood: 3.0,
            excessive_newlines: 2.0,
            blocked_domain: 14.0,
            excessive_caps: 2.0,
            repeated_characters: 2.0,
            emoji_flood: 2.0,
            zalgo_text: 4.0,
            blocked_phrase: 12.0,
            channel_hop_spam: 10.0,
            new_account_link: 8.0,
        }
    }
}

/// How hard the automod limits bite at one standing band. `thresholds` scales
/// every counted limit, so below 1.0 a member trips guards sooner, and
/// `timeouts` scales the resulting timeout.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Strictness {
    pub thresholds: f64,
    pub timeouts: f64,
}

impl Default for Strictness {
    fn default() -> Self {
        Self {
            thresholds: 1.0,
            timeouts: 1.0,
        }
    }
}

impl Strictness {
    /// Scales a counted limit. A limit of zero means "never allowed" and stays
    /// there; anything else keeps at least one, because a limit scaled to zero
    /// would delete every message the member sends.
    #[must_use]
    pub fn limit(self, limit: usize) -> usize {
        if limit == 0 {
            return 0;
        }
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        let scaled = (limit as f64 * self.thresholds).round().max(1.0) as usize;
        scaled
    }

    /// Scales a ratio limit, keeping it inside 0.0..=1.0.
    #[must_use]
    pub fn ratio(self, ratio: f64) -> f64 {
        (ratio * self.thresholds).clamp(0.05, 1.0)
    }

    /// Scales a weight threshold. Worse standing lowers the bar.
    #[must_use]
    pub fn threshold(self, weight: f64) -> f64 {
        weight * self.thresholds
    }

    /// Scales a punishment length, clamped to Discord's 28-day timeout ceiling.
    #[must_use]
    pub fn duration(self, seconds: u64) -> u64 {
        if seconds == 0 {
            return 0;
        }
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
        let scaled = (seconds as f64 * self.timeouts).round().max(1.0) as u64;
        scaled.min(28 * 24 * 60 * 60)
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StrictnessConfig {
    pub exemplary: Strictness,
    pub good: Strictness,
    pub neutral: Strictness,
    pub poor: Strictness,
    pub bad: Strictness,
}

impl Default for StrictnessConfig {
    fn default() -> Self {
        Self {
            exemplary: Strictness {
                thresholds: 1.25,
                timeouts: 0.75,
            },
            good: Strictness {
                thresholds: 1.1,
                timeouts: 0.9,
            },
            neutral: Strictness::default(),
            poor: Strictness {
                thresholds: 0.65,
                timeouts: 1.5,
            },
            bad: Strictness {
                thresholds: 0.4,
                timeouts: 2.5,
            },
        }
    }
}

/// A member's standing band, best to worst.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum StandingBand {
    #[default]
    Exemplary,
    Good,
    Neutral,
    Poor,
    Bad,
}

impl StandingBand {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exemplary => "exemplary",
            Self::Good => "good",
            Self::Neutral => "neutral",
            Self::Poor => "poor",
            Self::Bad => "bad",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Exemplary => "Exemplary",
            Self::Good => "Good",
            Self::Neutral => "Neutral",
            Self::Poor => "Poor",
            Self::Bad => "Bad",
        }
    }

    /// Icon slot used on standing cards.
    #[must_use]
    pub const fn icon(self) -> &'static str {
        match self {
            Self::Exemplary | Self::Good => "safe",
            Self::Neutral => "review",
            Self::Poor => "flagged",
            Self::Bad => "denied",
        }
    }
}

/// Standing: one number per member, carried by their moderation history. Every
/// punishment takes its weight off, and that weight fades back over
/// `decay_days`, so an old warning stops counting while a fresh one bites.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StandingConfig {
    pub enabled: bool,
    /// Standing a member with no history holds.
    pub starting_points: f64,
    /// Days over which one punishment's weight decays to nothing.
    pub decay_days: f64,
    /// Lower bound of each band, descending. A member sits in the best band
    /// whose bound they still meet.
    pub exemplary_at: f64,
    pub good_at: f64,
    pub neutral_at: f64,
    pub poor_at: f64,
    pub punishments: PunishmentWeights,
    pub violations: ViolationWeights,
    pub strictness: StrictnessConfig,
}

impl Default for StandingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            starting_points: 100.0,
            decay_days: 90.0,
            exemplary_at: 95.0,
            good_at: 80.0,
            neutral_at: 60.0,
            poor_at: 35.0,
            punishments: PunishmentWeights::default(),
            violations: ViolationWeights::default(),
            strictness: StrictnessConfig::default(),
        }
    }
}

impl StandingConfig {
    /// The band `points` falls into.
    #[must_use]
    pub fn band(&self, points: f64) -> StandingBand {
        if points >= self.exemplary_at {
            StandingBand::Exemplary
        } else if points >= self.good_at {
            StandingBand::Good
        } else if points >= self.neutral_at {
            StandingBand::Neutral
        } else if points >= self.poor_at {
            StandingBand::Poor
        } else {
            StandingBand::Bad
        }
    }

    /// How hard the automod limits bite at `band`. With standing disabled every
    /// member is judged by the configured limits exactly as written.
    #[must_use]
    pub fn strictness(&self, band: StandingBand) -> Strictness {
        if !self.enabled {
            return Strictness::default();
        }
        match band {
            StandingBand::Exemplary => self.strictness.exemplary,
            StandingBand::Good => self.strictness.good,
            StandingBand::Neutral => self.strictness.neutral,
            StandingBand::Poor => self.strictness.poor,
            StandingBand::Bad => self.strictness.bad,
        }
    }
}

/// The moderation record: who was warned, kicked, timed out or banned, why, and
/// by whom. It backs `/modlogs`, `/standing`, and the sweep that lifts expired
/// temporary bans.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModerationConfig {
    /// Registers `/warn`, `/kick`, `/ban`, `/unban`, `/unwarn`, `/modlogs` and
    /// `/standing`. The record itself is always kept, because standing and
    /// escalation are built on it.
    pub enabled: bool,
    pub store_path: String,
    /// Days of the banned member's messages Discord removes, from 0 to 7.
    pub ban_delete_message_days: u8,
    /// Tell the member why before Hugh acts. A closed DM is not an error.
    pub dm_on_action: bool,
    /// Longest duration `/ban` accepts, in seconds. Zero allows any length.
    pub max_ban_duration_seconds: u64,
    /// How often Hugh looks for temporary bans that have run out.
    pub expiry_sweep_seconds: u64,
    /// Cases shown by one `/modlogs` call.
    pub modlog_page_size: usize,
}

impl Default for ModerationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            store_path: "data/moderation.jsonl".to_owned(),
            ban_delete_message_days: 0,
            dm_on_action: true,
            max_ban_duration_seconds: 0,
            expiry_sweep_seconds: 60,
            modlog_page_size: 10,
        }
    }
}

/// The greeting Hugh posts when somebody joins.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WelcomeConfig {
    pub enabled: bool,
    #[serde(deserialize_with = "optional_snowflake")]
    pub channel_id: Option<u64>,
    /// Placeholders: `{user}` mention, `{username}`, `{display}`, `{server}`,
    /// `{count}`, `{ordinal}`, `{id}`.
    pub message: String,
    pub skip_bots: bool,
    /// Stay quiet while raid mode is on, so a join flood cannot turn the
    /// greeting into the raid's own megaphone.
    pub skip_during_raid: bool,
}

impl Default for WelcomeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            channel_id: None,
            message: "Welcome to {server}, {user}, you're the {ordinal} member!".to_owned(),
            skip_bots: true,
            skip_during_raid: true,
        }
    }
}

/// Who `/lock` silences. With no roles listed it falls back to `@everyone`,
/// which is what a lock means in a server that has not split its roles up.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LockConfig {
    #[serde(deserialize_with = "snowflakes")]
    pub restricted_role_ids: HashSet<u64>,
    /// Roles `/lock` leaves able to speak, by way of an explicit allow that
    /// outranks the deny above it.
    #[serde(deserialize_with = "snowflakes")]
    pub bypass_role_ids: HashSet<u64>,
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

/// Every icon slot Hugh renders, paired with the Lucide icon it is drawn from.
/// Hugh never prints an emoji of its own: an unconfigured slot renders as
/// nothing at all, and `scripts/icons.py` downloads this exact set into
/// `assets/icons` so an operator can upload them as the server's custom emoji
/// and point `[icons]` at them.
pub const ICONS: [(&str, &str); 33] = [
    ("case", "folder-open"),
    ("pin", "pin"),
    ("warn", "megaphone"),
    ("ban", "hammer"),
    ("unban", "user-round-check"),
    ("standing", "gauge"),
    ("welcome", "hand-heart"),
    ("reason", "file-text"),
    ("details", "search"),
    ("member", "user-round"),
    ("moderator", "shield"),
    ("channel", "hash"),
    ("deleted", "message-square-x"),
    ("timeout", "timer"),
    ("kicked", "door-open"),
    ("raid", "siren"),
    ("join", "user-round-plus"),
    ("role", "user-round-cog"),
    ("quarantine", "lock"),
    ("release", "lock-open"),
    ("lock", "lock-keyhole"),
    ("denied", "ban"),
    ("failed", "triangle-alert"),
    ("flagged", "shield-alert"),
    ("safe", "badge-check"),
    ("review", "clock"),
    ("unknown", "circle-question-mark"),
    ("roblox", "gamepad-2"),
    ("discord", "message-circle"),
    ("group", "users-round"),
    ("source", "satellite-dish"),
    ("forum", "message-square-text"),
    ("image", "image"),
];

/// Whether a value is a bare `:name:` emoji shortcode. Discord expands those
/// only in its own message box, so one in the config would be printed verbatim
/// on every card the slot appears on.
fn is_emoji_shortcode(value: &str) -> bool {
    let Some(name) = value
        .strip_prefix(':')
        .and_then(|rest| rest.strip_suffix(':'))
    else {
        return false;
    };
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Commands Hugh knows about, with the tier each one needs by default.
pub const COMMAND_LEVELS: [(&str, AccessLevel); 15] = [
    ("help", AccessLevel::Everyone),
    ("hugh", AccessLevel::Everyone),
    ("check", AccessLevel::Moderator),
    ("lock", AccessLevel::Moderator),
    ("unlock", AccessLevel::Moderator),
    ("banish", AccessLevel::Moderator),
    ("unbanish", AccessLevel::Moderator),
    ("warn", AccessLevel::Moderator),
    ("kick", AccessLevel::Moderator),
    ("ban", AccessLevel::Admin),
    ("unban", AccessLevel::Admin),
    ("unwarn", AccessLevel::Admin),
    ("modlogs", AccessLevel::Helper),
    ("standing", AccessLevel::Helper),
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
        match self.incident_channel_id {
            None => bail!(
                "incident_channel_id is not set; it is a top-level key, so in config.toml it must sit above the first [section] header, as a quoted ID: incident_channel_id = \"123456789012345678\" (see docs/setup.md)"
            ),
            Some(0) => bail!("incident_channel_id must be a non-zero Discord ID"),
            Some(_) => {}
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
        self.validate_automod()?;
        self.validate_standing()?;
        self.validate_moderation()?;
        self.validate_welcome()?;
        self.validate_permissions()?;
        self.validate_icons()?;
        Ok(())
    }

    fn validate_automod(&self) -> Result<()> {
        if self.automod.enabled {
            if !self.automod.max_caps_ratio.is_finite() || self.automod.max_caps_ratio <= 0.0 {
                bail!(
                    "automod.max_caps_ratio must be above 0.0; use 1.0 or more to accept any amount of shouting"
                );
            }
            if self.automod.channel_hop_threshold > 0 {
                positive(
                    "automod.channel_hop_window_seconds",
                    &self.automod.channel_hop_window_seconds,
                )?;
            }
            if self.automod.new_account_link_age_seconds < 0 {
                bail!("automod.new_account_link_age_seconds cannot be negative");
            }
            positive("automod.timeout_seconds", &self.automod.timeout_seconds)?;
            for phrase in &self.automod.blocked_phrases {
                if phrase.trim().is_empty() {
                    bail!("automod.blocked_phrases must not contain an empty entry");
                }
            }
        }
        if self.escalation.enabled {
            positive("escalation.window_seconds", &self.escalation.window_seconds)?;
            positive(
                "escalation.timeout_seconds",
                &self.escalation.timeout_seconds,
            )?;
            for (name, value) in [
                ("escalation.timeout_at", self.escalation.timeout_at),
                ("escalation.kick_at", self.escalation.kick_at),
                ("escalation.ban_at", self.escalation.ban_at),
            ] {
                if !value.is_finite() || value < 0.0 {
                    bail!("{name} must be zero or a positive weight");
                }
            }
        }
        Ok(())
    }

    fn validate_standing(&self) -> Result<()> {
        if !self.standing.enabled {
            return Ok(());
        }
        let standing = &self.standing;
        if !standing.starting_points.is_finite() || standing.starting_points <= 0.0 {
            bail!("standing.starting_points must be a positive number, such as 100");
        }
        if !standing.decay_days.is_finite() || standing.decay_days <= 0.0 {
            bail!("standing.decay_days must be a positive number of days");
        }
        let bands = [
            ("standing.exemplary_at", standing.exemplary_at),
            ("standing.good_at", standing.good_at),
            ("standing.neutral_at", standing.neutral_at),
            ("standing.poor_at", standing.poor_at),
        ];
        for (name, value) in bands {
            if !value.is_finite() || value < 0.0 || value > standing.starting_points {
                bail!("{name} must be between 0 and standing.starting_points");
            }
        }
        for pair in bands.windows(2) {
            let (upper, above) = pair[0];
            let (lower, below) = pair[1];
            if above <= below {
                bail!("{upper} must sit above {lower}");
            }
        }
        for (name, strictness) in [
            ("exemplary", standing.strictness.exemplary),
            ("good", standing.strictness.good),
            ("neutral", standing.strictness.neutral),
            ("poor", standing.strictness.poor),
            ("bad", standing.strictness.bad),
        ] {
            for (field, value) in [
                ("thresholds", strictness.thresholds),
                ("timeouts", strictness.timeouts),
            ] {
                if !value.is_finite() || !(0.05..=10.0).contains(&value) {
                    bail!("standing.strictness.{name}.{field} must be between 0.05 and 10.0");
                }
            }
        }
        Ok(())
    }

    fn validate_moderation(&self) -> Result<()> {
        if self.moderation.store_path.trim().is_empty() {
            bail!("moderation.store_path must not be empty");
        }
        if self.moderation.ban_delete_message_days > 7 {
            bail!("moderation.ban_delete_message_days must be between 0 and 7");
        }
        positive(
            "moderation.expiry_sweep_seconds",
            &self.moderation.expiry_sweep_seconds,
        )?;
        if !(1..=25).contains(&self.moderation.modlog_page_size) {
            bail!("moderation.modlog_page_size must be between 1 and 25");
        }
        Ok(())
    }

    fn validate_welcome(&self) -> Result<()> {
        if !self.welcome.enabled {
            return Ok(());
        }
        match self.welcome.channel_id {
            None => bail!("welcome is enabled but welcome.channel_id is not set"),
            Some(0) => bail!("welcome.channel_id must be a non-zero Discord ID"),
            Some(_) => {}
        }
        let message = self.welcome.message.trim();
        if message.is_empty() || message.chars().count() > 1_800 {
            bail!("welcome.message must contain 1 to 1800 characters");
        }
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
            if is_emoji_shortcode(value) {
                bail!(
                    "icons.{key} is the shortcode {value:?}, which Discord expands only in its                      own message box; paste the emoji itself as <:name:id>, or <a:name:id> for                      an animated one (see docs/configuration.md)"
                );
            }
        }
        Ok(())
    }

    /// The custom emoji configured for an icon slot. A slot the operator has
    /// not set, like an unknown slot, renders as nothing rather than falling
    /// back to an emoji Hugh picked for them.
    #[must_use]
    pub fn icon(&self, slot: &str) -> &str {
        self.icons
            .get(slot)
            .map(|value| value.trim())
            .unwrap_or_default()
    }

    /// An icon slot followed by one space, or nothing at all. Labels built with
    /// this read correctly whether or not the server has the artwork uploaded.
    #[must_use]
    pub fn mark(&self, slot: &str) -> String {
        let icon = self.icon(slot);
        if icon.is_empty() {
            String::new()
        } else {
            format!("{icon} ")
        }
    }

    /// The Lucide icon a slot is drawn from, for the asset download script and
    /// the configuration reference.
    #[must_use]
    pub fn lucide_icon(slot: &str) -> Option<&'static str> {
        ICONS
            .iter()
            .find(|(known, _)| *known == slot)
            .map(|(_, icon)| *icon)
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
incident_channel_id = "2"
surprise = true"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn trusted_role_grants_bypass() {
        let mut config: Config = toml::from_str(
            r#"guild_id = "1"
incident_channel_id = "2"
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
incident_channel_id = "2"

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
incident_channel_id = "2"

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
incident_channel_id = "2"

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
incident_channel_id = "2"
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
    fn incident_channel_is_required() {
        let mut config: Config = toml::from_str(r#"guild_id = "1""#).unwrap();
        config.role_guard.enabled = false;
        let message = config.validate().unwrap_err().to_string();
        assert!(message.contains("incident_channel_id"), "{message}");

        config.incident_channel_id = Some(0);
        assert!(config.validate().is_err());
        config.incident_channel_id = Some(2);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn icon_shortcodes_are_rejected() {
        let mut config: Config = toml::from_str(
            r#"guild_id = "1"
incident_channel_id = "2"

[icons]
timeout = ":timeout:""#,
        )
        .unwrap();
        config.role_guard.enabled = false;
        let message = config.validate().unwrap_err().to_string();
        assert!(message.contains("icons.timeout"), "{message}");
        assert!(message.contains("<:name:id>"), "{message}");
    }

    #[test]
    fn icon_custom_emoji_markup_is_accepted() {
        let mut config: Config = toml::from_str(
            r#"guild_id = "1"
incident_channel_id = "2"

[icons]
timeout = "<:timeout:123456789012345678>""#,
        )
        .unwrap();
        config.role_guard.enabled = false;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn unknown_command_names_are_rejected() {
        let config: Config = toml::from_str(
            r#"guild_id = "1"
incident_channel_id = "2"

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
incident_channel_id = "2"

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
incident_channel_id = "2"

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
incident_channel_id = "2"

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
