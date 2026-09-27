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
a durable case number, timestamp, detector name, action outcome, subject and
moderator snapshots (Discord ID, username, display name, avatar URL, and bot
flag), the reason, channel ID, and small structured evidence. Message bodies,
URL paths or queries, email addresses, tokens, and attachment contents are not
stored.

The same case is delivered through the configured Discord webhook with direct
user/channel references and readable durations. Mentions are disabled on
webhook delivery, so a report does not ping its subject or moderator. Protect
the file and webhook channel as moderation data, rotate the file using the
host's normal log rotation, and set a suitable retention period. Webhook
delivery failure is logged locally; it does not suppress enforcement. Case
numbers continue from the greatest valid `case_id` in the existing JSONL file.

`data/quarantine.jsonl` contains the user and role IDs needed for recovery. Do
not delete or edit it while members are quarantined. Back it up with the service
data and restrict filesystem access to the Hugh service account.

## Safe rollout

1. Configure only known IDs and validate with `--check-config`.
2. Start raid and standalone new-account protection in `log` mode.
3. Exercise each rule with test accounts beneath Hugh in the role hierarchy.
4. Confirm the incident webhook and JSONL record show the expected result.
5. Tune using normal traffic before enabling either account-age timeout/kick.
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
