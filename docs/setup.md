# Setup

## 1. Create the bot

1. Open the Discord Developer Portal and create an application named **Hugh**.
2. On **Bot**, create the bot user and reset/copy its token.
3. Enable **Server Members Intent** and **Message Content Intent**. Presence is
   not used and should remain disabled.
4. Never paste the token into `config.toml`, a commit, an issue, or a log.

Create a webhook in a private staff incident channel under **Edit Channel >
Integrations > Webhooks**. Protect its URL like the bot token: anyone with that
URL can post through the webhook.

## 2. Invite Hugh

In **OAuth2 > URL Generator**, select the `bot` and `applications.commands`
scopes and these bot permissions:

- View Channels
- Send Messages
- Read Message History
- Manage Messages
- Moderate Members
- Kick Members only if `young_account_action = "kick"`
- Manage Roles if the role guard or quarantine is enabled
- Manage Channels for `/lock` and quarantine isolation

Hugh does not require Administrator. Granting it is strongly discouraged.

Place Hugh's Discord role above every member it may time out or kick and above
every role it must remove. Discord's role hierarchy applies even if the matching
permission is present.

Restrict the webhook's incident channel so only appropriate staff can view it.
Reports include user, channel, role, and message IDs. A local JSONL copy remains
available if webhook delivery fails.

## 3. Quarantine resources

With quarantine enabled, Hugh creates or reuses an empty `Quarantined` role and
the `#quarantine` text channel when it connects. The names are configurable.
Hugh must have **Manage Roles** and **Manage Channels**, and its Discord role
must remain above the quarantine role.

At startup Hugh preserves unrelated overwrites while ensuring `@everyone`
cannot view the quarantine channel, the quarantine role and Hugh can view it,
and the quarantine role cannot view any other existing or newly created
channel. Hugh adds an explicit deny to accessible channels and safely keeps an
existing `@everyone` view deny on channels that are already private. Server
owners, administrators, and managed integration roles with an explicit channel
allow can still bypass this isolation. Verify the result before production use.

## 4. Configure

Enable Developer Mode under Discord **User Settings > Advanced**. You can then
right-click servers, channels, roles, and users to copy their IDs.

```text
cp config.example.toml config.toml
```

Edit every placeholder ID, then validate the file. Quarantine channel and role
IDs are optional; omit both for automatic provisioning:

```text
cargo run -- --config config.toml --check-config
```

Start with raid action `log`, watch normal traffic, and tune thresholds before
enabling `timeout` or `kick`.

## 5. Run

Linux/macOS:

```text
export DISCORD_TOKEN='replace-me'
export HUGH_INCIDENT_WEBHOOK_URL='https://discord.com/api/webhooks/...'
cargo run --release -- --config config.toml
```

PowerShell:

```powershell
$env:DISCORD_TOKEN = 'replace-me'
$env:HUGH_INCIDENT_WEBHOOK_URL = 'https://discord.com/api/webhooks/...'
cargo run --release -- --config config.toml
```

Set `RUST_LOG=hugh=debug` for troubleshooting or `HUGH_LOG_FORMAT=json` for
machine-readable process logs. Stop with Ctrl+C; Hugh shuts down its shards
cleanly.

## Docker

Copy the environment template, then edit `.env` (ignored by Git):

```text
cp .env.example .env
```

Replace both placeholder secrets in `.env` before starting Hugh.

Then run:

```text
docker compose up -d --build
docker compose logs -f hugh
```

The compose file mounts `config.toml` read-only and stores incident and
quarantine recovery logs in the named `hugh-data` volume.

## Updating

Stop the service, back up `config.toml` and `data/`, pull the desired tagged
release, validate the configuration, rebuild, and inspect startup logs. Read the
changelog before crossing a major version.
