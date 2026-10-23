"""HughMind: one transformer over a member, their history, and the room.

The sequence is laid out as

    [author] [history_1 ... history_K] [channel_1 ... channel_M] [target]

and the attention mask gives each part a different job:

- The author token carries a learned embedding per regular member: their
  habits, compressed into 64 numbers. Unknown members share slot 0.
- History tokens are causal: each sees only the author and earlier history,
  and predicts the shape of the message after it. That self-supervised task
  needs no labels, so every message trains the model, and at serving time the
  error on the newest prediction says how unusual the target is *for this
  person*.
- Channel tokens are causal among themselves.
- The target sees everything, and its output feeds the classifier.

Because no history token can see the target, the "unusual" score is never
contaminated by the message it is judging.
"""

from __future__ import annotations

from dataclasses import dataclass

import torch
import torch.nn.functional as F
from torch import nn

from .archive import FEATURE_COUNT
from .config import ModelConfig
from .data import SHAPE
from .labels import LABELS

AUTHOR, HISTORY, CHANNEL, TARGET = range(4)


class Block(nn.Module):
    def __init__(self, width: int, heads: int, dropout: float):
        super().__init__()
        self.norm_attention = nn.LayerNorm(width)
        self.attention = nn.MultiheadAttention(width, heads, dropout=dropout, batch_first=True)
        self.norm_feed = nn.LayerNorm(width)
        self.feed = nn.Sequential(
            nn.Linear(width, 4 * width),
            nn.GELU(),
            nn.Dropout(dropout),
            nn.Linear(4 * width, width),
        )
        self.dropout = nn.Dropout(dropout)

    def forward(self, x: torch.Tensor, blocked: torch.Tensor, need_weights: bool):
        normed = self.norm_attention(x)
        attended, weights = self.attention(
            normed,
            normed,
            normed,
            attn_mask=blocked,
            need_weights=need_weights,
            average_attn_weights=True,
        )
        x = x + self.dropout(attended)
        x = x + self.dropout(self.feed(self.norm_feed(x)))
        return x, weights


@dataclass
class Output:
    logits: torch.Tensor  # (B, len(LABELS))
    next_features: torch.Tensor  # (B, K, FEATURE_COUNT)
    attention: torch.Tensor | None  # (B, L): where the target looked


class HughMind(nn.Module):
    def __init__(self, config: ModelConfig, text_dim: int, authors: int):
        super().__init__()
        self.config = config
        self.text_dim = text_dim
        width = config.d_model
        length = config.history + config.channel + 2
        self.text = nn.Linear(text_dim, width)
        self.shape = nn.Sequential(nn.Linear(SHAPE, width), nn.GELU(), nn.Linear(width, width))
        self.segment = nn.Embedding(4, width)
        self.position = nn.Embedding(length, width)
        self.authors = nn.Embedding(authors + 1, config.author_dim)
        self.author = nn.Linear(config.author_dim, width)
        self.blocks = nn.ModuleList(
            Block(width, config.heads, config.dropout) for _ in range(config.layers)
        )
        self.norm = nn.LayerNorm(width)
        self.classify = nn.Sequential(
            nn.Linear(width, width),
            nn.GELU(),
            nn.Dropout(config.dropout),
            nn.Linear(width, len(LABELS)),
        )
        self.predict_next = nn.Linear(width, FEATURE_COUNT)
        segments = [AUTHOR] + [HISTORY] * config.history + [CHANNEL] * config.channel + [TARGET]
        self.register_buffer("segments", torch.tensor(segments), persistent=False)
        self.register_buffer(
            "structure", structure_mask(config.history, config.channel), persistent=False
        )

    def forward(self, batch: dict[str, torch.Tensor], need_weights: bool = False) -> Output:
        valid = batch["valid"]
        size = valid.shape[0]
        tokens = self.text(batch["text"]) + self.shape(batch["shape"])
        author = self.author(self.authors(batch["author"])).unsqueeze(1)
        x = torch.cat([author, tokens], dim=1)
        x = x + self.segment(self.segments)[None] + self.position.weight[None]

        present = torch.cat([torch.ones_like(valid[:, :1]), valid], dim=1)
        blocked = self.structure[None] | ~present[:, None, :]
        blocked = blocked.repeat_interleave(self.config.heads, dim=0)
        weights = None
        for index, block in enumerate(self.blocks):
            last = index == len(self.blocks) - 1
            x, weights = block(x, blocked, need_weights and last)
        x = self.norm(x)

        history = x[:, 1 : 1 + self.config.history]
        attention = weights[:, -1, :] if need_weights and weights is not None else None
        return Output(
            logits=self.classify(x[:, -1]),
            next_features=torch.sigmoid(self.predict_next(history)),
            attention=attention.reshape(size, -1) if attention is not None else None,
        )

    def grow_authors(self, authors: int) -> None:
        """Adds author slots for newly regular members, starting each at the
        average member rather than at noise."""
        current = self.authors.num_embeddings - 1
        if authors <= current:
            return
        old = self.authors.weight.data
        grown = nn.Embedding(authors + 1, old.shape[1]).to(old.device)
        grown.weight.data[: current + 1] = old
        grown.weight.data[current + 1 :] = old[1:].mean(0) if current > 0 else old[0]
        self.authors = grown


def structure_mask(history: int, channel: int) -> torch.Tensor:
    """True where attention is blocked. Every row may see the author token, so
    even a fully padded window still has one key to attend to."""
    length = history + channel + 2
    blocked = torch.ones(length, length, dtype=torch.bool)
    blocked[:, 0] = False
    first_history, first_channel, target = 1, 1 + history, length - 1
    for query in range(first_history, first_channel):
        blocked[query, first_history : query + 1] = False
    for query in range(first_channel, target):
        blocked[query, first_channel : query + 1] = False
    blocked[target, :] = False
    return blocked


def classification_loss(
    logits: torch.Tensor,
    target: torch.Tensor,
    mask: torch.Tensor,
    weight: torch.Tensor,
    gamma: float,
) -> torch.Tensor:
    """Focal binary cross-entropy over the labels that are known. Violations
    are rare; the focal term stops ten thousand easy negatives drowning out
    the handful of positives."""
    logits = logits.float()
    bce = F.binary_cross_entropy_with_logits(logits, target, reduction="none")
    probability = torch.sigmoid(logits)
    agreement = target * probability + (1 - target) * (1 - probability)
    scale = mask * weight[:, None]
    loss = bce * (1 - agreement).pow(gamma) * scale
    return loss.sum() / scale.sum().clamp(min=1.0)


def feature_loss(prediction: torch.Tensor, target: torch.Tensor, valid: torch.Tensor):
    error = F.smooth_l1_loss(prediction.float(), target, reduction="none", beta=0.05).mean(-1)
    valid = valid.float()
    return (error * valid).sum() / valid.sum().clamp(min=1.0)


def surprise(prediction: torch.Tensor, target: torch.Tensor) -> torch.Tensor:
    """How far a message's shape is from what its author's history predicted."""
    return (prediction.float() - target).abs().mean(-1)
