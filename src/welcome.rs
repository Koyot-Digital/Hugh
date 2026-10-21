//! The greeting Hugh posts when somebody joins: one plain message in the
//! configured channel, with the member's place in the server spelled out.

use serenity::{
    all::{ChannelId, Context, GuildId, Member},
    builder::{CreateAllowedMentions, CreateMessage},
};
use tracing::{error, warn};

use crate::config::Config;

/// Posts the greeting, unless configuration or a running raid says not to.
pub async fn greet(ctx: &Context, config: &Config, member: &Member, raid_active: bool) {
    if !config.welcome.enabled || (config.welcome.skip_bots && member.user.bot) {
        return;
    }
    if raid_active && config.welcome.skip_during_raid {
        warn!(
            user_id = member.user.id.get(),
            "skipped the welcome message while raid mode is active"
        );
        return;
    }
    let Some(channel_id) = config.welcome.channel_id else {
        return;
    };

    let count = member_count(ctx, member.guild_id).await;
    let server = guild_name(ctx, member.guild_id).await;
    let content = render(&config.welcome.message, member, &server, count);
    // Only the new member is mentionable, so a template with @everyone in it
    // cannot turn the greeting into a server-wide ping.
    let message = CreateMessage::new()
        .content(content)
        .allowed_mentions(CreateAllowedMentions::new().users(vec![member.user.id]));
    if let Err(error) = ChannelId::new(channel_id)
        .send_message(&ctx.http, message)
        .await
    {
        error!(
            ?error,
            channel_id,
            user_id = member.user.id.get(),
            "failed to post the welcome message"
        );
    }
}

/// Fills a welcome template in. Unknown placeholders are left alone so a typo
/// is visible in the channel rather than silently swallowed.
#[must_use]
#[allow(clippy::literal_string_with_formatting_args)]
pub fn render(template: &str, member: &Member, server: &str, count: Option<u64>) -> String {
    let user = &member.user;
    let display = member
        .nick
        .clone()
        .or_else(|| user.global_name.clone())
        .unwrap_or_else(|| user.name.clone());
    let replacements = [
        ("{user}", format!("<@{}>", user.id.get())),
        ("{username}", user.name.clone()),
        ("{display}", display),
        ("{server}", server.to_owned()),
        ("{id}", user.id.get().to_string()),
        (
            "{count}",
            count.map_or_else(|| "newest".to_owned(), |count| count.to_string()),
        ),
        (
            "{ordinal}",
            count.map_or_else(|| "newest".to_owned(), ordinal),
        ),
    ];
    let mut rendered = template.to_owned();
    for (placeholder, value) in replacements {
        rendered = rendered.replace(placeholder, &value);
    }
    rendered
}

/// `15047` becomes `15047th`, `21` becomes `21st`.
#[must_use]
pub fn ordinal(number: u64) -> String {
    let suffix = match (number % 100, number % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    format!("{number}{suffix}")
}

/// The member count, from the cache where the gateway keeps it current and
/// from the API when the cache has not caught up.
async fn member_count(ctx: &Context, guild_id: GuildId) -> Option<u64> {
    if let Some(count) = ctx
        .cache
        .guild(guild_id)
        .map(|guild| guild.member_count)
        .filter(|count| *count > 0)
    {
        return Some(count);
    }
    match guild_id.to_partial_guild_with_counts(&ctx.http).await {
        Ok(guild) => guild.approximate_member_count,
        Err(error) => {
            warn!(
                ?error,
                guild_id = guild_id.get(),
                "could not read the member count for the welcome message"
            );
            None
        }
    }
}

async fn guild_name(ctx: &Context, guild_id: GuildId) -> String {
    if let Some(name) = ctx.cache.guild(guild_id).map(|guild| guild.name.clone()) {
        return name;
    }
    guild_id
        .to_partial_guild(&ctx.http)
        .await
        .map_or_else(|_| "the server".to_owned(), |guild| guild.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinals_handle_the_teens_and_the_tens() {
        for (number, expected) in [
            (1, "1st"),
            (2, "2nd"),
            (3, "3rd"),
            (4, "4th"),
            (11, "11th"),
            (12, "12th"),
            (13, "13th"),
            (21, "21st"),
            (102, "102nd"),
            (111, "111th"),
            (15_047, "15047th"),
        ] {
            assert_eq!(ordinal(number), expected);
        }
    }
}
