//! Hugh Mind from Hugh's side: the client for the scorer, the review cards it
//! leads to, and the ✅ and ❌ buttons that turn staff judgement into labels.
//!
//! Nothing here holds up the gateway. A message is scored on its own task
//! after the rules have had their say, so a scorer that is down or slow costs
//! a log line, never a missed deletion.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    sync::atomic::{AtomicI64, Ordering},
    time::Duration,
};

use anyhow::{Context as _, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serenity::{
    all::{ButtonStyle, ChannelId, ComponentInteraction, Context, Http, Permissions},
    builder::{
        CreateActionRow, CreateAllowedMentions, CreateButton, CreateEmbed, CreateEmbedAuthor,
        CreateEmbedFooter, CreateInteractionResponse, CreateInteractionResponseMessage,
        CreateMessage,
    },
};
use tracing::{debug, warn};
use url::Url;

use crate::{
    archive::{Archive, ScoreRow},
    commands::member_level,
    config::{Config, MindConfig, MindMode},
    discipline::truncate,
    features,
    incident::UserSnapshot,
    message_log::quote,
};

const FEEDBACK_PREFIX: &str = "mind_feedback:";
/// An unreachable scorer is logged at most this often, in seconds.
const WARN_EVERY: i64 = 300;
/// Anomaly percentile at which a review card says the message is out of
/// character.
const UNUSUAL: f64 = 0.9;

#[derive(Debug)]
pub struct MindClient {
    http: reqwest::Client,
    score_url: Url,
    last_warned: AtomicI64,
}

#[derive(Debug, Serialize)]
struct ScoreRequest<'a> {
    message_id: String,
    channel_id: String,
    author_id: String,
    created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_to: Option<String>,
    content: &'a str,
    features: &'a [u8],
}

/// What the scorer made of a message. Probabilities are calibrated: 0.9 means
/// staff acted on nine in ten such messages in the period it was judged on.
#[derive(Debug, Clone, Deserialize)]
pub struct Score {
    pub model: String,
    pub actionable: f64,
    pub probabilities: BTreeMap<String, f64>,
    /// How out of character the message is for its author, as a percentile
    /// of every message the model was judged on. `None` for a first message.
    pub anomaly: Option<f64>,
    pub known_member: bool,
    #[serde(default)]
    pub focus: Vec<Focus>,
}

/// A message the model leaned on while judging this one.
#[derive(Debug, Clone, Deserialize)]
pub struct Focus {
    pub message_id: String,
    #[serde(default)]
    pub channel_id: Option<String>,
}

/// The message being scored, copied out of the gateway event so the scoring
/// task owns everything it needs.
#[derive(Debug, Clone)]
pub struct Subject {
    pub id: u64,
    pub channel_id: u64,
    pub reply_to: Option<u64>,
    pub created_at: i64,
    pub content: String,
    pub author: UserSnapshot,
    pub features: [u8; features::COUNT],
    /// Hugh's rules already removed it, so staff need no second look.
    pub flagged: bool,
}

impl MindClient {
    pub fn new(config: &MindConfig) -> Result<Self> {
        let mut base = Url::parse(config.endpoint.trim()).context("mind.endpoint is not a URL")?;
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(config.request_timeout_ms))
            .build()
            .context("failed to build the Hugh Mind client")?;
        Ok(Self {
            http,
            score_url: base.join("score")?,
            last_warned: AtomicI64::new(0),
        })
    }

    async fn score(&self, subject: &Subject) -> Result<Score> {
        let request = ScoreRequest {
            message_id: subject.id.to_string(),
            channel_id: subject.channel_id.to_string(),
            author_id: subject.author.id.to_string(),
            created_at: subject.created_at,
            reply_to: subject.reply_to.map(|id| id.to_string()),
            content: &subject.content,
            features: &subject.features,
        };
        let response = self
            .http
            .post(self.score_url.clone())
            .json(&request)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!("the scorer answered {status}: {}", truncate(&body, 200));
        }
        Ok(response.json().await?)
    }

    fn unreachable(&self, error: &anyhow::Error) {
        let now = Utc::now().timestamp();
        let last = self.last_warned.load(Ordering::Relaxed);
        if now - last >= WARN_EVERY
            && self
                .last_warned
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            warn!(
                ?error,
                endpoint = %self.score_url,
                "Hugh Mind could not score a message; the rules carry on alone"
            );
        } else {
            debug!(?error, "Hugh Mind could not score a message");
        }
    }
}

/// Scores a message, keeps the score, and puts the message in front of staff
/// when the model thinks the rules missed something.
pub async fn consider(
    http: &Http,
    config: &Config,
    client: &MindClient,
    archive: &Archive,
    subject: Subject,
) {
    let score = match client.score(&subject).await {
        Ok(score) => score,
        Err(error) => {
            client.unreachable(&error);
            return;
        }
    };
    archive.score(ScoreRow {
        message_id: subject.id,
        model: score.model.clone(),
        actionable: score.actionable,
        probabilities: serde_json::to_string(&score.probabilities).unwrap_or_default(),
        anomaly: score.anomaly,
        scored_at: Utc::now().timestamp_millis(),
    });
    if !needs_review(&config.mind, &subject, &score) {
        return;
    }
    let Some(channel) = config.mind.review_channel_id.or(config.incident_channel_id) else {
        return;
    };
    let message = CreateMessage::new()
        .embed(review_card(config, &subject, &score))
        .components(vec![feedback_buttons(
            subject.id,
            "Needs action",
            "It's fine",
        )])
        .allowed_mentions(CreateAllowedMentions::new());
    if let Err(error) = ChannelId::new(channel).send_message(http, message).await {
        warn!(?error, "failed to post a Hugh Mind review card");
    }
}

fn needs_review(config: &MindConfig, subject: &Subject, score: &Score) -> bool {
    config.mode == MindMode::Review
        && !subject.flagged
        && score.actionable >= config.review_threshold
}

fn review_card(config: &Config, subject: &Subject, score: &Score) -> CreateEmbed {
    let link = |channel: &str, message: &str| {
        format!(
            "https://discord.com/channels/{}/{channel}/{message}",
            config.guild_id
        )
    };
    let mut why = format!("**{:.0}%** likely to need action", score.actionable * 100.0);
    if let Some((label, probability)) = score
        .probabilities
        .iter()
        .filter(|(label, _)| label.as_str() != "actionable")
        .max_by(|left, right| left.1.total_cmp(right.1))
    {
        let _ = write!(why, " · mostly **{label}** ({:.0}%)", probability * 100.0);
    }
    if let Some(anomaly) = score.anomaly.filter(|anomaly| *anomaly >= UNUSUAL) {
        let _ = write!(
            why,
            "\nMore out of character for them than {:.0}% of messages",
            anomaly * 100.0
        );
    }
    let weighed = score
        .focus
        .iter()
        .filter_map(|focus| {
            let channel = focus.channel_id.as_deref()?;
            Some(format!("[message]({})", link(channel, &focus.message_id)))
        })
        .collect::<Vec<_>>()
        .join(" · ");

    let name = subject.author.display_name.as_ref().map_or_else(
        || format!("@{}", subject.author.username),
        |display| format!("@{} · {display}", subject.author.username),
    );
    let mut author = CreateEmbedAuthor::new(truncate(&name, 256));
    if !subject.author.avatar_url.is_empty() {
        author = author.icon_url(&subject.author.avatar_url);
    }
    let channel = subject.channel_id.to_string();
    let mut embed = CreateEmbed::new()
        .title(format!("{}Hugh Mind · worth a look", config.mark("review")))
        .author(author)
        .description(format!(
            "<@{}> in <#{channel}> · [jump to message]({})\n\n{}",
            subject.author.id,
            link(&channel, &subject.id.to_string()),
            quote(&subject.content, 1_500)
        ))
        .field("Why", why, false)
        .colour(0x00F0_B232)
        .footer(CreateEmbedFooter::new(format!(
            "Model {} · {} · ✅ and ❌ teach it",
            score.model,
            if score.known_member {
                "knows this member"
            } else {
                "new to this member"
            }
        )));
    if !weighed.is_empty() {
        embed = embed.field("It weighed", weighed, false);
    }
    embed
}

/// The ✅ and ❌ pair, carrying the message they judge.
pub fn feedback_buttons(message_id: u64, yes: &str, no: &str) -> CreateActionRow {
    CreateActionRow::Buttons(vec![
        CreateButton::new(format!("{FEEDBACK_PREFIX}{message_id}:1"))
            .label(format!("✅ {yes}"))
            .style(ButtonStyle::Success),
        CreateButton::new(format!("{FEEDBACK_PREFIX}{message_id}:0"))
            .label(format!("❌ {no}"))
            .style(ButtonStyle::Secondary),
    ])
}

#[must_use]
pub fn is_feedback_button(custom_id: &str) -> bool {
    custom_id.starts_with(FEEDBACK_PREFIX)
}

fn parse_feedback(custom_id: &str) -> Option<(u64, bool)> {
    let (message, verdict) = custom_id.strip_prefix(FEEDBACK_PREFIX)?.split_once(':')?;
    let verdict = match verdict {
        "1" => true,
        "0" => false,
        _ => return None,
    };
    Some((message.parse().ok()?, verdict))
}

/// Keeps a staff verdict and retires the buttons, so the card shows who
/// decided what.
pub async fn handle_feedback(
    ctx: &Context,
    component: &ComponentInteraction,
    config: &Config,
    archive: Option<&Archive>,
) -> Result<()> {
    let (message_id, verdict) =
        parse_feedback(&component.data.custom_id).context("that button is not one Hugh made")?;
    let member = component
        .member
        .as_ref()
        .context("these buttons only work inside the server")?;
    let roles: Vec<u64> = member.roles.iter().map(|role| role.get()).collect();
    let level = member_level(
        config,
        component.user.id.get(),
        &roles,
        member.permissions.unwrap_or_else(Permissions::empty),
    );
    let required = config.required_level("feedback");
    if level < required {
        let reply = CreateInteractionResponseMessage::new()
            .content(format!(
                "Teaching Hugh Mind needs the **{}** staff level; you hold **{}**.",
                required.as_str(),
                level.as_str()
            ))
            .ephemeral(true);
        component
            .create_response(&ctx.http, CreateInteractionResponse::Message(reply))
            .await?;
        return Ok(());
    }
    let archive = archive.context("the archive is off, so there is nowhere to keep the verdict")?;
    archive.feedback(
        message_id,
        component.user.id.get(),
        verdict,
        Utc::now().timestamp_millis(),
    );
    let note = if verdict {
        "✅ Needed action"
    } else {
        "❌ Was fine"
    };
    let update = CreateInteractionResponseMessage::new()
        .content(format!(
            "{note} · marked by <@{}>. Hugh Mind learns from it at its next training run.",
            component.user.id
        ))
        .components(Vec::new())
        .allowed_mentions(CreateAllowedMentions::new());
    component
        .create_response(&ctx.http, CreateInteractionResponse::UpdateMessage(update))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(extra: &str) -> Config {
        let config: Config = toml::from_str(&format!(
            "guild_id = \"1\"\nincident_channel_id = \"2\"\n\n[role_guard]\nenabled = false\n\n\
             [archive]\nenabled = true\n\n[mind]\nenabled = true\n{extra}"
        ))
        .unwrap();
        config.validate().unwrap();
        config
    }

    fn subject(flagged: bool) -> Subject {
        Subject {
            id: 55,
            channel_id: 7,
            reply_to: None,
            created_at: 0,
            content: "you are an idiot".to_owned(),
            author: UserSnapshot {
                id: 42,
                username: "pluh".to_owned(),
                display_name: None,
                avatar_url: String::new(),
                bot: false,
            },
            features: [0; features::COUNT],
            flagged,
        }
    }

    fn score(actionable: f64) -> Score {
        serde_json::from_value(serde_json::json!({
            "model": "20261023-120000",
            "actionable": actionable,
            "probabilities": {"actionable": actionable, "conduct": 0.88, "spam": 0.02},
            "top": "conduct",
            "anomaly": 0.97,
            "known_member": true,
            "focus": [{"message_id": "50", "channel_id": "7", "weight": 0.4}],
            "elapsed_ms": 4.0
        }))
        .unwrap()
    }

    #[test]
    fn feedback_buttons_round_trip() {
        let row = serde_json::to_value(feedback_buttons(55, "Yes", "No")).unwrap();
        let yes = row["components"][0]["custom_id"].as_str().unwrap();
        let no = row["components"][1]["custom_id"].as_str().unwrap();
        assert!(is_feedback_button(yes));
        assert_eq!(parse_feedback(yes), Some((55, true)));
        assert_eq!(parse_feedback(no), Some((55, false)));
        assert_eq!(parse_feedback("mind_feedback:55:2"), None);
        assert!(!is_feedback_button("check_pin:roblox_user:77"));
    }

    #[test]
    fn only_review_mode_puts_unflagged_confident_scores_in_front_of_staff() {
        let shadow = config("");
        let review = config("mode = \"review\"\nreview_threshold = 0.8");
        assert!(!needs_review(&shadow.mind, &subject(false), &score(0.95)));
        assert!(needs_review(&review.mind, &subject(false), &score(0.95)));
        assert!(!needs_review(&review.mind, &subject(false), &score(0.5)));
        // The rules already dealt with it.
        assert!(!needs_review(&review.mind, &subject(true), &score(0.95)));
    }

    #[test]
    fn a_review_card_explains_itself() {
        let config = config("mode = \"review\"");
        let card =
            serde_json::to_value(review_card(&config, &subject(false), &score(0.93))).unwrap();
        let why = card["fields"][0]["value"].as_str().unwrap();
        assert!(why.starts_with("**93%** likely to need action · mostly **conduct** (88%)"));
        assert!(why.contains("out of character for them than 97%"));
        assert_eq!(
            card["fields"][1]["value"].as_str().unwrap(),
            "[message](https://discord.com/channels/1/7/50)"
        );
        assert!(
            card["description"]
                .as_str()
                .unwrap()
                .contains("> you are an idiot")
        );
    }

    #[test]
    fn the_endpoint_may_carry_a_path() {
        let mut config = MindConfig::default();
        assert_eq!(
            MindClient::new(&config).unwrap().score_url.as_str(),
            "http://127.0.0.1:8765/score"
        );
        config.endpoint = "http://mind:8765/hugh".to_owned();
        assert_eq!(
            MindClient::new(&config).unwrap().score_url.as_str(),
            "http://mind:8765/hugh/score"
        );
    }
}
