//! The `/scrape` owner command: exports a forum channel as JSON Lines, one
//! self-contained record per post carrying its title, opening message, author,
//! tags, timestamps, and every image the post holds.
//!
//! Every line repeats the run and forum context on purpose. A denormalised
//! record loads into a database without a join, a partially written file is
//! still a valid import, and the `schema` field lets a later format change be
//! migrated rather than guessed at.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, Result, bail};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serenity::{
    all::{
        Attachment, Channel, ChannelFlags, ChannelId, ChannelType, Colour, CommandInteraction,
        CommandOptionType, Context, Embed, ForumTag, ForumTagId, GuildChannel, GuildId, Http,
        LightMethod, Message, MessageId, Request, Route, ThreadsData, Timestamp, User,
    },
    builder::{
        CreateAttachment, CreateCommand, CreateCommandOption, CreateEmbed, CreateEmbedFooter,
        EditInteractionResponse, GetMessages,
    },
};
use tokio::fs;
use tracing::{info, warn};

use crate::{
    commands::{option_bool, option_channel, option_integer},
    config::Config,
    incident::{Incident, IncidentReporter},
};

/// Record layout version. Bump it whenever a field changes meaning, so an
/// importer can tell two generations of export apart.
const SCHEMA: u32 = 1;

/// Discord returns at most 100 threads per archived page, and at most 100
/// messages per message page.
const PAGE: u8 = 100;

/// Upload budget for the export. Discord accepts 10 MiB from an unboosted
/// server; staying under it keeps the file attached instead of refused, and the
/// copy on disk is complete either way.
const ATTACH_LIMIT: usize = 7 * 1024 * 1024;

/// Media channels are forums with a gallery layout and the same thread model,
/// so `/scrape` reads them too. Serenity 0.12 has no variant for type 16.
const MEDIA: ChannelType = ChannelType::Unknown(16);

/// Extensions treated as images when Discord sends no content type.
const IMAGE_EXTENSIONS: [&str; 11] = [
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "avif", "tif", "tiff", "heic", "svg",
];

const NEUTRAL: Colour = Colour::from_rgb(88, 101, 242);

pub fn command() -> CreateCommand {
    CreateCommand::new("scrape")
        .description("Export a forum channel's posts, titles and images for archiving")
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::Channel,
                "forum",
                "Forum or media channel to export",
            )
            .required(true)
            .channel_types(vec![ChannelType::Forum, MEDIA]),
        )
        .add_option(CreateCommandOption::new(
            CommandOptionType::Boolean,
            "archived",
            "Include archived posts; defaults to yes",
        ))
        .add_option(CreateCommandOption::new(
            CommandOptionType::Boolean,
            "replies",
            "Capture the replies inside each post as well; defaults to no",
        ))
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::Integer,
                "limit",
                "Maximum posts to export, newest first",
            )
            .min_int_value(1),
        )
}

/// A person as the export records them. IDs are strings: a snowflake is wider
/// than a JSON number survives in most importers.
#[derive(Debug, Clone, Serialize)]
struct Person {
    id: String,
    username: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    bot: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    avatar_url: Option<String>,
}

impl From<&User> for Person {
    fn from(user: &User) -> Self {
        let avatar_url = user.face();
        Self {
            id: user.id.get().to_string(),
            username: user.name.clone(),
            display_name: user.global_name.clone(),
            bot: user.bot,
            avatar_url: (!avatar_url.is_empty()).then_some(avatar_url),
        }
    }
}

/// One image or file, flat enough to become a row of its own on import.
#[derive(Debug, Clone, Serialize)]
struct Media {
    /// `attachment` for an upload, `embed` for an image Discord unfurled from a
    /// link in the message.
    source: &'static str,
    message_id: String,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    proxy_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attachment_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size_bytes: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<u32>,
    /// The uploader's alt text, where they wrote one.
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    /// For an unfurled image, the page the link pointed at.
    #[serde(skip_serializing_if = "Option::is_none")]
    page_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct Reply {
    message_id: String,
    author: Person,
    posted_at: Timestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    edited_at: Option<Timestamp>,
    content: String,
}

/// One forum post, complete on its own line.
// Discord reports a post's state as independent flags, and the export keeps
// them as flat columns an importer can index rather than folding them into a
// state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize)]
struct Record {
    schema: u32,
    scraped_at: DateTime<Utc>,
    guild_id: String,
    forum_id: String,
    forum_name: String,
    post_id: String,
    title: String,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    author_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    author: Option<Person>,
    created_at: Timestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_message_at: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    archived_at: Option<Timestamp>,
    archived: bool,
    locked: bool,
    pinned: bool,
    tags: Vec<String>,
    /// Discord's own reply count for the post, which excludes the opening
    /// message. It is present even when replies were not captured.
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_count: Option<u32>,
    /// The opening message, as written. Markdown is kept exactly.
    body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    posted_at: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    edited_at: Option<Timestamp>,
    /// Accounts the opening message mentioned, already resolved by Discord.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    mentions: Vec<Person>,
    /// Every image in the post, each tagged with the message it came from.
    images: Vec<Media>,
    /// Non-image uploads, kept apart so an image table stays an image table.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    files: Vec<Media>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    replies: Vec<Reply>,
    /// True when the opening message could not be read, so an empty `body` is
    /// read as missing rather than as a post with no text.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    opening_message_missing: bool,
    /// True when the post holds more replies than the configured cap.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    replies_clipped: bool,
}

/// What the run found, for the summary card and the case log.
#[derive(Debug, Default)]
struct Totals {
    posts: usize,
    archived: usize,
    images: usize,
    files: usize,
    replies: usize,
    unreadable: usize,
    clipped: usize,
    /// More posts exist than were exported, because the limit was reached.
    more_available: bool,
}

/// Where the export landed.
#[derive(Debug)]
struct Export {
    name: String,
    path: String,
    bytes: usize,
    attached: bool,
}

pub async fn handle(
    ctx: &Context,
    command: &CommandInteraction,
    config: &Config,
    incidents: &IncidentReporter,
) -> Result<()> {
    if !config.scrape.enabled {
        bail!("forum exports are disabled in Hugh's configuration");
    }
    // A large forum takes minutes of paged requests, far past Discord's
    // three-second window, so the reply is deferred before any of it starts.
    command.defer_ephemeral(&ctx.http).await?;
    let guild_id = command.guild_id.context("this command is server-only")?;
    let channel_id =
        option_channel(&command.data.options, "forum").context("missing forum option")?;
    let forum = match channel_id.to_channel(&ctx.http).await? {
        Channel::Guild(channel) if channel.guild_id == guild_id => channel,
        _ => bail!("the selected channel does not belong to this server"),
    };
    if forum.kind != ChannelType::Forum && forum.kind != MEDIA {
        bail!(
            "<#{}> is a {} channel; /scrape reads forum and media channels, whose posts are threads",
            forum.id.get(),
            forum.kind.name()
        );
    }

    let include_archived = option_bool(&command.data.options, "archived").unwrap_or(true);
    let include_replies = option_bool(&command.data.options, "replies").unwrap_or(false);
    let limit = option_integer(&command.data.options, "limit")
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0)
        .map_or(config.scrape.max_posts, |value| {
            value.min(config.scrape.max_posts)
        });

    let scraped_at = Utc::now();
    let mut totals = Totals::default();
    let posts = collect_posts(
        ctx,
        &forum,
        include_archived,
        limit,
        &mut totals.more_available,
    )
    .await?;
    info!(
        forum_id = forum.id.get(),
        posts = posts.len(),
        archived = include_archived,
        replies = include_replies,
        "exporting forum posts"
    );

    let reply_cap = if include_replies {
        config.scrape.max_replies_per_post
    } else {
        0
    };
    let mut lines = String::new();
    for post in &posts {
        let record = read_post(ctx, &forum, post, scraped_at, reply_cap).await;
        totals.posts += 1;
        // Counted from the records rather than from the two listings, so the
        // split describes what was exported even when the limit dropped some.
        totals.archived += usize::from(record.archived);
        totals.images += record.images.len();
        totals.files += record.files.len();
        totals.replies += record.replies.len();
        totals.unreadable += usize::from(record.opening_message_missing);
        totals.clipped += usize::from(record.replies_clipped);
        lines.push_str(&serde_json::to_string(&record).context("failed to serialize a post")?);
        lines.push('\n');
    }

    // Every post failing means Hugh cannot read the forum at all, which is a
    // permissions problem worth naming rather than an empty export to puzzle over.
    if totals.posts > 0 && totals.unreadable == totals.posts {
        bail!(
            "no post in <#{}> could be read; Hugh needs View Channel and Read Message History there",
            forum.id.get()
        );
    }

    let export = write_export(config, &forum, scraped_at, lines).await?;
    let embed = summary_embed(config, &forum, &totals, &export, include_replies);
    let mut response = EditInteractionResponse::new().embed(embed);
    if export.attached {
        response = response.new_attachment(CreateAttachment::path(&export.path).await?);
    }
    command.edit_response(&ctx.http, response).await?;

    // Exporting a community's posts is a privileged bulk read of member
    // content, so it leaves the same durable case trail as an enforcement action.
    let mut incident = Incident::new(
        "forum_scrape",
        config.guild_id,
        "forum_exported",
        format!(
            "Exported {} post(s) from #{} for archiving",
            totals.posts, forum.name
        ),
    );
    incident.moderator = Some((&command.user).into());
    incident.channel_id = Some(forum.id.get());
    incident.details = BTreeMap::from([
        ("forum_name".into(), forum.name.clone().into()),
        ("posts".into(), (totals.posts as u64).into()),
        ("images".into(), (totals.images as u64).into()),
        ("replies".into(), (totals.replies as u64).into()),
        ("export_file".into(), export.name.clone().into()),
        ("archived_included".into(), include_archived.into()),
        ("replies_included".into(), include_replies.into()),
    ]);
    incidents.report(&ctx.http, incident).await;
    Ok(())
}

/// Lists the forum's posts, keeping the newest when the limit bites.
///
/// Active and archived posts are two separate Discord listings, and a post that
/// archives mid-run can appear in both, so they are merged by ID.
async fn collect_posts(
    ctx: &Context,
    forum: &GuildChannel,
    include_archived: bool,
    limit: usize,
    more_available: &mut bool,
) -> Result<Vec<GuildChannel>> {
    let mut posts: BTreeMap<u64, GuildChannel> = BTreeMap::new();
    let active = GuildId::new(forum.guild_id.get())
        .get_active_threads(&ctx.http)
        .await
        .context("failed to list the forum's active posts")?;
    for thread in active.threads {
        if thread.parent_id == Some(forum.id) {
            posts.insert(thread.id.get(), thread);
        }
    }

    if include_archived {
        let mut before: Option<String> = None;
        loop {
            if posts.len() >= limit {
                *more_available = true;
                break;
            }
            let page = archived_page(&ctx.http, forum.id, before.as_deref()).await?;
            let oldest = page
                .threads
                .iter()
                .filter_map(|thread| {
                    thread
                        .thread_metadata
                        .as_ref()
                        .and_then(|data| data.archive_timestamp)
                })
                .min();
            let mut added = 0_usize;
            for thread in page.threads {
                if thread.parent_id == Some(forum.id)
                    && posts.insert(thread.id.get(), thread).is_none()
                {
                    added += 1;
                }
            }
            // Without a fresh timestamp to page from, or after a page that told
            // us nothing new, asking again would repeat that page forever.
            match oldest {
                Some(stamp) if page.has_more && added > 0 => before = Some(stamp.to_string()),
                _ => break,
            }
        }
    }
    // Ascending IDs are chronological, which is how an archive reads best. When
    // the limit bites, the newest posts are the ones worth keeping.
    let mut posts: Vec<GuildChannel> = posts.into_values().collect();
    if posts.len() > limit {
        posts.drain(..posts.len() - limit);
        *more_available = true;
    }
    Ok(posts)
}

/// One page of public archived posts.
///
/// Discord pages this endpoint by archive timestamp rather than by snowflake,
/// and Serenity's helper only accepts a number for `before`. Building the
/// request here passes the RFC 3339 timestamp the endpoint documents.
async fn archived_page(http: &Http, forum: ChannelId, before: Option<&str>) -> Result<ThreadsData> {
    let mut params = vec![("limit", PAGE.to_string())];
    if let Some(before) = before {
        params.push(("before", before.to_owned()));
    }
    let request = Request::new(
        Route::ChannelArchivedPublicThreads { channel_id: forum },
        LightMethod::Get,
    )
    .params(Some(params));
    http.fire(request)
        .await
        .context("failed to list the forum's archived posts")
}

/// Builds a post's record. A post Hugh cannot read is recorded as unreadable
/// rather than failing the run: one locked post should not lose the rest.
async fn read_post(
    ctx: &Context,
    forum: &GuildChannel,
    post: &GuildChannel,
    scraped_at: DateTime<Utc>,
    reply_cap: usize,
) -> Record {
    let metadata = post.thread_metadata.as_ref();
    let mut record = Record {
        schema: SCHEMA,
        scraped_at,
        guild_id: forum.guild_id.get().to_string(),
        forum_id: forum.id.get().to_string(),
        forum_name: forum.name.clone(),
        post_id: post.id.get().to_string(),
        title: post.name.clone(),
        url: format!(
            "https://discord.com/channels/{}/{}",
            forum.guild_id.get(),
            post.id.get()
        ),
        author_id: post.owner_id.map(|owner| owner.get().to_string()),
        author: None,
        // A post created before Discord sent create timestamps still carries
        // its creation time inside its snowflake.
        created_at: metadata
            .and_then(|data| data.create_timestamp)
            .unwrap_or_else(|| post.id.created_at()),
        last_message_at: post.last_message_id.map(|id| id.created_at()),
        archived_at: metadata.and_then(|data| data.archive_timestamp),
        archived: metadata.is_some_and(|data| data.archived),
        locked: metadata.is_some_and(|data| data.locked),
        pinned: post.flags.contains(ChannelFlags::PINNED),
        tags: tag_names(&post.applied_tags, &forum.available_tags),
        reply_count: post.message_count,
        body: String::new(),
        posted_at: None,
        edited_at: None,
        mentions: Vec::new(),
        images: Vec::new(),
        files: Vec::new(),
        replies: Vec::new(),
        opening_message_missing: false,
        replies_clipped: false,
    };

    // A forum post's opening message shares the post's own ID, so it can be
    // fetched exactly even when the post holds thousands of replies.
    let mut seen = BTreeSet::new();
    match post.id.message(ctx, MessageId::new(post.id.get())).await {
        Ok(opening) => {
            record.author = Some((&opening.author).into());
            record.body.clone_from(&opening.content);
            record.posted_at = Some(opening.timestamp);
            record.edited_at = opening.edited_timestamp;
            record.mentions = opening.mentions.iter().map(Person::from).collect();
            let (images, files) = media_from(&opening, &mut seen);
            record.images = images;
            record.files = files;
        }
        Err(error) => {
            warn!(
                ?error,
                post_id = post.id.get(),
                "could not read a forum post's opening message"
            );
            record.opening_message_missing = true;
        }
    }

    if reply_cap > 0 {
        let (replies, clipped) = read_replies(ctx, post, reply_cap).await;
        record.replies_clipped = clipped;
        for message in &replies {
            let (images, files) = media_from(message, &mut seen);
            record.images.extend(images);
            record.files.extend(files);
        }
        record.replies = replies
            .iter()
            // A bare upload or a system notice has no text worth a row, and its
            // images were already collected above.
            .filter(|message| !message.content.trim().is_empty())
            .map(|message| Reply {
                message_id: message.id.get().to_string(),
                author: (&message.author).into(),
                posted_at: message.timestamp,
                edited_at: message.edited_timestamp,
                content: message.content.clone(),
            })
            .collect();
    }
    record
}

/// Reads a post's replies oldest first, so a cap keeps the start of the
/// conversation rather than a tail with no question in it.
async fn read_replies(ctx: &Context, post: &GuildChannel, cap: usize) -> (Vec<Message>, bool) {
    let mut collected: Vec<Message> = Vec::new();
    let mut after = MessageId::new(post.id.get());
    loop {
        let want = cap.saturating_sub(collected.len()).min(PAGE as usize);
        let Ok(want) = u8::try_from(want) else {
            break;
        };
        if want == 0 {
            // The cap is full. One more message means the rest was clipped.
            let more = post
                .id
                .messages(ctx, GetMessages::new().after(after).limit(1))
                .await
                .is_ok_and(|page| !page.is_empty());
            return (collected, more);
        }
        let page = match post
            .id
            .messages(ctx, GetMessages::new().after(after).limit(want))
            .await
        {
            Ok(page) => page,
            Err(error) => {
                // A post Hugh cannot read through is worth noting without
                // losing the posts either side of it.
                warn!(
                    ?error,
                    post_id = post.id.get(),
                    "failed to read a forum post's replies"
                );
                break;
            }
        };
        let exhausted = page.len() < want as usize;
        // Discord orders a page newest first, so the highest ID is where the
        // next page starts whichever way round it arrives.
        if let Some(newest) = page.iter().map(|message| message.id).max() {
            after = newest;
        }
        collected.extend(page);
        if exhausted {
            break;
        }
    }
    collected.sort_by_key(|message| message.id.get());
    (collected, false)
}

/// Splits a message's media into images and other files. `seen` holds one
/// record per URL across the whole post, so an image Discord both attaches and
/// unfurls is not recorded twice.
fn media_from(message: &Message, seen: &mut BTreeSet<String>) -> (Vec<Media>, Vec<Media>) {
    let message_id = message.id.get().to_string();
    let mut images = Vec::new();
    let mut files = Vec::new();
    for attachment in &message.attachments {
        if !seen.insert(attachment.url.clone()) {
            continue;
        }
        let media = Media {
            source: "attachment",
            message_id: message_id.clone(),
            url: attachment.url.clone(),
            proxy_url: Some(attachment.proxy_url.clone()),
            attachment_id: Some(attachment.id.get().to_string()),
            filename: Some(attachment.filename.clone()),
            content_type: attachment.content_type.clone(),
            size_bytes: Some(attachment.size),
            width: attachment.width,
            height: attachment.height,
            description: attachment.description.clone(),
            page_url: None,
        };
        if is_image(attachment) {
            images.push(media);
        } else {
            files.push(media);
        }
    }
    for embed in &message.embeds {
        images.extend(embed_media(embed, &message_id, seen));
    }
    (images, files)
}

/// Images Discord unfurled from a link in the message. An embed's thumbnail is
/// taken only when there is no full image, since the two are usually the same
/// picture at two sizes.
fn embed_media(embed: &Embed, message_id: &str, seen: &mut BTreeSet<String>) -> Vec<Media> {
    let picture = embed.image.as_ref().map_or_else(
        || {
            embed.thumbnail.as_ref().map(|thumbnail| {
                (
                    &thumbnail.url,
                    &thumbnail.proxy_url,
                    thumbnail.width,
                    thumbnail.height,
                )
            })
        },
        |image| Some((&image.url, &image.proxy_url, image.width, image.height)),
    );
    let Some((url, proxy_url, width, height)) = picture else {
        return Vec::new();
    };
    if url.trim().is_empty() || !seen.insert(url.clone()) {
        return Vec::new();
    }
    vec![Media {
        source: "embed",
        message_id: message_id.to_owned(),
        url: url.clone(),
        proxy_url: proxy_url.clone(),
        attachment_id: None,
        filename: None,
        content_type: None,
        size_bytes: None,
        width,
        height,
        description: None,
        page_url: embed.url.clone(),
    }]
}

/// Discord usually sends a content type; a filename extension is the fallback
/// for older uploads that carry none.
fn is_image(attachment: &Attachment) -> bool {
    if let Some(kind) = attachment.content_type.as_deref() {
        return kind.trim().to_ascii_lowercase().starts_with("image/");
    }
    attachment
        .filename
        .rsplit_once('.')
        .is_some_and(|(_, extension)| {
            IMAGE_EXTENSIONS.contains(&extension.trim().to_ascii_lowercase().as_str())
        })
}

/// Applied tags as the names a reader recognises, in the forum's own order.
fn tag_names(applied: &[ForumTagId], available: &[ForumTag]) -> Vec<String> {
    available
        .iter()
        .filter(|tag| applied.contains(&tag.id))
        .map(|tag| tag.name.clone())
        .collect()
}

async fn write_export(
    config: &Config,
    forum: &GuildChannel,
    at: DateTime<Utc>,
    lines: String,
) -> Result<Export> {
    let directory = config.scrape.output_dir.trim();
    fs::create_dir_all(directory)
        .await
        .with_context(|| format!("failed to create the export directory {directory}"))?;
    let name = export_name(&forum.name, forum.id.get(), at);
    let path = std::path::Path::new(directory).join(&name);
    let bytes = lines.len();
    fs::write(&path, lines)
        .await
        .with_context(|| format!("failed to write the export {}", path.display()))?;
    info!(file = %path.display(), bytes, "wrote forum export");
    Ok(Export {
        name,
        path: path.display().to_string(),
        bytes,
        attached: config.scrape.attach_results && bytes > 0 && bytes <= ATTACH_LIMIT,
    })
}

/// A file name built from the forum's name but never out of its characters: a
/// channel name may hold anything, and a path is not the place to find out.
fn export_name(forum_name: &str, forum_id: u64, at: DateTime<Utc>) -> String {
    let mut slug = String::new();
    for character in forum_name.to_lowercase().chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.chars().take(32).collect();
    let slug = slug.trim_matches('-');
    let stem = if slug.is_empty() { "forum" } else { slug };
    format!("{stem}-{forum_id}-{}.jsonl", at.format("%Y%m%dT%H%M%SZ"))
}

fn summary_embed(
    config: &Config,
    forum: &GuildChannel,
    totals: &Totals,
    export: &Export,
    include_replies: bool,
) -> CreateEmbed {
    let mut description = vec![format!(
        "**{}** post(s) exported from <#{}>.",
        totals.posts,
        forum.id.get()
    )];
    if totals.more_available {
        description.push(format!(
            "{} The newest posts were taken first; raise `limit` or `scrape.max_posts` to reach further back.",
            config.icon("review")
        ));
    }
    if totals.unreadable > 0 {
        description.push(format!(
            "{} {} post(s) had no readable opening message and are marked as such in the file.",
            config.icon("failed"),
            totals.unreadable
        ));
    }
    if totals.clipped > 0 {
        description.push(format!(
            "{} {} post(s) hold more replies than the per-post cap.",
            config.icon("review"),
            totals.clipped
        ));
    }

    let replies = if include_replies {
        format!("{} captured", totals.replies)
    } else {
        "Not captured".to_owned()
    };
    let mut export_lines = vec![
        format!("`{}`", export.name),
        format!(
            "{} · JSON Lines, one record per post",
            human_size(export.bytes)
        ),
        format!("Saved to `{}`", export.path),
    ];
    if !export.attached {
        export_lines.push(
            if export.bytes == 0 {
                "The file is empty, because the forum held no post to export."
            } else if export.bytes > ATTACH_LIMIT {
                "Too large to attach here, so only the copy on disk was kept."
            } else {
                "Attaching exports is turned off in Hugh's configuration."
            }
            .to_owned(),
        );
    }

    CreateEmbed::new()
        .title(format!(
            "{} Forum export · #{}",
            config.icon("forum"),
            forum.name
        ))
        .colour(NEUTRAL)
        .description(description.join("\n"))
        .field(
            format!("{} Posts", config.icon("forum")),
            format!(
                "{} active · {} archived",
                totals.posts.saturating_sub(totals.archived),
                totals.archived
            ),
            true,
        )
        .field(
            format!("{} Images", config.icon("image")),
            format!(
                "{} image(s) · {} other file(s)",
                totals.images, totals.files
            ),
            true,
        )
        .field(format!("{} Replies", config.icon("details")), replies, true)
        .field(
            format!("{} Export", config.icon("case")),
            export_lines.join("\n"),
            false,
        )
        .field(
            format!("{} Image links expire", config.icon("image")),
            "Discord signs attachment links, and they stop working after about a day. Every image keeps its message ID, attachment ID and filename, so the links can be refreshed from the API when the archive is loaded.",
            false,
        )
        .footer(CreateEmbedFooter::new(format!(
            "Schema {SCHEMA} · IDs are strings · timestamps are RFC 3339"
        )))
}

/// One decimal place, in whichever unit reads smallest, without a float in
/// sight: an export's size is counted in bytes and should stay exact.
fn human_size(bytes: usize) -> String {
    const KIB: usize = 1024;
    const MIB: usize = 1024 * KIB;
    let (unit, scale) = if bytes >= MIB {
        ("MiB", MIB)
    } else if bytes >= KIB {
        ("KiB", KIB)
    } else {
        return format!("{bytes} B");
    };
    format!("{}.{} {unit}", bytes / scale, bytes % scale * 10 / scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attachment(filename: &str, content_type: Option<&str>) -> Attachment {
        let mut value = serde_json::json!({
            "id": "1",
            "filename": filename,
            "proxy_url": "https://proxy.example/1",
            "size": 10,
            "url": "https://cdn.example/1",
        });
        if let Some(content_type) = content_type {
            value["content_type"] = content_type.into();
        }
        serde_json::from_value(value).unwrap()
    }

    fn timestamp() -> DateTime<Utc> {
        DateTime::from_timestamp(1_760_000_000, 0).unwrap()
    }

    #[test]
    fn images_are_recognised_by_type_then_extension() {
        assert!(is_image(&attachment("screenshot.bin", Some("image/png"))));
        assert!(is_image(&attachment("Screenshot.PNG", None)));
        assert!(!is_image(&attachment("notes.pdf", None)));
        assert!(!is_image(&attachment("notes.pdf", Some("application/pdf"))));
        // An upload with no extension at all is not guessed at.
        assert!(!is_image(&attachment("screenshot", None)));
    }

    #[test]
    fn embed_images_prefer_the_full_size_and_are_taken_once() {
        let embed: Embed = serde_json::from_value(serde_json::json!({
            "url": "https://example.com/page",
            "image": {"url": "https://cdn.example/full.png", "width": 800},
            "thumbnail": {"url": "https://cdn.example/thumb.png"},
        }))
        .unwrap();
        let mut seen = BTreeSet::new();

        let media = embed_media(&embed, "7", &mut seen);
        assert_eq!(media.len(), 1);
        assert_eq!(media[0].url, "https://cdn.example/full.png");
        assert_eq!(
            media[0].page_url.as_deref(),
            Some("https://example.com/page")
        );
        assert_eq!(media[0].width, Some(800));

        // The same picture unfurled again later in the post is not recorded twice.
        assert!(embed_media(&embed, "8", &mut seen).is_empty());
    }

    #[test]
    fn embeds_without_a_picture_contribute_nothing() {
        let embed: Embed = serde_json::from_value(serde_json::json!({
            "title": "A link with no preview image",
        }))
        .unwrap();
        assert!(embed_media(&embed, "7", &mut BTreeSet::new()).is_empty());
    }

    #[test]
    fn export_names_never_carry_a_channel_name_into_the_path() {
        assert_eq!(
            export_name("Bug Reports", 42, timestamp()),
            "bug-reports-42-20251009T085320Z.jsonl"
        );
        assert_eq!(
            export_name("../../etc/passwd", 42, timestamp()),
            "etc-passwd-42-20251009T085320Z.jsonl"
        );
        // A name with nothing usable in it still produces a predictable file.
        assert_eq!(
            export_name("🎨", 42, timestamp()),
            "forum-42-20251009T085320Z.jsonl"
        );
    }

    #[test]
    fn applied_tags_resolve_to_names_in_the_forum_order() {
        let available: Vec<ForumTag> = serde_json::from_value(serde_json::json!([
            {"id": "10", "name": "bug", "moderated": false},
            {"id": "11", "name": "resolved", "moderated": false},
            {"id": "12", "name": "idea", "moderated": false},
        ]))
        .unwrap();
        let applied = vec![ForumTagId::new(12), ForumTagId::new(10)];
        assert_eq!(tag_names(&applied, &available), vec!["bug", "idea"]);
        assert!(tag_names(&[], &available).is_empty());
    }

    #[test]
    fn a_record_serialises_as_one_line_without_its_empty_fields() {
        let post = Record {
            schema: SCHEMA,
            scraped_at: timestamp(),
            guild_id: "1".to_owned(),
            forum_id: "2".to_owned(),
            forum_name: "support".to_owned(),
            post_id: "3".to_owned(),
            title: "Cannot log in".to_owned(),
            url: "https://discord.com/channels/1/3".to_owned(),
            author_id: Some("4".to_owned()),
            author: None,
            created_at: Timestamp::from_unix_timestamp(1_760_000_000).unwrap(),
            last_message_at: None,
            archived_at: None,
            archived: false,
            locked: false,
            pinned: false,
            tags: vec!["bug".to_owned()],
            reply_count: Some(2),
            body: "It says my token expired.".to_owned(),
            posted_at: None,
            edited_at: None,
            mentions: Vec::new(),
            images: Vec::new(),
            files: Vec::new(),
            replies: Vec::new(),
            opening_message_missing: false,
            replies_clipped: false,
        };
        let line = serde_json::to_string(&post).unwrap();

        assert!(!line.contains('\n'));
        assert!(line.contains("\"schema\":1"));
        // Snowflakes stay strings so an importer cannot round them.
        assert!(line.contains("\"guild_id\":\"1\""));
        assert!(line.contains("\"title\":\"Cannot log in\""));
        assert!(line.contains("\"images\":[]"));
        // The quiet fields are left out entirely rather than written as null.
        assert!(!line.contains("\"author\":"));
        assert!(!line.contains("replies"));
        assert!(!line.contains("opening_message_missing"));
    }

    #[test]
    fn sizes_read_as_people_write_them() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KiB");
        assert_eq!(human_size(3 * 1024 * 1024), "3.0 MiB");
    }
}
