# Setup

## 1. Create the bot

1. Open the Discord Developer Portal and create an application named **Hugh**.
2. On **Bot**, create the bot user and reset/copy its token.
3. Enable **Server Members Intent** and **Message Content Intent**. Presence is
   not used and should remain disabled.
4. Never paste the token into `config.toml`, a commit, an issue, or a log.

## 2. Invite Hugh

In **OAuth2 > URL Generator**, select the `bot` scope and these bot permissions:

- View Channels
- Send Messages (for the alert channel)
- Read Message History
- Manage Messages
- Moderate Members
- Kick Members only if `young_account_action = "kick"`
- Manage Roles only if the role guard is enabled

Hugh does not require Administrator. Granting it is strongly discouraged.

Place Hugh's Discord role above every member it may time out or kick and above
every role it must remove. Discord's role hierarchy applies even if the matching
permission is present.

Restrict the configured alert channel so only Hugh and appropriate staff can
view it. The channel receives user, channel, role, and message IDs associated
with incidents.

## 3. Configure

Enable Developer Mode under Discord **User Settings > Advanced**. You can then
right-click servers, channels, roles, and users to copy their IDs.

```text
cp config.example.toml config.toml
```

Edit every placeholder ID, then validate the file:

```text
cargo run -- --config config.toml --check-config
```

Start with raid action `log`, watch normal traffic, and tune thresholds before
enabling `timeout` or `kick`.

## 4. Run

Linux/macOS:

```text
export DISCORD_TOKEN='replace-me'
cargo run --release -- --config config.toml
```

PowerShell:

```powershell
$env:DISCORD_TOKEN = 'replace-me'
cargo run --release -- --config config.toml
```

Set `RUST_LOG=hugh=debug` for troubleshooting or `HUGH_LOG_FORMAT=json` for
machine-readable process logs. Stop with Ctrl+C; Hugh shuts down its shards
cleanly.

## Docker

Create `.env` (ignored by Git):

```text
DISCORD_TOKEN=replace-me
```

Then run:

```text
docker compose up -d --build
docker compose logs -f hugh
```

The compose file mounts `config.toml` read-only and stores incident logs in the
named `hugh-data` volume.

## Updating

Stop the service, back up `config.toml` and `data/`, pull the desired tagged
release, validate the configuration, rebuild, and inspect startup logs. Read the
changelog before crossing a major version.
