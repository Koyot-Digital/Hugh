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
| `OPENAI_API_KEY` | Required when `[talk].enabled = true`; used only for OpenAI Responses API calls. |
| `HUGH_CONFIG` | Config path; overridden by `--config`. |
| `RUST_LOG` | Log filter, such as `hugh=debug,serenity=warn`. |
| `HUGH_LOG_FORMAT` | Set to `json` for structured process logs. |

Discord permits member timeouts up to 28 days; Hugh clamps values at that API
limit.

## `[talk]`

- `enabled`: registers `/talk` and requires `OPENAI_API_KEY` at startup.
- `owner_user_id`: the only Discord user allowed to invoke `/talk`.
- `carson_user_id`: Carson's Discord bot user ID. Only messages from this bot
  continue a conversation.
- `max_messages`: upper bound accepted by `/talk length`; valid from 2 to 200.

The requested length counts Hugh and Carson messages together. One conversation
may run in a channel at a time, and an idle conversation expires after 10
minutes. Hugh rotates through 10 built-in icebreakers, then calls the OpenAI
Responses API with the fixed `gpt-5-nano` model and a 96-token output cap for
each response. Conversation history is kept only in memory and is not written
to incident logs.

## Slash commands

Hugh registers commands in the configured guild at startup. `/lock`, `/banish`,
and `/unbanish` have Discord permission defaults and repeat authorization
checks inside Hugh. Each accepts an optional `reason` (up to 512 characters)
that is preserved in its case log. Members in `trusted_user_ids` may also use
them.

`/talk` does not use moderator permissions or `trusted_user_ids`; it checks the
invoking user against `talk.owner_user_id` inside the command handler.

`/lock` preserves the channel's existing `@everyone` overwrite and adds denies
for sending, creating threads, and reacting. This release intentionally does
not provide `/unlock`: safely restoring an exact pre-lock state requires a
separate durable channel snapshot. An administrator can reverse those specific
deny settings in Discord's channel permission editor.
