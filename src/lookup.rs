//! The `/check` staff command: account safety lookups against the Rotector
//! API, rendered as compact cards a moderator can act on, plus the button that
//! pins one into the mod log as a durable case.

use anyhow::{Context as _, Result, bail};
use serenity::{
    all::{
        ButtonStyle, Colour, CommandDataOption, CommandInteraction, CommandOptionType,
        ComponentInteraction, Context, ReactionType, User, UserId,
    },
    builder::{
        CreateActionRow, CreateAllowedMentions, CreateButton, CreateCommand, CreateCommandOption,
        CreateEmbed, CreateEmbedAuthor, CreateEmbedFooter, CreateInteractionResponse,
        CreateInteractionResponseMessage, CreateMessage, EditInteractionResponse,
    },
};

use crate::{
    commands::{member_level, option_integer, option_string, option_user, subcommand},
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
const NEUTRAL: Colour = Colour::from_rgb(88, 101, 242);
const SLATE: Colour = Colour::from_rgb(130, 140, 155);

/// Prefix for the pin button's custom ID: `check_pin:<lookup>:<target>`.
const PIN_ID: &str = "check_pin";

pub fn command() -> CreateCommand {
    CreateCommand::new("check")
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
    let (name, options) = subcommand(command).context("/check needs a subcommand")?;
    command.defer_ephemeral(&ctx.http).await?;

    let outcome = match name {
        "discord" => discord(ctx, command, client, config, options).await?,
        "roblox" => roblox(client, config, options).await?,
        "links" => links(client, config, options).await?,
        "group" => group(client, config, options).await?,
        "sources" => sources(client, config).await?,
        other => bail!("unknown /check subcommand {other:?}"),
    };

    let logged = outcome
        .incident(config)
        .filter(|_| config.rotector.log_flagged_lookups);
    // Answer the moderator first; the case log must not delay their result.
    command
        .edit_response(
            &ctx.http,
            EditInteractionResponse::new()
                .components(vec![outcome.pin_button(config)])
                .embed(outcome.embed),
        )
        .await?;
    if let Some(mut incident) = logged {
        incident.moderator = Some((&command.user).into());
        incident.channel_id = Some(command.channel_id.get());
        incidents.report(&ctx.http, incident).await;
    }
    Ok(())
}

/// True when this component belongs to `/check`'s pin button.
#[must_use]
pub fn is_pin_button(custom_id: &str) -> bool {
    custom_id.starts_with(PIN_ID)
}

/// Copies the card the moderator is looking at into the mod log as a numbered
/// case, so an ephemeral lookup becomes retained evidence.
pub async fn handle_pin(
    ctx: &Context,
    component: &ComponentInteraction,
    config: &Config,
    incidents: &IncidentReporter,
) -> Result<()> {
    let mut parts = component.data.custom_id.splitn(3, ':').skip(1);
    let lookup = parts.next().unwrap_or("lookup").to_owned();
    let target = parts.next().unwrap_or_default().to_owned();

    // The message is ephemeral, so only its owner can press this, but the tier
    // is re-checked anyway: authority comes from configuration, not from who
    // happens to hold a message.
    let roles: Vec<u64> = component.member.as_ref().map_or_else(Vec::new, |member| {
        member.roles.iter().map(|role| role.get()).collect()
    });
    let permissions = component
        .member
        .as_ref()
        .and_then(|member| member.permissions)
        .unwrap_or_default();
    let held = member_level(config, component.user.id.get(), &roles, permissions);
    let required = config.required_level("check");
    if held < required {
        bail!(
            "pinning needs the {} staff level and you have {}",
            required.as_str(),
            held.as_str()
        );
    }

    let cards: Vec<CreateEmbed> = component
        .message
        .embeds
        .iter()
        .cloned()
        .map(CreateEmbed::from)
        .collect();
    if cards.is_empty() {
        bail!("that lookup card is no longer available to pin");
    }

    // The lookup itself was ephemeral. Posting the card into the channel is the
    // point of the button: it turns a private result into something the rest of
    // the staff can see, quote, and keep.
    let posted = component
        .channel_id
        .send_message(
            &ctx.http,
            CreateMessage::new()
                .content(format!(
                    "{}Lookup pinned by <@{}>",
                    config.mark("pin"),
                    component.user.id.get()
                ))
                .allowed_mentions(CreateAllowedMentions::new())
                .embeds(cards.clone()),
        )
        .await
        .context("Hugh could not post in this channel; it needs Send Messages and Embed Links")?;

    // Acknowledge before the case log, so the click is answered well inside
    // Discord's three-second interaction window, and drop the button so the
    // same card cannot be posted twice.
    component
        .create_response(
            &ctx.http,
            CreateInteractionResponse::UpdateMessage(
                CreateInteractionResponseMessage::new()
                    .content(format!(
                        "{}Posted in this channel for everyone: {}",
                        config.mark("pin"),
                        posted.link()
                    ))
                    .embeds(cards.clone())
                    .components(Vec::new()),
            ),
        )
        .await?;

    let mut incident = Incident::new(
        "rotector_lookup",
        config.guild_id,
        "lookup_pinned",
        "A moderator pinned a Rotector lookup into the channel as evidence",
    );
    incident.moderator = Some((&component.user).into());
    incident.channel_id = Some(component.channel_id.get());
    incident.details.insert("lookup".into(), lookup.into());
    if !target.is_empty() {
        incident.details.insert("target".into(), target.into());
    }
    incident
        .details
        .insert("message_id".into(), posted.id.get().into());
    incidents.report_with(&ctx.http, incident, cards).await;
    Ok(())
}

/// A rendered lookup plus what the pin button and case log need about it.
struct Outcome {
    embed: CreateEmbed,
    lookup: &'static str,
    target: String,
    finding: Option<Finding>,
}

struct Finding {
    verdict: String,
    reason_types: String,
    subject: Option<UserSnapshot>,
}

impl Outcome {
    fn new(embed: CreateEmbed, lookup: &'static str, target: impl Into<String>) -> Self {
        Self {
            embed,
            lookup,
            target: target.into(),
            finding: None,
        }
    }

    fn with_finding(mut self, finding: Option<Finding>) -> Self {
        self.finding = finding;
        self
    }

    fn pin_button(&self, config: &Config) -> CreateActionRow {
        let mut button = CreateButton::new(format!("{PIN_ID}:{}:{}", self.lookup, self.target))
            .label("Pin for everyone")
            .style(ButtonStyle::Secondary);
        // A button label is plain text: emoji markup belongs in the separate
        // emoji field, or `<:case:1234>` would be printed verbatim. An icon
        // Discord cannot parse is simply left off the button.
        if let Ok(emoji) = ReactionType::try_from(config.icon("pin").to_owned()) {
            button = button.emoji(emoji);
        }
        CreateActionRow::Buttons(vec![button])
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
        incident.details.insert("lookup".into(), self.lookup.into());
        incident
            .details
            .insert("target".into(), self.target.clone().into());
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
    ctx: &Context,
    command: &CommandInteraction,
    client: &RotectorClient,
    config: &Config,
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

    // Prefer the user Discord already resolved for the option; fall back to a
    // fetch so a pasted ID still gets a face and a username on the card.
    let profile = match selected.and_then(|id| command.data.resolved.users.get(&id)) {
        Some(user) => Some(user.clone()),
        None => UserId::new(user_id).to_user(&ctx.http).await.ok(),
    };

    let Some(data) = client.discord_user(user_id).await? else {
        return Ok(Outcome::new(
            no_record(
                config,
                "discord",
                "Discord account",
                &format!("<@{user_id}> · `{user_id}`"),
                profile.as_ref(),
            ),
            "discord_user",
            user_id.to_string(),
        ));
    };
    let flag = FlagType::from_code(data.flag_type);
    Ok(Outcome::new(
        discord_embed(&data, flag, user_id, profile.as_ref(), config),
        "discord_user",
        user_id.to_string(),
    )
    .with_finding(flag.is_finding().then(|| Finding {
        verdict: verdict_text(flag, data.status_label.as_deref()),
        reason_types: reason_types(&data.reasons),
        subject: profile.as_ref().map(UserSnapshot::from),
    })))
}

fn discord_embed(
    data: &DiscordUser,
    flag: FlagType,
    user_id: u64,
    profile: Option<&User>,
    config: &Config,
) -> CreateEmbed {
    let mut embed = card(
        config,
        "discord",
        "Discord account check",
        flag.colour(),
        profile,
    )
    .description(verdict_block(
        config,
        flag,
        data.status_label.as_deref(),
        data.header_message.as_deref(),
        &format!("<@{user_id}> · `{}`", data.id.trim()),
    ));
    if let Some(findings) = reason_lines(&data.reasons) {
        embed = embed.field(
            format!("{}Findings", config.mark("details")),
            findings,
            false,
        );
    }
    if data.linked_roblox_accounts.is_empty() {
        embed = embed.field(
            format!("{}Linked Roblox accounts", config.mark("roblox")),
            "None on record",
            true,
        );
    } else {
        embed = embed.field(
            format!(
                "{}Linked Roblox accounts ({})",
                config.mark("roblox"),
                data.linked_roblox_accounts.len()
            ),
            linked_roblox_lines(config, &data.linked_roblox_accounts),
            false,
        );
    }
    embed.footer(footer(
        "A clean lookup is not a clearance · data from Rotector",
    ))
}

async fn roblox(
    client: &RotectorClient,
    config: &Config,
    options: &[CommandDataOption],
) -> Result<Outcome> {
    let id = roblox_id(options)?;
    let Some(data) = client.roblox_user(id).await? else {
        return Ok(Outcome::new(
            no_record(
                config,
                "roblox",
                "Roblox account",
                &roblox_user_ref(id),
                None,
            )
            .thumbnail(roblox_headshot(id)),
            "roblox_user",
            id.to_string(),
        ));
    };
    let flag = FlagType::from_code(data.flag_type);
    Ok(Outcome::new(
        roblox_embed(&data, flag, config),
        "roblox_user",
        id.to_string(),
    )
    .with_finding(flag.is_finding().then(|| Finding {
        verdict: verdict_text(flag, data.status_label.as_deref()),
        reason_types: reason_types(&data.reasons),
        subject: None,
    })))
}

fn roblox_embed(data: &RobloxUser, flag: FlagType, config: &Config) -> CreateEmbed {
    let id = data.id;
    let mut embed = card(
        config,
        "roblox",
        &format!("Roblox user {id}"),
        flag.colour(),
        None,
    )
    .thumbnail(roblox_headshot(id))
    .description(verdict_block(
        config,
        flag,
        data.status_label.as_deref(),
        data.header_message.as_deref(),
        &roblox_user_ref(id),
    ));
    if let Some(category) = readable(data.category_label.as_deref()) {
        embed = embed.field(
            format!("{}Category", config.mark("flagged")),
            category,
            true,
        );
    }
    if let Some(findings) = reason_lines(&data.reasons) {
        embed = embed.field(
            format!("{}Findings", config.mark("details")),
            findings,
            false,
        );
    }
    if let Some(provisional) = reason_lines(&data.provisional_reasons) {
        embed = embed.field(
            format!("{}Awaiting human review", config.mark("review")),
            provisional,
            false,
        );
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
        notes.push(format!("[Full records]({url})"));
    }
    if !notes.is_empty() {
        embed = embed.field(
            format!("{}Notes", config.mark("case")),
            clamp(&notes.join("\n")),
            false,
        );
    }
    embed.footer(footer(
        "Rotector's judgement, not Roblox moderation · data from Rotector",
    ))
}

async fn links(
    client: &RotectorClient,
    config: &Config,
    options: &[CommandDataOption],
) -> Result<Outcome> {
    let id = roblox_id(options)?;
    let Some(data) = client.roblox_user_links(id).await? else {
        return Ok(Outcome::new(
            no_record(
                config,
                "roblox",
                "Roblox account",
                &roblox_user_ref(id),
                None,
            )
            .thumbnail(roblox_headshot(id)),
            "roblox_links",
            id.to_string(),
        ));
    };
    Ok(Outcome::new(
        links_embed(&data, config),
        "roblox_links",
        id.to_string(),
    ))
}

fn links_embed(data: &RobloxDiscordLinks, config: &Config) -> CreateEmbed {
    let id = data.roblox_user_id;
    let mut embed = card(
        config,
        "discord",
        &format!("Accounts linked to Roblox user {id}"),
        NEUTRAL,
        None,
    )
    .thumbnail(roblox_headshot(id))
    .description(format!(
        "{}\n{}**{}** linked Discord account(s)  ·  {}**{}** known alt(s)",
        roblox_user_ref(id),
        config.mark("discord"),
        data.discord_accounts.len(),
        config.mark("roblox"),
        data.alt_accounts.len()
    ));
    if data.discord_accounts.is_empty() && data.alt_accounts.is_empty() {
        embed = embed.field(
            format!("{}Nothing on record", config.mark("unknown")),
            "Rotector has no linked accounts for this user.",
            false,
        );
    }
    if !data.discord_accounts.is_empty() {
        embed = embed.field(
            format!("{}Discord accounts", config.mark("discord")),
            discord_link_lines(&data.discord_accounts),
            false,
        );
    }
    if !data.alt_accounts.is_empty() {
        embed = embed.field(
            format!("{}Other Roblox accounts", config.mark("roblox")),
            alt_lines(&data.alt_accounts),
            false,
        );
    }
    embed.footer(footer(
        "Links are observations, not proof of shared ownership · data from Rotector",
    ))
}

async fn group(
    client: &RotectorClient,
    config: &Config,
    options: &[CommandDataOption],
) -> Result<Outcome> {
    let id = roblox_id(options)?;
    let Some(data) = client.roblox_group(id).await? else {
        return Ok(Outcome::new(
            no_record(config, "group", "Roblox group", &roblox_group_ref(id), None),
            "roblox_group",
            id.to_string(),
        ));
    };
    let flag = FlagType::from_code(data.flag_type);
    Ok(Outcome::new(
        group_embed(&data, flag, config),
        "roblox_group",
        id.to_string(),
    )
    .with_finding(flag.is_finding().then(|| Finding {
        verdict: verdict_text(flag, data.status_label.as_deref()),
        reason_types: reason_types(&data.reasons),
        subject: None,
    })))
}

fn group_embed(data: &RobloxGroup, flag: FlagType, config: &Config) -> CreateEmbed {
    let id = data.id;
    let mut embed = card(
        config,
        "group",
        &format!("Roblox group {id}"),
        flag.colour(),
        None,
    )
    .description(verdict_block(
        config,
        flag,
        data.status_label.as_deref(),
        data.header_message.as_deref(),
        &roblox_group_ref(id),
    ));
    if let Some(findings) = reason_lines(&data.reasons) {
        embed = embed.field(
            format!("{}Findings", config.mark("details")),
            findings,
            false,
        );
    }
    embed.footer(footer(
        "Group findings describe the group, not every member · data from Rotector",
    ))
}

async fn sources(client: &RotectorClient, config: &Config) -> Result<Outcome> {
    let providers = client.providers().await?;
    Ok(Outcome::new(
        sources_embed(&providers, config),
        "sources",
        "all",
    ))
}

fn sources_embed(providers: &[Provider], config: &Config) -> CreateEmbed {
    let mut embed = card(config, "source", "Detection sources", NEUTRAL, None)
        .footer(footer("Reported by the Rotector API"));
    if providers.is_empty() {
        return embed.description("Rotector reported no enabled sources.");
    }
    embed = embed.description(format!(
        "**{}** source(s) are enabled for Hugh's key.",
        providers.len()
    ));
    for provider in providers.iter().take(10) {
        let mut body = vec![
            readable(provider.description.as_deref())
                .unwrap_or_else(|| "No description provided.".to_owned()),
            // The stable source ID is what a finding's detector list cites.
            format!("Detector ID: `{}`", provider.id.trim()),
        ];
        if let Some(website) = readable(provider.website_url.as_deref()) {
            body.push(format!("[Website]({website})"));
        }
        if let Some(appeal) = readable(provider.appeal_url.as_deref()) {
            body.push(format!("Appeals: {appeal}"));
        }
        embed = embed.field(
            format!("{}{}", config.mark("source"), provider.name),
            clamp(&body.join("\n")),
            false,
        );
    }
    embed
}

/// Shared card shell: icon-prefixed title, colour, and the subject's avatar as
/// both the author glyph and the thumbnail when Discord gives us one.
fn card(
    config: &Config,
    icon: &str,
    title: &str,
    colour: Colour,
    profile: Option<&User>,
) -> CreateEmbed {
    let mut embed = CreateEmbed::new()
        .title(format!("{}{title}", config.mark(icon)).trim().to_owned())
        .colour(colour);
    if let Some(user) = profile {
        let name = user.global_name.as_ref().map_or_else(
            || format!("@{}", user.name),
            |display| format!("@{} · {display}", user.name),
        );
        embed = embed
            .author(CreateEmbedAuthor::new(clamp_to(&name, 256)).icon_url(user.face()))
            .thumbnail(user.face());
    }
    embed
}

fn no_record(
    config: &Config,
    icon: &str,
    kind: &str,
    target: &str,
    profile: Option<&User>,
) -> CreateEmbed {
    card(config, icon, &format!("No record · {kind}"), SLATE, profile)
        .description(format!(
            "{target}\n\n{}**No record**\nRotector holds nothing for this identifier. That is not \
             a clean verdict: the account may simply never have been analysed.",
            config.mark("unknown")
        ))
        .footer(footer("Rotector lookup"))
}

fn roblox_id(options: &[CommandDataOption]) -> Result<u64> {
    let id = option_integer(options, "id").context("missing id option")?;
    u64::try_from(id)
        .ok()
        .filter(|id| *id > 0)
        .context("the ID must be a positive whole number")
}

/// Verdict, optional provider headline, and the subject reference, as the
/// card's opening block.
fn verdict_block(
    config: &Config,
    flag: FlagType,
    status_label: Option<&str>,
    header: Option<&str>,
    reference: &str,
) -> String {
    let mut block = format!(
        "{reference}\n\n{}{}",
        config.mark(flag_icon(flag)),
        verdict_text(flag, status_label)
    );
    if let Some(header) = readable(header) {
        block.push('\n');
        block.push_str(&clamp_to(&header, 600));
    }
    block
}

const fn flag_icon(flag: FlagType) -> &'static str {
    match flag {
        FlagType::Safe => "safe",
        FlagType::Processing | FlagType::AwaitingReview => "review",
        FlagType::Unknown(_) => "unknown",
        _ => "flagged",
    }
}

fn verdict_text(flag: FlagType, status_label: Option<&str>) -> String {
    match readable(status_label) {
        Some(label) if !label.eq_ignore_ascii_case(flag.label()) => {
            format!("**{}** · {label}", flag.label())
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

fn linked_roblox_lines(config: &Config, accounts: &[LinkedRobloxAccount]) -> String {
    let mut lines = accounts
        .iter()
        .take(MAX_LISTED)
        .map(|account| {
            let flag = FlagType::from_code(account.flag_type);
            let detected = account
                .detected_at
                .filter(|value| *value > 0)
                .map(|value| format!(" · <t:{value}:R>"))
                .unwrap_or_default();
            format!(
                "{}[{}](https://www.roblox.com/users/{}/profile) — {}{detected}",
                config.mark(flag_icon(flag)),
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
                format!("<@{id}> · `{id}`")
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

/// Roblox's headshot endpoint redirects to its CDN, which Discord's embed proxy
/// follows, so no thumbnail API round trip is needed.
fn roblox_headshot(id: u64) -> String {
    format!(
        "https://www.roblox.com/headshot-thumbnail/image?userId={id}&width=420&height=420&format=png"
    )
}

fn roblox_user_ref(id: u64) -> String {
    format!("[Open Roblox profile](https://www.roblox.com/users/{id}/profile) · `{id}`")
}

fn roblox_group_ref(id: u64) -> String {
    format!("[Open Roblox group](https://www.roblox.com/groups/{id}) · `{id}`")
}

fn footer(note: &str) -> CreateEmbedFooter {
    CreateEmbedFooter::new(format!("{note} · {WEBSITE_URL}"))
}

fn readable(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn clamp(value: &str) -> String {
    clamp_to(value, FIELD_LIMIT)
}

fn clamp_to(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_owned();
    }
    value
        .chars()
        .take(limit.saturating_sub(1))
        .chain(std::iter::once('…'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rotector::Source;

    fn config() -> Config {
        toml::from_str("guild_id = \"1\"").unwrap()
    }

    /// A server that has uploaded the Lucide artwork and filled two slots in.
    fn branded_config() -> Config {
        let config: Config = toml::from_str(
            "guild_id = \"1\"
incident_channel_id = \"2\"

[icons]
flagged = \"<:flagged:123456789012345678>\"
safe = \"<:safe:123456789012345679>\"",
        )
        .unwrap();
        config.validate().unwrap();
        config
    }

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
            verdict_text(FlagType::Confirmed, Some("Unsafe")),
            "**Confirmed** · Unsafe"
        );
        assert_eq!(
            verdict_text(FlagType::Safe, Some("no findings")),
            "**No findings**"
        );
        assert_eq!(verdict_text(FlagType::Safe, None), "**No findings**");
    }

    #[test]
    fn verdict_block_opens_with_reference_and_icon() {
        let config = config();
        let block = verdict_block(
            &config,
            FlagType::Confirmed,
            Some("Unsafe"),
            Some("  Known predatory account  "),
            "<@42>",
        );
        assert!(block.starts_with("<@42>\n\n**Confirmed** · Unsafe"));
        assert!(block.ends_with("Known predatory account"));
        // With the artwork uploaded, the slot leads the verdict.
        let branded = branded_config();
        assert!(
            verdict_block(&branded, FlagType::Safe, None, None, "<@42>")
                .contains("<:safe:123456789012345679> **No findings**")
        );
    }

    /// An unset slot renders as nothing at all: Hugh never substitutes an emoji
    /// of its own, and `mark` leaves no stranded space behind in the label.
    #[test]
    fn icon_slots_are_empty_until_configured() {
        let config = config();
        assert_eq!(config.icon("flagged"), "");
        assert_eq!(config.mark("flagged"), "");
        assert_eq!(config.icon("not-a-slot"), "");
        // Every slot Hugh draws is declared once, in the table the icon script
        // reads out of the source.
        assert!(
            crate::config::ICONS
                .iter()
                .any(|(slot, icon)| *slot == "flagged" && *icon == "shield-alert")
        );
        assert!(
            !crate::config::ICONS
                .iter()
                .any(|(slot, _)| *slot == "not-a-slot")
        );

        let branded = branded_config();
        assert_eq!(branded.icon("flagged"), "<:flagged:123456789012345678>");
        assert_eq!(branded.mark("flagged"), "<:flagged:123456789012345678> ");
        assert_eq!(branded.icon("review"), "");
        assert_eq!(branded.mark("review"), "");
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
    fn pin_button_carries_the_lookup_target() {
        let config = config();
        let outcome = Outcome::new(CreateEmbed::new(), "roblox_user", "77");
        let row = serde_json::to_value(outcome.pin_button(&config)).unwrap();
        let button = &row["components"][0];
        assert_eq!(button["custom_id"], "check_pin:roblox_user:77");
        assert!(is_pin_button(button["custom_id"].as_str().unwrap()));
        assert_eq!(button["label"].as_str().unwrap(), "Pin for everyone");
        // Nothing is configured, so the button carries no emoji at all rather
        // than an empty one Discord would reject.
        assert!(button["emoji"].is_null());

        // A plain emoji belongs in the button's own emoji field, not the label,
        // which is plain text.
        let unicode: Config = toml::from_str(
            "guild_id = \"1\"

[icons]
pin = \"📌\"",
        )
        .unwrap();
        let row = serde_json::to_value(outcome.pin_button(&unicode)).unwrap();
        assert_eq!(
            row["components"][0]["emoji"]["name"].as_str().unwrap(),
            "📌"
        );

        let custom: Config = toml::from_str(
            "guild_id = \"1\"

[icons]
pin = \"<:pin:1560745728769523902>\"",
        )
        .unwrap();
        let row = serde_json::to_value(outcome.pin_button(&custom)).unwrap();
        let button = &row["components"][0];
        assert_eq!(button["label"].as_str().unwrap(), "Pin for everyone");
        assert_eq!(
            button["emoji"]["id"].as_str().unwrap(),
            "1560745728769523902"
        );
        assert_eq!(button["emoji"]["name"].as_str().unwrap(), "pin");
    }

    #[test]
    fn flagged_lookups_become_case_logs() {
        let config = config();
        let outcome =
            Outcome::new(CreateEmbed::new(), "discord_user", "42").with_finding(Some(Finding {
                verdict: "**Confirmed**".to_owned(),
                reason_types: "profile".to_owned(),
                subject: None,
            }));
        let incident = outcome.incident(&config).unwrap();
        assert_eq!(incident.action, "rotector_match");
        assert_eq!(incident.details["target"], serde_json::json!("42"));
        assert_eq!(
            incident.details["lookup"],
            serde_json::json!("discord_user")
        );

        // A clean lookup is never written to the case log automatically.
        assert!(
            Outcome::new(CreateEmbed::new(), "discord_user", "42")
                .incident(&config)
                .is_none()
        );
    }
}
