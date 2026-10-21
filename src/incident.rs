use std::{collections::BTreeMap, io::ErrorKind, path::Path, sync::Arc};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serenity::{
    all::{ChannelId, Colour, Http, Timestamp, User},
    builder::{
        CreateAllowedMentions, CreateEmbed, CreateEmbedAuthor, CreateEmbedFooter, CreateMessage,
    },
};
use tokio::{
    fs::{self, File, OpenOptions},
    io::AsyncWriteExt,
    sync::Mutex,
};
use tracing::{error, info, warn};

use crate::config::Config;

#[derive(Debug, Clone, Serialize)]
pub struct UserSnapshot {
    pub id: u64,
    pub username: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub avatar_url: String,
    pub bot: bool,
}

impl From<&User> for UserSnapshot {
    fn from(user: &User) -> Self {
        Self {
            id: user.id.get(),
            username: user.name.clone(),
            display_name: user.global_name.clone(),
            avatar_url: user.face(),
            bot: user.bot,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Incident {
    pub case_id: u64,
    pub timestamp: DateTime<Utc>,
    pub kind: String,
    pub guild_id: u64,
    /// Compatibility field for existing JSONL consumers. New code should use
    /// `subject` and `moderator`, whose meanings are unambiguous.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<UserSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moderator: Option<UserSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<u64>,
    pub action: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, serde_json::Value>,
}

impl Incident {
    pub fn new(
        kind: impl Into<String>,
        guild_id: u64,
        action: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            case_id: 0,
            timestamp: Utc::now(),
            kind: kind.into(),
            guild_id,
            actor_id: None,
            subject: None,
            moderator: None,
            channel_id: None,
            action: action.into(),
            reason: reason.into(),
            details: BTreeMap::new(),
        }
    }
}

#[derive(Debug)]
struct IncidentWriter {
    file: File,
    next_case_id: u64,
}

#[derive(Debug, Clone)]
pub struct IncidentSink {
    writer: Arc<Mutex<IncidentWriter>>,
}

pub struct IncidentReporter {
    sink: IncidentSink,
    channel_id: ChannelId,
    config: Arc<Config>,
}

impl IncidentReporter {
    #[must_use]
    pub const fn new(sink: IncidentSink, channel_id: ChannelId, config: Arc<Config>) -> Self {
        Self {
            sink,
            channel_id,
            config,
        }
    }

    pub async fn report(&self, http: &Http, incident: Incident) -> u64 {
        self.report_with(http, incident, Vec::new()).await
    }

    /// Records the case and delivers it, with `attached` cards shown beneath the
    /// case header. Returns the durable case number.
    pub async fn report_with(
        &self,
        http: &Http,
        mut incident: Incident,
        attached: Vec<CreateEmbed>,
    ) -> u64 {
        incident.actor_id = incident.actor_id.or_else(|| {
            incident
                .moderator
                .as_ref()
                .or(incident.subject.as_ref())
                .map(|user| user.id)
        });
        if let Err(error) = self.sink.record(&mut incident).await {
            error!(?error, "failed to persist security incident");
        }
        info!(
            case = incident.case_id,
            kind = incident.kind,
            subject = incident.subject.as_ref().map(|user| user.id),
            moderator = incident.moderator.as_ref().map(|user| user.id),
            "{} — {}",
            action_phrase(&incident.action),
            incident.reason
        );

        let mut embeds = vec![incident_embed(&incident, &self.config)];
        // Discord accepts ten embeds per message; the case header takes one.
        embeds.extend(attached.into_iter().take(9));
        // Hugh posts the card itself: a webhook cannot resolve the custom emoji
        // behind the icon slots, and its delivery ignores channel permissions.
        let message = CreateMessage::new()
            .allowed_mentions(CreateAllowedMentions::new())
            .embeds(embeds);
        if let Err(error) = self.channel_id.send_message(http, message).await {
            warn!(
                case_id = incident.case_id,
                channel_id = self.channel_id.get(),
                ?error,
                "failed to post the case card; the JSONL copy is still on disk"
            );
        }
        incident.case_id
    }
}

impl IncidentSink {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).await.with_context(|| {
                format!("failed to create incident directory {}", parent.display())
            })?;
        }
        let next_case_id = next_case_id(path).await?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
            .with_context(|| format!("failed to open incident log {}", path.display()))?;
        Ok(Self {
            writer: Arc::new(Mutex::new(IncidentWriter { file, next_case_id })),
        })
    }

    pub async fn record(&self, incident: &mut Incident) -> Result<u64> {
        let mut writer = self.writer.lock().await;
        if incident.case_id == 0 {
            incident.case_id = writer.next_case_id;
            writer.next_case_id = writer.next_case_id.saturating_add(1);
        } else {
            writer.next_case_id = writer.next_case_id.max(incident.case_id.saturating_add(1));
        }
        let mut line = serde_json::to_vec(incident).context("failed to serialize incident")?;
        line.push(b'\n');
        writer
            .file
            .write_all(&line)
            .await
            .context("failed to write incident")?;
        writer
            .file
            .flush()
            .await
            .context("failed to flush incident")?;
        drop(writer);
        Ok(incident.case_id)
    }
}

async fn next_case_id(path: &Path) -> Result<u64> {
    let data = match fs::read_to_string(path).await {
        Ok(data) => data,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(1),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to inspect incident log {}", path.display()));
        }
    };
    let last = data
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|record| record.get("case_id")?.as_u64())
        .max()
        .unwrap_or(0);
    Ok(last.saturating_add(1).max(1))
}

fn incident_embed(incident: &Incident, config: &Config) -> CreateEmbed {
    let headline = action_phrase(&incident.action);
    let mut embed = CreateEmbed::new()
        .title(format!(
            "{}Case #{} · {headline}",
            config.mark(action_icon(&incident.action)),
            incident.case_id
        ))
        .colour(action_colour(&incident.action));

    // The subject's face is the fastest thing to recognise in a busy mod log,
    // so it leads the card; with no subject the responsible moderator does.
    let portrait = incident.subject.as_ref().or(incident.moderator.as_ref());
    if let Some(user) = portrait {
        let name = user.display_name.as_ref().map_or_else(
            || format!("@{}", user.username),
            |display| format!("@{} · {display}", user.username),
        );
        let mut author = CreateEmbedAuthor::new(truncate(&name, 256));
        if !user.avatar_url.is_empty() {
            author = author.icon_url(&user.avatar_url);
        }
        embed = embed.author(author);
    }
    if let Some(subject) = &incident.subject
        && !subject.avatar_url.is_empty()
    {
        embed = embed.thumbnail(&subject.avatar_url);
    }

    // One line of clickable references beats three near-empty inline fields.
    let mut references = Vec::new();
    if let Some(subject) = &incident.subject {
        let tag = if subject.bot { " *(app)*" } else { "" };
        references.push(format!("{}<@{}>{tag}", config.mark("member"), subject.id));
    }
    if let Some(channel_id) = incident.channel_id {
        references.push(format!("{}<#{channel_id}>", config.mark("channel")));
    }
    references.push(incident.moderator.as_ref().map_or_else(
        || format!("{}Hugh · automatic", config.mark("moderator")),
        |user| format!("{}<@{}>", config.mark("moderator"), user.id),
    ));

    embed = embed.description(format!(
        "{}**{}**\n{}",
        config.mark("reason"),
        truncate(&incident.reason, 1_500),
        references.join("  ·  ")
    ));

    let details = format_evidence(&incident.details);
    if !details.is_empty() {
        embed = embed.field(
            format!("{}Details", config.mark("details")),
            truncate(&details, 1_024),
            false,
        );
    }
    if let Some(subject) = &incident.subject {
        embed = embed.field(
            format!("{}Discord ID", config.mark("member")),
            format!("`{}`", subject.id),
            true,
        );
    }

    let timestamp = Timestamp::from_unix_timestamp(incident.timestamp.timestamp())
        .unwrap_or_else(|_| Timestamp::now());
    embed
        .footer(CreateEmbedFooter::new(format!(
            "{} · quote Case #{} in moderator notes",
            title_case(&incident.kind),
            incident.case_id
        )))
        .timestamp(timestamp)
}

/// Icon slot for an outcome, so the card's leading glyph says at a glance
/// whether something was deleted, timed out, quarantined, or merely logged.
fn action_icon(action: &str) -> &'static str {
    if action.contains("failed") || action.contains("partial") {
        return "failed";
    }
    match action {
        "message_deleted" | "delete_failed" => "deleted",
        "message_deleted_and_member_timed_out" | "timed_out" => "timeout",
        "kicked" => "kicked",
        "raid_mode_enabled" => "raid",
        "logged" => "review",
        "roles_removed" => "role",
        "member_banished" => "quarantine",
        "member_released" => "release",
        "channel_locked" => "lock",
        "command_blocked" => "denied",
        "rotector_match" | "lookup_pinned" => "flagged",
        "forum_exported" => "forum",
        _ => "case",
    }
}

/// Plain-English headline for an outcome code, so a moderator reads what
/// happened instead of decoding `message_deleted_timeout_failed`.
fn action_phrase(action: &str) -> String {
    match action {
        "message_deleted" => "Message deleted",
        "message_deleted_and_member_timed_out" => "Message deleted · member timed out",
        "message_deleted_timeout_failed" => "Message deleted · timeout failed",
        "member_timed_out_delete_failed" => "Member timed out · message still up",
        "delete_failed" => "Message could not be deleted",
        "enforcement_failed" => "Enforcement failed",
        "raid_mode_enabled" => "Raid mode enabled",
        "logged" => "Logged for review, nothing enforced",
        "timed_out" => "Member timed out",
        "kicked" => "Member kicked",
        "roles_removed" => "Protected roles removed",
        "role_removal_failed" => "Protected roles could not be removed",
        "member_banished" => "Member quarantined",
        "member_partially_banished" => "Member quarantined · roles left behind",
        "member_released" => "Member released from quarantine",
        "channel_locked" => "Channel locked",
        "command_failed" => "Command failed",
        "command_blocked" => "Command blocked",
        "configuration_failed" => "Setup failed",
        "rotector_match" => "Rotector flagged this account",
        "lookup_pinned" => "Lookup pinned to the mod log",
        "forum_exported" => "Forum posts exported",
        other => return title_case(other),
    }
    .to_owned()
}

/// Reading order for the details block: what happened, then the counts that
/// explain it, then the bookkeeping identifiers.
const DETAIL_ORDER: [&str; 30] = [
    "source",
    "command",
    "forum_name",
    "posts",
    "images",
    "replies",
    "archived_included",
    "replies_included",
    "target_user_id",
    "lookup",
    "target",
    "verdict",
    "reason_types",
    "required_level",
    "held_level",
    "total_mentions",
    "link_count",
    "attachment_count",
    "coordinated_users",
    "blocked_domain",
    "account_age_seconds",
    "joins_in_window",
    "window_seconds",
    "raid_mode_seconds",
    "timeout_seconds",
    "timeout_skipped",
    "stored_role_ids",
    "restored_role_ids",
    "removed_role_ids",
    "message_id",
];

fn format_evidence(details: &BTreeMap<String, serde_json::Value>) -> String {
    let mut lines: Vec<(usize, String)> = details
        .iter()
        // The machine-readable reason codes repeat the "Why" line above.
        .filter(|(key, _)| key.as_str() != "reasons")
        .filter_map(|(key, value)| format_detail(key, value).map(|line| (detail_rank(key), line)))
        .collect();
    lines.sort_by_key(|(rank, _)| *rank);
    lines
        .into_iter()
        .map(|(_, line)| line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn detail_rank(key: &str) -> usize {
    DETAIL_ORDER
        .iter()
        .position(|known| *known == key)
        .unwrap_or(DETAIL_ORDER.len())
}

/// `None` hides the entry entirely: an empty list or a null is noise, not
/// evidence.
fn format_detail(key: &str, value: &serde_json::Value) -> Option<String> {
    let rendered = match value {
        serde_json::Value::Null => return None,
        serde_json::Value::Bool(flag) => (if *flag { "Yes" } else { "No" }).to_owned(),
        serde_json::Value::Number(number) if key.ends_with("_seconds") => number
            .as_u64()
            .map_or_else(|| number.to_string(), human_duration),
        serde_json::Value::Number(number) if key.ends_with("_user_id") => number
            .as_u64()
            .map_or_else(|| number.to_string(), |id| format!("<@{id}> `{id}`")),
        serde_json::Value::Number(number) if key.ends_with("_id") => format!("`{number}`"),
        serde_json::Value::Number(number) => number.to_string(),
        serde_json::Value::String(text) => format_detail_text(key, text),
        serde_json::Value::Array(items) if items.is_empty() => return None,
        serde_json::Value::Array(items) => format_detail_list(key, items),
        serde_json::Value::Object(_) => format!("`{value}`"),
    };
    Some(format!("**{}:** {rendered}", detail_label(key)))
}

fn format_detail_text(key: &str, text: &str) -> String {
    let text = text.trim().replace('`', "'");
    match key {
        "source" => match text.as_str() {
            "message_create" => "New message".to_owned(),
            "message_edit" => "Edited message".to_owned(),
            other => title_case(other),
        },
        "command" => format!("`/{text}`"),
        "lookup" => title_case(&text),
        "target" | "blocked_domain" => format!("`{text}`"),
        _ => text,
    }
}

fn format_detail_list(key: &str, items: &[serde_json::Value]) -> String {
    const SHOWN: usize = 8;
    let role_ids = key.ends_with("role_ids");
    let mut rendered: Vec<String> = items
        .iter()
        .take(SHOWN)
        .map(|item| match item.as_u64() {
            Some(id) if role_ids => format!("<@&{id}>"),
            Some(id) => format!("`{id}`"),
            None => format!("`{item}`"),
        })
        .collect();
    if items.len() > SHOWN {
        rendered.push(format!("and {} more", items.len() - SHOWN));
    }
    format!("{} — {}", items.len(), rendered.join(", "))
}

fn detail_label(key: &str) -> String {
    match key {
        "source" => "Trigger",
        "message_id" => "Message",
        "total_mentions" => "Mentions",
        "attachment_count" => "Attachments",
        "link_count" => "Links",
        "coordinated_users" => "Accounts posting the same text",
        "timeout_seconds" => "Timeout",
        "timeout_skipped" => "Timeout skipped",
        "blocked_domain" => "Blocked domain",
        "account_age_seconds" => "Account age",
        "joins_in_window" => "Joins in the window",
        "window_seconds" => "Join window",
        "raid_mode_seconds" => "Raid mode lasts",
        "stored_role_ids" => "Roles stored for recovery",
        "restored_role_ids" => "Roles restored",
        "removed_role_ids" => "Roles removed",
        "failed_role_ids" => "Roles Hugh could not change",
        "missing_role_ids" => "Deleted roles skipped",
        "target_user_id" => "Target",
        "required_level" => "Level required",
        "held_level" => "Level held",
        "reason_types" => "Finding types",
        "lookup" => "Lookup",
        "verdict" => "Verdict",
        "forum_name" => "Forum",
        "posts" => "Posts exported",
        "archived_included" => "Archived posts included",
        "replies_included" => "Replies captured",
        "export_file" => "Export",
        other => return title_case(other),
    }
    .to_owned()
}

fn human_duration(seconds: u64) -> String {
    let mut remaining = seconds;
    let mut parts = Vec::new();
    for (size, singular) in [
        (86_400, "day"),
        (3_600, "hour"),
        (60, "minute"),
        (1, "second"),
    ] {
        let count = remaining / size;
        if count > 0 {
            let suffix = if count == 1 { "" } else { "s" };
            parts.push(format!("{count} {singular}{suffix}"));
            remaining %= size;
        }
        if parts.len() == 2 {
            break;
        }
    }
    if parts.is_empty() {
        "0 seconds".to_owned()
    } else {
        parts.join(" ")
    }
}

fn title_case(value: &str) -> String {
    let joined = value
        .split('_')
        .filter(|word| !word.is_empty())
        .map(|word| match word {
            "id" => "ID",
            "ids" => "IDs",
            _ => word,
        })
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = joined.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + chars.as_str()
    })
}

fn action_colour(action: &str) -> Colour {
    if action.contains("failed") || action.contains("partial") {
        Colour::from_rgb(218, 55, 60)
    } else if action.contains("released") {
        Colour::from_rgb(35, 165, 90)
    } else if action == "logged" || action == "command_blocked" {
        Colour::from_rgb(240, 178, 50)
    } else if action == "rotector_match" {
        Colour::from_rgb(237, 110, 60)
    } else {
        Colour::from_rgb(237, 66, 69)
    }
}

fn truncate(value: &str, maximum: usize) -> String {
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

    #[tokio::test]
    async fn appends_valid_json_lines_with_durable_case_numbers() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("incidents.jsonl");
        let sink = IncidentSink::open(&path).await.unwrap();
        let mut first = Incident::new("test", 1, "logged", "First test");
        sink.record(&mut first).await.unwrap();
        assert_eq!(first.case_id, 1);
        drop(sink);

        let sink = IncidentSink::open(&path).await.unwrap();
        let mut second = Incident::new("test", 1, "logged", "Second test");
        sink.record(&mut second).await.unwrap();
        assert_eq!(second.case_id, 2);

        let data = fs::read_to_string(path).await.unwrap();
        let records = data
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(records[0]["case_id"], 1);
        assert_eq!(records[1]["case_id"], 2);
    }

    #[test]
    fn formats_readable_evidence() {
        let details = BTreeMap::from([
            ("timeout_seconds".into(), serde_json::json!(600)),
            ("message_id".into(), serde_json::json!(123)),
            ("source".into(), serde_json::json!("message_edit")),
            ("total_mentions".into(), serde_json::json!(12)),
            ("reasons".into(), serde_json::json!("mass_mention")),
            ("failed_role_ids".into(), serde_json::json!([])),
            ("stored_role_ids".into(), serde_json::json!([7, 8])),
        ]);
        let formatted = format_evidence(&details);
        let lines: Vec<&str> = formatted.lines().collect();

        // Trigger first, bookkeeping identifiers last, nothing empty, and no
        // duplicate of the reason codes already shown as "Why".
        assert_eq!(lines[0], "**Trigger:** Edited message");
        assert_eq!(lines[1], "**Mentions:** 12");
        assert_eq!(lines[2], "**Timeout:** 10 minutes");
        assert_eq!(lines[3], "**Roles stored for recovery:** 2 — <@&7>, <@&8>");
        assert_eq!(lines[4], "**Message:** `123`");
        assert_eq!(lines.len(), 5);
        assert!(!formatted.contains("mass_mention"));
    }

    #[test]
    fn long_lists_are_summarised() {
        let ids: Vec<u64> = (1..=12).collect();
        let details = BTreeMap::from([("removed_role_ids".into(), serde_json::json!(ids))]);
        let formatted = format_evidence(&details);
        assert!(formatted.starts_with("**Roles removed:** 12 — <@&1>,"));
        assert!(formatted.ends_with("and 4 more"));
    }

    #[test]
    fn outcome_codes_become_plain_english() {
        assert_eq!(
            action_phrase("message_deleted_and_member_timed_out"),
            "Message deleted · member timed out"
        );
        assert_eq!(
            action_phrase("member_released"),
            "Member released from quarantine"
        );
        // Unmapped codes still read as words rather than snake_case.
        assert_eq!(action_phrase("some_new_outcome"), "Some new outcome");
    }

    #[test]
    fn case_embed_contains_moderation_context() {
        let mut incident = Incident::new(
            "message_guard",
            1,
            "message_deleted_and_member_timed_out",
            "Mass mentions",
        );
        incident.case_id = 42;
        incident.channel_id = Some(7);
        incident.subject = Some(UserSnapshot {
            id: 9,
            username: "example".to_owned(),
            display_name: Some("Example User".to_owned()),
            avatar_url: "https://cdn.discordapp.com/embed/avatars/0.png".to_owned(),
            bot: false,
        });
        incident
            .details
            .insert("timeout_seconds".into(), serde_json::json!(600));

        let config: Config = toml::from_str("guild_id = \"1\"").unwrap();
        let embed = serde_json::to_value(incident_embed(&incident, &config)).unwrap();

        // The outcome's icon leads the title, so a scanning moderator sees the
        // kind of action before reading a word.
        assert_eq!(
            embed["title"].as_str().unwrap(),
            "⏳ Case #42 · Message deleted · member timed out"
        );

        // The subject's avatar appears twice over: as the author glyph and the
        // thumbnail.
        let avatar = "https://cdn.discordapp.com/embed/avatars/0.png";
        assert_eq!(
            embed["author"]["name"].as_str().unwrap(),
            "@example · Example User"
        );
        assert_eq!(embed["author"]["icon_url"].as_str().unwrap(), avatar);
        assert_eq!(embed["thumbnail"]["url"].as_str().unwrap(), avatar);

        // Reason plus every clickable reference on one line, not three fields.
        let description = embed["description"].as_str().unwrap();
        assert_eq!(
            description,
            "📝 **Mass mentions**\n👤 <@9>  ·  #️⃣ <#7>  ·  🛡️ Hugh · automatic"
        );

        let field_names = embed["fields"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|field| field["name"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(field_names, vec!["🔎 Details", "👤 Discord ID"]);
        assert!(
            embed["fields"][0]["value"]
                .as_str()
                .unwrap()
                .contains("**Timeout:** 10 minutes")
        );
        assert_eq!(embed["fields"][1]["value"].as_str().unwrap(), "`9`");
        assert!(
            embed["footer"]["text"]
                .as_str()
                .unwrap()
                .starts_with("Message guard")
        );
    }
}
