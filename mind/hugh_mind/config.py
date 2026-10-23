"""Hugh Mind's configuration: one TOML file, read strictly like Hugh's own.

Every section is optional and every key has a default, but an unknown key
stops startup, so a typo never silently falls back to a default.
"""

from __future__ import annotations

import tomllib
from dataclasses import dataclass, field, fields
from pathlib import Path
from typing import Any


@dataclass(frozen=True)
class Profile:
    """How hard a GPU (or the CPU) can be pushed."""

    name: str
    batch_size: int
    accumulation: int
    mlm_batch_size: int
    mlm_max_length: int
    embed_batch_size: int
    # Freezing the 250k-row word embedding table keeps optimiser state for the
    # encoder under a gigabyte, which is what lets a 3 GB card pretrain at all.
    freeze_word_embeddings: bool
    gradient_checkpointing: bool
    # On a small card the scorer runs on the CPU, so training keeps the GPU.
    serve_on_gpu: bool


PROFILES: dict[str, Profile] = {
    "6gb": Profile("6gb", 64, 1, 16, 128, 256, False, False, True),
    "3gb": Profile("3gb", 16, 4, 8, 96, 64, True, True, False),
    # A GTX 1050: room for the encoder and small batches, nothing to spare.
    "2gb": Profile("2gb", 16, 4, 4, 64, 64, True, True, False),
    "cpu": Profile("cpu", 32, 1, 4, 64, 32, True, False, False),
}


def profile_for(total_gb: float) -> str:
    """The profile `auto` picks for a card with this much memory."""
    if total_gb >= 5.0:
        return "6gb"
    if total_gb >= 2.5:
        return "3gb"
    if total_gb >= 1.5:
        return "2gb"
    return "cpu"


@dataclass(frozen=True)
class EncoderConfig:
    # A multilingual sentence encoder: 384 dimensions, mean pooled.
    base: str = "sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2"
    max_length: int = 128


@dataclass(frozen=True)
class ModelConfig:
    history: int = 32
    channel: int = 16
    d_model: int = 256
    layers: int = 4
    heads: int = 4
    author_dim: int = 64
    dropout: float = 0.1


@dataclass(frozen=True)
class TrainingConfig:
    epochs: int = 4
    incremental_epochs: int = 1
    learning_rate: float = 3e-4
    incremental_learning_rate: float = 1e-4
    weight_decay: float = 0.01
    # Members with fewer messages share the server-wide author slot.
    min_author_messages: int = 200
    # Share of training examples shown without their author, so the model
    # still works for somebody it has never met.
    author_dropout: float = 0.15
    # A message only counts as clean once nothing happened for this long.
    settle_hours: float = 24.0
    # A moderator action reaches back this far to the messages that earned it.
    action_window_minutes: float = 10.0
    negatives_per_positive: int = 50
    # Share of an incremental run spent on older examples, so a quarter-hour
    # of new data cannot make the model forget the last year.
    replay_fraction: float = 0.3
    holdout_fraction: float = 0.1
    target_precision: float = 0.9
    # A challenger may trail the champion's average precision by this much and
    # still be promoted, so noise alone does not freeze the model in place.
    promotion_tolerance: float = 0.002
    feature_loss_weight: float = 0.1
    focal_gamma: float = 2.0
    seed: int = 7
    keep_versions: int = 50


@dataclass(frozen=True)
class PretrainConfig:
    max_texts: int = 200_000
    # New messages needed since the last pretraining before another is worth it.
    min_new_texts: int = 5_000
    learning_rate: float = 5e-5
    mask_probability: float = 0.15


@dataclass(frozen=True)
class ScheduleConfig:
    incremental_minutes: int = 15
    # Local hour the nightly pretrain, re-embed and full retrain start.
    nightly_hour: int = 4
    min_new_messages: int = 200


@dataclass(frozen=True)
class ServeConfig:
    host: str = "127.0.0.1"
    port: int = 8765
    reload_seconds: int = 30


@dataclass(frozen=True)
class MindConfig:
    data_dir: Path
    archive: Path
    incidents: Path
    moderation: Path
    mind_dir: Path
    profile: str = "auto"
    # With this off the model learns from message shape alone, never text.
    train_on_content: bool = True
    encoder: EncoderConfig = field(default_factory=EncoderConfig)
    model: ModelConfig = field(default_factory=ModelConfig)
    training: TrainingConfig = field(default_factory=TrainingConfig)
    pretrain: PretrainConfig = field(default_factory=PretrainConfig)
    schedule: ScheduleConfig = field(default_factory=ScheduleConfig)
    serve: ServeConfig = field(default_factory=ServeConfig)

    @property
    def embeddings_path(self) -> Path:
        return self.mind_dir / "embeddings.sqlite3"

    @property
    def registry_dir(self) -> Path:
        return self.mind_dir / "registry"


_SECTIONS = {
    "encoder": EncoderConfig,
    "model": ModelConfig,
    "training": TrainingConfig,
    "pretrain": PretrainConfig,
    "schedule": ScheduleConfig,
    "serve": ServeConfig,
}
_TOP_LEVEL = {
    "data_dir",
    "archive",
    "incidents",
    "moderation",
    "mind_dir",
    "profile",
    "train_on_content",
}


def load(path: Path) -> MindConfig:
    with path.open("rb") as file:
        raw = tomllib.load(file)
    return parse(raw, path.parent)


def parse(raw: dict[str, Any], base: Path) -> MindConfig:
    unknown = set(raw) - _TOP_LEVEL - set(_SECTIONS)
    if unknown:
        raise ValueError(f"unknown configuration keys: {', '.join(sorted(unknown))}")

    def resolve(value: str, root: Path) -> Path:
        candidate = Path(value)
        return candidate if candidate.is_absolute() else (root / candidate).resolve()

    data_dir = resolve(raw.get("data_dir", "../data"), base)
    profile = raw.get("profile", "auto")
    if profile != "auto" and profile not in PROFILES:
        raise ValueError(f"profile must be auto, {', '.join(PROFILES)}; got {profile!r}")
    sections = {name: _section(cls, raw.get(name, {}), name) for name, cls in _SECTIONS.items()}
    config = MindConfig(
        data_dir=data_dir,
        archive=resolve(raw.get("archive", "archive.sqlite3"), data_dir),
        incidents=resolve(raw.get("incidents", "incidents.jsonl"), data_dir),
        moderation=resolve(raw.get("moderation", "moderation.jsonl"), data_dir),
        mind_dir=resolve(raw.get("mind_dir", "mind"), data_dir),
        profile=profile,
        train_on_content=bool(raw.get("train_on_content", True)),
        **sections,
    )
    _validate(config)
    return config


def _section(cls: type, table: dict[str, Any], name: str) -> Any:
    known = {item.name: item for item in fields(cls)}
    unknown = set(table) - set(known)
    if unknown:
        raise ValueError(f"[{name}] has no key {', '.join(sorted(unknown))}")
    values = {}
    for key, value in table.items():
        expected = type(getattr(cls(), key))
        # TOML writes 3 for 3.0; accept an integer wherever a float is wanted.
        if expected is float and isinstance(value, int) and not isinstance(value, bool):
            value = float(value)
        if not isinstance(value, expected):
            raise ValueError(f"[{name}] {key} must be {expected.__name__}")
        values[key] = value
    return cls(**values)


def _validate(config: MindConfig) -> None:
    model = config.model
    if model.d_model % model.heads:
        raise ValueError("[model] d_model must be a multiple of heads")
    if model.history < 1 or model.channel < 1:
        raise ValueError("[model] history and channel must both be at least 1")
    training = config.training
    if not 0.0 < training.holdout_fraction < 0.5:
        raise ValueError("[training] holdout_fraction must be between 0 and 0.5")
    if not 0.0 <= training.replay_fraction < 1.0:
        raise ValueError("[training] replay_fraction must be at least 0 and below 1")
    if not 0.0 < training.target_precision < 1.0:
        raise ValueError("[training] target_precision must be between 0 and 1")
    if not 0 <= config.schedule.nightly_hour <= 23:
        raise ValueError("[schedule] nightly_hour must be between 0 and 23")
