"""Reading what Hugh wrote: the message archive, the case log, and the record.

Everything here is read-only. Hugh's own writer thread owns the archive, and
SQLite's WAL journal lets this process read while it writes.
"""

from __future__ import annotations

import json
import sqlite3
from collections import defaultdict
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

import numpy as np

# Mirrors src/features.rs.
FEATURE_COUNT = 22
FEATURE_VERSION = 1


def connect(path: Path) -> sqlite3.Connection:
    connection = sqlite3.connect(f"file:{path.as_posix()}?mode=ro", uri=True, timeout=30)
    connection.execute("PRAGMA query_only = ON")
    return connection


@dataclass
class Messages:
    """Every archived message, oldest first, as parallel arrays."""

    ids: np.ndarray  # int64
    channel: np.ndarray  # int64
    author: np.ndarray  # int64
    reply_to: np.ndarray  # int64, 0 when not a reply
    created_at: np.ndarray  # int64 Unix milliseconds
    features: np.ndarray  # uint8 (N, FEATURE_COUNT)
    flagged: np.ndarray  # bool: Hugh's automod removed it
    case_id: np.ndarray  # int64, 0 when none
    deleted: np.ndarray  # bool

    def __len__(self) -> int:
        return len(self.ids)

    def index(self) -> dict[int, int]:
        return {int(message_id): row for row, message_id in enumerate(self.ids)}


def load_messages(connection: sqlite3.Connection) -> Messages:
    rows = connection.execute(
        "SELECT id, channel_id, author_id, COALESCE(reply_to, 0), created_at, features, "
        "flagged, COALESCE(case_id, 0), deleted_at IS NOT NULL "
        "FROM messages WHERE feature_version = ? ORDER BY created_at, id",
        (FEATURE_VERSION,),
    ).fetchall()
    count = len(rows)
    features = np.zeros((count, FEATURE_COUNT), dtype=np.uint8)
    for row, values in enumerate(rows):
        features[row] = np.frombuffer(values[5], dtype=np.uint8, count=FEATURE_COUNT)

    def column(position: int, dtype) -> np.ndarray:
        return np.fromiter((row[position] for row in rows), dtype=dtype, count=count)

    return Messages(
        ids=column(0, np.int64),
        channel=column(1, np.int64),
        author=column(2, np.int64),
        reply_to=column(3, np.int64),
        created_at=column(4, np.int64),
        features=features,
        flagged=column(6, np.bool_),
        case_id=column(7, np.int64),
        deleted=column(8, np.bool_),
    )


def texts(connection: sqlite3.Connection, after_id: int = 0, batch: int = 2_048):
    """Yields (id, text) for every message with text left, in ID order."""
    cursor = after_id
    while True:
        rows = connection.execute(
            "SELECT id, content FROM messages WHERE id > ? AND content IS NOT NULL "
            "AND content != '' ORDER BY id LIMIT ?",
            (cursor, batch),
        ).fetchall()
        if not rows:
            return
        yield from rows
        cursor = rows[-1][0]


def recent_texts(connection: sqlite3.Connection, limit: int) -> list[str]:
    rows = connection.execute(
        "SELECT content FROM messages WHERE content IS NOT NULL AND LENGTH(content) >= 8 "
        "ORDER BY created_at DESC LIMIT ?",
        (limit,),
    ).fetchall()
    return [row[0] for row in rows]


def feedback(connection: sqlite3.Connection) -> dict[int, int]:
    """Moderator verdicts per message: 1 needs action, 0 was fine. Where staff
    disagree, the latest verdict stands."""
    try:
        rows = connection.execute(
            "SELECT message_id, verdict FROM feedback ORDER BY at"
        ).fetchall()
    except sqlite3.OperationalError:
        # An archive from before feedback existed.
        return {}
    return {int(message_id): int(verdict) for message_id, verdict in rows}


@dataclass(frozen=True)
class Action:
    case_id: int
    user_id: int
    kind: str
    moderator_id: int | None
    at: int  # Unix milliseconds


@dataclass
class Record:
    actions: list[Action]
    revoked: set[int]

    def by_user(self) -> dict[int, np.ndarray]:
        """Times of every case per member, sorted, revoked cases left out."""
        times: dict[int, list[int]] = defaultdict(list)
        for action in self.actions:
            if action.case_id not in self.revoked:
                times[action.user_id].append(action.at)
        return {user: np.array(sorted(values), dtype=np.int64) for user, values in times.items()}


def read_record(path: Path) -> Record:
    actions: list[Action] = []
    revoked: set[int] = set()
    if not path.exists():
        return Record(actions, revoked)
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        event = json.loads(line)
        if event.get("event") == "recorded":
            moderator = event.get("moderator_id")
            actions.append(
                Action(
                    case_id=int(event["case_id"]),
                    user_id=int(event["user_id"]),
                    kind=event["kind"],
                    moderator_id=int(moderator) if moderator else None,
                    at=_millis(event["at"]),
                )
            )
        elif event.get("event") == "revoked":
            revoked.add(int(event["case_id"]))
    return Record(actions, revoked)


def read_automod_reasons(path: Path) -> dict[int, set[str]]:
    """The violation codes behind every message Hugh's automod removed."""
    reasons: dict[int, set[str]] = {}
    if not path.exists():
        return reasons
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        try:
            incident = json.loads(line)
        except json.JSONDecodeError:
            continue
        if incident.get("kind") != "message_guard":
            continue
        details = incident.get("details") or {}
        message_id = details.get("message_id")
        codes = details.get("reasons")
        if message_id is None or not codes:
            continue
        reasons[int(message_id)] = {code for code in str(codes).split(",") if code}
    return reasons


def _millis(timestamp: str) -> int:
    return int(datetime.fromisoformat(timestamp).timestamp() * 1000)
