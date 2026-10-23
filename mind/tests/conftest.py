"""A small synthetic server, laid out on disk exactly the way Hugh writes it.

Members:
- 1001-1005 chat normally.
- 1006 always types in capitals. Automod used to remove their messages for
  shouting and staff took every one of those cases back, which is the
  pattern the model should learn to forgive.
- 2001 spams in capitals; automod removed those messages and the cases stand.
- 3001 is rude; moderators warned them shortly after their worst messages.
"""

from __future__ import annotations

import json
import math
import random
import sqlite3
from datetime import UTC, datetime
from pathlib import Path

import numpy as np
import pytest

from hugh_mind import config as configuration
from hugh_mind.archive import FEATURE_COUNT

# Mirrors src/archive.rs at schema version 2.
SCHEMA = """
CREATE TABLE messages (
    id INTEGER PRIMARY KEY, channel_id INTEGER NOT NULL, author_id INTEGER NOT NULL,
    reply_to INTEGER, created_at INTEGER NOT NULL, content TEXT, attachments TEXT,
    features BLOB NOT NULL, feature_version INTEGER NOT NULL, edited_at INTEGER,
    deleted_at INTEGER, flagged INTEGER NOT NULL DEFAULT 0, case_id INTEGER
);
CREATE TABLE revisions (message_id INTEGER NOT NULL, replaced_at INTEGER NOT NULL, content TEXT);
CREATE TABLE feedback (
    message_id INTEGER NOT NULL, moderator_id INTEGER NOT NULL, verdict INTEGER NOT NULL,
    at INTEGER NOT NULL, PRIMARY KEY (message_id, moderator_id)
);
CREATE TABLE scores (
    message_id INTEGER PRIMARY KEY, model TEXT NOT NULL, actionable REAL NOT NULL,
    probabilities TEXT NOT NULL, anomaly REAL, scored_at INTEGER NOT NULL
);
"""

CALM = "hello there how is everyone doing today i liked the new update a lot thanks".split()
RUDE = "you are an idiot and your ideas are trash shut up".split()
SPAM = "FREE NITRO CLICK HERE NOW".split()
WORDS = sorted(set(CALM + RUDE + [word.lower() for word in SPAM] + ["caps", "lol", "wow"]))
START = int(datetime(2026, 9, 1, tzinfo=UTC).timestamp() * 1000)
DAY = 86_400_000


def features(text: str, gap_seconds: float) -> bytes:
    """A rough stand-in for src/features.rs: the slots the tests lean on."""
    letters = [character for character in text if character.isalpha()]
    caps = sum(character.isupper() for character in letters) / max(len(letters), 1)
    values = np.zeros(FEATURE_COUNT, dtype=np.uint8)
    values[0] = min(255, round(255 * math.log1p(len(text)) / math.log1p(4000)))
    values[1] = min(255, round(255 * math.log1p(len(text.split())) / math.log1p(1000)))
    values[3] = round(255 * caps)
    values[16] = min(255, text.count("!") * 30)
    values[21] = min(255, round(255 * math.log1p(gap_seconds) / math.log1p(604_800)))
    return values.tobytes()


def build_server(root: Path, days: int = 24, seed: int = 3) -> dict:
    rng = random.Random(seed)
    data = root / "data"
    data.mkdir(parents=True)
    connection = sqlite3.connect(data / "archive.sqlite3")
    connection.executescript(SCHEMA)
    incidents, record = [], []
    case = 0
    message_id = 10_000
    last_seen: dict[int, int] = {}
    rude_ids: list[int] = []

    def add(author: int, channel: int, at: int, text: str, flagged=False, case_id=None, reply=None):
        nonlocal message_id
        message_id += 1
        gap = max(0.0, (at - last_seen[author]) / 1000) if author in last_seen else 604_800
        last_seen[author] = at
        connection.execute(
            "INSERT INTO messages (id, channel_id, author_id, reply_to, created_at, content, "
            "features, feature_version, flagged, case_id, deleted_at) VALUES (?,?,?,?,?,?,?,?,?,?,?)",
            (message_id, channel, author, reply, at, text, features(text, gap), 1, int(flagged),
             case_id, at + 500 if flagged else None),
        )
        return message_id

    def stamp(at: int) -> str:
        return datetime.fromtimestamp(at / 1000, UTC).isoformat().replace("+00:00", "Z")

    for day in range(days):
        for slot in range(48):
            at = START + day * DAY + slot * 1_800_000 + rng.randrange(600_000)
            channel = rng.choice([1, 2])
            author = rng.choice([1001, 1002, 1003, 1004, 1005, 1006])
            text = " ".join(rng.choices(CALM, k=rng.randint(4, 10)))
            if author == 1006:
                text = text.upper() + " " + rng.choice(["LOL", "WOW"])
                if rng.random() < 0.3:
                    case += 1
                    removed = add(author, channel, at, text, flagged=True, case_id=case)
                    incidents.append({"case_id": case, "kind": "message_guard", "details": {
                        "message_id": removed, "reasons": "excessive_caps"}})
                    record.append({"event": "recorded", "case_id": case, "user_id": str(author),
                                   "kind": "automod", "reason": "Shouting", "at": stamp(at + 600),
                                   "weight": 2.0})
                    record.append({"event": "revoked", "case_id": case, "at": stamp(at + 7_200_000),
                                   "moderator_id": "9"})
                    continue
            add(author, channel, at, text)
        for _ in range(3):
            at = START + day * DAY + rng.randrange(DAY)
            case += 1
            text = " ".join(rng.choices(SPAM, k=6)) + "!!!"
            spam = add(2001, rng.choice([1, 2]), at, text, flagged=True, case_id=case)
            incidents.append({"case_id": case, "kind": "message_guard", "details": {
                "message_id": spam, "reasons": "excessive_caps,duplicate_spam"}})
            record.append({"event": "recorded", "case_id": case, "user_id": "2001", "kind": "automod",
                           "reason": "Spam", "at": stamp(at + 600), "weight": 7.0})
        for _ in range(4):
            at = START + day * DAY + rng.randrange(DAY - 3_600_000)
            channel = rng.choice([1, 2])
            text = " ".join(rng.choices(RUDE, k=rng.randint(4, 8)))
            rude_ids.append(add(3001, channel, at, text))
            if rng.random() < 0.6:
                case += 1
                record.append({"event": "recorded", "case_id": case, "user_id": "3001", "kind": "warn",
                               "reason": "Be nice", "moderator_id": "9", "at": stamp(at + 120_000),
                               "weight": 8.0})
    connection.commit()
    connection.close()
    (data / "incidents.jsonl").write_text("\n".join(json.dumps(line) for line in incidents) + "\n")
    (data / "moderation.jsonl").write_text("\n".join(json.dumps(line) for line in record) + "\n")
    return {"data": data, "rude_ids": rude_ids, "last_id": message_id}


def tiny_encoder(folder: Path) -> Path:
    from transformers import BertConfig, BertForMaskedLM, BertTokenizer

    folder.mkdir(parents=True, exist_ok=True)
    vocabulary = ["[PAD]", "[UNK]", "[CLS]", "[SEP]", "[MASK]", "!", *WORDS]
    (folder / "vocab.txt").write_text("\n".join(vocabulary) + "\n", encoding="utf-8")
    tokenizer = BertTokenizer(vocab_file=str(folder / "vocab.txt"), do_lower_case=True)
    model = BertForMaskedLM(
        BertConfig(
            vocab_size=len(vocabulary),
            hidden_size=32,
            num_hidden_layers=2,
            num_attention_heads=2,
            intermediate_size=64,
            max_position_embeddings=64,
        )
    )
    tokenizer.save_pretrained(folder)
    model.save_pretrained(folder)
    return folder


@pytest.fixture(scope="session")
def encoder_folder(tmp_path_factory) -> Path:
    return tiny_encoder(tmp_path_factory.mktemp("encoder"))


@pytest.fixture
def server(tmp_path, encoder_folder):
    built = build_server(tmp_path)
    raw = {
        "data_dir": str(built["data"]),
        "profile": "cpu",
        "encoder": {"base": str(encoder_folder), "max_length": 32},
        "model": {"history": 8, "channel": 4, "d_model": 32, "layers": 1, "heads": 2, "author_dim": 8},
        # A thousand examples need more passes and a bolder rate than a year does.
        "training": {"epochs": 15, "learning_rate": 1e-3, "min_author_messages": 20,
                     "negatives_per_positive": 10, "holdout_fraction": 0.2},
        "pretrain": {"max_texts": 400},
    }
    built["config"] = configuration.parse(raw, tmp_path)
    return built
