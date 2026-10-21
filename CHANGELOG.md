# Changelog

All notable changes will be documented here. This project follows Semantic
Versioning and the Keep a Changelog format.

## [Unreleased]

### Added

- A durable moderation record behind `/warn`, `/kick`, `/ban`, `/unban`,
  `/unwarn`, `/modlogs` and `/standing`. Warnings, timeouts, kicks, bans and
  automod hits are all filed as numbered cases, `/ban` accepts a length such as
  `1h30m` or `7d`, and Hugh lifts a temporary ban by itself when the time is
  up. A case can be taken back with `/unwarn` so it stops counting.
- **Standing**: one score per member out of `standing.starting_points` that
  every punishment takes weight off and that fades back over
  `standing.decay_days`. Standing picks a band, and the band scales the automod
  limits and timeout lengths that member is held to, so a good record earns
  more room and a bad one trips the guards sooner.
- Automatic escalation. Hugh adds each automod hit's weight to a rolling
  per-member total and steps up from deletion to timeout, kick and ban as that
  total passes the `[escalation]` thresholds, one step at a time.
- Automod guards for shouting, character padding, emoji floods, zalgo text,
  configurable blocked phrases, one message walked across several channels, and
  links posted by accounts created minutes ago.
- `/unlock`, which removes only what `/lock` took and leaves every other
  permission setting on the channel alone. Both now act on the roles named in
  `[lock]`, falling back to `@everyone`, and `lock.bypass_role_ids` keeps staff
  talking in a locked channel.
- An optional `[welcome]` greeting for new members, with `{user}`,
  `{username}`, `{display}`, `{server}`, `{count}`, `{ordinal}` and `{id}`
  placeholders. It stays quiet during a raid and for anyone Hugh has just
  removed.
- `/scrape`, an owner-only forum export: every post's title, opening message,
  author, tags, timestamps and images written as JSON Lines, one
  self-contained record per post, attached to the reply and saved under
  `scrape.output_dir`. Archived posts and per-post replies are optional, and
  each run is filed as a numbered case.
- `/check` staff lookups against the Rotector API for Discord accounts,
  Roblox accounts, linked accounts and alts, Roblox groups, and enabled
  detection sources, with a case log for every flagged result.
- A **Pin for everyone** button on every `/check` result, which re-posts the
  ephemeral card into the channel for all to see and files it as a numbered
  case.
- Configurable staff tiers (`everyone`, `helper`, `moderator`, `admin`,
  `owner`) with a required tier per command in `[permissions.commands]`, and a
  `permission_denied` case when a command is refused.
- Durable, numbered case logs with user snapshots, moderator identity, reasons,
  action outcomes, channel context, and structured evidence.
- Optional reasons for `/lock`, `/banish`, and `/unbanish`.
- Edited-message scanning, repeated-mention counting, coordinated multi-account
  spam detection, content limits, blocked domains, and standalone new-account
  screening.
- Case delivery to a private staff channel with a local JSONL fallback.
- `/lock`, `/banish`, `/unbanish`, `/help`, and `/hugh` guild commands.
- Durable quarantine role snapshots and automatic channel isolation.
- A project introduction when Hugh is mentioned.

### Changed

- Staff can no longer act on a member at or above their own tier.
- Unset icon slots now render as nothing at all. Hugh previously substituted a
  plain emoji of its own, which sat oddly beside a server's own branding.
- Hugh posts case cards itself, to the new required `incident_channel_id`,
  instead of through a webhook. A webhook cannot resolve the custom emoji
  configured in `[icons]`, so every icon arrived as literal `:name:` text in
  the case log. Hugh needs View Channel, Send Messages and Embed Links there.
- Case and lookup cards redesigned: icon-led titles, member avatars as the card
  author and thumbnail, Roblox headshots on Roblox lookups, one line of
  clickable references instead of three fields, and a configurable `[icons]`
  slot per label.
- Case reports now lead with a plain-English outcome, label every piece of
  evidence, render durations and role mentions, and omit empty details.
- Console logs are compact and target-free by default; `HUGH_LOG_FORMAT=json`
  is unchanged.

### Removed

- `HUGH_INCIDENT_WEBHOOK_URL`, replaced by `incident_channel_id` in
  `config.toml`.
- The `/talk` command, the OpenAI integration behind it, and `OPENAI_API_KEY`.
  Hugh no longer calls any AI service.

## [0.1.0] - 2026-09-22

### Added

- Single-guild event scoping and strict TOML configuration validation.
- Join-raid detection with young-account log, timeout, or kick actions.
- Mass mention, protected staff-role ping, flood, duplicate, and invite guards.
- Explicit-allowlist protected-role enforcement against self-elevation.
- Private Discord alerts and privacy-minimal append-only JSONL incidents.
- Graceful shutdown, structured logging, tests, Docker packaging, and guides.
