# Changelog

All notable changes will be documented here. This project follows Semantic
Versioning and the Keep a Changelog format.

## [Unreleased]

### Added

- `/rotector` staff lookups against the Rotector API for Discord accounts,
  Roblox accounts, linked accounts and alts, Roblox groups, and enabled
  detection sources, with a case log for every flagged result.
- Configurable staff tiers (`everyone`, `helper`, `moderator`, `admin`,
  `owner`) with a required tier per command in `[permissions.commands]`, and a
  `permission_denied` case when a command is refused.
- Durable, numbered case logs with user snapshots, moderator identity, reasons,
  action outcomes, channel context, and structured evidence.
- Optional reasons for `/lock`, `/banish`, and `/unbanish`.
- Edited-message scanning, repeated-mention counting, coordinated multi-account
  spam detection, content limits, blocked domains, and standalone new-account
  screening.
- Webhook incident delivery with a local JSONL fallback.
- `/lock`, `/banish`, `/unbanish`, `/help`, and `/hugh` guild commands.
- Durable quarantine role snapshots and automatic channel isolation.
- A project introduction when Hugh is mentioned.

### Changed

- Case reports now lead with a plain-English outcome, label every piece of
  evidence, render durations and role mentions, and omit empty details.
- Console logs are compact and target-free by default; `HUGH_LOG_FORMAT=json`
  is unchanged.

### Removed

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
