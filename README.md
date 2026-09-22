# Hugh

Hugh is a fast, single-server Discord security bot written in Rust. It focuses
on predictable, explainable moderation instead of opaque scoring.

The current core protects against:

- join raids and very young accounts joining during a raid;
- mass user, role, `@everyone`, and `@here` mentions;
- repeated pings of protected staff roles;
- message floods and repeated-message spam;
- unapproved Discord invite links;
- self-elevation into protected roles;
- reversible member quarantine with durable role snapshots;
- automatic quarantine role/channel provisioning and server-wide isolation;
- moderator-operated channel lockdowns.

Every threshold is configured in one TOML file. Trusted users and roles can be
exempted, all enforcement is limited to one guild, and significant actions are
delivered by webhook and written to both structured logs and an append-only
JSONL incident log.

## Quick start

1. Install stable Rust (1.97 or newer) and create a Discord application/bot.
2. Copy `config.example.toml` to `config.toml` and fill in the server, channel,
   and role IDs.
3. Create a private incident webhook, then enable the **Server Members Intent**
   and **Message Content Intent** in the Discord Developer Portal.
4. Invite Hugh with the permissions listed in [the setup guide](docs/setup.md).
5. Set `DISCORD_TOKEN` and `HUGH_INCIDENT_WEBHOOK_URL`, then start the bot:

   ```text
   cargo run --release -- --config config.toml
   ```

Validate configuration without connecting to Discord:

```text
cargo run -- --config config.toml --check-config
```

Docker users can instead copy `.env.example` to `.env`, fill in both required
secrets, and run `docker compose up -d --build` after creating the same config
file.

## Commands

- `/lock [channel]` locks the selected or current channel.
- `/banish user` stores the member's roles and moves them into quarantine.
- `/unbanish user` restores the stored roles and releases the member.
- `/help` shows the in-Discord guide.
- `/hugh` introduces the project and links to this repository.

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
