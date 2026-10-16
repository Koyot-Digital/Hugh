# Configuration reference

Hugh reads configuration once at startup. Restart it after changes. Unknown
keys and invalid values stop startup, preventing misspellings from silently
disabling protection. Discord IDs are strings in TOML to preserve their exact
integer value.

## Top-level values

| Key | Meaning |
| --- | --- |
| `guild_id` | The only server Hugh accepts events from. |
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
| `HUGH_INCIDENT_WEBHOOK_URL` | Required private Discord webhook URL for incidents. |
| `HUGH_ROTECTOR_API_KEY` | Required when `[rotector].enabled = true`; sent as the bearer token on Rotector lookups. |
| `HUGH_CONFIG` | Config path; overridden by `--config`. |
| `RUST_LOG` | Log filter, such as `hugh=debug,serenity=warn`. |
| `HUGH_LOG_FORMAT` | Set to `json` for structured process logs. |

Discord permits member timeouts up to 28 days; Hugh clamps values at that API
limit.

## `[rotector]`

Rotector is a third-party service that flags Roblox and Discord accounts for
predatory and inappropriate behaviour. When this section is enabled, Hugh
registers the staff-only `/rotector` command and reads
`HUGH_ROTECTOR_API_KEY` at startup.

- `enabled`: registers `/rotector` and requires the API key.
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
| `/rotector discord [user] [id]` | `/v2/lookup/rotector/discord/user/{id}` | Verdict, findings, linked flagged Roblox accounts. |
| `/rotector roblox id` | `/v2/lookup/rotector/roblox/user/{id}` | Verdict, category, findings, findings awaiting review, reviewer, record links. |
| `/rotector links id` | `/v2/lookup/rotector/roblox/user/{id}/discord` | Linked Discord accounts, tracked servers, and known alts. |
| `/rotector group id` | `/v2/lookup/rotector/roblox/group/{id}` | Group verdict and findings. |
| `/rotector sources` | `/v2/providers` | Detection sources enabled for Hugh's key. |

Verdicts come from the API's `flagType` field: no findings, flagged, confirmed,
being processed, provisional, mixed, past offender, redacted, or awaiting human
review. Only the first six count as findings, and only a finding produces a
case log. A `404` is reported as "no record", which is not a clearance: the
account may simply never have been analysed. Hugh does not screen joins against
Rotector; a moderator decides what to do with every result.

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
| `/rotector` | `moderator` |
| `/lock`, `/banish`, `/unbanish` | `moderator` |

While every tier list is empty, Hugh keeps Discord's own command gating on its
staff commands, so clients hide them from members without the matching Discord
permissions. Listing even one role or user in a tier that can satisfy a command
removes that Discord-side gate for it: the command becomes visible and Hugh
enforces the tier itself. This is what lets a plain staff role use `/banish`
without holding Manage Roles.

A refused command answers privately with the tier it needs and the tier the
member holds, and is recorded as a `permission_denied` case so repeated
attempts at staff tooling stay visible.

## Slash commands

Hugh registers commands in the configured guild at startup, gated as described
under `[permissions]`. `/lock`, `/banish`, and `/unbanish` each accept an
optional `reason` (up to 512 characters) that is preserved in its case log.
Members in `trusted_user_ids` may always use them.

Hugh re-checks the required tier inside its own handler on every invocation, so
a change to Discord permissions never bypasses `[permissions]`.

`/lock` preserves the channel's existing `@everyone` overwrite and adds denies
for sending, creating threads, and reacting. This release intentionally does
not provide `/unlock`: safely restoring an exact pre-lock state requires a
separate durable channel snapshot. An administrator can reverse those specific
deny settings in Discord's channel permission editor.
