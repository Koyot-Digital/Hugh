# Configuration reference

Hugh reads configuration once at startup. Restart it after changes. Unknown
keys and invalid values stop startup, preventing misspellings from silently
disabling protection. Discord IDs are strings in TOML to preserve their exact
integer value.

## Top-level values

| Key | Meaning |
| --- | --- |
| `guild_id` | The only server Hugh accepts events from. |
| `incident_channel_id` | Required private staff channel Hugh posts case cards to, under its own identity. It needs View Channel, Send Messages and Embed Links there. |
| `incident_log_path` | Append-only JSONL audit file. Default: `data/incidents.jsonl`. |
| `trusted_user_ids` | Users that bypass message and account-age guards. It does not grant protected roles. |
| `trusted_role_ids` | Roles that bypass message guards. Use very sparingly. |

Hugh ignores only its own messages to avoid moderation loops. New messages and
edits are checked, closing the loophole where harmless content is edited into
spam or a blocked link. Messages from other bots and webhooks are checked and
can be deleted, but automated senders are not timed out because Discord does
not treat them like ordinary members.

## `[mentions]`

- `max_total_mentions`: maximum actual user and role mention tokens plus active
  `@everyone`/`@here` tokens in one message. Repeated mentions of the same
  target count separately. A violation deletes and times out.
- `max_everyone_mentions`: use `0` to block active `@everyone`/`@here` mentions;
  a positive value permits up to that many active tokens in one message.
- `protected_role_ids`: staff or incident-response roles whose pings are
  rate-limited.
- `protected_role_max_pings`: allowed protected-role-pinging messages per user
  in the rolling window. With `2`, the third is acted on.
- `protected_role_window_seconds`: rolling window length.
- `timeout_seconds`: timeout applied for mention violations.

## `[spam]`

- `max_messages` and `window_seconds`: per-user rolling flood limit.
- `max_duplicates` and `duplicate_window_seconds`: repeated normalized message
  limit. Case, whitespace runs, and invisible zero-width characters are ignored.
- `max_coordinated_duplicates` and `coordinated_window_seconds`: distinct-user
  limit for the same normalized message across the server. This catches copied
  raid payloads even when every account sends only once.
- `coordinated_min_length`: messages shorter than this are excluded from the
  coordinated check to avoid treating common replies such as “hello” as raids.
- `timeout_seconds`: timeout applied for spam violations.

Message edits are checked against content rules but are not added to flood,
duplicate, or coordinated-spam windows.

## `[invites]`

When enabled, Discord invite links are deleted unless their code appears in
`allowed_codes`. Store only the code (for example `our-community`), not the full
URL. Vanity codes are supported. This detector intentionally does not block all
URLs.

## `[content]`

- `max_links`: maximum HTTP(S) or `www.` links in one message.
- `max_attachments`: maximum attachments in one message.
- `max_newlines`: maximum line-break characters in one message.
- `blocked_domains`: exact hosts to block. Subdomains are included; for example,
  `bad.example` also blocks `login.bad.example`. Enter hosts only, without a
  scheme or path.
- `timeout_seconds`: timeout used by all content-limit violations.

Only URL host names are added to incident evidence. Paths, queries, message
bodies, and attachment contents are not retained.

## `[raids]`

`join_threshold` joins inside `join_window_seconds` activates raid mode for
`raid_mode_seconds`. Further threshold-reaching joins extend it. During raid
mode, accounts younger than `minimum_account_age_seconds` receive one of:

- `log`: alert and record only (recommended while tuning);
- `timeout`: temporarily prevent communication; or
- `kick`: remove the member without banning them.

Hugh does not lock every channel automatically. Incorrect global permission
overwrites can lock out staff and are difficult to roll back safely.

## `[new_accounts]`

This guard screens accounts by age on every join, even when raid mode is not
active. `minimum_account_age_seconds`, `action`, and `timeout_seconds` work like
their raid equivalents. Start with `action = "log"`, measure normal joins, and
only then consider `timeout` or `kick`. When the raid rule also matches, the
raid policy takes precedence so one join receives only one enforcement action.

## `[role_guard]`

This is an explicit membership allowlist, not merely an event-diff detector.
Whenever Discord reports an update for a member who holds a
`protected_role_id`, Hugh removes that role unless the member is in
`authorized_member_ids`. This also repairs unauthorized assignments Hugh may
have missed while offline.

List every legitimate holder before enabling it. The server owner cannot have
roles removed by bots, and Hugh can remove only roles below its highest role.

## `[automod]`

Content-shape guards that look at how a message is written rather than what it
links to.

- `max_caps_ratio`: share of letters that may be uppercase, from 0.0 to 1.0.
  Use 1.0 or more to accept any amount of shouting.
- `caps_min_letters`: shorter messages are never judged for shouting. A
  three-letter message is uppercase by accident as often as by intent.
- `max_repeated_characters`: longest run of one repeated character, as in
  `aaaaaaaaaaaa`. Whitespace runs are ignored, because ordinary formatting is
  made of them.
- `max_emoji`: emoji per message, counting both Unicode and custom `<:name:id>`
  forms.
- `max_combining_marks`: longest run of combining marks, which is what stacks
  "zalgo" text.
- `blocked_phrases`: case-insensitive substrings Hugh refuses. Matching happens
  with zero-width characters stripped, so padding cannot hide a phrase.
- `channel_hop_threshold` and `channel_hop_window_seconds`: distinct channels
  one member may repeat a message across inside the window. This catches a
  single message walked around the server, which no per-channel flood limit
  sees. The threshold never falls below two channels, however strict a member's
  standing makes it: one channel is simply a message.
- `new_account_link_age_seconds`: accounts younger than this may not post
  links. Zero disables the check.
- `timeout_seconds`: timeout used by these violations.

## `[escalation]`

Hugh adds the weight of every automod hit to a rolling per-member total and
steps up the response as that total climbs, so a member who trips one guard
after another stops being handled one message at a time.

- `window_seconds`: how long a hit keeps counting towards the total. Once the
  window empties the member starts over, so a quiet month is not followed by an
  instant ban for one slip.
- `timeout_at`, `kick_at`, `ban_at`: accumulated weight at which each step takes
  over. Zero disables a step, and the member's standing scales every threshold.
- `timeout_seconds`: timeout the escalated step applies, before standing
  scaling.
- `ban_duration_seconds`: length of an escalated ban. Zero bans permanently.

Hugh climbs one step at a time and never repeats a step it has already taken
inside the window. An escalation to kick or ban supersedes the timeout: there is
no point timing out somebody who is about to be removed. The weight each
violation contributes is set in `[standing.violations]`.

## `[standing]`

One score per member, carried by their moderation record. Every punishment takes
its weight off, and that weight fades back over `decay_days`, so an old warning
stops counting while a fresh one bites. Standing never goes below zero, and a
revoked case stops costing anything at all.

- `starting_points`: standing a member with no history holds. Default `100.0`.
- `decay_days`: days over which one punishment's weight decays to nothing.
- `exemplary_at`, `good_at`, `neutral_at`, `poor_at`: lower bound of each band,
  descending. A member sits in the best band whose bound they still meet, and
  below `poor_at` they are in the worst band.
- `[standing.punishments]`: weight each kind of punishment takes off: `warn`,
  `timeout`, `kick`, `ban`, `temporary_ban`, `quarantine`, and `automod` for a
  hit whose violations carry no weight of their own.
- `[standing.violations]`: weight each automod violation contributes, both to
  the escalation total and to the standing cost of the resulting case.
- `[standing.strictness.<band>]`: how hard the limits bite in that band.
  `thresholds` scales every counted limit, so below 1.0 a member trips guards
  sooner; `timeouts` scales the resulting timeout. The bands are `exemplary`,
  `good`, `neutral`, `poor` and `bad`, and `neutral` is the 1.0 baseline every
  limit is written against.

A scaled limit never reaches zero, because a limit of zero would delete every
message the member sends; a limit configured as zero means "never allowed" and
stays there. Scaled timeouts are clamped to Discord's 28-day ceiling.

## `[moderation]`

The record behind `/warn`, `/kick`, `/ban`, `/unban`, `/unwarn`, `/modlogs` and
`/standing`.

- `enabled`: registers those commands. The record itself is always kept, because
  standing and escalation are built on it.
- `store_path`: append-only case journal. Default: `data/moderation.jsonl`.
- `ban_delete_message_days`: days of the banned member's messages Discord
  removes, from 0 to 7. `/ban` can override it per call.
- `dm_on_action`: tell the member why before Hugh acts. A closed DM is not an
  error.
- `max_ban_duration_seconds`: longest duration `/ban` accepts. Zero allows any
  length.
- `expiry_sweep_seconds`: how often Hugh looks for temporary bans that have run
  out. Each expiry is filed as its own case.
- `modlog_page_size`: cases shown by one `/modlogs` call.

A temporary ban is marked served before the unban is retried, so a ban Discord
has already dropped is not retried on every sweep from then on.

## `[welcome]`

- `enabled` and `channel_id`: the greeting and where it goes.
- `message`: the template. `{user}` becomes a mention; `{username}`,
  `{display}`, `{server}`, `{count}`, `{ordinal}` and `{id}` are filled in as
  text. An unknown placeholder is left in place, so a typo shows up in the
  channel rather than being silently swallowed.
- `skip_bots`: stay quiet for applications joining.
- `skip_during_raid`: stay quiet while raid mode is on, so a join flood cannot
  turn the welcome channel into the raid.

Nobody Hugh has just removed on arrival is welcomed.

## `[lock]`

- `restricted_role_ids`: the roles `/lock` silences. With none listed it falls
  back to `@everyone`, which is what a lock means in a server that has not split
  its roles up.
- `bypass_role_ids`: roles `/lock` leaves able to speak, by way of an explicit
  allow that outranks the deny above it. A role in both lists is silenced.

## `[quarantine]`

- `enabled`: enables `/banish`, `/unbanish`, and channel isolation.
- `channel_name` and `role_name`: exact names Hugh creates or reuses when it
  connects. Defaults: `quarantine` and `Quarantined`.
- `channel_id` and `role_id`: optional advanced overrides for existing
  resources. Set both or neither.
- `store_path`: append-only role recovery journal. Default:
  `data/quarantine.jsonl`.

Hugh ensures the quarantine role cannot view every other existing and newly
created channel. It adds an explicit role deny to accessible channels; channels
already hidden by an `@everyone` view deny need no redundant overwrite. On the
quarantine channel, it denies `@everyone` access and explicitly allows the
quarantine role and Hugh to view and send messages.
Discord owners and administrators bypass channel overwrites, and managed
integration roles cannot be removed; account for those exceptions when testing.

The quarantine role must not appear in `trusted_role_ids` or the role guard's
`protected_role_ids`. A member's original roles are flushed to disk before any
Discord roles are changed. A repeated `/banish` cannot overwrite that snapshot,
and `/unbanish` clears it only after restoration and release succeed.

Discord-managed integration roles cannot be removed or manually restored by a
bot. Hugh leaves them untouched. Deleted roles are reported and skipped during
restoration.

## Environment

| Variable | Purpose |
| --- | --- |
| `DISCORD_TOKEN` | Required bot token. |
| `HUGH_ROTECTOR_API_KEY` | Required when `[rotector].enabled = true`; sent as the bearer token on Rotector lookups. |
| `HUGH_CONFIG` | Config path; overridden by `--config`. |
| `RUST_LOG` | Log filter, such as `hugh=debug,serenity=warn`. |
| `HUGH_LOG_FORMAT` | Set to `json` for structured process logs. |

Discord permits member timeouts up to 28 days; Hugh clamps values at that API
limit.

## `[rotector]`

Rotector is a third-party service that flags Roblox and Discord accounts for
predatory and inappropriate behaviour. When this section is enabled, Hugh
registers the staff-only `/check` command and reads
`HUGH_ROTECTOR_API_KEY` at startup.

- `enabled`: registers `/check` and requires the API key.
- `base_url`: API origin. Default: `https://roscoe.rayward.app`.
- `language`: locale for the status text the API composes, such as `es` or
  `zh-CN`. Unknown values fall back to English.
- `request_timeout_seconds`: per-request timeout, from 1 to 60.
- `log_flagged_lookups`: write a case log whenever a lookup returns a flagged
  account, so a staff decision has a durable reference.

Hugh sends nothing but the identifier a moderator asked about. Message content,
member lists, and server metadata are never uploaded. Results answer privately
to the moderator who ran the lookup:

| Subcommand | Endpoint | Shows |
| --- | --- | --- |
| `/check discord [user] [id]` | `/v2/lookup/rotector/discord/user/{id}` | Verdict, findings, linked flagged Roblox accounts. |
| `/check roblox id` | `/v2/lookup/rotector/roblox/user/{id}` | Verdict, category, findings, findings awaiting review, reviewer, record links. |
| `/check links id` | `/v2/lookup/rotector/roblox/user/{id}/discord` | Linked Discord accounts, tracked servers, and known alts. |
| `/check group id` | `/v2/lookup/rotector/roblox/group/{id}` | Group verdict and findings. |
| `/check sources` | `/v2/providers` | Detection sources enabled for Hugh's key. |

Verdicts come from the API's `flagType` field: no findings, flagged, confirmed,
being processed, provisional, mixed, past offender, redacted, or awaiting human
review. Only the first six count as findings, and only a finding produces a
case log. A `404` is reported as "no record", which is not a clearance: the
account may simply never have been analysed. Hugh does not screen joins against
Rotector; a moderator decides what to do with every result.

### Pinning a lookup

A `/check` result is ephemeral: only the moderator who ran it can see it, and
it disappears. Each result therefore carries a **Pin for everyone** button.
Pressing it posts the same card into the channel as an ordinary message that
every member of that channel can see, quote, and scroll back to, credited to the
moderator who pinned it. The button then disappears, so one card cannot be
posted twice.

The pinned card is also filed as a numbered case (kind `rotector_lookup`,
action `lookup_pinned`) in the incident log and the incident channel, recording
who pinned it, where, and the ID of the public message.

Hugh needs **Send Messages** and **Embed Links** in the channel where the button
is pressed; without them the lookup stays private and the moderator is told why.
Pinning re-checks the `check` tier rather than trusting whoever holds the
message. Mentions are suppressed on the posted card, so pinning never pings the
account under review.

## `[scrape]`

`/scrape` exports a forum channel for archiving: every post's title, opening
message, author, tags, timestamps, and images, written as
[JSON Lines](https://jsonlines.org/) — one independent JSON object per line.
It is an `owner` command by default, because it is a bulk read of member
content rather than a moderation action.

- `enabled`: registers `/scrape`. Set it to `false` to drop the command.
- `output_dir`: directory the exports are written to. Default: `data/scrapes`.
- `max_posts`: ceiling on posts per run, from 1 to 10000. Default: `500`. The
  command's own `limit` option can lower this but never raise it.
- `max_replies_per_post`: ceiling on replies captured per post, used only when
  the command is called with `replies: true`. Default: `200`.
- `attach_results`: also attach the export to the reply. A file over 7 MiB is
  left on disk only, since Discord refuses larger uploads from most servers.

### Running it

`/scrape forum [archived] [replies] [limit]`

- `forum`: the forum or media channel to export. Discord's picker is limited to
  those two channel types, since only they keep their posts as threads.
- `archived`: include archived posts. Defaults to yes.
- `replies`: also capture the replies inside each post. Defaults to no, which
  keeps a run to one request per post.
- `limit`: maximum posts for this run, newest first.

Active and archived posts are two separate Discord listings; Hugh merges them
by ID, so a post that archives mid-run is exported once. Posts are written
oldest first. When a limit is reached, the newest posts are kept and the reply
says so.

Hugh needs **View Channel** and **Read Message History** on the forum. A single
post it cannot read is recorded with `opening_message_missing` rather than
failing the run; if no post can be read, the command stops and names the two
permissions. A large forum takes minutes of paged requests, so the answer is
deferred and arrives when the file is written.

Every run is filed as a numbered case (kind `forum_scrape`, action
`forum_exported`) recording who exported what, from where, and how much.

### The export format

Each line is a complete post, repeating its run and forum context so a record
can be loaded without a join and a half-written file is still a valid import.
IDs are strings, because a Discord snowflake does not survive a 64-bit JSON
number in most importers, and timestamps are RFC 3339.

| Field | Meaning |
| --- | --- |
| `schema` | Record layout version, currently `1`. |
| `scraped_at` | When the run started. Identical on every line of a file. |
| `guild_id`, `forum_id`, `forum_name` | Where the post came from. |
| `post_id`, `title`, `url` | The post itself and a link back to it. |
| `author_id`, `author` | The post's author; `author` is omitted when the opening message could not be read. |
| `created_at`, `last_message_at`, `archived_at` | Post lifetime. |
| `archived`, `locked`, `pinned` | Post state at export time. |
| `tags` | Applied forum tags, by name, in the forum's own order. |
| `reply_count` | Discord's reply count, which excludes the opening message. Present even when replies were not captured. |
| `body`, `posted_at`, `edited_at` | The opening message, with its markdown kept exactly as written. |
| `mentions` | Accounts the opening message mentioned, already resolved. |
| `images` | Every image in the post, each tagged with the message it came from. |
| `files` | Non-image uploads, kept apart so an image table stays an image table. |
| `replies` | Captured replies, oldest first, present only with `replies: true`. |
| `opening_message_missing` | Present and `true` when the opening message could not be read. |
| `replies_clipped` | Present and `true` when the post holds more replies than the cap. |

An image or file carries `source` (`attachment` for an upload, `embed` for a
picture Discord unfurled from a link), `message_id`, `url`, and whatever of
`proxy_url`, `attachment_id`, `filename`, `content_type`, `size_bytes`,
`width`, `height`, `description` and `page_url` Discord provided. An image that
appears twice in a post is recorded once.

Discord signs attachment links and they stop working roughly a day later, so
the URLs in an export are not a long-term store. Each image keeps its message
ID, attachment ID and filename, which is what Discord's API needs to issue
fresh links when the archive is loaded. Copy the files themselves if the
archive has to outlive those links.

## `[icons]`

Case and lookup cards are built around icon slots. An unset slot renders as
nothing at all, because Hugh never substitutes an emoji of its own and leaves no
stranded space behind, so this section is entirely optional. Set a slot to an
emoji to brand the cards:

```toml
[icons]
flagged = "<:shield_alert:123456789012345678>"
safe = "<:badge_check:123456789012345678>"
```

Values are emoji, not names — use `<:name:id>` for a static emoji and
`<a:name:id>` for an animated one. With Developer Mode on, right-click an
uploaded emoji and copy its ID, or type `\:name:` in Discord to reveal the full
form. A bare `:name:` shortcode stops startup, because Discord expands
shortcodes only in its own message box and a bot sending one would have it
printed verbatim. An unknown slot name stops startup too; the slots are:

| Slot | Where it appears | Lucide icon to upload |
| --- | --- | --- |
| `case` | case titles with no specific outcome | `folder-open` |
| `pin` | the Pin to mod log button | `pin` |
| `warn` | member warned | `megaphone` |
| `ban` | member banned, permanently or temporarily | `hammer` |
| `unban` | ban lifted or expired | `user-round-check` |
| `standing` | the `/standing` card | `gauge` |
| `welcome` | the welcome greeting | `hand-heart` |
| `reason` | the "why" line of every case | `file-text` |
| `details` | evidence block, lookup findings | `search` |
| `member` | member reference, Discord ID field | `user-round` |
| `moderator` | who acted on the case | `shield` |
| `channel` | channel reference | `hash` |
| `deleted` | message deleted | `message-square-x` |
| `timeout` | member timed out | `timer` |
| `kicked` | member kicked | `door-open` |
| `raid` | raid mode enabled | `siren` |
| `join` | join screening | `user-round-plus` |
| `role` | protected roles removed | `user-round-cog` |
| `quarantine` | member quarantined | `lock` |
| `release` | member released, case taken back, channel unlocked | `lock-open` |
| `lock` | channel locked | `lock-keyhole` |
| `denied` | command blocked | `ban` |
| `failed` | any failed or partial enforcement | `triangle-alert` |
| `flagged` | Rotector findings, category | `shield-alert` |
| `safe` | no findings | `badge-check` |
| `review` | awaiting review, logged-only outcomes | `clock` |
| `unknown` | no record, unrecognised status | `circle-question-mark` |
| `roblox` | Roblox accounts and alts | `gamepad-2` |
| `discord` | Discord accounts | `message-circle` |
| `group` | Roblox groups | `users-round` |
| `source` | detection sources | `satellite-dish` |
| `forum` | forum exports | `message-square-text` |
| `image` | exported images | `image` |

## `[permissions]`

Command access uses five tiers: `everyone`, `helper`, `moderator`, `admin`, and
`owner`. A member who satisfies a higher tier satisfies every tier below it, and
anyone in `trusted_user_ids` counts as `owner`.

- `helper_role_ids`, `helper_user_ids`, `moderator_role_ids`,
  `moderator_user_ids`, `admin_role_ids`, `admin_user_ids`, `owner_role_ids`,
  and `owner_user_ids`: tier membership.
- `allow_discord_permissions`: when true (the default), Discord's own
  Administrator permission grants `admin`, and any of Moderate Members, Manage
  Roles, Manage Channels, Manage Messages, Kick Members, Ban Members, or Manage
  Server grants `moderator`.
- `[permissions.commands]`: the tier each command requires. Unlisted commands
  keep their default, and an unknown command name stops startup.

| Command | Default tier |
| --- | --- |
| `/help`, `/hugh` | `everyone` |
| `/modlogs`, `/standing` | `helper` |
| `/check` | `moderator` |
| `/lock`, `/unlock`, `/banish`, `/unbanish` | `moderator` |
| `/warn`, `/kick` | `moderator` |
| `/ban`, `/unban`, `/unwarn` | `admin` |
| `/scrape` | `owner` |

While every tier list is empty, Hugh keeps Discord's own command gating on its
staff commands, so clients hide them from members without the matching Discord
permissions. Listing even one role or user in a tier that can satisfy a command
removes that Discord-side gate for it: the command becomes visible and Hugh
enforces the tier itself. This is what lets a plain staff role use `/banish`
without holding Manage Roles.

A refused command answers privately with the tier it needs and the tier the
member holds, and is recorded as a `permission_denied` case so repeated
attempts at staff tooling stay visible.

No Discord permission grants the `owner` tier — Administrator reaches `admin`
and stops there. An `owner` command such as `/scrape` therefore needs its
holders listed in `owner_user_ids`, `owner_role_ids`, or `trusted_user_ids`
before anyone can run it.

## Slash commands

Hugh registers commands in the configured guild at startup, gated as described
under `[permissions]`. Each staff command accepts a `reason` (up to 512
characters) that is preserved in its case log; it is required on `/warn` and
optional elsewhere. Members in `trusted_user_ids` may always use them.

Nobody can act on a member at or above their own tier, so a moderator cannot
warn, kick or ban another moderator.

`/ban` takes an optional `length` such as `30m`, `1h30m`, `2d` or `1w`; omitting
it bans permanently, and a temporary ban is lifted by the sweep described under
`[moderation]`. `/unban` takes a user ID rather than a member, since the account
has already left. `/unwarn` takes a case number from `/modlogs` and revokes it,
so it stops costing standing.

Hugh re-checks the required tier inside its own handler on every invocation, so
a change to Discord permissions never bypasses `[permissions]`.

`/lock` adds denies for sending, creating threads, and reacting to each role in
`lock.restricted_role_ids`, or to `@everyone` when that list is empty, and
leaves every other permission setting on the channel as it was. Roles in
`lock.bypass_role_ids` receive an explicit allow for the same permissions, so
staff keep talking in a locked channel without Hugh having to guess at their
roles.

`/unlock` takes those same permission bits back out, and removes an overwrite
entirely once nothing is left in it, so the channel inherits from its category
the way it did before the lock. Because it clears only the bits `/lock` sets, it
cannot disturb an unrelated setting, and no durable channel snapshot is needed.
