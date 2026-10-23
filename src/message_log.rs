//! The staff message log: what a deleted message said, and what an edited one
//! said before. Discord's own events carry neither, so both come from the
//! archive.

use std::{collections::BTreeSet, fmt::Write as _};

use chrono::{DateTime, SecondsFormat, Utc};
use serenity::{
    all::{ChannelId, Http, Message},
    builder::{
        CreateAllowedMentions, CreateAttachment, CreateEmbed, CreateEmbedFooter, CreateMessage,
    },
};
use tracing::warn;

use crate::{archive::StoredMessage, config::Config, discipline::truncate};

/// Room a quoted message gets in a card's description.
const QUOTE_LIMIT: usize = 3_500;
/// Members named on a bulk-deletion card before the rest are counted instead.
const NAMED_AUTHORS: usize = 10;

/// Posts what Discord just deleted. One message gets a card of its own; a bulk
/// deletion gets one card and a transcript, so a purge cannot flood the log.
pub async fn deleted(http: &Http, config: &Config, messages: &[StoredMessage]) {
    let log = &config.message_log;
    let Some(channel) = log.channel_id.filter(|_| log.enabled && log.log_deletes) else {
        return;
    };
    let messages: Vec<&StoredMessage> = messages
        .iter()
        .filter(|message| !log.ignored_channel_ids.contains(&message.channel_id))
        .collect();
    let post = match messages.as_slice() {
        [] => return,
        [message] => CreateMessage::new().embed(deleted_card(config, message)),
        many => {
            let (embed, transcript) = bulk_card(config, many);
            CreateMessage::new().embed(embed).add_file(transcript)
        }
    };
    send(http, ChannelId::new(channel), post).await;
}

/// Posts an edit beside the text it replaced.
pub async fn edited(http: &Http, config: &Config, message: &Message, previous: &str) {
    let log = &config.message_log;
    let Some(channel) = log.channel_id.filter(|_| log.enabled && log.log_edits) else {
        return;
    };
    if log.ignored_channel_ids.contains(&message.channel_id.get()) {
        return;
    }
    let embed = CreateEmbed::new()
        .title(format!("{}Message edited", config.mark("details")))
        .description(format!(
            "<@{}> in <#{}> · [jump to message]({})",
            message.author.id,
            message.channel_id,
            message.link()
        ))
        .field("Before", quote(previous, 1_024), false)
        .field("After", quote(&message.content, 1_024), false)
        .colour(0x00F0_B232)
        .footer(CreateEmbedFooter::new(format!("Message {}", message.id)));
    send(
        http,
        ChannelId::new(channel),
        CreateMessage::new().embed(embed),
    )
    .await;
}

fn deleted_card(config: &Config, message: &StoredMessage) -> CreateEmbed {
    let text = message.content.as_deref().map_or_else(
        || "*The archive no longer holds this message's text.*".to_owned(),
        |content| quote(content, QUOTE_LIMIT),
    );
    let mut embed = CreateEmbed::new()
        .title(format!("{}Message deleted", config.mark("deleted")))
        .description(format!(
            "<@{}> in <#{}> · sent <t:{}:R>\n\n{text}",
            message.author_id,
            message.channel_id,
            message.created_at / 1_000
        ))
        .colour(0x00ED_4245);
    if !message.attachments.is_empty() {
        embed = embed.field(
            format!("{}Attachments", config.mark("image")),
            truncate(&message.attachments.join("\n"), 1_024),
            false,
        );
    }
    embed.footer(CreateEmbedFooter::new(format!(
        "Message {}{}",
        message.id,
        removed_by(message)
    )))
}

fn bulk_card(config: &Config, messages: &[&StoredMessage]) -> (CreateEmbed, CreateAttachment) {
    let authors: BTreeSet<u64> = messages.iter().map(|message| message.author_id).collect();
    let named = authors
        .iter()
        .take(NAMED_AUTHORS)
        .map(|id| format!("<@{id}>"))
        .collect::<Vec<_>>()
        .join(", ");
    let others = authors.len().saturating_sub(NAMED_AUTHORS);
    let others = if others > 0 {
        format!(" and {others} more")
    } else {
        String::new()
    };
    let channel = messages[0].channel_id;
    let embed = CreateEmbed::new()
        .title(format!(
            "{}{} messages deleted",
            config.mark("deleted"),
            messages.len()
        ))
        .description(format!(
            "In <#{channel}>, from {named}{others}. Every message is in the attached transcript."
        ))
        .colour(0x00ED_4245);

    let transcript = messages
        .iter()
        .map(|message| {
            let when = DateTime::<Utc>::from_timestamp_millis(message.created_at).map_or_else(
                || message.created_at.to_string(),
                |at| at.to_rfc3339_opts(SecondsFormat::Secs, true),
            );
            let mut line = format!(
                "[{when}] {} (message {}){}: {}",
                message.author_id,
                message.id,
                removed_by(message),
                message
                    .content
                    .as_deref()
                    .unwrap_or("<text no longer archived>")
            );
            if !message.attachments.is_empty() {
                let _ = write!(line, " [attachments: {}]", message.attachments.join(", "));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n");
    let name = format!(
        "deleted-{channel}-{}.txt",
        Utc::now().format("%Y%m%d-%H%M%S")
    );
    (
        embed,
        CreateAttachment::bytes(transcript.into_bytes(), name),
    )
}

fn removed_by(message: &StoredMessage) -> String {
    if !message.flagged {
        return String::new();
    }
    message.case_id.map_or_else(
        || " · removed by Hugh automod".to_owned(),
        |case| format!(" · removed by Hugh, Case #{case}"),
    )
}

/// A message as a block quote, so it cannot be mistaken for Hugh's own words.
pub fn quote(content: &str, maximum: usize) -> String {
    if content.trim().is_empty() {
        return "*No text*".to_owned();
    }
    let quoted = content
        .lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    truncate(&quoted, maximum)
}

async fn send(http: &Http, channel: ChannelId, message: CreateMessage) {
    // A logged message can hold any mention its author typed, and the log
    // must never ping them again.
    let message = message.allowed_mentions(CreateAllowedMentions::new());
    if let Err(error) = channel.send_message(http, message).await {
        warn!(
            ?error,
            channel_id = channel.get(),
            "failed to post to the message log"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        let config: Config = toml::from_str(
            "guild_id = \"1\"\nincident_channel_id = \"2\"\n\n[role_guard]\nenabled = false",
        )
        .unwrap();
        config.validate().unwrap();
        config
    }

    fn stored(id: u64, author_id: u64, content: Option<&str>) -> StoredMessage {
        StoredMessage {
            id,
            channel_id: 30,
            author_id,
            created_at: 1_700_000_000_000,
            content: content.map(str::to_owned),
            attachments: Vec::new(),
            flagged: false,
            case_id: None,
        }
    }

    #[test]
    fn quotes_every_line_and_marks_empty_text() {
        assert_eq!(quote("one\ntwo", 100), "> one\n> two");
        assert_eq!(quote("   ", 100), "*No text*");
        assert_eq!(quote(&"a".repeat(50), 10).chars().count(), 10);
    }

    #[test]
    fn a_deletion_card_says_who_removed_it() {
        let config = config();
        let mut message = stored(5, 7, Some("buy cheap nitro"));
        message.flagged = true;
        message.case_id = Some(12);
        let card = serde_json::to_value(deleted_card(&config, &message)).unwrap();
        let description = card["description"].as_str().unwrap();
        assert!(description.contains("<@7> in <#30>"));
        assert!(description.contains("> buy cheap nitro"));
        assert_eq!(
            card["footer"]["text"].as_str().unwrap(),
            "Message 5 · removed by Hugh, Case #12"
        );

        let gone = serde_json::to_value(deleted_card(&config, &stored(6, 7, None))).unwrap();
        assert!(
            gone["description"]
                .as_str()
                .unwrap()
                .contains("no longer holds")
        );
    }

    #[test]
    fn a_bulk_deletion_names_each_author_once() {
        let config = config();
        let first = stored(1, 7, Some("one"));
        let second = stored(2, 8, Some("two"));
        let third = stored(3, 7, None);
        let (card, _) = bulk_card(&config, &[&first, &second, &third]);
        let card = serde_json::to_value(card).unwrap();
        assert_eq!(card["title"].as_str().unwrap(), "3 messages deleted");
        assert!(
            card["description"]
                .as_str()
                .unwrap()
                .starts_with("In <#30>, from <@7>, <@8>.")
        );
    }
}
