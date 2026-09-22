# Security and operations

Hugh reduces response time; it is not a replacement for Discord's built-in
verification level, AutoMod, explicit staff permissions, MFA, and a tested
incident plan.

## Recommended server posture

- Require 2FA for moderation actions.
- Disable `Mention @everyone, @here, and All Roles` for ordinary members.
- Give staff separate daily-use and emergency roles.
- Remove `Administrator` wherever narrower permissions work.
- Review integrations, webhooks, bots, and role hierarchy regularly.
- Enable Discord AutoMod as an independent layer.
- Keep Hugh's role above protected roles but below roles it never needs to
  manage. Do not grant Hugh Administrator.

## Token handling

Use a service secret/environment variable. Restrict who can inspect the service
environment and logs. If a token is exposed, reset it immediately in the
Developer Portal and restart Hugh. The token and incident webhook URL are never
intentionally logged or written to the incident file. Rotate the webhook as
well as the token if either is exposed.

## Incident records

Each line of `data/incidents.jsonl` is a standalone JSON object. Records contain
timestamps, Discord IDs, detector names, action outcomes, and small numeric
details. Message content, usernames, email addresses, tokens, and attachment
data are not stored.

The same summary is delivered through the configured Discord webhook. Protect
the file and webhook channel as moderation data, rotate the file using the
host's normal log rotation, and set a suitable retention period. Webhook
delivery failure is logged locally; it does not suppress enforcement.

`data/quarantine.jsonl` contains the user and role IDs needed for recovery. Do
not delete or edit it while members are quarantined. Back it up with the service
data and restrict filesystem access to the Hugh service account.

## Safe rollout

1. Configure only known IDs and validate with `--check-config`.
2. Start raid protection in `log` mode.
3. Exercise each rule with test accounts beneath Hugh in the role hierarchy.
4. Confirm the incident webhook and JSONL record show the expected result.
5. Tune using normal traffic before enabling raid timeout/kick.
6. Test `/banish` and `/unbanish` with expendable roles, including a restart
   between those commands.
7. Test loss of `Manage Messages`, `Moderate Members`, `Manage Channels`, and
   `Manage Roles` so operators recognize failure logs.

## Recovery

If Hugh is misbehaving, stop its process first. Discord timeouts can be removed
from the member moderation UI. Kicked users can rejoin with a valid invite.
If automated restoration fails, keep the quarantine journal and restore the
listed role IDs manually before removing the quarantine role. Hugh does not ban
accounts: “banish” means reversible quarantine, not a Discord ban.

See [SECURITY.md](../SECURITY.md) for private vulnerability reporting.
