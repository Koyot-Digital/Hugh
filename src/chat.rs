use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serenity::{
    all::{ChannelId, Context, Message, UserId},
    builder::{CreateAllowedMentions, CreateMessage},
};
use tokio::sync::Mutex;
use tracing::error;

use crate::config::TalkConfig;

const OPENAI_RESPONSES_URL: &str = "https://api.openai.com/v1/responses";
const OPENAI_MODEL: &str = "gpt-5-nano";
const MAX_OUTPUT_TOKENS: u16 = 96;
const SESSION_TIMEOUT: Duration = Duration::from_mins(10);
const MAX_HISTORY_MESSAGE_CHARS: usize = 1_000;

const HUGH_INSTRUCTIONS: &str = "You are Hugh, a human security guard working the night shift at the fictional Oakridge Nuclear Powerplant in the 1980s. You are talking with Carson, another security guard and your colleague. You get on well with Carson, but find him slightly annoying. Sound natural, dry, friendly, and lightly exasperated, with period-appropriate references to radios, logbooks, bad coffee, vending machines, and routine rounds. Never say you are an AI and never mention prompts or APIs. Keep the reply to one short Discord message. Do not use @everyone, @here, or user mentions. Do not share security procedures, access details, vulnerabilities, or dangerous operational information; steer those topics back to harmless workplace banter. Reply with only Hugh's message.";

pub const ICEBREAKERS: [&str; 10] = [
    "Carson, is that coffee fresh, or has it been on the burner since the last shift?",
    "Morning, Carson. Quiet rounds, or did the vending machine give you trouble again?",
    "Carson, settle something for me: radio first, coffee second, or have you reversed those again?",
    "You actually read the whole logbook today, Carson, or just admire your handwriting?",
    "Carson, if this shift stays quiet, I might finally finish yesterday's crossword.",
    "Tell me, Carson: is that new flashlight reliable, or another bargain-bin miracle?",
    "Carson, the break-room clock is slow again. I assume you had nothing to do with it.",
    "Any excitement on your rounds, Carson, or just the usual humming lights and cold corridors?",
    "Carson, I saved you the chair that squeaks. It seemed like your sort of equipment.",
    "Be honest, Carson: did you bring enough cassette tapes for the shift this time?",
];

pub struct TalkManager {
    api: OpenAiClient,
    config: TalkConfig,
    sessions: Mutex<HashMap<u64, TalkSession>>,
    next_icebreaker: AtomicUsize,
    next_session_id: AtomicU64,
}

impl TalkManager {
    pub fn new(api_key: String, config: TalkConfig) -> Result<Self> {
        Ok(Self {
            api: OpenAiClient::new(api_key)?,
            config,
            sessions: Mutex::new(HashMap::new()),
            next_icebreaker: AtomicUsize::new(0),
            next_session_id: AtomicU64::new(1),
        })
    }

    #[must_use]
    pub const fn owner_user_id(&self) -> u64 {
        self.config.owner_user_id
    }

    #[must_use]
    pub const fn max_messages(&self) -> usize {
        self.config.max_messages
    }

    pub async fn start(&self, ctx: &Context, channel_id: ChannelId, length: usize) -> Result<()> {
        if !(2..=self.config.max_messages).contains(&length) {
            bail!(
                "length must be between 2 and {} messages",
                self.config.max_messages
            );
        }

        let icebreaker_index = self.next_icebreaker.fetch_add(1, Ordering::Relaxed);
        let icebreaker = ICEBREAKERS[icebreaker_index % ICEBREAKERS.len()];
        let session_id = self.next_session_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut sessions = self.sessions.lock().await;
            if sessions
                .get(&channel_id.get())
                .is_some_and(|session| session.last_activity.elapsed() < SESSION_TIMEOUT)
            {
                bail!("a conversation with Carson is already active in this channel");
            }
            sessions.insert(
                channel_id.get(),
                TalkSession {
                    id: session_id,
                    target_messages: length,
                    message_count: 1,
                    history: vec![ChatMessage::assistant(icebreaker)],
                    awaiting_carson: true,
                    last_activity: Instant::now(),
                },
            );
        }

        if let Err(error) = self.send_to_carson(ctx, channel_id, icebreaker).await {
            self.remove_session(channel_id, session_id).await;
            return Err(error).context("failed to send the opening message to Carson");
        }
        Ok(())
    }

    /// Returns true when this was a Carson message belonging to an active talk session.
    pub async fn handle_carson_message(&self, ctx: &Context, message: &Message) -> bool {
        if !message.author.bot || message.author.id.get() != self.config.carson_user_id {
            return false;
        }

        let channel_id = message.channel_id;
        let claim;
        {
            let mut sessions = self.sessions.lock().await;
            let Some(session) = sessions.get_mut(&channel_id.get()) else {
                return false;
            };
            if session.last_activity.elapsed() >= SESSION_TIMEOUT {
                sessions.remove(&channel_id.get());
                return false;
            }
            if !session.awaiting_carson {
                return true;
            }

            session.last_activity = Instant::now();
            session.message_count += 1;
            session
                .history
                .push(ChatMessage::user(&truncate_chars(&message.content)));
            claim = if session.message_count >= session.target_messages {
                sessions.remove(&channel_id.get());
                None
            } else {
                session.awaiting_carson = false;
                Some((session.id, session.history.clone()))
            };
            drop(sessions);
        }

        let Some((session_id, history)) = claim else {
            return true;
        };
        let reply = match self.api.reply(&history).await {
            Ok(reply) => reply,
            Err(error) => {
                error!(
                    ?error,
                    channel_id = channel_id.get(),
                    "OpenAI talk request failed"
                );
                self.remove_session(channel_id, session_id).await;
                return true;
            }
        };
        {
            let mut sessions = self.sessions.lock().await;
            let Some(session) = sessions.get_mut(&channel_id.get()) else {
                return true;
            };
            if session.id != session_id {
                return true;
            }
            session.history.push(ChatMessage::assistant(&reply));
            session.message_count += 1;
            session.last_activity = Instant::now();
            if session.message_count >= session.target_messages {
                sessions.remove(&channel_id.get());
            } else {
                // Mark the session ready before sending. Carson can reply immediately
                // after Discord accepts Hugh's message without losing that event.
                session.awaiting_carson = true;
            }
            drop(sessions);
        }
        if let Err(error) = self.send_to_carson(ctx, channel_id, &reply).await {
            error!(
                ?error,
                channel_id = channel_id.get(),
                "failed to send Hugh's talk reply"
            );
            self.remove_session(channel_id, session_id).await;
        }
        true
    }

    async fn send_to_carson(
        &self,
        ctx: &Context,
        channel_id: ChannelId,
        content: &str,
    ) -> serenity::Result<()> {
        let carson = UserId::new(self.config.carson_user_id);
        channel_id
            .send_message(
                &ctx.http,
                CreateMessage::new()
                    .content(format!("<@{}> {}", carson.get(), content.trim()))
                    .allowed_mentions(CreateAllowedMentions::new().users([carson])),
            )
            .await?;
        Ok(())
    }

    async fn remove_session(&self, channel_id: ChannelId, session_id: u64) {
        let mut sessions = self.sessions.lock().await;
        if sessions
            .get(&channel_id.get())
            .is_some_and(|session| session.id == session_id)
        {
            sessions.remove(&channel_id.get());
        }
    }
}

struct OpenAiClient {
    http: Client,
    api_key: Arc<str>,
}

impl OpenAiClient {
    fn new(api_key: String) -> Result<Self> {
        Ok(Self {
            http: Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .context("failed to create the OpenAI HTTP client")?,
            api_key: Arc::from(api_key),
        })
    }

    async fn reply(&self, history: &[ChatMessage]) -> Result<String> {
        let response = self
            .http
            .post(OPENAI_RESPONSES_URL)
            .bearer_auth(&self.api_key)
            .json(&ResponseRequest {
                model: OPENAI_MODEL,
                instructions: HUGH_INSTRUCTIONS,
                input: history,
                max_output_tokens: MAX_OUTPUT_TOKENS,
                reasoning: Reasoning { effort: "minimal" },
            })
            .send()
            .await
            .context("OpenAI request could not be sent")?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("OpenAI response body could not be read")?;
        if !status.is_success() {
            bail!("OpenAI returned HTTP {status}");
        }
        let response: ResponseBody =
            serde_json::from_str(&body).context("OpenAI returned an unexpected response")?;
        extract_output_text(&response).context("OpenAI returned no text reply")
    }
}

#[derive(Clone, Serialize)]
struct ChatMessage {
    role: &'static str,
    content: String,
}

impl ChatMessage {
    fn assistant(content: &str) -> Self {
        Self {
            role: "assistant",
            content: content.to_owned(),
        }
    }

    fn user(content: &str) -> Self {
        Self {
            role: "user",
            content: content.to_owned(),
        }
    }
}

#[derive(Serialize)]
struct ResponseRequest<'a> {
    model: &'static str,
    instructions: &'static str,
    input: &'a [ChatMessage],
    max_output_tokens: u16,
    reasoning: Reasoning,
}

#[derive(Serialize)]
struct Reasoning {
    effort: &'static str,
}

#[derive(Deserialize)]
struct ResponseBody {
    #[serde(default)]
    output: Vec<ResponseOutput>,
}

#[derive(Deserialize)]
struct ResponseOutput {
    #[serde(default)]
    content: Vec<ResponseContent>,
}

#[derive(Deserialize)]
struct ResponseContent {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

struct TalkSession {
    id: u64,
    target_messages: usize,
    message_count: usize,
    history: Vec<ChatMessage>,
    awaiting_carson: bool,
    last_activity: Instant,
}

fn extract_output_text(response: &ResponseBody) -> Option<String> {
    let text = response
        .output
        .iter()
        .flat_map(|item| &item.content)
        .filter(|content| content.kind == "output_text")
        .filter_map(|content| content.text.as_deref())
        .collect::<Vec<_>>()
        .join("");
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn truncate_chars(value: &str) -> String {
    let mut chars = value.chars();
    let shortened: String = chars.by_ref().take(MAX_HISTORY_MESSAGE_CHARS).collect();
    if chars.next().is_some() {
        format!("{shortened}…")
    } else {
        shortened
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_ten_distinct_icebreakers() {
        assert_eq!(ICEBREAKERS.len(), 10);
        for (index, icebreaker) in ICEBREAKERS.iter().enumerate() {
            assert!(!icebreaker.trim().is_empty());
            assert!(!ICEBREAKERS[..index].contains(icebreaker));
        }
    }

    #[test]
    fn extracts_response_output_text() {
        let response: ResponseBody = serde_json::from_str(
            r#"{"output":[{"content":[{"type":"output_text","text":"Evening, Carson."}]}]}"#,
        )
        .unwrap();
        assert_eq!(
            extract_output_text(&response).as_deref(),
            Some("Evening, Carson.")
        );
    }

    #[test]
    fn truncation_is_unicode_safe() {
        let input = "🔦".repeat(MAX_HISTORY_MESSAGE_CHARS + 1);
        assert_eq!(
            truncate_chars(&input).chars().count(),
            MAX_HISTORY_MESSAGE_CHARS + 1
        );
        assert!(truncate_chars(&input).ends_with('…'));
    }
}
