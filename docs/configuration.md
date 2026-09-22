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
| `trusted_user_ids` | Users that bypass message guards and raid enforcement. |
| `trusted_role_ids` | Roles that bypass message guards. Use very sparingly. |

Hugh ignores only its own messages to avoid moderation loops. Messages from
other bots and webhooks are checked and can be deleted, but automated senders
are not timed out because Discord does not treat them like ordinary members.

## `[mentions]`

- `max_total_mentions`: maximum actual user and role mentions plus an active
  `@everyone`/`@here` mention in one message. A violation deletes and times out.
- `max_everyone_mentions`: use `0` to block active `@everyone`/`@here` mentions;
  a positive value allows them through this check. Discord exposes this as a
  per-message flag rather than a count.
- `protected_role_ids`: staff or incident-response roles whose pings are
  rate-limited.
- `protected_role_max_pings`: allowed protected-role-pinging messages per user
  in the rolling window. With `2`, the third is acted on.
- `protected_role_window_seconds`: rolling window length.
- `timeout_seconds`: timeout applied for mention violations.

## `[spam]`

- `max_messages` and `window_seconds`: per-user rolling flood limit.
- `max_duplicates` and `duplicate_window_seconds`: repeated normalized message
  limit. Leading/trailing whitespace and letter case are ignored.
- `timeout_seconds`: timeout applied for spam violations.

## `[invites]`

When enabled, Discord invite links are deleted unless their code appears in
`allowed_codes`. Store only the code (for example `our-community`), not the full
URL. Vanity codes are supported. This detector intentionally does not block all
URLs.

## `[raids]`

`join_threshold` joins inside `join_window_seconds` activates raid mode for
`raid_mode_seconds`. Further threshold-reaching joins extend it. During raid
mode, accounts younger than `minimum_account_age_seconds` receive one of:

- `log`: alert and record only (recommended while tuning);
- `timeout`: temporarily prevent communication; or
- `kick`: remove the member without banning them.

Hugh does not lock every channel automatically. Incorrect global permission
overwrites can lock out staff and are difficult to roll back safely.

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
| `HUGH_CONFIG` | Config path; overridden by `--config`. |
| `RUST_LOG` | Log filter, such as `hugh=debug,serenity=warn`. |
| `HUGH_LOG_FORMAT` | Set to `json` for structured process logs. |

Discord permits member timeouts up to 28 days; Hugh clamps values at that API
limit.

## Slash commands

Hugh registers commands in the configured guild at startup. `/lock`, `/banish`,
and `/unbanish` have Discord permission defaults and repeat authorization
checks inside Hugh. Members in `trusted_user_ids` may also use them.

`/lock` preserves the channel's existing `@everyone` overwrite and adds denies
for sending, creating threads, and reacting. This release intentionally does
not provide `/unlock`: safely restoring an exact pre-lock state requires a
separate durable channel snapshot. An administrator can reverse those specific
deny settings in Discord's channel permission editor.
