"""Training, judging and promoting a model.

The safeguards that keep the model honest:

- **Split by time.** The newest tenth of labeled messages is held out, so the
  model is always judged on a future it never saw, the way it will be used.
- **Calibrated.** Raw scores are mapped through a per-label Platt fit on the
  held-out period, so 0.9 means "nine times in ten, staff would act".
- **Champion vs challenger.** A new model goes live only if it matches or
  beats the live one on the same held-out messages. Otherwise it is kept on
  disk for inspection and the live model stays.
- **Replay.** A quarter-hourly run trains on what is new plus a sample of
  older examples, so a busy afternoon cannot make the model forget the year.
"""

from __future__ import annotations

import logging
import math
from dataclasses import asdict, dataclass, replace
from datetime import UTC, datetime

import numpy as np
import torch

from . import archive
from . import labels as labeling
from .config import MindConfig, ModelConfig
from .data import Corpus, assemble, author_slots
from .device import Runtime, autocast
from .embeddings import EmbeddingStore
from .labels import ACTIONABLE, LABELS, Labels
from .model import HughMind, classification_loss, feature_loss, surprise
from .registry import Registry

log = logging.getLogger(__name__)

FORMAT = 1
# Fewest positives either period needs before a model is worth training.
MIN_POSITIVES = 5
MIN_LABELED = 50


class NotEnoughData(Exception):
    """The archive does not yet hold enough labeled history to learn from."""


@dataclass
class Workspace:
    corpus: Corpus
    labels: Labels
    text_dim: int
    encoder: str | None


@dataclass
class Split:
    train: np.ndarray
    valid: np.ndarray
    cutoff: int


@dataclass
class Evaluation:
    logits: np.ndarray
    target: np.ndarray
    mask: np.ndarray
    surprise: np.ndarray


def load_workspace(config: MindConfig, registry: Registry, encoder: str | None) -> Workspace:
    connection = archive.connect(config.archive)
    try:
        messages = archive.load_messages(connection)
        verdicts = archive.feedback(connection)
    finally:
        connection.close()
    if len(messages) == 0:
        raise NotEnoughData("the archive holds no messages yet")
    labels = labeling.build(
        messages,
        archive.read_automod_reasons(config.incidents),
        archive.read_record(config.moderation),
        verdicts,
        now=int(messages.created_at.max()),
        settle_hours=config.training.settle_hours,
        action_window_minutes=config.training.action_window_minutes,
    )
    if encoder is None or not config.train_on_content:
        text = np.zeros((len(messages), 1), dtype=np.float16)
        has_text = np.zeros(len(messages), dtype=bool)
        return Workspace(Corpus.from_messages(messages, text, has_text), labels, 1, None)
    dimension = encoder_dimension(registry.encoder_source(encoder))
    text, has_text = _load_text(config, messages.ids, encoder, dimension)
    return Workspace(Corpus.from_messages(messages, text, has_text), labels, dimension, encoder)


def encoder_dimension(source: str) -> int:
    from transformers import AutoConfig

    return int(AutoConfig.from_pretrained(source).hidden_size)


def _load_text(config: MindConfig, ids: np.ndarray, encoder: str, dimension: int):
    store = EmbeddingStore(config.embeddings_path)
    try:
        return store.load(ids, encoder, dimension)
    finally:
        store.close()


def split(workspace: Workspace, holdout: float) -> Split:
    labeled = np.flatnonzero(workspace.labels.labeled)
    if len(labeled) < MIN_LABELED:
        raise NotEnoughData(
            f"{len(labeled)} labeled messages so far; at least {MIN_LABELED} are needed"
        )
    sent = workspace.corpus.created_at[labeled]
    cutoff = int(np.quantile(sent, 1.0 - holdout))
    result = Split(labeled[sent < cutoff], labeled[sent >= cutoff], cutoff)
    positive = workspace.labels.positive
    for name, rows in (("training", result.train), ("held-out", result.valid)):
        found = int(positive[rows].sum())
        if found < MIN_POSITIVES:
            raise NotEnoughData(
                f"the {name} period holds {found} messages that needed action; at least "
                f"{MIN_POSITIVES} are needed"
            )
    return result


def author_vocabulary(
    corpus: Corpus, cutoff: int, minimum: int, existing: dict[int, int] | None = None
) -> dict[int, int]:
    """Members regular enough before `cutoff` to earn their own slot. Slots
    are only ever added, so an incremental run keeps every member it knew."""
    authors, counts = np.unique(corpus.author[corpus.created_at < cutoff], return_counts=True)
    vocabulary = dict(existing or {})
    for author in authors[counts >= minimum]:
        vocabulary.setdefault(int(author), len(vocabulary) + 1)
    return vocabulary


def batches(
    corpus: Corpus,
    rows: np.ndarray,
    vocabulary: dict[int, int],
    model_config: ModelConfig,
    batch_size: int,
    device: torch.device,
    author_dropout: float = 0.0,
    rng: np.random.Generator | None = None,
):
    for start in range(0, len(rows), batch_size):
        chunk = rows[start : start + batch_size]
        hist, chan = corpus.context(chunk, model_config.history, model_config.channel)
        slots = author_slots(vocabulary, corpus.author[chunk])
        if author_dropout and rng is not None:
            slots[rng.random(len(slots)) < author_dropout] = 0
        batch = assemble(corpus, chunk, hist, chan, slots)
        yield chunk, {key: value.to(device) for key, value in batch.items()}


def sample_epoch(rows: np.ndarray, labels: Labels, ratio: int, rng: np.random.Generator):
    positive = labels.positive[rows]
    positives, negatives = rows[positive], rows[~positive]
    limit = ratio * max(len(positives), 1)
    if len(negatives) > limit:
        negatives = rng.choice(negatives, limit, replace=False)
    epoch = np.concatenate([positives, negatives])
    rng.shuffle(epoch)
    return epoch


def fit(
    model: HughMind,
    workspace: Workspace,
    rows: np.ndarray,
    vocabulary: dict[int, int],
    config: MindConfig,
    runtime: Runtime,
    epochs: int,
    learning_rate: float,
    rng: np.random.Generator,
) -> float:
    training = config.training
    profile = runtime.profile
    labels = workspace.labels
    device = runtime.device
    optimizer = torch.optim.AdamW(
        model.parameters(), lr=learning_rate, weight_decay=training.weight_decay
    )
    per_epoch = len(sample_epoch(rows, labels, training.negatives_per_positive, rng))
    steps = max(1, math.ceil(per_epoch / profile.batch_size / profile.accumulation) * epochs)
    warmup = max(1, steps // 20)

    def schedule(step: int) -> float:
        if step < warmup:
            return (step + 1) / warmup
        progress = (step - warmup) / max(1, steps - warmup)
        return 0.5 * (1.0 + math.cos(math.pi * min(progress, 1.0)))

    scheduler = torch.optim.lr_scheduler.LambdaLR(optimizer, schedule)
    scaler = torch.amp.GradScaler(enabled=runtime.amp_dtype == torch.float16)
    model.train()
    loss_value = float("nan")
    for epoch in range(epochs):
        epoch_rows = sample_epoch(rows, labels, training.negatives_per_positive, rng)
        total, count = 0.0, 0
        optimizer.zero_grad(set_to_none=True)
        for step, (chunk, batch) in enumerate(
            batches(
                workspace.corpus,
                epoch_rows,
                vocabulary,
                model.config,
                profile.batch_size,
                device,
                training.author_dropout,
                rng,
            )
        ):
            target = torch.from_numpy(labels.target[chunk]).to(device)
            mask = torch.from_numpy(labels.mask[chunk]).to(device)
            weight = torch.from_numpy(labels.weight[chunk]).to(device)
            with autocast(runtime):
                output = model(batch)
            loss = classification_loss(
                output.logits, target, mask, weight, training.focal_gamma
            ) + training.feature_loss_weight * feature_loss(
                output.next_features, batch["next_features"], batch["next_valid"]
            )
            scaler.scale(loss / profile.accumulation).backward()
            if (step + 1) % profile.accumulation == 0:
                scaler.unscale_(optimizer)
                torch.nn.utils.clip_grad_norm_(model.parameters(), 1.0)
                scaler.step(optimizer)
                scaler.update()
                optimizer.zero_grad(set_to_none=True)
                scheduler.step()
            total += float(loss.detach())
            count += 1
        loss_value = total / max(count, 1)
        log.info("epoch %d/%d · %d examples · loss %.4f", epoch + 1, epochs, len(epoch_rows), loss_value)
    return loss_value


@torch.inference_mode()
def evaluate(
    model: HughMind,
    corpus: Corpus,
    labels: Labels,
    rows: np.ndarray,
    vocabulary: dict[int, int],
    runtime: Runtime,
) -> Evaluation:
    model.eval()
    logits, surprises = [], []
    for _, batch in batches(
        corpus, rows, vocabulary, model.config, runtime.profile.batch_size * 2, runtime.device
    ):
        with autocast(runtime):
            output = model(batch)
        logits.append(output.logits.float().cpu().numpy())
        error = surprise(output.next_features[:, -1], batch["next_features"][:, -1])
        error = torch.where(batch["next_valid"][:, -1], error, torch.nan)
        surprises.append(error.cpu().numpy())
    return Evaluation(
        logits=np.concatenate(logits) if logits else np.zeros((0, len(LABELS))),
        target=labels.target[rows],
        mask=labels.mask[rows],
        surprise=np.concatenate(surprises) if surprises else np.zeros(0),
    )


def average_precision(scores: np.ndarray, target: np.ndarray) -> float:
    positives = target.sum()
    if positives == 0 or len(scores) == 0:
        return float("nan")
    order = np.argsort(-scores, kind="stable")
    hits = target[order]
    precision = np.cumsum(hits) / np.arange(1, len(hits) + 1)
    return float((precision * hits).sum() / positives)


def platt(logits: np.ndarray, target: np.ndarray) -> tuple[float, float]:
    """Fits p = sigmoid(a·logit + b) by Newton's method. Focal loss trains
    confident but miscalibrated scores; this puts them back on a real scale."""
    if len(logits) == 0 or target.min() == target.max():
        return 1.0, 0.0
    # Platt's own target smoothing: a held-out period with 30 positives can
    # never justify a probability of exactly one, however well they separate.
    positives = float(target.sum())
    negatives = float(len(target) - positives)
    y = np.where(target > 0, (positives + 1) / (positives + 2), 1 / (negatives + 2))
    # Fitting on standardised logits keeps very confident or tightly bunched
    # scores from sending the first Newton step somewhere useless.
    centre = float(logits.mean())
    scale = float(logits.std()) or 1.0
    x = (logits.astype(np.float64) - centre) / scale

    def loss(a: float, b: float) -> float:
        z = a * x + b
        return float(np.sum(np.logaddexp(0.0, z) - y * z))

    a, b = 1.0, 0.0
    current = loss(a, b)
    for _ in range(100):
        p = _sigmoid(a * x + b)
        w = p * (1.0 - p)
        gradient = np.array([((p - y) * x).sum(), (p - y).sum()])
        hessian = np.array([[(w * x * x).sum(), (w * x).sum()], [(w * x).sum(), w.sum()]])
        step = np.linalg.solve(hessian + np.eye(2) * 1e-6, gradient)
        # Halve the step until it actually lowers the loss.
        size = 1.0
        while size > 1e-4:
            trial = loss(a - size * step[0], b - size * step[1])
            if trial <= current:
                break
            size /= 2
        else:
            break
        a, b, current = a - size * step[0], b - size * step[1], trial
        if np.abs(size * step).max() < 1e-9:
            break
    # Back from standardised units: a·(l - centre)/scale + b.
    a, b = a / scale, b - a * centre / scale
    if not (math.isfinite(a) and math.isfinite(b)) or a <= 0:
        return 1.0, 0.0
    return float(a), float(b)


def calibrated(logits: np.ndarray, calibration: list[tuple[float, float]]) -> np.ndarray:
    a = np.array([pair[0] for pair in calibration])
    b = np.array([pair[1] for pair in calibration])
    return _sigmoid(logits * a + b)


def _sigmoid(z: np.ndarray) -> np.ndarray:
    # The tanh form never overflows, however large the logit.
    return 0.5 * (1.0 + np.tanh(0.5 * z))


def threshold_for(probabilities: np.ndarray, target: np.ndarray, precision: float):
    """The lowest score at which flagged messages are still right `precision`
    of the time, or None when no score gets there."""
    if target.sum() == 0:
        return None
    order = np.argsort(-probabilities, kind="stable")
    hits = np.cumsum(target[order])
    achieved = hits / np.arange(1, len(order) + 1)
    good = np.flatnonzero((achieved >= precision) & (target[order] > 0))
    if len(good) == 0:
        return None
    return float(probabilities[order][good[-1]])


def summarise(evaluation: Evaluation, target_precision: float):
    metrics: dict[str, dict[str, float | int | None]] = {}
    calibration: list[tuple[float, float]] = []
    thresholds: dict[str, float | None] = {}
    for index, name in enumerate(LABELS):
        known = evaluation.mask[:, index] > 0
        logits = evaluation.logits[known, index]
        target = evaluation.target[known, index]
        pair = platt(logits, target)
        calibration.append(pair)
        probabilities = calibrated(logits[:, None], [pair])[:, 0]
        thresholds[name] = threshold_for(probabilities, target, target_precision)
        metrics[name] = {
            "examples": int(known.sum()),
            "positives": int(target.sum()),
            "average_precision": _clean(average_precision(logits, target)),
            "prevalence": _clean(float(target.mean()) if len(target) else float("nan")),
            "brier": _clean(float(((probabilities - target) ** 2).mean()) if len(target) else float("nan")),
            "threshold": thresholds[name],
        }
    finite = evaluation.surprise[np.isfinite(evaluation.surprise)]
    anomaly = np.quantile(finite, np.linspace(0, 1, 101)).tolist() if len(finite) else []
    return metrics, calibration, thresholds, anomaly


def restore(checkpoint: dict, device: torch.device) -> tuple[HughMind, dict[int, int]]:
    vocabulary = {int(author): int(slot) for author, slot in checkpoint["authors"].items()}
    model = HughMind(ModelConfig(**checkpoint["model_config"]), checkpoint["text_dim"], len(vocabulary))
    model.load_state_dict(checkpoint["state"])
    return model.to(device).eval(), vocabulary


def train(
    config: MindConfig, runtime: Runtime, registry: Registry, *, incremental: bool
) -> str | None:
    """Trains a challenger, judges it, and promotes it if it earns it.
    Returns the promoted version, or None when the live model stays."""
    training = config.training
    rng = np.random.default_rng(training.seed)
    torch.manual_seed(training.seed)

    champion_version = registry.current_model()
    champion = registry.load_model(champion_version) if champion_version else None
    if incremental and champion is None:
        log.info("no live model yet, so this run trains one from scratch")
        incremental = False
    if incremental:
        encoder = champion["encoder"]
    else:
        encoder = registry.current_encoder() if config.train_on_content else None
    workspace = load_workspace(config, registry, encoder)
    periods = split(workspace, training.holdout_fraction)
    corpus = workspace.corpus

    if incremental:
        model, known = restore(champion, runtime.device)
        vocabulary = author_vocabulary(corpus, periods.cutoff, training.min_author_messages, known)
        model.grow_authors(len(vocabulary))
        model = model.to(runtime.device)
        since = champion["trained_until"]
        fresh = periods.train[corpus.created_at[periods.train] >= since]
        if len(fresh) == 0:
            log.info("nothing new has settled since the live model was trained")
            return None
        older = periods.train[corpus.created_at[periods.train] < since]
        wanted = int(len(fresh) * training.replay_fraction / (1.0 - training.replay_fraction))
        replay = rng.choice(older, min(wanted, len(older)), replace=False) if len(older) else older
        rows = np.concatenate([fresh, replay])
        epochs, learning_rate = training.incremental_epochs, training.incremental_learning_rate
    else:
        vocabulary = author_vocabulary(corpus, periods.cutoff, training.min_author_messages)
        model = HughMind(config.model, workspace.text_dim, len(vocabulary)).to(runtime.device)
        rows = periods.train
        epochs, learning_rate = training.epochs, training.learning_rate

    log.info(
        "%s training on %d examples (%d held out), %d regular members, encoder %s, %s",
        "incremental" if incremental else "full",
        len(rows),
        len(periods.valid),
        len(vocabulary),
        encoder or "off",
        runtime.description,
    )
    loss = fit(model, workspace, rows, vocabulary, config, runtime, epochs, learning_rate, rng)
    evaluation = evaluate(model, corpus, workspace.labels, periods.valid, vocabulary, runtime)
    metrics, calibration, thresholds, anomaly = summarise(evaluation, training.target_precision)
    challenger_score = metrics["actionable"]["average_precision"]

    champion_score = None
    if champion is not None:
        champion_corpus = corpus
        if champion["encoder"] != workspace.encoder:
            champion_corpus = _with_text(config, registry, corpus, champion["encoder"])
        champion_model, champion_vocabulary = restore(champion, runtime.device)
        champion_eval = evaluate(
            champion_model, champion_corpus, workspace.labels, periods.valid, champion_vocabulary, runtime
        )
        known = champion_eval.mask[:, ACTIONABLE] > 0
        champion_score = _clean(
            average_precision(champion_eval.logits[known, ACTIONABLE], champion_eval.target[known, ACTIONABLE])
        )
        del champion_model

    if challenger_score is None:
        promote = False
    elif champion is None:
        # The first model must at least beat guessing at the base rate.
        promote = challenger_score > (metrics["actionable"]["prevalence"] or 0.0)
    elif champion_score is None:
        # The live model cannot be scored on this period at all.
        promote = True
    else:
        promote = challenger_score >= champion_score - training.promotion_tolerance

    version = registry.new_version()
    summary = {
        "labels": metrics,
        "encoder": workspace.encoder,
        "mode": "incremental" if incremental else "full",
        "loss": loss,
        "examples": int(len(rows)),
        "held_out": int(len(periods.valid)),
        "members": len(vocabulary),
        "champion": champion_version,
        "champion_average_precision": champion_score,
        "promoted": promote,
        "device": runtime.description,
    }
    checkpoint = {
        "format": FORMAT,
        "version": version,
        "labels": LABELS,
        "model_config": asdict(model.config),
        "text_dim": workspace.text_dim,
        "authors": vocabulary,
        "state": {key: value.detach().cpu() for key, value in model.state_dict().items()},
        "calibration": calibration,
        "thresholds": thresholds,
        "anomaly": anomaly,
        "metrics": summary,
        "encoder": workspace.encoder,
        "trained_until": periods.cutoff,
        "trained_at": datetime.now(UTC).isoformat(timespec="seconds"),
    }
    registry.save_model(version, checkpoint)
    if promote:
        registry.promote(model=version)
        log.info(
            "promoted %s: average precision %.3f (live model had %s)",
            version,
            challenger_score,
            "none" if champion_score is None else f"{champion_score:.3f}",
        )
    else:
        log.info(
            "kept %s live: challenger %s scored %s against %s",
            champion_version,
            version,
            challenger_score,
            champion_score,
        )
    registry.prune(training.keep_versions)
    return version if promote else None


def _with_text(config: MindConfig, registry: Registry, corpus: Corpus, encoder: str | None) -> Corpus:
    if encoder is None:
        text = np.zeros((len(corpus), 1), dtype=np.float16)
        has_text = np.zeros(len(corpus), dtype=bool)
    else:
        dimension = encoder_dimension(registry.encoder_source(encoder))
        text, has_text = _load_text(config, corpus.ids, encoder, dimension)
    return replace(corpus, text=text, has_text=has_text)


def _clean(value: float) -> float | None:
    return None if value is None or not math.isfinite(value) else float(value)
