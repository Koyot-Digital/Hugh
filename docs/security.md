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
a durable case number, timestamp, detector name, action outcome, subject and
moderator snapshots (Discord ID, username, display name, avatar URL, and bot
flag), the reason, channel ID, and small structured evidence. Message bodies,
URL paths or queries, email addresses, tokens, and attachment contents are not
stored.

The same case is posted by Hugh to `incident_channel_id` as a card
built for reading rather than parsing: the title states the outcome in plain
English ("Case #42 · Message deleted · member timed out"), the description gives
the reason, and the details block renders each piece of evidence with a human
label, durations as "10 minutes", role IDs as role mentions, and empty or
duplicated entries dropped. The JSONL record keeps the raw codes, so dashboards
are unaffected. Mentions are disabled on delivery, so a report does not
ping its subject or moderator. Protect
the file and incident channel as moderation data, rotate the file using the
host's normal log rotation, and set a suitable retention period. A failure to
post is logged locally; it does not suppress enforcement. Case
numbers continue from the greatest valid `case_id` in the existing JSONL file.

`data/quarantine.jsonl` contains the user and role IDs needed for recovery. Do
not delete or edit it while members are quarantined. Back it up with the service
data and restrict filesystem access to the Hugh service account.

## Forum exports

`/scrape` is the one feature that does retain message content, because copying
posts out of a forum is its entire purpose. An export under
`scrape.output_dir` holds post titles, message text, author identities, and
image URLs for a whole channel. Treat those files as a copy of the community's
content rather than as moderation metadata: restrict them to the service
account, keep them out of version control (`data/` is already ignored), and
delete a run you no longer need.

The command is `owner` by default and cannot be reached by a Discord permission
alone, so only the people listed in `[permissions]` can produce one. Every run
is recorded as a numbered case naming who exported which forum, so an export
cannot be taken quietly. Set `scrape.enabled = false` where no one should be
able to take one at all.

## Safe rollout

1. Configure only known IDs and validate with `--check-config`.
2. Start raid and standalone new-account protection in `log` mode.
3. Exercise each rule with test accounts beneath Hugh in the role hierarchy.
4. Confirm the incident channel and JSONL record show the expected result.
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
