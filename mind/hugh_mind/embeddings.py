"""Message text as vectors: the encoder, and the store that caches its output.

Embedding a year of messages takes minutes on the 1060, so it happens once per
message per encoder version, not once per training run. The quarter-hourly
runs then train on cached vectors, and only the nightly job, which adapts the
encoder to the server's own language, has to embed everything again.
"""

from __future__ import annotations

import logging
import sqlite3
from pathlib import Path

import numpy as np
import torch

from . import archive

log = logging.getLogger(__name__)


class Encoder:
    """A sentence encoder: mean-pooled, unit-length vectors."""

    def __init__(self, source: str | Path, version: str, device: torch.device, max_length: int):
        from transformers import AutoModel, AutoTokenizer

        self.version = version
        self.device = device
        self.max_length = max_length
        self.tokenizer = AutoTokenizer.from_pretrained(str(source))
        self.model = AutoModel.from_pretrained(str(source)).to(device).eval()
        self.dimension = int(self.model.config.hidden_size)

    @torch.inference_mode()
    def embed(self, texts: list[str], batch_size: int = 64) -> np.ndarray:
        vectors = np.zeros((len(texts), self.dimension), dtype=np.float32)
        for start in range(0, len(texts), batch_size):
            chunk = texts[start : start + batch_size]
            inputs = self.tokenizer(
                chunk,
                padding=True,
                truncation=True,
                max_length=self.max_length,
                return_tensors="pt",
            ).to(self.device)
            hidden = self.model(**inputs).last_hidden_state
            weights = inputs["attention_mask"].unsqueeze(-1).to(hidden.dtype)
            pooled = (hidden * weights).sum(1) / weights.sum(1).clamp(min=1e-6)
            pooled = torch.nn.functional.normalize(pooled, dim=-1)
            vectors[start : start + len(chunk)] = pooled.float().cpu().numpy()
        return vectors


class EmbeddingStore:
    """int8 vectors with one scale each, keyed by message ID and encoder."""

    def __init__(self, path: Path):
        path.parent.mkdir(parents=True, exist_ok=True)
        self.connection = sqlite3.connect(path, timeout=30)
        self.connection.execute("PRAGMA journal_mode = WAL")
        self.connection.execute(
            "CREATE TABLE IF NOT EXISTS embeddings ("
            "id INTEGER NOT NULL, encoder TEXT NOT NULL, scale REAL NOT NULL, "
            "vector BLOB NOT NULL, PRIMARY KEY (id, encoder))"
        )
        self.connection.commit()

    def close(self) -> None:
        self.connection.close()

    def known(self, encoder: str) -> set[int]:
        rows = self.connection.execute("SELECT id FROM embeddings WHERE encoder = ?", (encoder,))
        return {row[0] for row in rows}

    def put(self, ids: list[int], vectors: np.ndarray, encoder: str) -> None:
        quantized, scales = quantize(vectors)
        self.connection.executemany(
            "INSERT OR REPLACE INTO embeddings (id, encoder, scale, vector) VALUES (?, ?, ?, ?)",
            [
                (int(message_id), encoder, float(scale), row.tobytes())
                for message_id, scale, row in zip(ids, scales, quantized, strict=True)
            ],
        )
        self.connection.commit()

    def load(self, ids: np.ndarray, encoder: str, dimension: int) -> tuple[np.ndarray, np.ndarray]:
        """Vectors in the order of `ids`, and which of them exist."""
        position = {int(message_id): row for row, message_id in enumerate(ids)}
        vectors = np.zeros((len(ids), dimension), dtype=np.float16)
        present = np.zeros(len(ids), dtype=bool)
        rows = self.connection.execute(
            "SELECT id, scale, vector FROM embeddings WHERE encoder = ?", (encoder,)
        )
        for message_id, scale, blob in rows:
            row = position.get(message_id)
            if row is None:
                continue
            vectors[row] = np.frombuffer(blob, dtype=np.int8).astype(np.float32) * scale
            present[row] = True
        return vectors, present

    def fetch(self, ids: list[int], encoder: str) -> dict[int, np.ndarray]:
        """A handful of vectors by ID, for the live scorer."""
        if not ids:
            return {}
        marks = ",".join("?" for _ in ids)
        rows = self.connection.execute(
            f"SELECT id, scale, vector FROM embeddings WHERE encoder = ? AND id IN ({marks})",
            (encoder, *ids),
        )
        return {
            message_id: np.frombuffer(blob, dtype=np.int8).astype(np.float32) * scale
            for message_id, scale, blob in rows
        }

    def drop_other_encoders(self, keep: set[str]) -> int:
        marks = ",".join("?" for _ in keep)
        cursor = self.connection.execute(
            f"DELETE FROM embeddings WHERE encoder NOT IN ({marks})", tuple(keep)
        )
        self.connection.commit()
        return cursor.rowcount


def quantize(vectors: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    scales = np.abs(vectors).max(axis=1) / 127.0
    scales[scales == 0] = 1.0
    quantized = np.clip(np.round(vectors / scales[:, None]), -127, 127).astype(np.int8)
    return quantized, scales.astype(np.float32)


def embed_pending(
    archive_connection: sqlite3.Connection,
    store: EmbeddingStore,
    encoder: Encoder,
    batch_size: int,
) -> int:
    """Embeds every archived message the store lacks for this encoder."""
    known = store.known(encoder.version)
    pending_ids: list[int] = []
    pending_texts: list[str] = []
    done = 0
    for message_id, text in archive.texts(archive_connection):
        if message_id in known:
            continue
        pending_ids.append(message_id)
        pending_texts.append(text)
        if len(pending_ids) >= 4_096:
            store.put(pending_ids, encoder.embed(pending_texts, batch_size), encoder.version)
            done += len(pending_ids)
            log.info("embedded %d messages so far", done)
            pending_ids, pending_texts = [], []
    if pending_ids:
        store.put(pending_ids, encoder.embed(pending_texts, batch_size), encoder.version)
        done += len(pending_ids)
    return done
