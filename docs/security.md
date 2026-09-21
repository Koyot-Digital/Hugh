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
Developer Portal and restart Hugh. The token is never intentionally logged or
written to the incident file.

## Incident records

Each line of `data/incidents.jsonl` is a standalone JSON object. Records contain
timestamps, Discord IDs, detector names, action outcomes, and small numeric
details. Message content, usernames, email addresses, tokens, and attachment
data are not stored.

Protect the file as moderation data, rotate it using the host's normal log
rotation, and set a retention period appropriate to your community. Alert
delivery failure is logged locally; it does not suppress enforcement.

## Safe rollout

1. Configure only known IDs and validate with `--check-config`.
2. Start raid protection in `log` mode.
3. Exercise each rule with test accounts beneath Hugh in the role hierarchy.
4. Confirm the alert channel and JSONL record show the expected result.
5. Tune using normal traffic before enabling raid timeout/kick.
6. Test loss of `Manage Messages`, `Moderate Members`, and `Manage Roles` so
   operators recognize failure logs.

## Recovery

If Hugh is misbehaving, stop its process first. Discord timeouts can be removed
from the member moderation UI. Kicked users can rejoin with a valid invite.
Removed roles must be restored by an authorized administrator. Hugh does not
ban accounts or automatically edit channel permission overwrites in this
release.

See [SECURITY.md](../SECURITY.md) for private vulnerability reporting.
