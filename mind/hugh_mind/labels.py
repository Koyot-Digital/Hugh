"""Turning what already happened on the server into training labels.

Nobody labels messages by hand. Each source below is a vote with a weight that
reflects how far it can be trusted:

- A message Hugh's automod removed is a positive for the categories of the
  guards it tripped, unless the case was later taken back with `/unwarn`.
  A taken-back case is a strong negative: the rules were wrong about it.
- A moderator's warning, mute, kick, ban or quarantine reaches back to the
  member's last few messages before it, as weak positives for `conduct`.
  This is the category rules cannot see, and the one the model is for.
- A ✅ or ❌ pressed on a case card or a review card overrides everything, at
  triple weight.
- A message that settled with nothing happening to its author for a day either
  side is a clean negative.

Anything else, mostly the last day's messages, stays unlabeled until it settles.
"""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np

from .archive import Messages, Record

LABELS = ["actionable", "spam", "mentions", "links", "style", "content", "conduct"]
ACTIONABLE = 0
CONDUCT = LABELS.index("conduct")
RULE_LABELS = [LABELS.index(name) for name in ("spam", "mentions", "links", "style", "content")]

# Mirrors the violation codes in src/detection.rs.
CATEGORY = {
    "message_flood": "spam",
    "duplicate_spam": "spam",
    "coordinated_spam": "spam",
    "channel_hop_spam": "spam",
    "mass_mention": "mentions",
    "everyone_mention": "mentions",
    "protected_role_ping": "mentions",
    "unapproved_invite": "links",
    "link_flood": "links",
    "blocked_domain": "links",
    "new_account_link": "links",
    "excessive_caps": "style",
    "repeated_characters": "style",
    "emoji_flood": "style",
    "zalgo_text": "style",
    "excessive_newlines": "style",
    "attachment_flood": "style",
    "blocked_phrase": "content",
}

# Punishments a moderator hands out by judgement. Automod cases are already
# covered message by message.
JUDGEMENTS = {"warn", "timeout", "kick", "ban", "temporary_ban", "quarantine"}
# Weight of the messages before a moderator action, most recent first.
ACTION_WEIGHTS = (0.7, 0.5, 0.4)
FEEDBACK_WEIGHT = 3.0
REVOKED_WEIGHT = 2.0


@dataclass
class Labels:
    target: np.ndarray  # float32 (N, len(LABELS))
    mask: np.ndarray  # float32 (N, len(LABELS)): 1 where the label is known
    weight: np.ndarray  # float32 (N,)

    @property
    def labeled(self) -> np.ndarray:
        return (self.weight > 0) & (self.mask.sum(axis=1) > 0)

    @property
    def positive(self) -> np.ndarray:
        return self.labeled & (self.target[:, ACTIONABLE] > 0) & (self.mask[:, ACTIONABLE] > 0)


def build(
    messages: Messages,
    reasons: dict[int, set[str]],
    record: Record,
    verdicts: dict[int, int],
    now: int,
    settle_hours: float,
    action_window_minutes: float,
) -> Labels:
    count = len(messages)
    target = np.zeros((count, len(LABELS)), dtype=np.float32)
    mask = np.zeros_like(target)
    weight = np.zeros(count, dtype=np.float32)
    settle = int(settle_hours * 3_600_000)

    # Clean negatives: settled, still standing, and nobody's case nearby.
    cases = record.by_user()
    settled = messages.created_at <= now - settle
    clear = np.ones(count, dtype=bool)
    for author in np.unique(messages.author):
        times = cases.get(int(author))
        if times is None:
            continue
        rows = np.flatnonzero(messages.author == author)
        sent = messages.created_at[rows]
        # Distance to the nearest of the member's cases, either side.
        after = np.searchsorted(times, sent)
        nearest = np.full(len(rows), np.iinfo(np.int64).max)
        has_after = after < len(times)
        nearest[has_after] = times[after[has_after]] - sent[has_after]
        has_before = after > 0
        before_gap = sent[has_before] - times[after[has_before] - 1]
        nearest[has_before] = np.minimum(nearest[has_before], before_gap)
        clear[rows] = nearest > settle
    negative = settled & clear & ~messages.deleted & ~messages.flagged
    mask[negative] = 1.0
    weight[negative] = 1.0

    # Automod removals.
    case_revoked = np.isin(messages.case_id, list(record.revoked)) & (messages.case_id > 0)
    for row in np.flatnonzero(messages.flagged):
        if case_revoked[row]:
            target[row] = 0.0
            mask[row] = 1.0
            weight[row] = REVOKED_WEIGHT
            continue
        codes = reasons.get(int(messages.ids[row]), set())
        categories = {CATEGORY[code] for code in codes if code in CATEGORY}
        target[row, ACTIONABLE] = 1.0
        mask[row, ACTIONABLE] = 1.0
        if categories:
            # Every rule ran on this message, so a guard that did not fire is
            # a known zero for its category.
            for label in RULE_LABELS:
                mask[row, label] = 1.0
                target[row, label] = float(LABELS[label] in categories)
        weight[row] = 1.0

    # Moderator judgements, reaching back to the messages that earned them.
    window = int(action_window_minutes * 60_000)
    by_author: dict[int, np.ndarray] = {}
    for action in record.actions:
        if (
            action.kind not in JUDGEMENTS
            or action.moderator_id is None
            or action.case_id in record.revoked
        ):
            continue
        rows = by_author.get(action.user_id)
        if rows is None:
            rows = np.flatnonzero(messages.author == action.user_id)
            by_author[action.user_id] = rows
        sent = messages.created_at[rows]
        before = rows[(sent <= action.at) & (sent >= action.at - window)]
        for rank, row in enumerate(before[::-1][: len(ACTION_WEIGHTS)]):
            target[row, ACTIONABLE] = 1.0
            target[row, CONDUCT] = 1.0
            mask[row, ACTIONABLE] = 1.0
            mask[row, CONDUCT] = 1.0
            # A message the rules already labeled keeps its rule labels.
            weight[row] = max(weight[row] if messages.flagged[row] else 0.0, ACTION_WEIGHTS[rank])

    # Staff verdicts outrank every inference above.
    index = messages.index()
    for message_id, verdict in verdicts.items():
        row = index.get(message_id)
        if row is None:
            continue
        if verdict:
            target[row, ACTIONABLE] = 1.0
            mask[row, ACTIONABLE] = 1.0
            if not messages.flagged[row]:
                target[row, CONDUCT] = 1.0
                mask[row, CONDUCT] = 1.0
        else:
            target[row] = 0.0
            mask[row] = 1.0
        weight[row] = FEEDBACK_WEIGHT

    return Labels(target, mask, weight)
