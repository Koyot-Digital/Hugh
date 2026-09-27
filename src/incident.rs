use std::{collections::BTreeMap, io::ErrorKind, path::Path, sync::Arc};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serenity::{
    all::{Colour, Http, Timestamp, User, Webhook},
    builder::{CreateAllowedMentions, CreateEmbed, CreateEmbedFooter, ExecuteWebhook},
};
use tokio::{
    fs::{self, File, OpenOptions},
    io::AsyncWriteExt,
    sync::Mutex,
};
use tracing::{error, info, warn};

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
    webhook: Webhook,
}

impl IncidentReporter {
    #[must_use]
    pub const fn new(sink: IncidentSink, webhook: Webhook) -> Self {
        Self { sink, webhook }
    }

    pub async fn report(&self, http: &Http, mut incident: Incident) {
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
            case_id = incident.case_id,
            kind = incident.kind,
            action = incident.action,
            subject_id = incident.subject.as_ref().map(|user| user.id),
            moderator_id = incident.moderator.as_ref().map(|user| user.id),
            "security incident"
        );

        let message = ExecuteWebhook::new()
            .username("Hugh Security")
            .allowed_mentions(CreateAllowedMentions::new())
            .embed(incident_embed(&incident));
        if let Err(error) = self.webhook.execute(http, false, message).await {
            warn!(
                case_id = incident.case_id,
                ?error,
                "failed to deliver incident webhook"
            );
        }
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

fn incident_embed(incident: &Incident) -> CreateEmbed {
    let action = title_case(&incident.action);
    let mut embed = CreateEmbed::new()
        .title(format!("Case #{} • {action}", incident.case_id))
        .colour(action_colour(&incident.action));

    if let Some(subject) = &incident.subject {
        let username = subject.display_name.as_ref().map_or_else(
            || format!("@{}", subject.username),
            |display| format!("@{}\n{}", subject.username, display),
        );
        embed = embed
            .field("User", format!("<@{}>", subject.id), true)
            .field("Username", truncate(&username, 1_024), true)
            .field("Discord ID", format!("`{}`", subject.id), true)
            .thumbnail(&subject.avatar_url);
    }

    let moderator = incident.moderator.as_ref().map_or_else(
        || "Hugh • Automatic".to_owned(),
        |user| format!("<@{}>\n@{}", user.id, user.username),
    );
    embed = embed
        .field("Moderator", moderator, true)
        .field("Action", action, true);
    if let Some(channel_id) = incident.channel_id {
        embed = embed.field("Channel", format!("<#{channel_id}>\n`{channel_id}`"), true);
    }
    embed = embed.field("Reason", truncate(&incident.reason, 1_024), false);

    let evidence = format_evidence(&incident.details);
    if !evidence.is_empty() {
        embed = embed.field("Evidence & context", truncate(&evidence, 1_024), false);
    }

    let timestamp = Timestamp::from_unix_timestamp(incident.timestamp.timestamp())
        .unwrap_or_else(|_| Timestamp::now());
    embed
        .footer(CreateEmbedFooter::new(format!(
            "Use Case #{} when adding moderator context • Hugh Security",
            incident.case_id
        )))
        .timestamp(timestamp)
}

fn format_evidence(details: &BTreeMap<String, serde_json::Value>) -> String {
    details
        .iter()
        .filter(|(key, _)| key.as_str() != "reasons")
        .map(|(key, value)| {
            let label = title_case(key);
            let value = match value {
                serde_json::Value::Number(number) if key.ends_with("_seconds") => number
                    .as_u64()
                    .map_or_else(|| value.to_string(), human_duration),
                serde_json::Value::String(value) => value.clone(),
                _ => value.to_string(),
            };
            format!("**{label}:** `{}`", value.replace('`', "'"))
        })
        .collect::<Vec<_>>()
        .join("\n")
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
    } else if action == "logged" {
        Colour::from_rgb(240, 178, 50)
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
        ]);
        let formatted = format_evidence(&details);
        assert!(formatted.contains("Message ID"));
        assert!(formatted.contains("10 minutes"));
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

        let embed = serde_json::to_value(incident_embed(&incident)).unwrap();
        assert!(embed["title"].as_str().unwrap().starts_with("Case #42"));
        let field_names = embed["fields"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|field| field["name"].as_str())
            .collect::<Vec<_>>();
        for expected in [
            "User",
            "Username",
            "Discord ID",
            "Moderator",
            "Action",
            "Channel",
            "Reason",
            "Evidence & context",
        ] {
            assert!(field_names.contains(&expected));
        }
    }
}
