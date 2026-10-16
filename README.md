# Hugh

Hugh is a fast, single-server Discord security bot written in Rust. It focuses
on predictable, explainable moderation instead of opaque scoring.

The current core protects against:

- join raids and very young accounts joining during a raid;
- mass user, role, `@everyone`, and `@here` mentions;
- repeated pings of protected staff roles;
- message floods and repeated-message spam;
- coordinated copy/paste spam across multiple accounts;
- unapproved Discord invite links;
- blocked domains, link floods, attachment floods, and excessive line breaks;
- unsafe content added by editing an existing message;
- configurable screening of newly created accounts outside raid mode;
- self-elevation into protected roles;
- reversible member quarantine with durable role snapshots;
- automatic quarantine role/channel provisioning and server-wide isolation;
- moderator-operated channel lockdowns;
- staff account-safety lookups through the Rotector API.

Every threshold is configured in one TOML file. Trusted users and roles can be
exempted, command access is set per command across five staff tiers, all
enforcement is limited to one guild, and significant actions are delivered as
numbered webhook cases and written to both structured logs and an append-only
JSONL incident log.

Case reports are written to be read: an icon and plain-English outcome lead the
title, the subject's avatar anchors the card, every clickable reference sits on
one line, and the details block labels each piece of evidence, renders durations
as "10 minutes", shows role IDs as role mentions, and drops empty fields. Icons
are configurable per slot, so a server can swap in its own Lucide-style emoji.
Message bodies are never retained.

## Quick start

1. Install stable Rust (1.97 or newer) and create a Discord application/bot.
2. Copy `config.example.toml` to `config.toml` and fill in the server, channel,
   and role IDs.
3. Create a private incident webhook, then enable the **Server Members Intent**
   and **Message Content Intent** in the Discord Developer Portal.
4. Invite Hugh with the permissions listed in [the setup guide](docs/setup.md).
5. Set `DISCORD_TOKEN` and `HUGH_INCIDENT_WEBHOOK_URL` (plus
   `HUGH_ROTECTOR_API_KEY` if you enable `[rotector]`), then start the bot:

   ```text
   cargo run --release -- --config config.toml
   ```

Validate configuration without connecting to Discord:

```text
cargo run -- --config config.toml --check-config
```

Docker users can instead copy `.env.example` to `.env`, fill in the required
secrets, and run `docker compose up -d --build` after creating the same config
file.

## Commands

- `/lock [channel] [reason]` locks the selected or current channel.
- `/banish user [reason]` stores the member's roles and moves them into quarantine.
- `/unbanish user [reason]` restores the stored roles and releases the member.
- `/check …` runs staff account-safety lookups (see below).
- `/help` shows the in-Discord guide, including the tier each command needs.
- `/hugh` introduces the project and links to this repository.

Each command's required tier — `everyone`, `helper`, `moderator`, `admin`, or
`owner` — is set in `[permissions.commands]`, and tier membership is a list of
roles and users per tier. A higher tier always satisfies a lower one. Left
unconfigured, Hugh behaves as before and relies on Discord's own permission
gating.

With `[rotector]` enabled, staff can check accounts against the Rotector API:

- `/check discord [user] [id]` — findings for a Discord account, plus linked
  flagged Roblox accounts.
- `/check roblox id` — findings for a Roblox account, with category,
  reviewer, and record links.
- `/check links id` — Discord accounts, tracked servers, and alts linked to
  a Roblox account.
- `/check group id` — findings for a Roblox group.
- `/check sources` — which detection sources the key reports on.

Lookups answer privately, send only the identifier being checked, and write a
case log when a result comes back flagged. Each result carries a **Pin for
everyone** button that re-posts the card into the channel as a normal message,
turning a private, disappearing result into something the whole staff channel
can see and keep. Pinned cards are also filed as numbered cases.

Mentioning Hugh also shows the short project introduction.

## Documentation

- [Installation and Discord setup](docs/setup.md)
- [Configuration reference](docs/configuration.md)
- [Architecture and extension guide](docs/architecture.md)
- [Security model and operating guide](docs/security.md)
- [Contributing](CONTRIBUTING.md) and [security disclosures](SECURITY.md)

## Design goals

- **Fast:** one async runtime, short in-memory hot paths, no database lookup per
  message, and bounded/pruned rate-limit state.
- **Secure:** no token in config, minimum intents, single-guild scoping,
  conservative defaults, validated configuration, and no message content in
  incident logs.
- **Modular:** pure detectors are separate from Discord event handling and
  enforcement. A new detector can be added without changing storage or startup.

Hugh is licensed under the [MIT License](LICENSE).
