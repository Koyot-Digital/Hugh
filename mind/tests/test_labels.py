import numpy as np

from hugh_mind.archive import Action, Messages, Record
from hugh_mind.labels import ACTIONABLE, CONDUCT, FEEDBACK_WEIGHT, LABELS, REVOKED_WEIGHT, build

HOUR = 3_600_000


def messages(rows):
    """rows: (id, author, at, flagged, case_id, deleted)"""
    count = len(rows)
    return Messages(
        ids=np.array([row[0] for row in rows], dtype=np.int64),
        channel=np.ones(count, dtype=np.int64),
        author=np.array([row[1] for row in rows], dtype=np.int64),
        reply_to=np.zeros(count, dtype=np.int64),
        created_at=np.array([row[2] for row in rows], dtype=np.int64),
        features=np.zeros((count, 22), dtype=np.uint8),
        flagged=np.array([row[3] for row in rows], dtype=bool),
        case_id=np.array([row[4] for row in rows], dtype=np.int64),
        deleted=np.array([row[5] for row in rows], dtype=bool),
    )


def label(rows, reasons=None, actions=(), revoked=(), verdicts=None, now=100 * HOUR):
    return build(
        messages(rows),
        reasons or {},
        Record(list(actions), set(revoked)),
        verdicts or {},
        now=now,
        settle_hours=24,
        action_window_minutes=10,
    )


def test_a_settled_message_with_nothing_nearby_is_a_clean_negative():
    labels = label([(1, 7, 10 * HOUR, False, 0, False), (2, 7, 99 * HOUR, False, 0, False)])
    assert labels.labeled.tolist() == [True, False], "the second has not settled yet"
    assert labels.target[0].sum() == 0
    assert labels.mask[0].sum() == len(LABELS)


def test_an_automod_removal_labels_the_guards_it_tripped_and_no_others():
    labels = label(
        [(1, 7, 10 * HOUR, True, 5, True)],
        reasons={1: {"excessive_caps", "duplicate_spam"}},
        actions=[Action(5, 7, "automod", None, 10 * HOUR)],
    )
    row = dict(zip(LABELS, labels.target[0], strict=True))
    known = dict(zip(LABELS, labels.mask[0], strict=True))
    assert row["actionable"] == row["style"] == row["spam"] == 1
    assert row["links"] == 0 and known["links"] == 1
    # Rules say nothing about conduct, so it stays unknown rather than zero.
    assert known["conduct"] == 0


def test_a_case_staff_took_back_is_a_strong_negative():
    labels = label(
        [(1, 7, 10 * HOUR, True, 5, True)],
        reasons={1: {"excessive_caps"}},
        actions=[Action(5, 7, "automod", None, 10 * HOUR)],
        revoked={5},
    )
    assert labels.target[0].sum() == 0
    assert labels.mask[0].sum() == len(LABELS)
    assert labels.weight[0] == REVOKED_WEIGHT


def test_a_warning_reaches_back_to_the_messages_that_earned_it():
    rows = [
        (1, 7, 10 * HOUR - 20 * 60_000, False, 0, False),  # too early to blame
        (2, 7, 10 * HOUR - 5 * 60_000, False, 0, False),
        (3, 7, 10 * HOUR - 60_000, False, 0, False),
        (4, 8, 10 * HOUR - 60_000, False, 0, False),  # somebody else
    ]
    labels = label(rows, actions=[Action(9, 7, "warn", 42, 10 * HOUR)])
    assert labels.target[:, CONDUCT].tolist() == [0, 1, 1, 0]
    # The newest message before the warning carries the most weight.
    assert labels.weight[2] > labels.weight[1] > 0
    # Twenty minutes before a warning is not clean either: it stays unlabeled.
    assert not labels.labeled[0]
    assert labels.labeled[3]


def test_automod_cases_do_not_count_as_moderator_judgement():
    labels = label(
        [(1, 7, 10 * HOUR - 60_000, False, 0, False)],
        actions=[Action(9, 7, "automod", None, 10 * HOUR)],
    )
    assert labels.target[0, CONDUCT] == 0


def test_staff_verdicts_override_everything():
    rows = [(1, 7, 10 * HOUR, True, 5, True), (2, 8, 10 * HOUR, False, 0, False)]
    labels = label(
        rows,
        reasons={1: {"blocked_phrase"}},
        actions=[Action(5, 7, "automod", None, 10 * HOUR)],
        verdicts={1: 0, 2: 1},
    )
    assert labels.target[0].sum() == 0
    assert labels.target[1, ACTIONABLE] == labels.target[1, CONDUCT] == 1
    assert labels.weight.tolist() == [FEEDBACK_WEIGHT, FEEDBACK_WEIGHT]
