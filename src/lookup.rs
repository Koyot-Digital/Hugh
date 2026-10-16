//! The `/rotector` staff command: account safety lookups against the Rotector
//! API, rendered as compact, readable case cards.

use anyhow::{Context as _, Result, bail};
use serenity::{
    all::{Colour, CommandDataOption, CommandInteraction, CommandOptionType, Context, UserId},
    builder::{
        CreateCommand, CreateCommandOption, CreateEmbed, CreateEmbedFooter, EditInteractionResponse,
    },
};

use crate::{
    commands::{option_integer, option_string, option_user, subcommand},
    config::Config,
    incident::{Incident, IncidentReporter, UserSnapshot},
    rotector::{
        AltAccount, DiscordAccountLink, DiscordUser, FlagType, LinkedRobloxAccount, Provider,
        Reason, RobloxDiscordLinks, RobloxGroup, RobloxUser, RotectorClient, WEBSITE_URL,
    },
};

/// Discord caps an embed field value at 1024 characters.
const FIELD_LIMIT: usize = 1_024;
const MAX_LISTED: usize = 8;
const UNKNOWN_COLOUR: (u8, u8, u8) = (130, 140, 155);

pub fn command() -> CreateCommand {
    CreateCommand::new("rotector")
        .description("Look up account safety findings from Rotector")
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "discord",
                "Check a Discord account",
            )
            .add_sub_option(CreateCommandOption::new(
                CommandOptionType::User,
                "user",
                "Member to check",
            ))
            .add_sub_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "id",
                    "Discord user ID, for accounts that are not in this server",
                )
                .min_length(2)
                .max_length(20),
            ),
        )
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "roblox",
                "Check a Roblox account",
            )
            .add_sub_option(
                CreateCommandOption::new(CommandOptionType::Integer, "id", "Roblox user ID")
                    .required(true)
                    .min_int_value(1),
            ),
        )
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "links",
                "Show Discord accounts and alts linked to a Roblox account",
            )
            .add_sub_option(
                CreateCommandOption::new(CommandOptionType::Integer, "id", "Roblox user ID")
                    .required(true)
                    .min_int_value(1),
            ),
        )
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::SubCommand,
                "group",
                "Check a Roblox group",
            )
            .add_sub_option(
                CreateCommandOption::new(CommandOptionType::Integer, "id", "Roblox group ID")
                    .required(true)
                    .min_int_value(1),
            ),
        )
        .add_option(CreateCommandOption::new(
            CommandOptionType::SubCommand,
            "sources",
            "List the detection sources Rotector currently reports on",
        ))
}

pub async fn handle(
    ctx: &Context,
    command: &CommandInteraction,
    config: &Config,
    client: Option<&RotectorClient>,
    incidents: &IncidentReporter,
) -> Result<()> {
    let client = client.context("Rotector lookups are disabled in Hugh's configuration")?;
    let (name, options) = subcommand(command).context("/rotector needs a subcommand")?;
    command.defer_ephemeral(&ctx.http).await?;

    let outcome = match name {
        "discord" => discord(command, client, options).await?,
        "roblox" => roblox(client, options).await?,
        "links" => links(client, options).await?,
        "group" => group(client, options).await?,
        "sources" => sources(client).await?,
        other => bail!("unknown /rotector subcommand {other:?}"),
    };

    let logged = outcome
        .incident(config)
        .filter(|_| config.rotector.log_flagged_lookups);
    // Answer the moderator first; the case log must not delay their result.
    command
        .edit_response(
            &ctx.http,
            EditInteractionResponse::new().embed(outcome.embed),
        )
        .await?;
    if let Some(mut incident) = logged {
        incident.moderator = Some((&command.user).into());
        incident.channel_id = Some(command.channel_id.get());
        incidents.report(&ctx.http, incident).await;
    }
    Ok(())
}

/// A rendered lookup plus the few facts a case log needs about it.
struct Outcome {
    embed: CreateEmbed,
    finding: Option<Finding>,
}

struct Finding {
    lookup: &'static str,
    target: String,
    verdict: String,
    reason_types: String,
    subject: Option<UserSnapshot>,
}

impl Outcome {
    const fn plain(embed: CreateEmbed) -> Self {
        Self {
            embed,
            finding: None,
        }
    }

    fn incident(&self, config: &Config) -> Option<Incident> {
        let finding = self.finding.as_ref()?;
        let mut incident = Incident::new(
            "rotector_lookup",
            config.guild_id,
            "rotector_match",
            format!("Rotector reports: {}", finding.verdict),
        );
        incident.subject.clone_from(&finding.subject);
        incident
            .details
            .insert("lookup".into(), finding.lookup.into());
        incident
            .details
            .insert("target".into(), finding.target.clone().into());
        incident
            .details
            .insert("verdict".into(), finding.verdict.clone().into());
        if !finding.reason_types.is_empty() {
            incident
                .details
                .insert("reason_types".into(), finding.reason_types.clone().into());
        }
        Some(incident)
    }
}

async fn discord(
    command: &CommandInteraction,
    client: &RotectorClient,
    options: &[CommandDataOption],
) -> Result<Outcome> {
    let selected = option_user(options, "user");
    let typed = option_string(options, "id")
        .map(|id| {
            id.parse::<u64>()
                .ok()
                .filter(|id| *id > 0)
                .context("that Discord ID is not a valid snowflake")
        })
        .transpose()?;
    let user_id = selected
        .map(UserId::get)
        .or(typed)
        .context("choose a member or pass a Discord user ID")?;

    let Some(data) = client.discord_user(user_id).await? else {
        return Ok(Outcome::plain(no_record(
            "Discord account",
            &format!("<@{user_id}> (`{user_id}`)"),
        )));
    };
    let flag = FlagType::from_code(data.flag_type);
    let subject =
        selected.and_then(|id| command.data.resolved.users.get(&id).map(UserSnapshot::from));

    Ok(Outcome {
        embed: discord_embed(&data, flag, user_id),
        finding: flag.is_finding().then(|| Finding {
            lookup: "discord_user",
            target: user_id.to_string(),
            verdict: verdict(flag, data.status_label.as_deref()),
            reason_types: reason_types(&data.reasons),
            subject,
        }),
    })
}

fn discord_embed(data: &DiscordUser, flag: FlagType, user_id: u64) -> CreateEmbed {
    let mut embed = CreateEmbed::new()
        .title("Rotector · Discord account check")
        .colour(flag.colour())
        .field(
            "Account",
            format!("<@{user_id}>\n`{}`", data.id.trim()),
            true,
        )
        .field("Verdict", verdict(flag, data.status_label.as_deref()), true)
        .field(
            "Linked flagged Roblox accounts",
            data.linked_roblox_accounts.len().to_string(),
            true,
        );
    if let Some(header) = readable(data.header_message.as_deref()) {
        embed = embed.description(header);
    }
    if let Some(findings) = reason_lines(&data.reasons) {
        embed = embed.field("Findings", findings, false);
    }
    if !data.linked_roblox_accounts.is_empty() {
        embed = embed.field(
            "Linked Roblox accounts",
            linked_roblox_lines(&data.linked_roblox_accounts),
            false,
        );
    }
    embed.footer(footer(
        "Rotector reports only flagged links; a clean lookup is not a clearance",
    ))
}

async fn roblox(client: &RotectorClient, options: &[CommandDataOption]) -> Result<Outcome> {
    let id = roblox_id(options)?;
    let Some(data) = client.roblox_user(id).await? else {
        return Ok(Outcome::plain(no_record(
            "Roblox account",
            &roblox_user_link(id),
        )));
    };
    let flag = FlagType::from_code(data.flag_type);
    Ok(Outcome {
        embed: roblox_embed(&data, flag),
        finding: flag.is_finding().then(|| Finding {
            lookup: "roblox_user",
            target: id.to_string(),
            verdict: verdict(flag, data.status_label.as_deref()),
            reason_types: reason_types(&data.reasons),
            subject: None,
        }),
    })
}

fn roblox_embed(data: &RobloxUser, flag: FlagType) -> CreateEmbed {
    let id = data.id;
    let mut embed = CreateEmbed::new()
        .title(format!("Rotector · Roblox user {id}"))
        .colour(flag.colour())
        .field("Account", roblox_user_link(id), true)
        .field("Verdict", verdict(flag, data.status_label.as_deref()), true);
    if let Some(category) = readable(data.category_label.as_deref()) {
        embed = embed.field("Category", category, true);
    }
    if let Some(header) = readable(data.header_message.as_deref()) {
        embed = embed.description(header);
    }
    if let Some(findings) = reason_lines(&data.reasons) {
        embed = embed.field("Findings", findings, false);
    }
    if let Some(provisional) = reason_lines(&data.provisional_reasons) {
        embed = embed.field("Awaiting human review", provisional, false);
    }

    let mut notes = Vec::new();
    if let Some(reviewer) = &data.reviewer {
        let name = reviewer.display_name.as_ref().map_or_else(
            || reviewer.username.clone(),
            |display| format!("{} ({display})", reviewer.username),
        );
        notes.push(format!("Verified by **{name}**"));
    }
    if data.is_locked == Some(true) {
        notes.push("The Roblox account is locked or banned".to_owned());
    }
    if let Some(updated) = data.last_updated.filter(|value| *value > 0) {
        notes.push(format!("Last analysed <t:{updated}:R>"));
    }
    for url in data.record_urls.iter().take(3) {
        notes.push(format!("Full records: {url}"));
    }
    if !notes.is_empty() {
        embed = embed.field("Notes", clamp(&notes.join("\n")), false);
    }
    embed.footer(footer(
        "Findings are Rotector's judgement, not Roblox moderation",
    ))
}

async fn links(client: &RotectorClient, options: &[CommandDataOption]) -> Result<Outcome> {
    let id = roblox_id(options)?;
    let Some(data) = client.roblox_user_links(id).await? else {
        return Ok(Outcome::plain(no_record(
            "Roblox account",
            &roblox_user_link(id),
        )));
    };
    Ok(Outcome::plain(links_embed(&data, id)))
}

fn links_embed(data: &RobloxDiscordLinks, id: u64) -> CreateEmbed {
    let mut embed = CreateEmbed::new()
        .title(format!("Rotector · Accounts linked to Roblox user {id}"))
        .colour(Colour::from_rgb(88, 101, 242))
        .field(
            "Roblox account",
            roblox_user_link(data.roblox_user_id),
            true,
        )
        .field(
            "Linked Discord accounts",
            data.discord_accounts.len().to_string(),
            true,
        )
        .field("Known alts", data.alt_accounts.len().to_string(), true);
    if data.discord_accounts.is_empty() && data.alt_accounts.is_empty() {
        embed = embed.description("Rotector has no linked accounts on record for this user.");
    }
    if !data.discord_accounts.is_empty() {
        embed = embed.field(
            "Discord accounts",
            discord_link_lines(&data.discord_accounts),
            false,
        );
    }
    if !data.alt_accounts.is_empty() {
        embed = embed.field(
            "Other Roblox accounts",
            alt_lines(&data.alt_accounts),
            false,
        );
    }
    embed.footer(footer(
        "Links are observations, not proof that the same person owns both accounts",
    ))
}

async fn group(client: &RotectorClient, options: &[CommandDataOption]) -> Result<Outcome> {
    let id = roblox_id(options)?;
    let Some(data) = client.roblox_group(id).await? else {
        return Ok(Outcome::plain(no_record(
            "Roblox group",
            &roblox_group_link(id),
        )));
    };
    let flag = FlagType::from_code(data.flag_type);
    Ok(Outcome {
        embed: group_embed(&data, flag),
        finding: flag.is_finding().then(|| Finding {
            lookup: "roblox_group",
            target: id.to_string(),
            verdict: verdict(flag, data.status_label.as_deref()),
            reason_types: reason_types(&data.reasons),
            subject: None,
        }),
    })
}

fn group_embed(data: &RobloxGroup, flag: FlagType) -> CreateEmbed {
    let id = data.id;
    let mut embed = CreateEmbed::new()
        .title(format!("Rotector · Roblox group {id}"))
        .colour(flag.colour())
        .field("Group", roblox_group_link(id), true)
        .field("Verdict", verdict(flag, data.status_label.as_deref()), true);
    if let Some(header) = readable(data.header_message.as_deref()) {
        embed = embed.description(header);
    }
    if let Some(findings) = reason_lines(&data.reasons) {
        embed = embed.field("Findings", findings, false);
    }
    embed.footer(footer(
        "Group findings describe the group, not every member",
    ))
}

async fn sources(client: &RotectorClient) -> Result<Outcome> {
    let providers = client.providers().await?;
    Ok(Outcome::plain(sources_embed(&providers)))
}

fn sources_embed(providers: &[Provider]) -> CreateEmbed {
    let mut embed = CreateEmbed::new()
        .title("Rotector · Detection sources")
        .colour(Colour::from_rgb(88, 101, 242))
        .footer(footer("Source list reported by the Rotector API"));
    if providers.is_empty() {
        return embed.description("Rotector reported no enabled sources.");
    }
    embed = embed.description(format!(
        "{} source(s) are currently enabled for Hugh's key.",
        providers.len()
    ));
    for provider in providers.iter().take(10) {
        let mut body = vec![
            readable(provider.description.as_deref())
                .unwrap_or_else(|| "No description provided.".to_owned()),
        ];
        if let Some(website) = readable(provider.website_url.as_deref()) {
            body.push(website);
        }
        if let Some(appeal) = readable(provider.appeal_url.as_deref()) {
            body.push(format!("Appeals: {appeal}"));
        }
        let body = body.join("\n");
        embed = embed.field(
            format!("{} (`{}`)", provider.name, provider.id),
            clamp(&body),
            false,
        );
    }
    embed
}

fn no_record(kind: &str, target: &str) -> CreateEmbed {
    CreateEmbed::new()
        .title(format!("Rotector · No record for this {kind}"))
        .colour(Colour::from_rgb(
            UNKNOWN_COLOUR.0,
            UNKNOWN_COLOUR.1,
            UNKNOWN_COLOUR.2,
        ))
        .field("Looked up", target, false)
        .description(
            "Rotector holds no record for this identifier. That is not a clean verdict: the \
             account may simply never have been analysed.",
        )
        .footer(footer("Rotector lookup"))
}

fn roblox_id(options: &[CommandDataOption]) -> Result<u64> {
    let id = option_integer(options, "id").context("missing id option")?;
    u64::try_from(id)
        .ok()
        .filter(|id| *id > 0)
        .context("the ID must be a positive whole number")
}

fn verdict(flag: FlagType, status_label: Option<&str>) -> String {
    match readable(status_label) {
        Some(label) if !label.eq_ignore_ascii_case(flag.label()) => {
            format!("**{}**\n{label}", flag.label())
        }
        _ => format!("**{}**", flag.label()),
    }
}

fn reason_types(reasons: &[Reason]) -> String {
    reasons
        .iter()
        .map(|reason| reason.kind.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn reason_lines(reasons: &[Reason]) -> Option<String> {
    if reasons.is_empty() {
        return None;
    }
    let mut lines = Vec::new();
    for reason in reasons.iter().take(MAX_LISTED) {
        let heading = format!("**{}**", reason.title.trim());
        let detail = reason
            .sources
            .iter()
            .map(|source| {
                let found = readable(source.summary.as_deref()).map_or_else(
                    || source.label.trim().to_owned(),
                    |summary| format!("{}: {summary}", source.label.trim()),
                );
                readable(source.credit.as_deref()).map_or_else(
                    || found.clone(),
                    |credit| format!("{found} (data from {credit})"),
                )
            })
            .collect::<Vec<_>>()
            .join(" · ");
        lines.push(if detail.is_empty() {
            heading
        } else {
            format!("{heading}\n{detail}")
        });
    }
    if reasons.len() > MAX_LISTED {
        lines.push(format!("…and {} more", reasons.len() - MAX_LISTED));
    }
    Some(clamp(&lines.join("\n")))
}

fn linked_roblox_lines(accounts: &[LinkedRobloxAccount]) -> String {
    let mut lines = accounts
        .iter()
        .take(MAX_LISTED)
        .map(|account| {
            let flag = FlagType::from_code(account.flag_type);
            let detected = account
                .detected_at
                .filter(|value| *value > 0)
                .map(|value| format!(" · detected <t:{value}:R>"))
                .unwrap_or_default();
            format!(
                "[{}](https://www.roblox.com/users/{}/profile) — {}{detected}",
                account.roblox_username.trim(),
                account.roblox_user_id,
                flag.label()
            )
        })
        .collect::<Vec<_>>();
    if accounts.len() > MAX_LISTED {
        lines.push(format!("…and {} more", accounts.len() - MAX_LISTED));
    }
    clamp(&lines.join("\n"))
}

fn discord_link_lines(accounts: &[DiscordAccountLink]) -> String {
    let mut lines = accounts
        .iter()
        .take(MAX_LISTED)
        .map(|account| {
            let id = account.id.trim();
            // Developer keys receive masked IDs, which must not be rendered as
            // a mention.
            let name = if id.chars().all(|character| character.is_ascii_digit()) {
                format!("<@{id}> (`{id}`)")
            } else {
                format!("`{id}`")
            };
            let detected = account
                .detected_at
                .filter(|value| *value > 0)
                .map(|value| format!(" · detected <t:{value}:R>"))
                .unwrap_or_default();
            let servers = if account.servers.is_empty() {
                String::new()
            } else {
                let names = account
                    .servers
                    .iter()
                    .take(3)
                    .map(|server| server.safe_name.trim())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("\nTracked servers ({}): {names}", account.servers.len())
            };
            format!("{name}{detected}{servers}")
        })
        .collect::<Vec<_>>();
    if accounts.len() > MAX_LISTED {
        lines.push(format!("…and {} more", accounts.len() - MAX_LISTED));
    }
    clamp(&lines.join("\n"))
}

fn alt_lines(accounts: &[AltAccount]) -> String {
    let mut lines = accounts
        .iter()
        .take(MAX_LISTED)
        .map(|account| {
            let detected = account
                .detected_at
                .filter(|value| *value > 0)
                .map(|value| format!(" · detected <t:{value}:R>"))
                .unwrap_or_default();
            format!(
                "[{}](https://www.roblox.com/users/{}/profile){detected}",
                account.roblox_username.trim(),
                account.roblox_user_id
            )
        })
        .collect::<Vec<_>>();
    if accounts.len() > MAX_LISTED {
        lines.push(format!("…and {} more", accounts.len() - MAX_LISTED));
    }
    clamp(&lines.join("\n"))
}

fn roblox_user_link(id: u64) -> String {
    format!("[Open profile](https://www.roblox.com/users/{id}/profile)\n`{id}`")
}

fn roblox_group_link(id: u64) -> String {
    format!("[Open group](https://www.roblox.com/groups/{id})\n`{id}`")
}

fn footer(note: &str) -> CreateEmbedFooter {
    CreateEmbedFooter::new(format!("{note} • {WEBSITE_URL}"))
}

fn readable(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn clamp(value: &str) -> String {
    if value.chars().count() <= FIELD_LIMIT {
        return value.to_owned();
    }
    value
        .chars()
        .take(FIELD_LIMIT - 1)
        .chain(std::iter::once('…'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rotector::Source;

    fn reason(title: &str, summary: Option<&str>) -> Reason {
        Reason {
            kind: title.to_lowercase(),
            title: title.to_owned(),
            sources: vec![Source {
                label: "Rotector".to_owned(),
                summary: summary.map(str::to_owned),
                credit: None,
            }],
        }
    }

    #[test]
    fn findings_read_as_titled_bullets() {
        let lines = reason_lines(&[reason("Profile", Some("Inappropriate bio"))]).unwrap();
        assert_eq!(lines, "**Profile**\nRotector: Inappropriate bio");
        assert!(reason_lines(&[]).is_none());
    }

    #[test]
    fn long_findings_are_summarised_and_clamped() {
        let many: Vec<Reason> = (0..12)
            .map(|index| reason(&format!("Reason {index}"), None))
            .collect();
        let lines = reason_lines(&many).unwrap();
        assert!(lines.contains("…and 4 more"));
        assert!(lines.chars().count() <= FIELD_LIMIT);
    }

    #[test]
    fn verdict_avoids_repeating_the_same_label() {
        assert_eq!(
            verdict(FlagType::Confirmed, Some("Unsafe")),
            "**Confirmed**\nUnsafe"
        );
        assert_eq!(
            verdict(FlagType::Safe, Some("no findings")),
            "**No findings**"
        );
        assert_eq!(verdict(FlagType::Safe, None), "**No findings**");
    }

    #[test]
    fn masked_discord_ids_are_not_rendered_as_mentions() {
        let lines = discord_link_lines(&[DiscordAccountLink {
            id: "12****89".to_owned(),
            detected_at: None,
            servers: Vec::new(),
        }]);
        assert_eq!(lines, "`12****89`");
    }

    #[test]
    fn flagged_lookups_become_case_logs() {
        let outcome = Outcome {
            embed: CreateEmbed::new(),
            finding: Some(Finding {
                lookup: "discord_user",
                target: "42".to_owned(),
                verdict: "**Confirmed**".to_owned(),
                reason_types: "profile".to_owned(),
                subject: None,
            }),
        };
        let config: Config = toml::from_str("guild_id = \"1\"").unwrap();
        let incident = outcome.incident(&config).unwrap();
        assert_eq!(incident.action, "rotector_match");
        assert_eq!(incident.details["target"], serde_json::json!("42"));
        assert_eq!(
            incident.details["lookup"],
            serde_json::json!("discord_user")
        );
    }
}
