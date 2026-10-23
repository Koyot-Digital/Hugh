"""Model inputs: who said what before this message, and what the channel was saying.

Every example is one target message seen through two windows:

- the author's own last `history` messages, anywhere on the server, which is
  how the model learns what is normal *for this person*
- the channel's last `channel` messages from anyone, which is how it tells
  banter from a pile-on and sees who the message answers

Training and the live scorer both go through `assemble`, so the model never
sees a layout at serving time it was not trained on.
"""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np
import torch

from .archive import FEATURE_COUNT, Messages

# Per-token scalars beside the 22 features: age relative to the target, two
# context flags, and whether the token has text at all.
SCALARS = 4
SHAPE = FEATURE_COUNT + SCALARS
WEEK_SECONDS = 604_800.0


@dataclass
class Corpus:
    ids: np.ndarray
    channel: np.ndarray
    author: np.ndarray
    reply_to: np.ndarray
    created_at: np.ndarray
    features: np.ndarray  # uint8 (N, 22)
    text: np.ndarray  # float16 (N, D)
    has_text: np.ndarray  # bool
    _author_order: np.ndarray | None = None
    _author_rank: np.ndarray | None = None
    _author_start: np.ndarray | None = None
    _channel_order: np.ndarray | None = None
    _channel_rank: np.ndarray | None = None
    _channel_start: np.ndarray | None = None

    @classmethod
    def from_messages(cls, messages: Messages, text: np.ndarray, has_text: np.ndarray) -> Corpus:
        corpus = cls(
            ids=messages.ids,
            channel=messages.channel,
            author=messages.author,
            reply_to=messages.reply_to,
            created_at=messages.created_at,
            features=messages.features,
            text=text,
            has_text=has_text,
        )
        corpus._author_order, corpus._author_rank, corpus._author_start = _groups(messages.author)
        corpus._channel_order, corpus._channel_rank, corpus._channel_start = _groups(
            messages.channel
        )
        return corpus

    def __len__(self) -> int:
        return len(self.ids)

    def context(self, targets: np.ndarray, history: int, channel: int):
        """The history and channel windows for each target row, oldest first,
        left-padded with -1. Rows are in time order, so "before" is "earlier
        in the group"."""
        hist = _window(self._author_order, self._author_rank, self._author_start, targets, history)
        chan = _window(
            self._channel_order, self._channel_rank, self._channel_start, targets, channel
        )
        return hist, chan


def _groups(keys: np.ndarray):
    order = np.argsort(keys, kind="stable")
    rank = np.empty_like(order)
    rank[order] = np.arange(len(order))
    sorted_keys = keys[order]
    boundary = np.ones(len(order), dtype=bool)
    boundary[1:] = sorted_keys[1:] != sorted_keys[:-1]
    start_of_position = np.maximum.accumulate(np.where(boundary, np.arange(len(order)), 0))
    start = start_of_position[rank]
    return order, rank, start


def _window(order, rank, start, targets: np.ndarray, size: int) -> np.ndarray:
    positions = rank[targets][:, None] - size + np.arange(size)[None, :]
    valid = positions >= start[targets][:, None]
    window = order[np.clip(positions, 0, None)]
    return np.where(valid, window, -1)


def assemble(
    corpus: Corpus,
    targets: np.ndarray,
    hist: np.ndarray,
    chan: np.ndarray,
    authors: np.ndarray,
) -> dict[str, torch.Tensor]:
    """Builds one padded batch. `authors` is already mapped to model slots."""
    batch = len(targets)
    k = hist.shape[1]
    rows = np.concatenate([hist, chan, targets[:, None]], axis=1)
    valid = rows >= 0
    safe = np.clip(rows, 0, None)

    text = corpus.text[safe].astype(np.float32)
    text[~valid] = 0.0
    features = corpus.features[safe].astype(np.float32) / 255.0
    features[~valid] = 0.0

    sent = corpus.created_at[safe]
    target_sent = corpus.created_at[targets][:, None]
    age = np.log1p(np.clip((target_sent - sent) / 1000.0, 0.0, None)) / np.log1p(WEEK_SECONDS)
    age = np.clip(age, 0.0, 1.0)

    target_channel = corpus.channel[targets][:, None]
    target_author = corpus.author[targets][:, None]
    target_reply = corpus.reply_to[targets][:, None]
    is_history = np.zeros_like(valid)
    is_history[:, :k] = True
    is_channel = np.zeros_like(valid)
    is_channel[:, k:-1] = True
    # History: was it in this channel, and was it a reply. Channel: was it the
    # author, and is it the message this one answers. Target: is it a reply.
    flag_a = np.where(
        is_history,
        corpus.channel[safe] == target_channel,
        np.where(is_channel, corpus.author[safe] == target_author, target_reply != 0),
    )
    flag_b = np.where(
        is_history,
        corpus.reply_to[safe] != 0,
        is_channel & (corpus.ids[safe] == target_reply) & (target_reply != 0),
    )
    scalars = np.stack(
        [age, flag_a.astype(np.float32), flag_b.astype(np.float32), corpus.has_text[safe]],
        axis=-1,
    ).astype(np.float32)
    scalars[~valid] = 0.0
    shape = np.concatenate([features, scalars], axis=-1)

    # Each history token predicts the features of the message after it: the
    # next history token, or for the newest one, the target itself.
    following = np.concatenate([hist[:, 1:], targets[:, None]], axis=1)
    next_valid = (hist >= 0) & (following >= 0)
    next_features = corpus.features[np.clip(following, 0, None)].astype(np.float32) / 255.0

    return {
        "text": torch.from_numpy(text),
        "shape": torch.from_numpy(shape),
        "valid": torch.from_numpy(valid),
        "author": torch.from_numpy(authors.astype(np.int64)),
        "next_features": torch.from_numpy(next_features),
        "next_valid": torch.from_numpy(next_valid),
        "rows": torch.from_numpy(rows.reshape(batch, -1)),
    }


def author_slots(vocab: dict[int, int], raw: np.ndarray) -> np.ndarray:
    """Maps Discord IDs to embedding rows. Slot 0 is the server at large."""
    return np.fromiter((vocab.get(int(author), 0) for author in raw), dtype=np.int64, count=len(raw))
