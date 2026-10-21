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
- shouting, character padding, emoji floods, zalgo text, and blocked phrases;
- one message walked around the server, channel by channel;
- links from accounts created minutes ago;
- unsafe content added by editing an existing message;
- configurable screening of newly created accounts outside raid mode;
- self-elevation into protected roles;
- reversible member quarantine with durable role snapshots;
- automatic quarantine role/channel provisioning and server-wide isolation;
- moderator-operated channel lockdowns, scoped to the roles you name;
- repeat offending, through automatic escalation from deletion to timeout,
  kick and ban;
- staff account-safety lookups through the Rotector API;
- owner-only forum exports for archiving a community's posts.

Hugh keeps a durable moderation record per member: warnings, timeouts, kicks,
bans and automod hits, each as a numbered case. That record carries a
**standing** score out of 100 that every punishment takes weight off and that
fades back over time, and standing in turn decides how tightly the automod
limits are drawn around that member. A member in good standing is given more
room; one with a bad record trips the guards sooner and is timed out for
longer.

Every threshold is configured in one TOML file. Trusted users and roles can be
exempted, command access is set per command across five staff tiers, all
enforcement is limited to one guild, and significant actions are posted as
numbered cases to a private staff channel and written to both structured logs
and an append-only JSONL incident log.

Case reports are written to be read: an icon and plain-English outcome lead the
title, the subject's avatar anchors the card, every clickable reference sits on
one line, and the details block labels each piece of evidence, renders durations
as "10 minutes", shows role IDs as role mentions, and drops empty fields. Icons
are configurable per slot, so a server can swap in its own Lucide-style emoji.
Message bodies are never retained, with one deliberate exception: the
owner-only `/scrape` export, whose purpose is to copy a forum's posts out.

## Quick start

1. Install stable Rust (1.97 or newer) and create a Discord application/bot.
2. Copy `config.example.toml` to `config.toml` and fill in the server, channel,
   and role IDs.
3. Create a private staff channel for case cards and set `incident_channel_id`,
   then enable the **Server Members Intent** and **Message Content Intent** in
   the Discord Developer Portal.
4. Invite Hugh with the permissions listed in [the setup guide](docs/setup.md).
5. Set `DISCORD_TOKEN` (plus `HUGH_ROTECTOR_API_KEY` if you enable
   `[rotector]`), then start the bot:

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

- `/lock [channel] [reason]` stops the roles named in `[lock]` — `@everyone`
  when none are named — from speaking in the selected or current channel.
- `/unlock [channel] [reason]` gives the channel back, removing only what the
  lock took and leaving every other permission setting alone.
- `/warn user reason` records a warning against the member's standing.
- `/kick user [reason]` removes the member from the server.
- `/ban user [reason] [length] [delete_days]` bans permanently, or for a length
  such as `1h30m` or `7d`, which Hugh lifts by itself when the time is up.
- `/unban user [reason]` lifts a ban by user ID.
- `/unwarn case [reason]` takes a case back so it stops counting.
- `/modlogs user` shows every case on a member's record.
- `/standing [user]` shows a member's standing and the automod strictness it
  earns them.
- `/banish user [reason]` stores the member's roles and moves them into quarantine.
- `/unbanish user [reason]` restores the stored roles and releases the member.
- `/check …` runs staff account-safety lookups (see below).
- `/scrape forum [archived] [replies] [limit]` exports a forum channel's posts,
  titles and images as JSON Lines, ready to load into a database. Owner only.
- `/help` shows the in-Discord guide, including the tier each command needs.
- `/hugh` introduces the project and links to this repository.

Each command's required tier — `everyone`, `helper`, `moderator`, `admin`, or
`owner` — is set in `[permissions.commands]`, and tier membership is a list of
roles and users per tier. A higher tier always satisfies a lower one. Left
unconfigured, Hugh behaves as before and relies on Discord's own permission
gating. Nobody can act on a member at or above their own tier.

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

`/scrape` walks a forum or media channel and writes one JSON object per post —
title, opening message, author, tags, timestamps, and every image, with
non-image uploads kept separate. Replies are included on request. IDs are
strings and timestamps are RFC 3339, so a record imports without a join and a
partial file is still valid. The export is attached to the private reply and
written to `scrape.output_dir`; each run is filed as a numbered case. See the
[configuration reference](docs/configuration.md) for the record shape, and note
that no Discord permission reaches the `owner` tier, so `/scrape` needs its
holders listed in `[permissions]`.

With `[welcome]` enabled Hugh greets new arrivals in the channel you name,
from a template of your own: `{user}`, `{username}`, `{display}`, `{server}`,
`{count}`, `{ordinal}` and `{id}` are filled in, and an unknown placeholder is
left in place so a typo shows up rather than vanishing. Nobody Hugh has just
removed is welcomed, and the greeting stays quiet during a raid.

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
